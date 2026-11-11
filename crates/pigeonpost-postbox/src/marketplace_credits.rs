//! Prepaid credits for publisher-hosted jobs (credits v1; design in
//! `docs/planning/2026-11-11-credits-payouts-backoffice.md` §3.5, contract with saas_be in the
//! billing bridge's README).
//!
//! Third-party services are sold only for credits. A publisher prices each operation of a
//! publisher-hosted listing **per job, in USD cents**; the price is mirrored to saas_be (through the
//! billing bridge, `store-adapter/`) as an immutable `per_job` offer. Running a priced operation:
//!
//! 1. Before the job is queued, the postbox asks for a **hold** of the offer's price on the buyer's
//!    credit (idempotency key = the job id). Too little credit: `402 insufficient_credit` with
//!    `available_cents`, `required_cents` and `buy_url`, and no job.
//! 2. A `succeeded` result delivered to the buyer: **capture** (saas_be journals the sale: 10% to
//!    Piyote, the rest payable to the publisher). `failed`, expired, service mailbox lost, cancelled
//!    before delivery, or the job never started: **release**.
//! 3. Captures and releases are an outbox (`nest_hosted_charges`): the worker retries each until
//!    saas_be answers, every call is idempotent there, and a charge row only ever moves forward, so
//!    a job is never charged twice and a hold is never left behind.
//!
//! Free operations (no price row) skip all of this. The money (balances, holds, sales, refunds)
//! lives only in saas_be; this module stores what it asked for and what it heard back.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET /v1/publishers/{publisher}/listings/{listing}/operations/{operation}/price` | `PublisherManage` | the publisher's team |
//! | `PUT\|DELETE /v1/publishers/{publisher}/listings/{listing}/operations/{operation}/price` | `NestPublish` | a certified publisher's admin, with step-up |

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, not_found, parse};
use crate::marketplace_m1::{conflict_code, digest_of};
use crate::marketplace_m2::{publisher_admin, publisher_read, release_in};
use crate::marketplace_pay::{billing_unavailable, release_line, BridgeError};
use crate::policy::{authorize, Action, Actor, Resource};
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};

/// The most one job may cost: $10,000.
pub(crate) const MAX_JOB_CENTS: i64 = 1_000_000;
/// A paid job's deadline is at most a week: saas_be holds expire after 8 days, so a hold always
/// outlives the job it pays for (contract: "default 8 days").
pub(crate) const PRICED_MAX_DEADLINE_HOURS: i64 = 7 * 24;
/// How long a hold may stay unconfirmed while its job is being admitted. Past this, a `holding`
/// row with no job is released by the worker.
const HOLD_GRACE_SECS: i64 = 300;

fn buy_url() -> String {
    std::env::var("POSTBOX_CREDITS_BUY_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://pigeonpost.dev/account#credits".into())
}

// ---- prices and offers ----------------------------------------------------------------------------

/// The saas_be offer id for one per-job price: listing, publisher, release line, operation and
/// amount. Any change is a new id (offers are immutable there).
pub(crate) fn offer_id_for(
    listing: &str,
    publisher: &str,
    line: &str,
    operation: &str,
    cents: i64,
) -> String {
    let h = digest_of(
        format!("pigeonpost-mp-offer\0{listing}\0{publisher}\0{line}\0per_job\0USD\0{cents}\0{operation}")
            .as_bytes(),
    );
    format!("mpo_{}", &h.trim_start_matches("sha256:")[..32])
}

/// What an operation should be on sale as now: its price on the listing's current release line.
struct Desired {
    offer_id: String,
    listing_id: String,
    operation: String,
    publisher_id: String,
    release_line: String,
    title: String,
    cents: i64,
}

