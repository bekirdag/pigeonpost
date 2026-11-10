//! Marketplace M2b: what the web console needs around listings.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET /v1/publishers/{publisher}/listing-stats` | `PublisherManage` | the publisher's team: installation counts per listing, never who installed |
//! | `POST /v1/marketplace/listings/{listing}/reports` | `NestInstall` | a person who can see the listing; 10 a day |
//! | `GET /v1/staff/listing-reports`, `POST /v1/staff/listing-reports/{report}/close` | `NestReview` | a reviewer with step-up |
//! | `GET /v1/staff/listings?status=&publisher=&cursor=` | `NestReview` | a reviewer with step-up |
//!
//! Price terms live on the listing (`price.amount_minor`, `price.currency`, `price.term`): the
//! launch terms are `one_off` and `monthly`, currencies TRY, USD and EUR, and the amount stays 0
//! until payments are wired (saas_be offers mirror these fields).

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, iso, not_found, parse};
use crate::marketplace_m2::{gate, new_id, publisher_read, staff_gate, text, Need, PAGE};
use crate::policy::{authorize, Action, Actor, Resource};
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

const REASONS: &[&str] = &[
    "malicious",
    "misleading",
    "broken",
    "privacy",
    "illegal",
    "spam",
    "other",
];
const CURRENCIES: &[&str] = &["TRY", "USD", "EUR"];
const TERMS: &[&str] = &["one_off", "monthly"];

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/publishers/{publisher}/listing-stats", get(stats))
        .route("/v1/marketplace/listings/{listing}/reports", post(report))
        .route("/v1/staff/listing-reports", get(staff_reports))
        .route(
            "/v1/staff/listing-reports/{report}/close",
            post(close_report),
        )
        .route("/v1/staff/listings", get(staff_listings))
}

