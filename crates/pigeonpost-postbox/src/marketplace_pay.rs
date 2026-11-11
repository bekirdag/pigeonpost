//! Marketplace payments (M3): paid third-party listings, one-off and monthly. No wallet.
//!
//! The postbox stays the authority for listings, prices as publishers set them, and
//! installations. Money is the financial authority's (saas_be marketplace finance), reached only
//! through the billing bridge (`store-adapter/`), the one Pigeonpost component that talks to
//! saas_be. The postbox calls the bridge's internal routes with `POSTBOX_BRIDGE_TOKEN`; the bridge
//! calls this module's internal route with the billing service token.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET /v1/publishers/{publisher}/listings/{listing}/price` | `PublisherManage` | the publisher's team |
//! | `PUT\|DELETE /v1/publishers/{publisher}/listings/{listing}/price` | `NestPublish` | a certified publisher's admin, with step-up |
//! | `POST /v1/marketplace/purchases` | `NestInstall` | a person, for their own account (the bridge calls it with the buyer's token) |
//! | `PUT /internal/v1/marketplace/licences/{licence}` | `ManageBilling` | the billing bridge |
//!
//! The flow:
//! 1. A publisher prices a listing. The price is mirrored to saas_be as an immutable offer whose id
//!    is derived from listing, publisher, release line, term, currency and amount, so a change is
//!    a new offer and the old one is retired. A price saas_be cannot invoice exactly is refused
//!    with `422 price_not_invoiceable` and the `nearest` prices that can be.
//! 2. A buyer's purchase (`POST /v1/marketplace/purchases`, through the bridge) reserves the
//!    installation id the payment will admit and fixes the intent: `{action: install, listing_id,
//!    release_line, owner_kind, owner_id, installation_id}`. The bridge quotes and checks out.
//! 3. Installing a priced listing (`POST /v1/nest-installations` with `purchase_id`) consumes the
//!    payment's authorization through the bridge for exactly that intent (single use, idempotent by
//!    the installation id) and records the licence. A replay returns the installation.
//! 4. The bridge's reconciler projects each licence here. Without access the worker is stopped and
//!    no job is admitted (`402 licence_inactive`); with access again it runs on the next job; past
//!    the deletion boundary the installation is uninstalled and the worker and its data removed.
//!    A sweep here enforces the boundary even if no new projection arrives.
//!
//! An organization buys too (M2b's org installations): the buyer must be one of the org's Nest
//! admins who also manages its billing (`ManageAgents` and `ManageBilling`; the bridge checks the
//! billing side again), the purchase's owner is `org:<id>`, and the org install
//! (`POST /v1/orgs/{org}/nest-installations` with `purchase_id`) consumes it.

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, iso, not_found, parse};
use crate::marketplace_m1::{conflict_code, flag_gate};
use crate::marketplace_m2::{
    live_attested_in, not_runnable, publisher_admin, publisher_read, release_in,
};
use crate::policy::{authorize, Action, Actor, Resource};
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{post, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publishers/{publisher}/listings/{listing}/price",
            put(set_price).get(read_price).delete(clear_price),
        )
        .route("/v1/marketplace/purchases", post(purchase))
}

pub(crate) fn internal_router() -> Router<AppState> {
    Router::new().route(
        "/internal/v1/marketplace/licences/{licence}",
        put(project_licence),
    )
}

// ---- the bridge client ------------------------------------------------------------------------------

enum Transport {
    Disabled,
    Http {
        url: String,
        token: String,
        client: reqwest::Client,
    },
}

/// The billing bridge's internal marketplace routes (`POSTBOX_BRIDGE_URL`, `POSTBOX_BRIDGE_TOKEN`
/// or `POSTBOX_BRIDGE_TOKEN_FILE`). Unset: priced listings cannot be mirrored or sold.
pub(crate) struct Bridge {
    transport: Transport,
}

impl Default for Bridge {
    fn default() -> Self {
        Bridge {
            transport: Transport::Disabled,
        }
    }
}

/// Why a bridge call produced no answer.
enum BridgeError {
    Unavailable,
}

impl Bridge {
    pub(crate) fn from_env() -> Self {
        let url = std::env::var("POSTBOX_BRIDGE_URL").unwrap_or_default();
        let token = match std::env::var("POSTBOX_BRIDGE_TOKEN_FILE") {
            Ok(path) if !path.trim().is_empty() => {
                std::fs::read_to_string(path.trim()).unwrap_or_default()
            }
            _ => std::env::var("POSTBOX_BRIDGE_TOKEN").unwrap_or_default(),
        };
        if url.trim().is_empty() && token.trim().is_empty() {
            return Bridge::default();
        }
        let b = Bridge::http(&url, &token);
        if matches!(b.transport, Transport::Disabled) {
            tracing::warn!("POSTBOX_BRIDGE_URL/POSTBOX_BRIDGE_TOKEN unusable; paid listings off");
        }
        b
    }

    /// `https://…` (or plain HTTP to loopback / a private address) and a token of 32+ characters.
    pub(crate) fn http(url: &str, token: &str) -> Self {
        let url = url.trim().trim_end_matches('/').to_string();
        let token = token.trim().to_string();
        if !crate::nests::url_ok(&url) || token.len() < 32 {
            return Bridge::default();
        }
        match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(70))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => Bridge {
                transport: Transport::Http { url, token, client },
            },
            Err(_) => Bridge::default(),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> Result<(u16, Value), BridgeError> {
        let Transport::Http { url, token, client } = &self.transport else {
            return Err(BridgeError::Unavailable);
        };
        let resp = client
            .request(method, format!("{url}{path}"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|_| BridgeError::Unavailable)?;
        let status = resp.status().as_u16();
        let v = resp.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, v))
    }
}

fn billing_unavailable() -> GErr {
    GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "billing_unavailable",
        "payments are not answering right now; try again shortly",
    )
}

/// The bridge's refusal codes this module passes on, as static codes.
fn bridge_code(v: &Value) -> &'static str {
    match v["code"].as_str().unwrap_or_default() {
        "price_not_invoiceable" => "price_not_invoiceable",
        "currency_unavailable" => "currency_unavailable",
        "intent_mismatch" => "intent_mismatch",
        "authorization_used" => "authorization_used",
        "payment_required" => "payment_required",
        "payment_pending" => "payment_pending",
        "payment_failed" => "payment_failed",
        "payment_revoked" => "payment_revoked",
        "payment_expired" => "payment_expired",
        "not_found" => "not_found",
        _ => "billing_refused",
    }
}