fn desired_in(c: &Connection, listing: &str) -> GResult<Vec<Desired>> {
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT publisher_id, title, release_id FROM nest_listings WHERE id = ?1",
            [listing],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((publisher, title, release)) = row else {
        return Ok(Vec::new());
    };
    let rel = release_in(c, &release)?.ok_or_else(not_found)?;
    let line = release_line(&rel.version);
    let mut stmt = c.prepare(
        "SELECT operation, amount_cents FROM nest_hosted_prices WHERE listing_id = ?1
          ORDER BY operation",
    )?;
    let prices = stmt
        .query_map([listing], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(prices
        .into_iter()
        .map(|(op, cents)| {
            let label = rel.manifest["operations"]
                .as_array()
                .and_then(|ops| ops.iter().find(|o| o["name"] == op.as_str()))
                .and_then(|o| o["title"].as_str())
                .unwrap_or(&op)
                .to_string();
            Desired {
                offer_id: offer_id_for(listing, &publisher, &line, &op, cents),
                listing_id: listing.to_string(),
                operation: op,
                publisher_id: publisher.clone(),
                release_line: line.clone(),
                title: format!("{title} — {label}").chars().take(200).collect(),
                cents,
            }
        })
        .collect())
}

/// An operation's per-job price and, when it is on sale, the active offer a hold names.
pub(crate) struct JobPrice {
    pub(crate) cents: i64,
    pub(crate) offer: Option<String>,
}

pub(crate) fn job_price_in(
    c: &Connection,
    listing: &str,
    operation: &str,
) -> GResult<Option<JobPrice>> {
    let Some(d) = desired_in(c, listing)?
        .into_iter()
        .find(|d| d.operation == operation)
    else {
        return Ok(None);
    };
    let active: bool = c
        .query_row(
            "SELECT 1 FROM nest_hosted_offers WHERE offer_id = ?1 AND state = 'active'",
            [&d.offer_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok(Some(JobPrice {
        cents: d.cents,
        offer: active.then_some(d.offer_id),
    }))
}

/// The `price` of each operation of a publisher-hosted listing (`null`: free), for the listing
/// page and the Run form.
pub(crate) fn price_operations_in(c: &Connection, listing: &str, body: &mut Value) -> GResult<()> {
    let Some(ops) = body["operations"].as_array_mut() else {
        return Ok(());
    };
    for op in ops.iter_mut() {
        let name = op["name"].as_str().unwrap_or_default().to_string();
        op["price"] = match job_price_in(c, listing, &name)? {
            Some(p) => json!({
                "amount_cents": p.cents, "currency": "USD", "term": "per_job",
                "funding": "credits", "on_sale": p.offer.is_some(),
            }),
            None => Value::Null,
        };
    }
    Ok(())
}

/// For the staff review of an integration: the per-job prices its template's listings carry.
pub(crate) fn template_prices_in(c: &Connection, template: Option<&str>) -> GResult<Value> {
    let Some(template) = template else {
        return Ok(json!([]));
    };
    let mut stmt = c.prepare(
        "SELECT p.listing_id, p.operation, p.amount_cents, p.currency
           FROM nest_hosted_prices p JOIN nest_listings l ON l.id = p.listing_id
          WHERE l.template_id = ?1 ORDER BY p.listing_id, p.operation",
    )?;
    let rows = stmt
        .query_map([template], |r| {
            Ok(json!({
                "listing_id": r.get::<_, String>(0)?, "operation": r.get::<_, String>(1)?,
                "amount_cents": r.get::<_, i64>(2)?, "currency": r.get::<_, String>(3)?,
                "term": "per_job",
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Value::Array(rows))
}

fn offer_body(d: &Desired, status: &str) -> Value {
    json!({
        "listing_id": d.listing_id, "publisher_id": d.publisher_id, "release_line": d.release_line,
        "title": d.title, "term": "per_job", "currency": "USD", "price_minor": d.cents,
        "status": status,
    })
}

/// Mirror every priced operation's desired offer (through the bridge) and retire every other
/// offer of the listing. The first refusal is returned; what was refused stays `pending` with its
/// error and the worker tries again.
async fn sync_offers(state: &AppState, listing: &str) -> GResult<()> {
    let lid = listing.to_string();
    let desired = state.store.group_read(move |c| desired_in(c, &lid)).await?;
    let mut first_error: Option<GErr> = None;
    let now = now_unix() as i64;
    for d in &desired {
        let id = d.offer_id.clone();
        let active = state
            .store
            .group_read(move |c| -> GResult<bool> {
                Ok(c.query_row(
                    "SELECT 1 FROM nest_hosted_offers WHERE offer_id = ?1 AND state = 'active'",
                    [&id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
            })
            .await?;
        if active {
            continue;
        }
        let row = (
            d.offer_id.clone(),
            d.listing_id.clone(),
            d.operation.clone(),
            d.publisher_id.clone(),
            d.release_line.clone(),
            d.cents,
        );
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "INSERT INTO nest_hosted_offers (offer_id, listing_id, operation, publisher_id,
                        release_line, amount_cents, state, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?7)
                     ON CONFLICT(offer_id) DO UPDATE SET state = 'pending', updated_at = ?7",
                    params![row.0, row.1, row.2, row.3, row.4, row.5, now],
                )?;
                Ok(())
            })
            .await?;
        let answer = state
            .mp_bridge
            .call(
                reqwest::Method::PUT,
                &format!("/internal/marketplace/offers/{}", d.offer_id),
                offer_body(d, "active"),
            )
            .await;
        let outcome: Result<(), GErr> = match &answer {
            Ok((200, v))
                if v["offer"]["offer_id"] == d.offer_id.as_str()
                    && v["offer"]["price_minor"] == d.cents
                    && v["offer"]["term"] == "per_job"
                    && v["offer"]["currency"] == "USD" =>
            {
                Ok(())
            }
            Ok((200, _)) => Err(billing_unavailable()),
            Ok((s, v)) => Err(crate::marketplace_pay::bridge_refusal(*s, v)),
            Err(BridgeError::Unavailable) => Err(billing_unavailable()),
        };
        let (id, lid, op, ok) = (
            d.offer_id.clone(),
            d.listing_id.clone(),
            d.operation.clone(),
            outcome.is_ok(),
        );
        let code = outcome.as_ref().err().map(|e| e.code);
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                if ok {
                    tx.execute(
                        "UPDATE nest_hosted_offers SET state = 'active', last_error = NULL,
                            updated_at = ?2 WHERE offer_id = ?1",
                        params![id, now],
                    )?;
                    // The operation's earlier offers stop being sold at once.
                    tx.execute(
                        "UPDATE nest_hosted_offers SET state = 'retiring', updated_at = ?4
                          WHERE listing_id = ?1 AND operation = ?2 AND offer_id <> ?3
                            AND state IN ('active','pending')",
                        params![lid, op, id, now],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE nest_hosted_offers SET last_error = ?2, updated_at = ?3
                          WHERE offer_id = ?1",
                        params![id, code, now],
                    )?;
                }
                Ok(())
            })
            .await?;
        if let Err(e) = outcome {
            first_error.get_or_insert(e);
        }
    }
    retire_others(
        state,
        listing,
        desired.iter().map(|d| d.offer_id.clone()).collect(),
    )
    .await;
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Retire, in saas_be, every offer of the listing not in `keep`. A failure stays `retiring` and
/// the worker tries again; nothing can be held against it meanwhile (holds use the desired offer).
async fn retire_others(state: &AppState, listing: &str, keep: Vec<String>) {
    let lid = listing.to_string();
    let now = now_unix() as i64;
    type Row = (String, String, String, String, i64);
    let rows: Vec<Row> = state
        .store
        .group_tx(move |tx| -> GResult<Vec<Row>> {
            let mut stmt = tx.prepare(
                "SELECT offer_id FROM nest_hosted_offers WHERE listing_id = ?1
                   AND state IN ('active','pending')",
            )?;
            let live = stmt
                .query_map([&lid], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            for id in live.iter().filter(|id| !keep.contains(id)) {
                tx.execute(
                    "UPDATE nest_hosted_offers SET state = 'retiring', updated_at = ?2
                      WHERE offer_id = ?1",
                    params![id, now],
                )?;
            }
            let mut stmt = tx.prepare(
                "SELECT offer_id, operation, publisher_id, release_line, amount_cents
                   FROM nest_hosted_offers WHERE listing_id = ?1 AND state = 'retiring'",
            )?;
            let rows = stmt
                .query_map([&lid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<Row>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for (offer, operation, publisher, line, cents) in rows {
        let d = Desired {
            offer_id: offer.clone(),
            listing_id: listing.to_string(),
            operation,
            publisher_id: publisher,
            release_line: line,
            title: "retired offer".into(),
            cents,
        };
        let done = matches!(
            state
                .mp_bridge
                .call(
                    reqwest::Method::PUT,
                    &format!("/internal/marketplace/offers/{offer}"),
                    offer_body(&d, "retired"),
                )
                .await,
            Ok((200, _))
        );
        let now = now_unix() as i64;
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE nest_hosted_offers SET
                        state = CASE WHEN ?2 THEN 'retired' ELSE state END,
                        last_error = CASE WHEN ?2 THEN NULL ELSE 'retire_failed' END,
                        updated_at = ?3
                      WHERE offer_id = ?1",
                    params![offer, done, now],
                )?;
                Ok(())
            })
            .await;
    }
}

/// After a publisher-hosted listing is published (possibly on a new release line): mirror its
/// per-job offers. A failure does not undo the publication; the worker retries.
pub(crate) async fn after_publish(state: &AppState, listing: &str) {
    if !is_hosted(state, listing).await.unwrap_or(false) {
        return;
    }
    if let Err(e) = sync_offers(state, listing).await {
        tracing::warn!(
            listing,
            code = e.code,
            "per-job offers not mirrored after publish"
        );
    }
}

/// Listings whose per-job offers are not all mirrored (or not all retired) yet.
async fn offers_once(state: &AppState) -> usize {
    let listings: Vec<String> = state
        .store
        .group_read(move |c| -> GResult<Vec<String>> {
            let mut stmt = c.prepare(
                "SELECT DISTINCT listing_id FROM nest_hosted_prices
                 UNION SELECT DISTINCT listing_id FROM nest_hosted_offers
                  WHERE state IN ('pending','retiring')
                 LIMIT 50",
            )?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut due = Vec::new();
            for id in ids {
                let stale: bool = c
                    .query_row(
                        "SELECT 1 FROM nest_hosted_offers WHERE listing_id = ?1
                           AND state IN ('pending','retiring') LIMIT 1",
                        [&id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                let missing = desired_in(c, &id)?.iter().any(|d| {
                    c.query_row(
                        "SELECT 1 FROM nest_hosted_offers WHERE offer_id = ?1 AND state = 'active'",
                        [&d.offer_id],
                        |_| Ok(()),
                    )
                    .optional()
                    .ok()
                    .flatten()
                    .is_none()
                });
                if stale || missing {
                    due.push(id);
                }
            }
            Ok(due)
        })
        .await
        .unwrap_or_default();
    for l in &listings {
        if let Err(e) = sync_offers(state, l).await {
            tracing::warn!(
                listing = l,
                code = e.code,
                "per-job offer mirror will be retried"
            );
        }
    }
    listings.len()
}

// ---- the publisher's price routes -----------------------------------------------------------------

pub(crate) async fn is_hosted(state: &AppState, listing: &str) -> GResult<bool> {
    let l = listing.to_string();
    state
        .store
        .group_read(move |c| -> GResult<bool> {
            Ok(c.query_row(
                "SELECT hosting = 'publisher' FROM nest_listings WHERE id = ?1",
                [&l],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
        })
        .await
}

/// The publisher's own publisher-hosted listing and an operation its current integration
/// declares; anything else is `404`.
async fn operation_of(
    state: &AppState,
    publisher: &str,
    listing: &str,
    operation: &str,
) -> GResult<()> {
    let (l, p, o) = (
        listing.to_string(),
        publisher.to_string(),
        operation.to_string(),
    );
    state
        .store
        .group_read(move |c| -> GResult<()> {
            let release: Option<(String, String)> = c
                .query_row(
                    "SELECT release_id, hosting FROM nest_listings WHERE id = ?1
                       AND publisher_id = ?2",
                    params![l, p],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (release, hosting) = release.ok_or_else(not_found)?;
            if hosting != "publisher" {
                return Err(conflict_code(
                    "not_publisher_hosted",
                    "per-job prices are for publisher-hosted listings",
                ));
            }
            let rel = release_in(c, &release)?.ok_or_else(not_found)?;
            let declared = rel.manifest["operations"]
                .as_array()
                .is_some_and(|ops| ops.iter().any(|x| x["name"] == o.as_str()));
            if declared {
                Ok(())
            } else {
                Err(GErr::new(
                    StatusCode::NOT_FOUND,
                    "unknown_operation",
                    "this listing has no such operation",
                ))
            }
        })
        .await
}

fn price_view_in(c: &Connection, listing: &str, operation: &str) -> GResult<Value> {
    let desired = desired_in(c, listing)?
        .into_iter()
        .find(|d| d.operation == operation);
    #[allow(clippy::type_complexity)]
    let offer: Option<(String, String, Option<String>, String)> = match &desired {
        Some(d) => c
            .query_row(
                "SELECT offer_id, state, last_error, release_line FROM nest_hosted_offers
                  WHERE offer_id = ?1",
                [&d.offer_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?,
        None => None,
    };
    Ok(json!({
        "listing_id": listing,
        "operation": operation,
        "price": desired.as_ref().map(|d| json!({
            "amount_cents": d.cents, "currency": "USD", "term": "per_job",
        })),
        "offer": match (offer, &desired) {
            (Some((id, st, err, line)), _) => json!({
                "offer_id": id, "state": st, "on_sale": st == "active",
                "release_line": line, "error": err,
            }),
            (None, Some(d)) => json!({
                "offer_id": d.offer_id, "state": "pending", "on_sale": false,
                "release_line": d.release_line, "error": null,
            }),
            (None, None) => Value::Null,
        },
    }))
}

/// `GET /v1/publishers/{publisher}/listings/{listing}/operations/{operation}/price`
pub(crate) async fn read_job_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing, operation)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let f = crate::policy::Facts {
            publisher_permitted: publisher_read(&state, &headers, &account, &publisher).await,
            ..facts(&state)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::PublisherManage,
            &Resource::Publisher(&publisher),
            &f,
        )
        .map_err(|d| deny(d, true))?;
        operation_of(&state, &publisher, &listing, &operation).await?;
        let v = state
            .store
            .group_read(move |c| price_view_in(c, &listing, &operation))
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobPriceReq {
    amount_cents: i64,
    #[serde(default)]
    currency: Option<String>,
}

/// `PUT /v1/publishers/{publisher}/listings/{listing}/operations/{operation}/price` — what one job
/// of this operation costs, in USD cents. Mirrored to the financial authority as a `per_job` offer
/// before it stands; jobs already running keep the hold they were admitted with.
pub(crate) async fn set_job_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing, operation)): Path<(String, String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account, pid) =
            publisher_admin(&state, &headers, &publisher, Action::NestPublish).await?;
        let req: JobPriceReq = parse(body)?;
        if !(1..=MAX_JOB_CENTS).contains(&req.amount_cents) {
            return Err(GErr::bad(
                "invalid_request",
                "amount_cents: 1 to 1000000 (remove the price to make the operation free)",
            ));
        }
        if req.currency.as_deref().is_some_and(|c| c != "USD") {
            return Err(GErr::bad(
                "invalid_request",
                "currency: USD (credits are kept and spent in US dollars)",
            ));
        }
        operation_of(&state, &pid, &listing, &operation).await?;
        let (lid, op) = (listing.clone(), operation.clone());
        let previous: Option<i64> = state
            .store
            .group_read(move |c| -> GResult<Option<i64>> {
                Ok(c.query_row(
                    "SELECT amount_cents FROM nest_hosted_prices WHERE listing_id = ?1
                       AND operation = ?2",
                    params![lid, op],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await?;
        let now = now_unix() as i64;
        let (lid, op, who, cents) = (
            listing.clone(),
            operation.clone(),
            account.clone(),
            req.amount_cents,
        );
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "INSERT INTO nest_hosted_prices (listing_id, operation, amount_cents, currency,
                        updated_by, updated_at) VALUES (?1, ?2, ?3, 'USD', ?4, ?5)
                     ON CONFLICT(listing_id, operation) DO UPDATE SET
                        amount_cents = excluded.amount_cents, updated_by = excluded.updated_by,
                        updated_at = excluded.updated_at",
                    params![lid, op, cents, who, now],
                )?;
                Ok(())
            })
            .await?;
        if let Err(e) = sync_offers(&state, &listing).await {
            // A refused price never stands: put the previous one back.
            let (lid, op) = (listing.clone(), operation.clone());
            state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    match previous {
                        Some(p) => tx.execute(
                            "UPDATE nest_hosted_prices SET amount_cents = ?3
                              WHERE listing_id = ?1 AND operation = ?2",
                            params![lid, op, p],
                        )?,
                        None => tx.execute(
                            "DELETE FROM nest_hosted_prices WHERE listing_id = ?1
                               AND operation = ?2",
                            params![lid, op],
                        )?,
                    };
                    Ok(())
                })
                .await?;
            // The refused offer is not wanted any more; the restored one is mirrored again.
            let _ = sync_offers(&state, &listing).await;
            return Err(e);
        }
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            "marketplace.listing.job_price",
            Some(format!("listing:{listing}")),
            json!({ "operation": operation, "amount_cents": req.amount_cents, "currency": "USD",
                    "term": "per_job" }),
            None,
            crate::marketplace_m1::user_agent(&headers),
            now_unix(),
        );
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                crate::store::audit::append_in(tx, &audit)?;
                price_view_in(tx, &listing, &operation)
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/publishers/{publisher}/listings/{listing}/operations/{operation}/price` — make the
/// operation free. Its offer is retired; running jobs keep their holds.
pub(crate) async fn clear_job_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing, operation)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        let (principal, account, pid) =
            publisher_admin(&state, &headers, &publisher, Action::NestPublish).await?;
        operation_of(&state, &pid, &listing, &operation).await?;
        let (lid, op) = (listing.clone(), operation.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "DELETE FROM nest_hosted_prices WHERE listing_id = ?1 AND operation = ?2",
                    params![lid, op],
                )?;
                Ok(())
            })
            .await?;
        let _ = sync_offers(&state, &listing).await;
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            "marketplace.listing.job_price",
            Some(format!("listing:{listing}")),
            json!({ "operation": operation, "amount_cents": null }),
            None,
            crate::marketplace_m1::user_agent(&headers),
            now_unix(),
        );
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                crate::store::audit::append_in(tx, &audit)?;
                price_view_in(tx, &listing, &operation)
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

// ---- holds ----------------------------------------------------------------------------------------

fn owner(account: &str) -> Value {
    json!({ "owner_kind": "account", "owner_id": account })
}

fn not_on_sale() -> GErr {
    GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "not_on_sale",
        "this service's price is being updated; try again in a minute",
    )
}

/// Place the hold for `job` (about to be queued) on the buyer's credit: `Ok(Some(cents))` held,
/// `Ok(None)`: the operation is free. On an ambiguous answer the charge row stays `holding` and the worker releases whatever
/// was held, so a buyer who sees an error is never left with credit tied up.
pub(crate) async fn hold(
    state: &AppState,
    account: &str,
    listing: &str,
    operation: &str,
    job: &str,
) -> GResult<Option<i64>> {
    let (l, o) = (listing.to_string(), operation.to_string());
    let price = state
        .store
        .group_read(move |c| job_price_in(c, &l, &o))
        .await?;
    let Some(price) = price else {
        return Ok(None);
    };
    let offer = price.offer.ok_or_else(not_on_sale)?;
    let now = now_unix() as i64;
    let (j, a, l, of, cents) = (
        job.to_string(),
        account.to_string(),
        listing.to_string(),
        offer.clone(),
        price.cents,
    );
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "INSERT INTO nest_hosted_charges (job_id, account_id, listing_id, offer_id,
                    amount_cents, state, next_attempt_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'holding', ?6, ?7, ?7)",
                params![j, a, l, of, cents, now + HOLD_GRACE_SECS, now],
            )?;
            Ok(())
        })
        .await?;
    let answer = state
        .mp_bridge
        .call(
            reqwest::Method::POST,
            "/internal/credits/holds",
            json!({ "owner": owner(account), "offer_id": offer, "job_ref": job,
                    "idempotency_key": job }),
        )
        .await;
    let j = job.to_string();
    match answer {
        Ok((200 | 201, v)) if v["hold_id"].as_str().is_some_and(|h| !h.is_empty()) => {
            let hold_id = v["hold_id"].as_str().unwrap_or_default().to_string();
            let expires = v["expires_at"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.timestamp());
            let same = v["amount_cents"].as_i64() == Some(price.cents);
            state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    tx.execute(
                        "UPDATE nest_hosted_charges SET hold_id = ?2, hold_expires_at = ?3,
                            state = CASE WHEN ?4 THEN state ELSE 'release_pending' END,
                            release_reason = CASE WHEN ?4 THEN NULL ELSE 'price_mismatch' END,
                            next_attempt_at = CASE WHEN ?4 THEN next_attempt_at ELSE ?5 END,
                            updated_at = ?5
                          WHERE job_id = ?1",
                        params![j, hold_id, expires, same, now_unix() as i64],
                    )?;
                    Ok(())
                })
                .await?;
            if !same {
                tracing::warn!(job, "credit hold for a different amount; releasing it");
                return Err(not_on_sale());
            }
            Ok(Some(price.cents))
        }
        Ok((status, v)) if (400..500).contains(&status) => {
            // A definite refusal: nothing is held.
            state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    tx.execute("DELETE FROM nest_hosted_charges WHERE job_id = ?1", [&j])?;
                    Ok(())
                })
                .await?;
            if status == 402 {
                let detail = |k: &str| v[k].as_i64().or_else(|| v["details"][k].as_i64());
                return Err(GErr::new(
                    StatusCode::PAYMENT_REQUIRED,
                    "insufficient_credit",
                    "your credit balance is too low for this job; buy credits and run it again",
                )
                .with("available_cents", detail("available_cents").unwrap_or(0))
                .with(
                    "required_cents",
                    detail("required_cents").unwrap_or(price.cents),
                )
                .with("buy_url", buy_url()));
            }
            tracing::warn!(job, status, "credit hold refused");
            Err(if status == 404 {
                not_on_sale()
            } else {
                billing_unavailable()
            })
        }
        _ => {
            // No answer, or a server error: the hold may exist. The worker settles it.
            state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    tx.execute(
                        "UPDATE nest_hosted_charges SET state = 'release_pending',
                            release_reason = 'job_not_started', next_attempt_at = ?2,
                            updated_at = ?2
                          WHERE job_id = ?1 AND state = 'holding'",
                        params![j, now_unix() as i64],
                    )?;
                    Ok(())
                })
                .await?;
            Err(billing_unavailable())
        }
    }
}

/// In the transaction that queues the job: its hold now pays for it.
pub(crate) fn attach_in(tx: &Transaction<'_>, job: &str, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE nest_hosted_charges SET state = 'held', updated_at = ?2
          WHERE job_id = ?1 AND state = 'holding'",
        params![job, now],
    )?;
    Ok(())
}

/// The job was not queued after all: release its hold.
pub(crate) async fn abandon(state: &AppState, job: &str) {
    let j = job.to_string();
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "UPDATE nest_hosted_charges SET state = 'release_pending',
                    release_reason = 'job_not_started', next_attempt_at = ?2, updated_at = ?2
                  WHERE job_id = ?1 AND state = 'holding'",
                params![j, now_unix() as i64],
            )?;
            Ok(())
        })
        .await;
}

/// A job's charge as its buyer sees it: `held` (reserved), `charged` or `released`.
pub(crate) fn charge_json_in(c: &Connection, job: &str) -> rusqlite::Result<Value> {
    let row: Option<(i64, String, Option<String>)> = c
        .query_row(
            "SELECT amount_cents, state, last_error FROM nest_hosted_charges WHERE job_id = ?1",
            [job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match row {
        None => Value::Null,
        Some((cents, st, err)) => json!({
            "amount_cents": cents,
            "currency": "USD",
            "status": public_status(&st, err.as_deref()),
        }),
    })
}

fn public_status(state: &str, error: Option<&str>) -> &'static str {
    match state {
        "holding" | "held" | "capture_pending" => "held",
        "captured" => "charged",
        "refused" if error == Some("hold_captured") => "charged",
        _ => "released",
    }
}

/// `(state, last_error)` of a job's charge, for staff.
pub(crate) fn staff_charge_in(
    c: &Connection,
    job: &str,
) -> rusqlite::Result<(Option<String>, Option<String>)> {
    Ok(c.query_row(
        "SELECT state, last_error FROM nest_hosted_charges WHERE job_id = ?1",
        [job],
        |r| Ok((Some(r.get(0)?), r.get(1)?)),
    )
    .optional()?
    .unwrap_or((None, None)))
}

// ---- the outbox -----------------------------------------------------------------------------------

fn backoff(attempts: i64) -> i64 {
    (30_i64 << attempts.clamp(0, 7)).min(3600)
}

/// Move held charges after their jobs, then send every due capture and release. Safe to run
/// concurrently and repeatedly: each call to saas_be is idempotent, and a row only moves forward.
pub(crate) async fn settle_once(state: &AppState) -> usize {
    let now = now_unix() as i64;
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            // A succeeded result delivered to the buyer (or, after the last attempt, kept on the
            // job page for them): capture.
            tx.execute(
                "UPDATE nest_hosted_charges SET state = 'capture_pending', attempts = 0,
                    next_attempt_at = ?1, updated_at = ?1
                  WHERE state = 'held' AND job_id IN (SELECT id FROM nest_hosted_jobs
                    WHERE state = 'succeeded' AND delivery IN ('delivered','undeliverable'))",
                [now],
            )?;
            // Failed, expired, service lost, cancelled: release.
            tx.execute(
                "UPDATE nest_hosted_charges SET state = 'release_pending', attempts = 0,
                    release_reason = (SELECT COALESCE(j.error_code, j.state) FROM nest_hosted_jobs j
                                       WHERE j.id = nest_hosted_charges.job_id),
                    next_attempt_at = ?1, updated_at = ?1
                  WHERE state = 'held' AND job_id IN (SELECT id FROM nest_hosted_jobs
                    WHERE state IN ('failed','expired','cancelled'))",
                [now],
            )?;
            // Admission never finished (the request died between hold and queue): release.
            tx.execute(
                "UPDATE nest_hosted_charges SET state = 'release_pending',
                    release_reason = 'job_not_started', updated_at = ?1
                  WHERE state = 'holding' AND next_attempt_at <= ?1
                    AND job_id NOT IN (SELECT id FROM nest_hosted_jobs)",
                [now],
            )?;
            tx.execute(
                "UPDATE nest_hosted_charges SET state = 'held', updated_at = ?1
                  WHERE state = 'holding' AND next_attempt_at <= ?1 AND hold_id IS NOT NULL
                    AND job_id IN (SELECT id FROM nest_hosted_jobs)",
                [now],
            )?;
            Ok(())
        })
        .await;
    type Due = (
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        i64,
    );
    let due: Vec<Due> = state
        .store
        .group_read(move |c| -> GResult<Vec<Due>> {
            let mut stmt = c.prepare(
                "SELECT job_id, state, hold_id, offer_id, account_id, release_reason, attempts
                   FROM nest_hosted_charges
                  WHERE state IN ('capture_pending','release_pending') AND next_attempt_at <= ?1
                  ORDER BY next_attempt_at LIMIT 50",
            )?;
            let rows = stmt
                .query_map([now], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    let mut done = 0;
    for (job, st, hold, offer, account, reason, attempts) in due {
        if settle(state, &job, &st, hold, &offer, &account, reason, attempts).await {
            done += 1;
        }
    }
    done
}

enum Outcome {
    Captured(Value),
    Released,
    /// No hold existed (a replayed hold was refused): nothing to release.
    NothingHeld,
    Refused(String),
    Retry(String),
}

#[allow(clippy::too_many_arguments)]
async fn settle(
    state: &AppState,
    job: &str,
    st: &str,
    hold: Option<String>,
    offer: &str,
    account: &str,
    reason: Option<String>,
    attempts: i64,
) -> bool {
    let bridge = &state.mp_bridge;
    // An unconfirmed hold: replay it (same idempotency key, so saas_be answers with the hold it
    // made, or makes one) to learn its id, then release it.
    let hold = match hold {
        Some(h) => Some(h),
        None => match bridge
            .call(
                reqwest::Method::POST,
                "/internal/credits/holds",
                json!({ "owner": owner(account), "offer_id": offer, "job_ref": job,
                        "idempotency_key": job }),
            )
            .await
        {
            Ok((200 | 201, v)) => v["hold_id"].as_str().map(str::to_string),
            Ok((s, _)) if (400..500).contains(&s) => None,
            _ => {
                record(
                    state,
                    job,
                    Outcome::Retry("bridge_unavailable".into()),
                    attempts,
                )
                .await;
                return false;
            }
        },
    };
    let outcome = match (st, hold) {
        (_, None) => Outcome::NothingHeld,
        ("capture_pending", Some(h)) => {
            match bridge
                .call(
                    reqwest::Method::POST,
                    &format!("/internal/credits/holds/{h}/capture"),
                    json!({ "job_ref": job }),
                )
                .await
            {
                Ok((200 | 201, v)) => Outcome::Captured(v),
                Ok((s @ (400 | 404 | 409 | 422), v)) => Outcome::Refused(
                    v["code"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("capture_{s}")),
                ),
                Ok((s, _)) => Outcome::Retry(format!("bridge_{s}")),
                Err(BridgeError::Unavailable) => Outcome::Retry("bridge_unavailable".into()),
            }
        }
        (_, Some(h)) => {
            match bridge
                .call(
                    reqwest::Method::POST,
                    &format!("/internal/credits/holds/{h}/release"),
                    json!({ "reason": reason.unwrap_or_else(|| "released".into()) }),
                )
                .await
            {
                Ok((200 | 201, _)) => Outcome::Released,
                // A captured hold cannot be released; it was paid for.
                Ok((409, _)) => Outcome::Refused("hold_captured".into()),
                Ok((s @ (400 | 404 | 422), v)) => Outcome::Refused(
                    v["code"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("release_{s}")),
                ),
                Ok((s, _)) => Outcome::Retry(format!("bridge_{s}")),
                Err(BridgeError::Unavailable) => Outcome::Retry("bridge_unavailable".into()),
            }
        }
    };
    let settled = !matches!(outcome, Outcome::Retry(_));
    if let Outcome::Refused(code) = &outcome {
        tracing::warn!(
            job,
            code,
            "credit charge refused for good; staff will see it"
        );
    }
    record(state, job, outcome, attempts).await;
    settled
}

async fn record(state: &AppState, job: &str, outcome: Outcome, attempts: i64) {
    let (j, now) = (job.to_string(), now_unix() as i64);
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            match outcome {
                Outcome::Captured(v) => tx.execute(
                    "UPDATE nest_hosted_charges SET state = 'captured', sale_id = ?2,
                        commission_cents = ?3, publisher_cents = ?4, last_error = NULL,
                        settled_at = ?5, updated_at = ?5
                      WHERE job_id = ?1 AND state = 'capture_pending'",
                    params![
                        j,
                        v["sale_id"].as_str(),
                        v["commission_cents"].as_i64(),
                        v["publisher_cents"].as_i64(),
                        now
                    ],
                )?,
                Outcome::Released | Outcome::NothingHeld => tx.execute(
                    "UPDATE nest_hosted_charges SET state = 'released', last_error = NULL,
                        settled_at = ?2, updated_at = ?2
                      WHERE job_id = ?1 AND state = 'release_pending'",
                    params![j, now],
                )?,
                Outcome::Refused(code) => tx.execute(
                    "UPDATE nest_hosted_charges SET state = 'refused', last_error = ?2,
                        settled_at = ?3, updated_at = ?3
                      WHERE job_id = ?1 AND state IN ('capture_pending','release_pending')",
                    params![j, code, now],
                )?,
                Outcome::Retry(code) => tx.execute(
                    "UPDATE nest_hosted_charges SET attempts = attempts + 1, last_error = ?2,
                        next_attempt_at = ?3, updated_at = ?4
                      WHERE job_id = ?1 AND state IN ('capture_pending','release_pending')",
                    params![j, code, now + backoff(attempts), now],
                )?,
            };
            Ok(())
        })
        .await;
}

/// The worker's turn (with the hosted-job worker): mirror offers that are behind, then settle.
pub(crate) async fn work_once(state: &AppState) -> usize {
    offers_once(state).await + settle_once(state).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_ids_change_with_every_immutable_field() {
        let a = offer_id_for("lst_a", "pub_a", "v1", "hr.research", 2500);
        assert_eq!(a, offer_id_for("lst_a", "pub_a", "v1", "hr.research", 2500));
        assert!(a.starts_with("mpo_") && a.len() == 36);
        for other in [
            offer_id_for("lst_b", "pub_a", "v1", "hr.research", 2500),
            offer_id_for("lst_a", "pub_b", "v1", "hr.research", 2500),
            offer_id_for("lst_a", "pub_a", "v2", "hr.research", 2500),
            offer_id_for("lst_a", "pub_a", "v1", "hr.other", 2500),
            offer_id_for("lst_a", "pub_a", "v1", "hr.research", 2501),
            crate::marketplace_pay::offer_id_for("lst_a", "pub_a", "v1", "one_off", "USD", 2500),
        ] {
            assert_ne!(a, other);
        }
    }

    #[test]
    fn buyers_see_three_charge_states() {
        assert_eq!(public_status("holding", None), "held");
        assert_eq!(public_status("capture_pending", None), "held");
        assert_eq!(public_status("captured", None), "charged");
        assert_eq!(public_status("release_pending", None), "released");
        assert_eq!(public_status("refused", Some("hold_captured")), "charged");
        assert_eq!(public_status("refused", Some("hold_expired")), "released");
        assert_eq!(backoff(0), 30);
        assert_eq!(backoff(20), 3600);
    }
}