/// `{amount_minor, currency, term}` as listing reads show it.
pub(crate) fn price_json(currency: String, term: String) -> Value {
    json!({ "amount_minor": 0, "currency": currency, "term": term, "funding": "test_credits" })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct PriceReq {
    #[serde(default)]
    amount_minor: Option<i64>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    term: Option<String>,
}

/// Validate price terms: `(currency, term)` to store, each `None` when not sent.
pub(crate) fn price_terms(
    p: Option<&PriceReq>,
) -> GResult<(Option<&'static str>, Option<&'static str>)> {
    let Some(p) = p else {
        return Ok((None, None));
    };
    if p.amount_minor.is_some_and(|a| a != 0) {
        return Err(GErr::bad(
            "price_not_available",
            "priced listings arrive with payments; price.amount_minor must be 0",
        ));
    }
    let currency = match &p.currency {
        None => None,
        Some(c) => Some(
            *CURRENCIES
                .iter()
                .find(|x| **x == c.as_str())
                .ok_or_else(|| GErr::bad("invalid_request", "price.currency: TRY, USD or EUR"))?,
        ),
    };
    let term = match &p.term {
        None => None,
        Some(t) => Some(
            *TERMS
                .iter()
                .find(|x| **x == t.as_str())
                .ok_or_else(|| GErr::bad("invalid_request", "price.term: one_off or monthly"))?,
        ),
    };
    Ok((currency, term))
}

/// A publisher role's target account: `account_id`, or the account behind an org `member_id`
/// (an active member of the publisher's org).
pub(crate) async fn role_target(
    state: &AppState,
    org: &str,
    account: &Option<String>,
    member: &Option<String>,
) -> GResult<String> {
    match (account, member) {
        (Some(a), None) => Ok(a.clone()),
        (None, Some(m)) => {
            let (o, m) = (org.to_string(), m.clone());
            state
                .store
                .group_read(move |c| -> GResult<Option<String>> {
                    Ok(c.query_row(
                        "SELECT account_id FROM org_members WHERE org_id = ?1 AND id = ?2
                           AND status = 'active' AND account_id IS NOT NULL",
                        params![o, m],
                        |r| r.get(0),
                    )
                    .optional()?)
                })
                .await?
                .ok_or_else(|| {
                    GErr::bad(
                        "not_org_member",
                        "a publisher role goes to an active member of the org",
                    )
                })
        }
        _ => Err(GErr::bad(
            "invalid_request",
            "either account_id or member_id",
        )),
    }
}

/// `GET /v1/publishers/{publisher}/listing-stats` — counts only.
async fn stats(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Read,
        )
        .await?;
        let pid = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT l.id, l.status,
                        (SELECT COUNT(*) FROM nest_installations i WHERE i.listing_id = l.id AND i.status = 'active'),
                        (SELECT COUNT(*) FROM nest_installations i WHERE i.listing_id = l.id AND i.status = 'active' AND i.owner_kind = 'account'),
                        (SELECT COUNT(*) FROM nest_installations i WHERE i.listing_id = l.id AND i.status = 'active' AND i.owner_kind = 'org'),
                        (SELECT COUNT(*) FROM nest_installations i WHERE i.listing_id = l.id),
                        (SELECT COUNT(*) FROM nest_installations i WHERE i.listing_id = l.id AND i.status = 'uninstalled')
                       FROM nest_listings l WHERE l.publisher_id = ?1 ORDER BY l.created_at, l.id LIMIT 500",
                )?;
                let rows = stmt
                    .query_map([&pid], |r| {
                        Ok(json!({
                            "listing_id": r.get::<_, String>(0)?,
                            "status": r.get::<_, String>(1)?,
                            "installations": {
                                "active": r.get::<_, i64>(2)?,
                                "active_accounts": r.get::<_, i64>(3)?,
                                "active_orgs": r.get::<_, i64>(4)?,
                                "total": r.get::<_, i64>(5)?,
                                "uninstalled": r.get::<_, i64>(6)?,
                            },
                        }))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportReq {
    reason: String,
    #[serde(default)]
    detail: Option<String>,
}

/// `POST /v1/marketplace/listings/{listing}/reports` — one open report per person and listing.
async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(listing): Path<String>,
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
        let req: ReportReq = parse(body)?;
        if !REASONS.contains(&req.reason.as_str()) {
            return Err(GErr::bad(
                "invalid_request",
                format!("reason: one of {}", REASONS.join(", ")),
            ));
        }
        let detail = match &req.detail {
            Some(d) if !d.trim().is_empty() => text(d, "detail", 1, 500)?,
            _ => String::new(),
        };
        let lid = listing.clone();
        let row: Option<(String, String, String)> = state
            .store
            .group_read(move |c| -> GResult<Option<(String, String, String)>> {
                Ok(c.query_row(
                    "SELECT publisher_id, visibility, status FROM nest_listings WHERE id = ?1",
                    [&lid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?)
            })
            .await?;
        let Some((publisher, visibility, status)) = row else {
            return Err(not_found());
        };
        // Only a listing the person can see.
        let visible = status == "published"
            && match visibility.as_str() {
                "public" | "unlisted" => true,
                "private" => publisher_read(&state, &headers, &account, &publisher).await,
                _ => {
                    let (l, v, a) = (listing.clone(), visibility.clone(), account.clone());
                    state
                        .store
                        .group_read(move |c| -> GResult<bool> {
                            Ok(crate::marketplace_grants::account_grant_in(
                                c,
                                &l,
                                &v,
                                &a,
                                crate::marketplace_grants::Use::Discover,
                            )?)
                        })
                        .await?
                }
            };
        if !visible {
            return Err(not_found());
        }
        crate::groups::rate_limit(&format!("listingreport:{account}"), 10, 86_400)?;
        let now = now_unix() as i64;
        let id = new_id("lrp");
        let (rid, who) = (id.clone(), account.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let open: Option<String> = tx
                    .query_row(
                        "SELECT id FROM nest_listing_reports WHERE listing_id = ?1 AND reporter = ?2 AND status = 'open'",
                        params![listing, who],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = open {
                    return Err(GErr::conflict("already_reported", "you already reported this listing")
                        .with("report_id", existing));
                }
                tx.execute(
                    "INSERT INTO nest_listing_reports (id, listing_id, reporter, reason, detail, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)",
                    params![rid, listing, who, req.reason, detail, now],
                )?;
                Ok(())
            })
            .await?;
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": { "id": id, "status": "open", "created_at": iso(now) } })),
        ))
    }
    .await;
    into_response(result)
}

fn offset_of(q: &HashMap<String, String>) -> GResult<usize> {
    match q.get("cursor") {
        None => Ok(0),
        Some(c) => c
            .parse()
            .map_err(|_| GErr::bad("invalid_cursor", "not a cursor from this list")),
    }
}