fn bridge_refusal(status: u16, v: &Value) -> GErr {
    let message = v["error"]
        .as_str()
        .unwrap_or("payments refused this")
        .chars()
        .take(300)
        .collect::<String>();
    match status {
        402 => GErr::new(StatusCode::PAYMENT_REQUIRED, bridge_code(v), message),
        404 => GErr::new(StatusCode::NOT_FOUND, "not_found", "no such purchase"),
        409 => GErr::conflict(bridge_code(v), message),
        422 => GErr::new(StatusCode::UNPROCESSABLE_ENTITY, bridge_code(v), message)
            .with("nearest", v["nearest"].clone()),
        _ => billing_unavailable(),
    }
}

// ---- prices and offers ----------------------------------------------------------------------------

const TERMS: [&str; 2] = ["one_off", "monthly"];
const CURRENCIES: [&str; 3] = ["TRY", "USD", "EUR"];

/// The release line a one-off licence covers: the release's major version (`v1` for `1.4.2`).
pub(crate) fn release_line(version: &str) -> String {
    let major = version.split('.').next().unwrap_or("0");
    let major: String = major.chars().filter(|c| c.is_ascii_digit()).collect();
    format!("v{}", if major.is_empty() { "0" } else { &major })
}

/// The saas_be offer id for one combination of the immutable offer fields.
pub(crate) fn offer_id_for(
    listing: &str,
    publisher: &str,
    line: &str,
    term: &str,
    currency: &str,
    amount: i64,
) -> String {
    let h = crate::marketplace_m1::digest_of(
        format!(
            "pigeonpost-mp-offer\0{listing}\0{publisher}\0{line}\0{term}\0{currency}\0{amount}"
        )
        .as_bytes(),
    );
    let hex = h.trim_start_matches("sha256:");
    format!("mpo_{}", &hex[..32])
}

#[derive(Debug, Clone)]
struct Price {
    amount_minor: i64,
    currency: String,
    term: String,
}

fn price_in(c: &Connection, listing: &str) -> rusqlite::Result<Option<Price>> {
    c.query_row(
        "SELECT amount_minor, currency, term FROM nest_listing_prices WHERE listing_id = ?1",
        [listing],
        |r| {
            Ok(Price {
                amount_minor: r.get(0)?,
                currency: r.get(1)?,
                term: r.get(2)?,
            })
        },
    )
    .optional()
}

/// Whether the publisher priced this listing (publisher-hosted jobs refuse priced listings until
/// prepaid credits exist).
pub(crate) fn is_priced_in(c: &Connection, listing: &str) -> rusqlite::Result<bool> {
    Ok(price_in(c, listing)?.is_some())
}

/// The one price model (`ListingPrice`): the publisher's price (`funding: card`), or zero (test
/// credits) with the listing's preferred currency and term for a free one (M2b).
pub(crate) fn price_json_in(c: &Connection, listing: &str) -> rusqlite::Result<Value> {
    Ok(match price_in(c, listing)? {
        Some(p) => json!({
            "amount_minor": p.amount_minor, "currency": p.currency, "term": p.term,
            "funding": "card", "on_sale": sellable_offer_in(c, listing)?.is_some(),
        }),
        None => {
            let terms: Option<(String, String)> = c
                .query_row(
                    "SELECT price_currency, price_term FROM nest_listings WHERE id = ?1",
                    [listing],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (currency, term) = terms.unwrap_or_else(|| ("TRY".into(), "one_off".into()));
            crate::marketplace_listings::price_json(currency, term)
        }
    })
}

/// Put the listing's real price on a listing object that carries its `id` (the row mappers only
/// see the free terms).
pub(crate) fn with_price_in(c: &Connection, mut listing: Value) -> rusqlite::Result<Value> {
    if let Some(id) = listing["id"].as_str().map(str::to_string) {
        listing["price"] = price_json_in(c, &id)?;
    }
    Ok(listing)
}

/// The offer a listing should have now: its price on its current release line.
struct Desired {
    offer_id: String,
    listing_id: String,
    publisher_id: String,
    release_line: String,
    title: String,
    price: Price,
}

fn desired_in(c: &Connection, listing: &str) -> GResult<Option<Desired>> {
    let Some(price) = price_in(c, listing)? else {
        return Ok(None);
    };
    let (publisher, title, release): (String, String, String) = c.query_row(
        "SELECT publisher_id, title, release_id FROM nest_listings WHERE id = ?1",
        [listing],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let rel = release_in(c, &release)?.ok_or_else(not_found)?;
    let line = release_line(&rel.version);
    Ok(Some(Desired {
        offer_id: offer_id_for(
            listing,
            &publisher,
            &line,
            &price.term,
            &price.currency,
            price.amount_minor,
        ),
        listing_id: listing.to_string(),
        publisher_id: publisher,
        release_line: line,
        title,
        price,
    }))
}

/// `(offer_id, term, currency, amount, release_line)` of the listing's active, sellable offer.
#[allow(clippy::type_complexity)]
fn sellable_offer_in(
    c: &Connection,
    listing: &str,
) -> rusqlite::Result<Option<(String, String, String, i64, String)>> {
    c.query_row(
        "SELECT offer_id, term, currency, amount_minor, release_line FROM nest_listing_offers
          WHERE listing_id = ?1 AND state = 'active' AND sellable = 1
          ORDER BY updated_at DESC LIMIT 1",
        [listing],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .optional()
}

fn offer_body(d: &Desired, status: &str) -> Value {
    json!({
        "listing_id": d.listing_id, "publisher_id": d.publisher_id, "release_line": d.release_line,
        "title": d.title, "term": d.price.term, "currency": d.price.currency,
        "price_minor": d.price.amount_minor, "status": status,
    })
}

/// Mirror the listing's desired offer to saas_be (through the bridge) and retire the others.
/// `Ok(false)`: the listing has no price. A refusal leaves what was there.
async fn sync_offer(state: &AppState, listing: &str) -> GResult<bool> {
    let lid = listing.to_string();
    let desired = state.store.group_read(move |c| desired_in(c, &lid)).await?;
    let Some(d) = desired else {
        retire_others(state, listing, None).await;
        return Ok(false);
    };
    let (status, v) = state
        .mp_bridge
        .call(
            reqwest::Method::PUT,
            &format!("/internal/marketplace/offers/{}", d.offer_id),
            offer_body(&d, "active"),
        )
        .await
        .map_err(|BridgeError::Unavailable| billing_unavailable())?;
    if status != 200 {
        return Err(bridge_refusal(status, &v));
    }
    let offer = &v["offer"];
    if offer["offer_id"] != d.offer_id.as_str()
        || offer["price_minor"] != d.price.amount_minor
        || offer["term"] != d.price.term.as_str()
    {
        return Err(billing_unavailable());
    }
    let sellable = offer["sellable"].as_bool() == Some(true);
    let now = now_unix() as i64;
    let row = (
        d.offer_id.clone(),
        d.listing_id.clone(),
        d.publisher_id.clone(),
    );
    let (line, term, cur, amount) = (
        d.release_line.clone(),
        d.price.term.clone(),
        d.price.currency.clone(),
        d.price.amount_minor,
    );
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "INSERT INTO nest_listing_offers (offer_id, listing_id, publisher_id, release_line,
                    term, currency, amount_minor, state, sellable, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?9)
                 ON CONFLICT(offer_id) DO UPDATE SET state = 'active', sellable = excluded.sellable,
                    last_error = NULL, updated_at = excluded.updated_at",
                params![
                    row.0,
                    row.1,
                    row.2,
                    line,
                    term,
                    cur,
                    amount,
                    sellable as i64,
                    now
                ],
            )?;
            tx.execute(
                "UPDATE nest_listing_offers SET state = 'retiring', updated_at = ?3
                  WHERE listing_id = ?1 AND offer_id <> ?2 AND state = 'active'",
                params![row.1, row.0, now],
            )?;
            Ok(())
        })
        .await?;
    retire_others(state, listing, Some(&d.offer_id)).await;
    Ok(true)
}

/// Retire, in saas_be, every offer of the listing but `keep`. Best effort: one that cannot be
/// retired now stays `retiring` and the sweep tries again. Nothing new can be bought from it in
/// the meantime, because every purchase starts here on the active offer.
async fn retire_others(state: &AppState, listing: &str, keep: Option<&str>) {
    let (lid, keep) = (listing.to_string(), keep.map(str::to_string));
    let now = now_unix() as i64;
    type Row = (String, String, String, String, String, i64, String);
    let rows: Vec<Row> = state
        .store
        .group_tx(move |tx| -> GResult<Vec<Row>> {
            tx.execute(
                "UPDATE nest_listing_offers SET state = 'retiring', updated_at = ?3
                  WHERE listing_id = ?1 AND state = 'active' AND offer_id IS NOT ?2",
                params![lid, keep, now],
            )?;
            let mut stmt = tx.prepare(
                "SELECT o.offer_id, o.publisher_id, o.release_line, o.term, o.currency,
                        o.amount_minor, l.title
                   FROM nest_listing_offers o JOIN nest_listings l ON l.id = o.listing_id
                  WHERE o.listing_id = ?1 AND o.state = 'retiring'",
            )?;
            let rows = stmt
                .query_map([&lid], |r| {
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
                .collect::<rusqlite::Result<Vec<Row>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for (offer, publisher, line, term, currency, amount, title) in rows {
        let d = Desired {
            offer_id: offer.clone(),
            listing_id: listing.to_string(),
            publisher_id: publisher,
            release_line: line,
            title,
            price: Price {
                amount_minor: amount,
                currency,
                term,
            },
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
                    "UPDATE nest_listing_offers SET state = CASE WHEN ?2 THEN 'retired' ELSE state END,
                        sellable = CASE WHEN ?2 THEN 0 ELSE sellable END,
                        last_error = CASE WHEN ?2 THEN NULL ELSE 'retire_failed' END, updated_at = ?3
                      WHERE offer_id = ?1",
                    params![offer, done, now],
                )?;
                Ok(())
            })
            .await;
    }
}

/// After a listing is published (M2 `publish_listing`): mirror its price on the release line it
/// now serves. A failure does not undo the publication; the listing is just not on sale until the
/// sweep (or the publisher) mirrors it.
pub(crate) async fn after_publish(state: &AppState, listing: &str) {
    if let Err(e) = sync_offer(state, listing).await {
        tracing::warn!(
            listing,
            code = e.code,
            "marketplace offer not mirrored after publish"
        );
        let (lid, code) = (listing.to_string(), e.code);
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE nest_listing_offers SET last_error = ?2 WHERE listing_id = ?1
                       AND state = 'active'",
                    params![lid, code],
                )?;
                Ok(())
            })
            .await;
    }
}