/// `GET /v1/staff/listing-reports?status=open|closed&cursor=` — the report queue, oldest first,
/// with how many open reports each listing has. Reporters are not named.
async fn staff_reports(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        staff_gate(&state, &headers, Action::NestReview, "listing-reports", None).await?;
        let status = q.get("status").cloned().unwrap_or_else(|| "open".into());
        if !matches!(status.as_str(), "open" | "closed") {
            return Err(GErr::bad("invalid_request", "status: open or closed"));
        }
        let offset = offset_of(&q)?;
        let mut rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT r.id, r.listing_id, r.reason, r.detail, r.status, r.created_at, r.closed_at,
                            r.resolution, l.title, l.status, l.publisher_id,
                            (SELECT COUNT(*) FROM nest_listing_reports x WHERE x.listing_id = r.listing_id AND x.status = 'open')
                       FROM nest_listing_reports r JOIN nest_listings l ON l.id = r.listing_id
                      WHERE r.status = ?1 ORDER BY r.created_at, r.id LIMIT ?2 OFFSET ?3",
                )?;
                let rows = stmt
                    .query_map(params![status, (PAGE + 1) as i64, offset as i64], |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "listing_id": r.get::<_, String>(1)?,
                            "reason": r.get::<_, String>(2)?,
                            "detail": r.get::<_, String>(3)?,
                            "status": r.get::<_, String>(4)?,
                            "created_at": iso(r.get(5)?),
                            "closed_at": r.get::<_, Option<i64>>(6)?.map(iso),
                            "resolution": r.get::<_, Option<String>>(7)?,
                            "listing": {
                                "title": r.get::<_, String>(8)?,
                                "status": r.get::<_, String>(9)?,
                                "publisher_id": r.get::<_, String>(10)?,
                                "open_reports": r.get::<_, i64>(11)?,
                            },
                        }))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        let next = (rows.len() > PAGE).then(|| (offset + PAGE).to_string());
        rows.truncate(PAGE);
        Ok(Json(json!({ "data": rows, "next_cursor": next })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseReq {
    resolution: String,
}

/// `POST /v1/staff/listing-reports/{report}/close` — record what staff did (moderation itself
/// stays `POST /v1/staff/listings/{listing}/moderation`).
async fn close_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(report): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (_, account) =
            staff_gate(&state, &headers, Action::NestReview, "listing-reports", None).await?;
        let req: CloseReq = parse(body)?;
        let resolution = text(&req.resolution, "resolution", 1, 500)?;
        let now = now_unix() as i64;
        let id = report.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let n = tx.execute(
                    "UPDATE nest_listing_reports SET status = 'closed', closed_by = ?2, closed_at = ?3,
                        resolution = ?4 WHERE id = ?1 AND status = 'open'",
                    params![id, account, now, resolution],
                )?;
                if n == 0 {
                    return Err(not_found());
                }
                Ok(())
            })
            .await?;
        Ok(Json(json!({ "data": { "id": report, "status": "closed" } })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/listings?status=&publisher=&cursor=` — every listing, for moderation, newest
/// first, with its open report count.
async fn staff_listings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        staff_gate(&state, &headers, Action::NestReview, "listings", None).await?;
        let status = q.get("status").cloned().unwrap_or_default();
        if !status.is_empty()
            && !matches!(
                status.as_str(),
                "draft" | "published" | "suspended" | "withdrawn"
            )
        {
            return Err(GErr::bad(
                "invalid_request",
                "status: draft, published, suspended or withdrawn",
            ));
        }
        let publisher = q.get("publisher").cloned().unwrap_or_default();
        if publisher.len() > 64 {
            return Err(GErr::bad("invalid_request", "publisher: a publisher id"));
        }
        let offset = offset_of(&q)?;
        let mut rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT l.id, l.publisher_id, p.display_name, l.title, l.visibility, l.status,
                            l.moderation_reason, l.address, r.version, r.security_state, l.created_at,
                            l.updated_at, l.published_at, l.price_currency, l.price_term,
                            (SELECT COUNT(*) FROM nest_listing_reports x WHERE x.listing_id = l.id AND x.status = 'open')
                       FROM nest_listings l JOIN publisher_profiles p ON p.id = l.publisher_id
                       JOIN nest_releases r ON r.id = l.release_id
                      WHERE (?1 = '' OR l.status = ?1) AND (?2 = '' OR l.publisher_id = ?2)
                      ORDER BY l.updated_at DESC, l.id LIMIT ?3 OFFSET ?4",
                )?;
                let rows = stmt
                    .query_map(params![status, publisher, (PAGE + 1) as i64, offset as i64], |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "publisher_id": r.get::<_, String>(1)?,
                            "publisher": { "display_name": r.get::<_, String>(2)? },
                            "title": r.get::<_, String>(3)?,
                            "visibility": r.get::<_, String>(4)?,
                            "status": r.get::<_, String>(5)?,
                            "moderation_reason": r.get::<_, Option<String>>(6)?,
                            "address": r.get::<_, Option<String>>(7)?,
                            "version": r.get::<_, String>(8)?,
                            "security_state": r.get::<_, String>(9)?,
                            "created_at": iso(r.get(10)?),
                            "updated_at": iso(r.get(11)?),
                            "published_at": r.get::<_, Option<i64>>(12)?.map(iso),
                            "price": price_json(r.get(13)?, r.get(14)?),
                            "open_reports": r.get::<_, i64>(15)?,
                        }))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        let next = (rows.len() > PAGE).then(|| (offset + PAGE).to_string());
        rows.truncate(PAGE);
        Ok(Json(json!({ "data": rows, "next_cursor": next })))
    }
    .await;
    into_response(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_terms_are_launch_terms_at_zero() {
        let ok = PriceReq {
            amount_minor: Some(0),
            currency: Some("USD".into()),
            term: Some("monthly".into()),
        };
        assert_eq!(
            price_terms(Some(&ok)).unwrap(),
            (Some("USD"), Some("monthly"))
        );
        assert_eq!(price_terms(None).unwrap(), (None, None));
        let priced = PriceReq {
            amount_minor: Some(100),
            ..PriceReq::default()
        };
        assert_eq!(
            price_terms(Some(&priced)).unwrap_err().code,
            "price_not_available"
        );
        for (c, t) in [("GBP", "monthly"), ("TRY", "yearly")] {
            let bad = PriceReq {
                amount_minor: None,
                currency: Some(c.into()),
                term: Some(t.into()),
            };
            assert!(price_terms(Some(&bad)).is_err());
        }
    }
}