fn price_view_in(c: &Connection, listing: &str) -> GResult<Value> {
    let price = price_in(c, listing)?;
    let desired = desired_in(c, listing)?;
    #[allow(clippy::type_complexity)]
    let offer: Option<(String, String, i64, Option<String>, String)> = match &desired {
        Some(d) => c
            .query_row(
                "SELECT offer_id, state, sellable, last_error, release_line FROM nest_listing_offers
                  WHERE offer_id = ?1",
                [&d.offer_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?,
        None => None,
    };
    Ok(json!({
        "listing_id": listing,
        "price": price.map(|p| json!({ "amount_minor": p.amount_minor, "currency": p.currency, "term": p.term })),
        "offer": match (offer, &desired) {
            (Some((id, st, sellable, err, line)), _) => json!({
                "offer_id": id, "state": st, "on_sale": st == "active" && sellable == 1,
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

async fn listing_of(state: &AppState, listing: &str, publisher: &str) -> GResult<()> {
    let (l, p) = (listing.to_string(), publisher.to_string());
    let found = state
        .store
        .group_read(move |c| -> GResult<bool> {
            Ok(c.query_row(
                "SELECT 1 FROM nest_listings WHERE id = ?1 AND publisher_id = ?2",
                params![l, p],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })
        .await?;
    if found {
        Ok(())
    } else {
        Err(not_found())
    }
}

/// `GET /v1/publishers/{publisher}/listings/{listing}/price`
async fn read_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
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
        listing_of(&state, &listing, &publisher).await?;
        let v = state
            .store
            .group_read(move |c| price_view_in(c, &listing))
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PriceReq {
    amount_minor: i64,
    currency: String,
    term: String,
}

/// `PUT /v1/publishers/{publisher}/listings/{listing}/price` — set the listing's price (tax
/// included, minor units). It is mirrored to the financial authority at once; a price it cannot
/// invoice exactly is refused with `422 price_not_invoiceable` and `details.nearest`, and nothing
/// changes. Existing licences keep the terms they were bought on.
async fn set_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account, pid) =
            publisher_admin(&state, &headers, &publisher, Action::NestPublish).await?;
        let req: PriceReq = parse(body)?;
        if !(1..=100_000_000).contains(&req.amount_minor) {
            return Err(GErr::bad(
                "invalid_request",
                "amount_minor: 1 to 100000000 (remove the price to make the listing free)",
            ));
        }
        if !CURRENCIES.contains(&req.currency.as_str()) {
            return Err(GErr::bad("invalid_request", "currency: TRY, USD or EUR"));
        }
        if !TERMS.contains(&req.term.as_str()) {
            return Err(GErr::bad("invalid_request", "term: one_off or monthly"));
        }
        listing_of(&state, &listing, &pid).await?;
        // Mirror first: nothing is stored unless the financial authority accepts the price.
        let lid = listing.clone();
        let previous = state
            .store
            .group_read(move |c| -> GResult<Option<Price>> { Ok(price_in(c, &lid)?) })
            .await?;
        let now = now_unix() as i64;
        let (lid, who) = (listing.clone(), account.clone());
        let p = (req.amount_minor, req.currency.clone(), req.term.clone());
        let upsert = move |tx: &Transaction<'_>| -> GResult<()> {
            tx.execute(
                "INSERT INTO nest_listing_prices (listing_id, amount_minor, currency, term,
                    updated_by, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(listing_id) DO UPDATE SET amount_minor = excluded.amount_minor,
                    currency = excluded.currency, term = excluded.term,
                    updated_by = excluded.updated_by, updated_at = excluded.updated_at",
                params![lid, p.0, p.1, p.2, who, now],
            )?;
            Ok(())
        };
        state.store.group_tx(upsert).await?;
        if let Err(e) = sync_offer(&state, &listing).await {
            // Put the previous price back: a refused price never stands.
            let lid = listing.clone();
            state
                .store
                .group_tx(move |tx| -> GResult<()> {
                    match previous {
                        Some(p) => tx.execute(
                            "UPDATE nest_listing_prices SET amount_minor = ?2, currency = ?3,
                                term = ?4 WHERE listing_id = ?1",
                            params![lid, p.amount_minor, p.currency, p.term],
                        )?,
                        None => tx.execute(
                            "DELETE FROM nest_listing_prices WHERE listing_id = ?1",
                            [&lid],
                        )?,
                    };
                    Ok(())
                })
                .await?;
            return Err(e);
        }
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            "marketplace.listing.price",
            Some(format!("listing:{listing}")),
            json!({ "amount_minor": req.amount_minor, "currency": req.currency, "term": req.term }),
            None,
            crate::marketplace_m1::user_agent(&headers),
            now_unix(),
        );
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                crate::store::audit::append_in(tx, &audit)?;
                price_view_in(tx, &listing)
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/publishers/{publisher}/listings/{listing}/price` — make the listing free again.
/// Its offers are retired; licences already bought keep their terms.
async fn clear_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
) -> Response {
    let result = async {
        let (_, _, pid) =
            publisher_admin(&state, &headers, &publisher, Action::NestPublish).await?;
        listing_of(&state, &listing, &pid).await?;
        let lid = listing.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "DELETE FROM nest_listing_prices WHERE listing_id = ?1",
                    [&lid],
                )?;
                Ok(())
            })
            .await?;
        retire_others(&state, &listing, None).await;
        let v = state
            .store
            .group_read(move |c| price_view_in(c, &listing))
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

// ---- purchases ------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PurchaseReq {
    listing_id: String,
    owner_kind: String,
    owner_id: String,
    intent_id: String,
}

struct PurchaseRow {
    installation_id: String,
    account_id: String,
    listing_id: String,
    offer_id: String,
    release_line: String,
    owner_kind: String,
    owner_id: String,
    status: String,
}

fn purchase_in(
    c: &Connection,
    where_: &str,
    key: &[&str],
) -> rusqlite::Result<Option<PurchaseRow>> {
    c.query_row(
        &format!(
            "SELECT installation_id, account_id, listing_id, offer_id, release_line, owner_kind,
                    owner_id, status FROM nest_mp_purchases WHERE {where_}"
        ),
        rusqlite::params_from_iter(key.iter()),
        |r| {
            Ok(PurchaseRow {
                installation_id: r.get(0)?,
                account_id: r.get(1)?,
                listing_id: r.get(2)?,
                offer_id: r.get(3)?,
                release_line: r.get(4)?,
                owner_kind: r.get(5)?,
                owner_id: r.get(6)?,
                status: r.get(7)?,
            })
        },
    )
    .optional()
}

/// The exact intent a payment for this purchase is bound to (saas_be hashes it).
fn intent_of(p: &PurchaseRow) -> Value {
    json!({
        "action": "install",
        "listing_id": p.listing_id,
        "release_line": p.release_line,
        "owner_kind": p.owner_kind,
        "owner_id": p.owner_id,
        "installation_id": p.installation_id,
    })
}

fn purchase_json_in(c: &Connection, p: &PurchaseRow) -> rusqlite::Result<Value> {
    let (term, currency, amount, title): (String, String, i64, String) = c.query_row(
        "SELECT o.term, o.currency, o.amount_minor, l.title FROM nest_listing_offers o
           JOIN nest_listings l ON l.id = o.listing_id WHERE o.offer_id = ?1",
        [&p.offer_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    Ok(json!({
        "purchase_id": p.installation_id,
        "installation_id": p.installation_id,
        "listing_id": p.listing_id,
        "owner_kind": p.owner_kind,
        "owner_id": p.owner_id,
        "status": p.status,
        "offer": {
            "offer_id": p.offer_id, "term": term, "currency": currency, "amount_minor": amount,
            "release_line": p.release_line, "title": title,
        },
        "intent": intent_of(p),
    }))
}

/// `POST /v1/marketplace/purchases` — start buying a priced listing. Checks, as an install would,
/// that the listing is published, visible to the caller, on an attested release and not already
/// installed, and that it is on sale; then reserves the installation id the payment will admit.
/// The billing bridge calls this with the buyer's own token, once per checkout intent; a replay of
/// the same intent returns the same purchase.
async fn purchase(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        authorize(
            &Actor::Principal(&principal),
            Action::NestInstall,
            &Resource::Account(&account),
            &facts(&state),
        )
        .map_err(|d| deny(d, false))?;
        let req: PurchaseReq = parse(body)?;
        if req.intent_id.len() != 32
            || !req
                .intent_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(GErr::bad("invalid_request", "intent_id: 32 hex characters"));
        }
        let org = match req.owner_kind.as_str() {
            "account" if req.owner_id == account => None,
            "account" => {
                return Err(GErr::forbidden(
                    "forbidden",
                    "an account buys for itself only",
                ))
            }
            // An org purchase: someone who may install for the org and manages its billing.
            "org" => {
                let ctx = crate::org::org_ctx(&state, &headers, &req.owner_id).await?;
                ctx.require(&state, crate::org::Perm::ManageAgents)?;
                ctx.require(&state, crate::org::Perm::ManageBilling)?;
                ctx.require_writable()?;
                Some(ctx.org.id.clone())
            }
            _ => return Err(GErr::bad("invalid_request", "owner_kind: account or org")),
        };
        // A replay of the same intent.
        let (acct, intent) = (account.clone(), req.intent_id.clone());
        let known = state
            .store
            .group_read(move |c| -> GResult<Option<Value>> {
                match purchase_in(c, "account_id = ?1 AND intent_id = ?2", &[&acct, &intent])? {
                    Some(p) => Ok(Some(json!({ "listing": p.listing_id, "data": purchase_json_in(c, &p)? }))),
                    None => Ok(None),
                }
            })
            .await?;
        if let Some(k) = known {
            if k["listing"] != req.listing_id.as_str() {
                return Err(GErr::conflict(
                    "intent_conflict",
                    "this intent was used for another listing",
                ));
            }
            return Ok((StatusCode::OK, Json(json!({ "data": k["data"] }))));
        }
        installable(&state, &headers, &account, &req.listing_id, org.as_deref()).await?;
        let installation = format!("ins_{}", crate::groups::rand_b32(16));
        let (lid, acct, intent) = (req.listing_id.clone(), account.clone(), req.intent_id.clone());
        let (owner_kind, owner_id) = match &org {
            Some(o) => ("org", o.clone()),
            None => ("account", account.clone()),
        };
        let now = now_unix() as i64;
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let live: Option<String> = tx
                    .query_row(
                        "SELECT id FROM nest_installations WHERE listing_id = ?1
                           AND owner_kind = ?2 AND owner_id = ?3 AND status = 'active'",
                        params![lid, owner_kind, owner_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = live {
                    return Err(GErr::conflict(
                        "already_installed",
                        if owner_kind == "org" {
                            "this organization already has this listing installed"
                        } else {
                            "this account already has this listing installed"
                        },
                    )
                    .with("installation_id", existing));
                }
                let desired = desired_in(tx, &lid)?;
                let offer = sellable_offer_in(tx, &lid)?;
                let (Some(d), Some(o)) = (desired, offer) else {
                    return Err(conflict_code("not_on_sale", "this listing is not on sale"));
                };
                if d.offer_id != o.0 {
                    return Err(conflict_code("not_on_sale", "this listing's price is changing"));
                }
                tx.execute(
                    "INSERT INTO nest_mp_purchases (installation_id, account_id, intent_id,
                        listing_id, offer_id, release_line, owner_kind, owner_id, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9)",
                    params![installation, acct, intent, lid, o.0, o.4, owner_kind, owner_id, now],
                )?;
                let p = purchase_in(tx, "installation_id = ?1", &[&installation])?
                    .ok_or_else(not_found)?;
                Ok(purchase_json_in(tx, &p)?)
            })
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// The checks an install makes before anything is reserved: published, visible to the caller, a
/// certified publisher, and a live attested release. Anything else reads as `404`, or the
/// release's own refusal.
async fn installable(
    state: &AppState,
    headers: &HeaderMap,
    account: &str,
    listing: &str,
    org: Option<&str>,
) -> GResult<()> {
    let lid = listing.to_string();
    let (gid, oid, acct) = (lid.clone(), org.map(str::to_string), account.to_string());
    // A granted listing (M2b) needs the grant's install switch, for the org or the account.
    let granted = state
        .store
        .group_read(move |c| -> GResult<bool> {
            let visibility: Option<String> = c
                .query_row(
                    "SELECT visibility FROM nest_listings WHERE id = ?1",
                    [&gid],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(v) = visibility else {
                return Ok(false);
            };
            if !matches!(v.as_str(), "invite_only" | "organization") {
                return Ok(false);
            }
            let use_ = crate::marketplace_grants::Use::Install;
            Ok(match &oid {
                Some(o) => crate::marketplace_grants::org_grant_in(c, &gid, &v, o, use_)?,
                None => crate::marketplace_grants::account_grant_in(c, &gid, &v, &acct, use_)?,
            })
        })
        .await?;
    type Found = (String, String, String, String, bool);
    let found = state
        .store
        .group_read(move |c| -> GResult<Option<(Found, Option<GErr>)>> {
            let row: Option<(String, String, String, String)> = c
                .query_row(
                    "SELECT l.publisher_id, l.visibility, l.status, p.status FROM nest_listings l
                       JOIN publisher_profiles p ON p.id = l.publisher_id WHERE l.id = ?1",
                    [&lid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((publisher, visibility, status, pstatus)) = row else {
                return Ok(None);
            };
            let release: String = c.query_row(
                "SELECT release_id FROM nest_listings WHERE id = ?1",
                [&lid],
                |r| r.get(0),
            )?;
            let rel = release_in(c, &release)?.ok_or_else(not_found)?;
            let runnable = if live_attested_in(c, &rel)? {
                None
            } else {
                Some(not_runnable(&rel))
            };
            Ok(Some((
                (publisher, visibility, status, pstatus, true),
                runnable,
            )))
        })
        .await?;
    let Some(((publisher, visibility, status, pstatus, _), runnable)) = found else {
        return Err(not_found());
    };
    let visible = status == "published"
        && pstatus == "approved"
        && (matches!(visibility.as_str(), "public" | "unlisted")
            || granted
            || (visibility == "private"
                && publisher_read(state, headers, account, &publisher).await));
    if !visible {
        return Err(not_found());
    }
    match runnable {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ---- admission (called by M2's install) -------------------------------------------------------------

/// A paid installation's licence, from the consumed authorization.
pub(crate) struct Paid {
    pub(crate) installation_id: String,
    licence_id: String,
    offer_id: String,
    term: String,
    status: String,
    access: bool,
    paid_through: Option<i64>,
    deletion_due_at: Option<i64>,
}

/// What an install of `listing` needs, decided before the install's transaction.
pub(crate) enum Admission {
    /// No price: the M1/M2 zero-priced path.
    Free,
    /// Bought: install under `installation_id` once [`consume`] has admitted it.
    Pending(Pending),
    /// This purchase already installed: answer with that installation.
    Replay(String),
}

fn secs(v: &Value) -> Option<i64> {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
}

/// For M2's `POST /v1/nest-installations` (`org: None`) and M2b's org install (`org: Some`): a
/// priced listing installs only with a paid purchase for the same owner (the caller's own, or the
/// org's, bought by any of its Nest admins who manage billing), whose authorization is consumed
/// here (through the bridge) for exactly the purchase's intent, with the installation id as the
/// reference. Idempotent: a retry after a lost answer consumes nothing new.
pub(crate) async fn admit_install(
    state: &AppState,
    listing: &str,
    account: &str,
    org: Option<&str>,
    purchase_id: Option<&str>,
) -> GResult<Admission> {
    let lid = listing.to_string();
    let priced = state
        .store
        .group_read(move |c| -> GResult<Option<Value>> {
            Ok(match price_in(c, &lid)? {
                Some(_) => Some(price_json_in(c, &lid)?),
                None => None,
            })
        })
        .await?;
    let Some(price) = priced else {
        return Ok(Admission::Free);
    };
    let Some(purchase_id) = purchase_id else {
        return Err(GErr::new(
            StatusCode::PAYMENT_REQUIRED,
            "payment_required",
            "this listing is paid; buy it first",
        )
        .with("price", price));
    };
    let pid = purchase_id.to_string();
    let found = state
        .store
        .group_read(move |c| -> GResult<Option<(PurchaseRow, bool)>> {
            let Some(p) = purchase_in(c, "installation_id = ?1", &[&pid])? else {
                return Ok(None);
            };
            let installed: bool = c
                .query_row(
                    "SELECT 1 FROM nest_installations WHERE id = ?1",
                    [&p.installation_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            Ok(Some((p, installed)))
        })
        .await?;
    let Some((p, installed)) = found else {
        return Err(not_found());
    };
    let owner_ok = match org {
        Some(o) => p.owner_kind == "org" && p.owner_id == o,
        None => p.owner_kind == "account" && p.account_id == account,
    };
    if !owner_ok || p.listing_id != listing {
        return Err(not_found());
    }
    if p.status == "installed" && installed {
        return Ok(Admission::Replay(p.installation_id));
    }
    Ok(Admission::Pending(Pending {
        installation_id: p.installation_id.clone(),
        row: p,
    }))
}

/// A purchase about to be installed: its id is the installation's.
pub(crate) struct Pending {
    pub(crate) installation_id: String,
    row: PurchaseRow,
}

/// Consume the purchase's payment authorization (through the bridge) for exactly its intent, with
/// the installation id as the reference. Called once the install's own checks have passed;
/// a retry after a lost answer consumes nothing new.
pub(crate) async fn consume(state: &AppState, pending: Pending) -> GResult<Paid> {
    let p = pending.row;
    let (status, v) = state
        .mp_bridge
        .call(
            reqwest::Method::POST,
            "/internal/marketplace/authorizations/consume",
            json!({
                "purchase_id": p.installation_id,
                "intent": intent_of(&p),
                "consumer_reference": p.installation_id,
            }),
        )
        .await
        .map_err(|BridgeError::Unavailable| billing_unavailable())?;
    if status != 200 {
        return Err(bridge_refusal(status, &v));
    }
    let l = &v["licence"];
    let licence_id = l["licence_id"].as_str().unwrap_or_default().to_string();
    let lstatus = l["status"].as_str().unwrap_or_default().to_string();
    if licence_id.is_empty()
        || l["listing_id"] != p.listing_id.as_str()
        || l["owner_kind"] != p.owner_kind.as_str()
        || l["owner_id"] != p.owner_id.as_str()
        || ![
            "active",
            "canceling",
            "paused",
            "expired",
            "terminated",
            "revoked",
        ]
        .contains(&lstatus.as_str())
    {
        return Err(billing_unavailable());
    }
    if l["access"].as_bool() != Some(true) {
        return Err(GErr::new(
            StatusCode::PAYMENT_REQUIRED,
            "licence_inactive",
            "this purchase's licence is not active",
        ));
    }
    let term = l["term"].as_str().unwrap_or("one_off").to_string();
    Ok(Paid {
        installation_id: p.installation_id,
        licence_id,
        offer_id: p.offer_id,
        term,
        status: lstatus,
        access: true,
        paid_through: secs(&l["paid_through"]),
        deletion_due_at: secs(&l["deletion_due_at"]),
    })
}

/// Inside the install's transaction: the licence row and the purchase marked installed.
pub(crate) fn record_in(tx: &Transaction<'_>, paid: &Paid, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO nest_installation_licences (installation_id, licence_id, offer_id, term,
            status, access, paid_through, deletion_due_at, runtime, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', 0, ?9, ?9)
         ON CONFLICT(installation_id) DO NOTHING",
        params![
            paid.installation_id,
            paid.licence_id,
            paid.offer_id,
            paid.term,
            paid.status,
            paid.access as i64,
            paid.paid_through,
            paid.deletion_due_at,
            now
        ],
    )?;
    tx.execute(
        "UPDATE nest_mp_purchases SET status = 'installed', installed_at = ?2
          WHERE installation_id = ?1",
        params![paid.installation_id, now],
    )?;
    Ok(())
}

/// For an installation's JSON (M2 `inst_json_in`): how it is funded and, if paid, its licence.
pub(crate) fn funding_in(
    c: &Connection,
    installation: &str,
    stored: &str,
) -> rusqlite::Result<(String, Value)> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        String,
        i64,
        Option<i64>,
        i64,
        Option<i64>,
        String,
    )> = c
        .query_row(
            "SELECT licence_id, term, status, access, paid_through, cancel_at_period_end,
                    deletion_due_at, runtime FROM nest_installation_licences
              WHERE installation_id = ?1",
            [installation],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        None => (stored.to_string(), Value::Null),
        Some((id, term, status, access, paid, cancel, due, runtime)) => (
            "licence".to_string(),
            json!({
                "licence_id": id, "term": term, "status": status, "access": access == 1,
                "paid_through": paid.map(iso), "cancel_at_period_end": cancel == 1,
                "deletion_due_at": due.map(iso), "runtime": runtime,
            }),
        ),
    })
}

/// For job admission (M1): an installation whose licence gives no access takes no jobs.
pub(crate) fn licence_inactive_in(c: &Connection, installation: &str) -> rusqlite::Result<bool> {
    Ok(c.query_row(
        "SELECT access FROM nest_installation_licences WHERE installation_id = ?1",
        [installation],
        |r| r.get::<_, i64>(0),
    )
    .optional()?
        == Some(0))
}

// ---- licence projection -----------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LicenceReq {
    installation_id: String,
    listing_id: String,
    owner_kind: String,
    owner_id: String,
    term: String,
    status: String,
    access: bool,
    paid_through: Option<String>,
    cancel_at_period_end: bool,
    deletion_due_at: Option<String>,
    revoked_at: Option<String>,
    reconciliation_version: i64,
}

/// What the worker host has to be told, decided in the projection's transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    None,
    Stop,
    Remove,
}

struct Projected {
    listing_id: String,
    buyer_context: String,
    owner_id: String,
    effect: Effect,
    runtime: String,
}

/// Apply a licence's state inside a transaction: store it if it is newer, then decide what the
/// installation needs. Idempotent: the same state twice does nothing the second time.
fn apply_in(tx: &Transaction<'_>, licence: &str, req: &LicenceReq, now: i64) -> GResult<Projected> {
    #[allow(clippy::type_complexity)]
    let row: Option<(String, i64, String, String, String, String, String)> = tx
        .query_row(
            "SELECT x.installation_id, x.version, x.runtime, i.listing_id, i.owner_id,
                    i.buyer_context, i.status
               FROM nest_installation_licences x JOIN nest_installations i ON i.id = x.installation_id
              WHERE x.licence_id = ?1",
            [licence],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let Some((installation, version, runtime, listing, owner, context, istatus)) = row else {
        return Err(not_found());
    };
    if installation != req.installation_id
        || listing != req.listing_id
        || req.owner_kind != "account"
        || owner != req.owner_id
    {
        return Err(conflict_code(
            "licence_mismatch",
            "this licence belongs to another installation",
        ));
    }
    if req.reconciliation_version > version {
        tx.execute(
            "UPDATE nest_installation_licences SET status = ?2, access = ?3, paid_through = ?4,
                cancel_at_period_end = ?5, deletion_due_at = ?6, revoked_at = ?7, term = ?8,
                version = ?9, updated_at = ?10 WHERE licence_id = ?1",
            params![
                licence,
                req.status,
                req.access as i64,
                req.paid_through.as_deref().and_then(|s| secs(&json!(s))),
                req.cancel_at_period_end as i64,
                req.deletion_due_at.as_deref().and_then(|s| secs(&json!(s))),
                req.revoked_at.as_deref().and_then(|s| secs(&json!(s))),
                req.term,
                req.reconciliation_version,
                now
            ],
        )?;
    }
    decide_in(
        tx,
        &installation,
        &listing,
        &context,
        &owner,
        &runtime,
        &istatus,
        now,
    )
}

/// The effect the stored licence calls for now.
#[allow(clippy::too_many_arguments)]
fn decide_in(
    tx: &Transaction<'_>,
    installation: &str,
    listing: &str,
    context: &str,
    owner: &str,
    runtime: &str,
    istatus: &str,
    now: i64,
) -> GResult<Projected> {
    let (status, access, due): (String, i64, Option<i64>) = tx.query_row(
        "SELECT status, access, deletion_due_at FROM nest_installation_licences
          WHERE installation_id = ?1",
        [installation],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let boundary = access == 0
        && (due.is_some_and(|d| d <= now)
            || (due.is_none() && matches!(status.as_str(), "terminated" | "revoked")));
    let mut effect = Effect::None;
    let mut next = runtime.to_string();
    if boundary && runtime != "removed" {
        if istatus == "active" {
            uninstall_in(tx, installation, listing, owner, now)?;
        }
        effect = Effect::Remove;
    } else if access == 0 && runtime == "running" {
        cancel_jobs_in(tx, listing, owner, "licence_paused", now)?;
        effect = Effect::Stop;
    } else if access == 1 && runtime == "stopped" {
        // Resumed: the worker host restarts the worker with the next admitted job.
        tx.execute(
            "UPDATE nest_installation_licences SET runtime = 'running', updated_at = ?2
              WHERE installation_id = ?1",
            params![installation, now],
        )?;
        next = "running".into();
    }
    Ok(Projected {
        listing_id: listing.to_string(),
        buyer_context: context.to_string(),
        owner_id: owner.to_string(),
        effect,
        runtime: next,
    })
}

fn cancel_jobs_in(
    tx: &Transaction<'_>,
    listing: &str,
    owner: &str,
    reason: &str,
    now: i64,
) -> rusqlite::Result<usize> {
    let jobs: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT id FROM nest_jobs WHERE listing_id = ?1 AND account_id = ?2
               AND execution IN ('queued','running','waiting_input')",
        )?;
        let rows = stmt
            .query_map(params![listing, owner], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        rows
    };
    for job in &jobs {
        tx.execute(
            "UPDATE nest_jobs SET execution = 'cancelled', error_code = ?3,
                revision = revision + 1, updated_at = ?2 WHERE id = ?1",
            params![job, now, reason],
        )?;
        tx.execute(
            "INSERT INTO nest_job_events (job_id, kind, detail, created_at)
             VALUES (?1, 'nest.job.cancelled', ?2, ?3)",
            params![job, json!({ "execution": "cancelled" }).to_string(), now],
        )?;
    }
    Ok(jobs.len())
}

/// The deletion boundary: the installation is uninstalled as by its owner (configuration and
/// secrets deleted, open jobs cancelled), recorded with the actor `licence`.
fn uninstall_in(
    tx: &Transaction<'_>,
    installation: &str,
    listing: &str,
    owner: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let release: String = tx.query_row(
        "SELECT release_id FROM nest_installations WHERE id = ?1",
        [installation],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE nest_installations SET status = 'uninstalled', config = '{}',
            uninstalled_at = ?2, updated_at = ?2 WHERE id = ?1 AND status = 'active'",
        params![installation, now],
    )?;
    tx.execute(
        "DELETE FROM nest_installation_secrets WHERE installation_id = ?1",
        [installation],
    )?;
    cancel_jobs_in(tx, listing, owner, "licence_ended", now)?;
    tx.execute(
        "INSERT INTO nest_installation_events (installation_id, kind, from_release, to_release,
            actor, created_at) VALUES (?1, 'uninstalled', ?2, NULL, 'licence', ?3)",
        params![installation, release, now],
    )?;
    Ok(())
}

/// Tell the worker host, and record what it confirmed.
async fn carry_out(state: &AppState, installation: &str, p: &Projected) -> String {
    let (path, confirmed) = match p.effect {
        Effect::None => return p.runtime.clone(),
        Effect::Stop => {
            let answered = state
                .nests
                .call(
                    reqwest::Method::POST,
                    "/v1/mp/workers/stop",
                    Some(json!({ "listing_id": p.listing_id, "buyer_context": p.buyer_context })),
                )
                .await;
            ("/v1/mp/workers/stop", matches!(answered, Ok((200, _))))
        }
        // An ended licence destroys the worker like an uninstall (hardening: crypto-erase and a
        // tombstone, so the id never comes back).
        Effect::Remove => (
            "/v1/mp/workers/destroy",
            crate::marketplace_hardening::destroy_worker(state, &p.listing_id, &p.buyer_context)
                .await
                .is_ok(),
        ),
    };
    if !confirmed {
        // Not confirmed: admission is already off; the next projection or sweep tries again.
        tracing::warn!(
            installation,
            path,
            "marketplace licence effect not confirmed by nestd"
        );
        return p.runtime.clone();
    }
    let next = if p.effect == Effect::Stop {
        "stopped"
    } else {
        "removed"
    };
    let (iid, n) = (installation.to_string(), next.to_string());
    let now = now_unix() as i64;
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "UPDATE nest_installation_licences SET runtime = ?2, updated_at = ?3
                  WHERE installation_id = ?1",
                params![iid, n, now],
            )?;
            Ok(())
        })
        .await;
    if p.effect == Effect::Remove {
        state.event_hub.wake(&p.owner_id);
    }
    next.to_string()
}

/// `PUT /internal/v1/marketplace/licences/{licence}` — the billing bridge projects one licence
/// onto its installation (reconciler, or right after a cancel). `reconciliation_version` only
/// increases; an older or equal one changes nothing stored, but the effects are still checked
/// (the deletion boundary is a time).
async fn project_licence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(licence): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(resp) = crate::internal_api::authorize_service(
        &state,
        &headers,
        crate::internal_api::Endpoint::MarketplaceLicences,
        Action::ManageBilling,
    ) {
        return *resp;
    }
    let result = async {
        flag_gate(&state).await?;
        let req: LicenceReq = parse(body)?;
        if ![
            "active",
            "canceling",
            "paused",
            "expired",
            "terminated",
            "revoked",
        ]
        .contains(&req.status.as_str())
            || !TERMS.contains(&req.term.as_str())
        {
            return Err(GErr::bad(
                "invalid_request",
                "unknown licence status or term",
            ));
        }
        let now = now_unix() as i64;
        let (l, installation) = (licence.clone(), req.installation_id.clone());
        let p = state
            .store
            .group_tx(move |tx| apply_in(tx, &l, &req, now))
            .await?;
        let runtime = carry_out(&state, &installation, &p).await;
        Ok(Json(json!({ "data": {
            "installation_id": installation, "licence_id": licence, "runtime": runtime,
        }})))
    }
    .await;
    into_response(result)
}

// ---- sweep ------------------------------------------------------------------------------------------

/// Once: retry offers that are not mirrored or not retired yet, and enforce deletion boundaries
/// that passed without a new projection.
pub(crate) async fn sweep_once(state: &AppState) -> usize {
    let now = now_unix() as i64;
    type Due = (String, String, String, String, String, String);
    let (listings, due): (Vec<String>, Vec<Due>) = match state
        .store
        .group_read(move |c| -> GResult<(Vec<String>, Vec<Due>)> {
            let mut stmt = c.prepare(
                "SELECT DISTINCT p.listing_id FROM nest_listing_prices p
                   JOIN nest_listings l ON l.id = p.listing_id
                  WHERE l.status = 'published'
                    AND NOT EXISTS (SELECT 1 FROM nest_listing_offers o
                                     WHERE o.listing_id = p.listing_id AND o.state = 'active'
                                       AND o.last_error IS NULL)
                 UNION
                 SELECT DISTINCT listing_id FROM nest_listing_offers WHERE state = 'retiring'",
            )?;
            let listings = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            let mut stmt = c.prepare(
                "SELECT x.installation_id, x.runtime, i.listing_id, i.owner_id, i.buyer_context,
                        i.status
                   FROM nest_installation_licences x JOIN nest_installations i
                     ON i.id = x.installation_id
                  WHERE x.access = 0 AND x.runtime <> 'removed'
                    AND (x.deletion_due_at <= ?1
                         OR (x.deletion_due_at IS NULL AND x.status IN ('terminated','revoked')))",
            )?;
            let due = stmt
                .query_map([now], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<Due>>>()?;
            Ok((listings, due))
        })
        .await
    {
        Ok(v) => v,
        Err(_) => return 0,
    };
    let mut n = 0;
    for listing in listings {
        if sync_offer(state, &listing).await.is_ok() {
            n += 1;
        }
    }
    for (installation, runtime, listing, owner, context, istatus) in due {
        let iid = installation.clone();
        let p = state
            .store
            .group_tx(move |tx| {
                decide_in(
                    tx, &iid, &listing, &context, &owner, &runtime, &istatus, now,
                )
            })
            .await;
        if let Ok(p) = p {
            carry_out(state, &installation, &p).await;
            n += 1;
        }
    }
    n
}

/// Every minute: [`sweep_once`].
pub(crate) fn start_sweeper(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            if crate::marketplace_m1::flag_gate(&state).await.is_ok() {
                sweep_once(&state).await;
            }
        }
    });
}
