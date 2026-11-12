//! Marketplace M2b: what the web console needs around listings.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET /v1/publishers/{publisher}/listing-stats` | `PublisherManage` | the publisher's team: installation counts per listing, never who installed |
//! | `POST /v1/marketplace/listings/{listing}/reports` | `NestInstall` | a person who can see the listing; 10 a day |
//! | `GET /v1/staff/listing-reports`, `POST /v1/staff/listing-reports/{report}/close` | `MarketplaceOps` | a reviewer or ops, with step-up |
//! | `GET /v1/staff/listings?status=&publisher=&cursor=` | `MarketplaceOps` | a reviewer or ops, with step-up |
//! | `PUT\|DELETE /v1/publishers/{publisher}/listings/{listing}/logo` | `PublisherManage` | a certified publisher's admins |
//! | `PUT /v1/publishers/{publisher}/listings/{listing}/storefront` | `PublisherManage` | same: category, support and privacy links |
//! | `GET /v1/marketplace/listings/{listing}/logo/{hash}/{size}.webp` | `NestDiscover` | anyone holding the URL (public, immutable) |
//! | `POST\|DELETE /internal/v1/marketplace/samples` | ops service | seed or withdraw the coming-soon samples |
//!
//! ## The marketplace look (developer-program plan §1)
//!
//! - **Logo.** PNG, JPEG or WebP only (SVG is refused: it can carry script), at most 1 MB, square,
//!   at least 256 px. The picture worker (`avatars::Processor`) decodes it under its limits and
//!   re-encodes it; the 512, 128 and 64 px WebPs are kept in the logo blob store, with every byte
//!   of the original container (EXIF, ICC, XMP) left behind. The URL carries the first 16 hex
//!   digits of the 512 px WebP's sha256, so it can be cached for a year and a new logo is a new
//!   URL. Logos are public by design; the publisher console needs them before the listing is.
//! - **Default logo.** A listing without one has `logo: null`; clients draw the initials on a
//!   colour derived from the listing id (`site/marketplace-core.js`, `MP.logo`).
//! - **`coming_soon`.** Shown in the catalog, never sold, installed or run: every buy and run route
//!   answers `409 coming_soon` for it. The nine first-party samples are `coming_soon`.
//! - Reads gain `logo`, `category`, `featured`, `coming_soon`, `sample`, `support_url`,
//!   `privacy_url`, `price_from` (a publisher-hosted listing's cheapest per-run price) and
//!   `publisher.verified`.
//!
//! Price terms live on the listing (`price.amount_minor`, `price.currency`, `price.term`): the
//! launch terms are `one_off` and `monthly`, currencies TRY, USD and EUR, and the amount stays 0
//! until payments are wired (saas_be offers mirror these fields).

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, iso, not_found, parse};
use crate::marketplace_m2::{gate, new_id, publisher_read, staff_gate, text, Need, PAGE};
use crate::policy::{authorize, Action, Actor, Resource};
use crate::{now_unix, AppState};
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

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
        .route(
            "/v1/publishers/{publisher}/listings/{listing}/logo",
            put(upload_logo)
                .delete(remove_logo)
                .layer(axum::extract::DefaultBodyLimit::max(LOGO_MAX + 1024)),
        )
        .route(
            "/v1/publishers/{publisher}/listings/{listing}/storefront",
            put(storefront),
        )
        .route(
            "/v1/marketplace/listings/{listing}/logo/{hash}/{file}",
            get(logo_blob),
        )
}

pub(crate) fn internal_router() -> Router<AppState> {
    Router::new().route(
        "/internal/v1/marketplace/samples",
        post(seed_samples).delete(withdraw_samples),
    )
}

/// `{amount_minor, currency, term}` as listing reads show it.
/// A free listing's price: zero, test credits, the preferred terms. A priced one is
/// `marketplace_pay::price_json_in`.
pub(crate) fn price_json(currency: String, term: String) -> Value {
    json!({ "amount_minor": 0, "currency": currency, "term": term, "funding": "test_credits",
            "on_sale": false })
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
            "set a price with PUT /v1/publishers/{publisher}/listings/{listing}/price (step-up); here price.amount_minor must be 0",
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
        staff_gate(&state, &headers, Action::MarketplaceOps, "listing-reports", None).await?;
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
                rows.into_iter()
                    .map(|v| Ok(crate::marketplace_pay::with_price_in(c, v)?))
                    .collect()
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
            staff_gate(&state, &headers, Action::MarketplaceOps, "listing-reports", None).await?;
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
        staff_gate(&state, &headers, Action::MarketplaceOps, "listings", None).await?;
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

// ---- the marketplace look: logos, storefront details, coming soon -----------------------------------

/// Largest logo upload.
pub(crate) const LOGO_MAX: usize = 1024 * 1024;
/// Smallest side a logo may have.
const LOGO_MIN_SIDE: u32 = 256;
/// The sizes kept, largest first. The picture worker makes 256 px too; a logo does not need it.
pub(crate) const LOGO_SIZES: [u32; 3] = [512, 128, 64];
/// Hex digits of the 512 px digest in a logo URL.
const LOGO_HASH_LEN: usize = 16;

/// The public path of one logo size (a path on the postbox, like picture URLs).
fn logo_url(listing: &str, hash: &str, size: u32) -> String {
    format!(
        "/v1/marketplace/listings/{listing}/logo/{}/{size}.webp",
        &hash[..LOGO_HASH_LEN.min(hash.len())]
    )
}

/// `{hash, urls: {"64", "128", "512"}}`, or null for a listing drawn with the default.
pub(crate) fn logo_json(listing: &str, hash: Option<&str>) -> Value {
    match hash {
        Some(h) if h.len() >= LOGO_HASH_LEN => {
            let mut urls = serde_json::Map::new();
            for size in LOGO_SIZES.iter().rev() {
                urls.insert(size.to_string(), json!(logo_url(listing, h, *size)));
            }
            json!({ "hash": &h[..LOGO_HASH_LEN], "urls": urls })
        }
        _ => Value::Null,
    }
}

/// Add the look to a listing read: logo, category, featured, coming soon, sample, links, the
/// cheapest per-run price of a publisher-hosted listing, and `publisher.verified` when the read
/// names the publisher.
pub(crate) fn with_look_in(c: &Connection, mut v: Value) -> rusqlite::Result<Value> {
    let Some(id) = v["id"].as_str().map(str::to_string) else {
        return Ok(v);
    };
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Option<String>,
        i64,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        String,
        Option<i64>,
    )> = c
        .query_row(
            "SELECT l.logo_hash, l.coming_soon, l.category, l.featured_rank, l.support_url,
                    l.privacy_url, p.status,
                    (SELECT MIN(amount_cents) FROM nest_hosted_prices h WHERE h.listing_id = l.id)
               FROM nest_listings l JOIN publisher_profiles p ON p.id = l.publisher_id
              WHERE l.id = ?1",
            [&id],
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
    let Some((logo, soon, category, featured, support, privacy, pstatus, from)) = row else {
        return Ok(v);
    };
    let hosted = v["hosting"] == "publisher";
    if let Some(o) = v.as_object_mut() {
        o.insert("logo".into(), logo_json(&id, logo.as_deref()));
        o.insert("coming_soon".into(), json!(soon == 1));
        o.insert(
            "sample".into(),
            json!(soon == 1 && id.starts_with(crate::store::marketplace_look::SAMPLE_PREFIX)),
        );
        o.insert("category".into(), json!(category));
        o.insert("featured".into(), json!(featured.is_some()));
        o.insert("featured_rank".into(), json!(featured));
        o.insert("support_url".into(), json!(support));
        o.insert("privacy_url".into(), json!(privacy));
        o.insert(
            "price_from".into(),
            match from {
                Some(cents) if hosted => {
                    json!({ "amount_cents": cents, "currency": "USD", "unit": "run" })
                }
                _ => Value::Null,
            },
        );
        if let Some(p) = o.get_mut("publisher").and_then(Value::as_object_mut) {
            p.insert("verified".into(), json!(pstatus == "approved"));
        }
    }
    Ok(v)
}

/// `409 coming_soon` when `listing` is a published public listing that is not available yet.
/// Every buy, install and run route asks this right after it reads its body.
pub(crate) async fn refuse_coming_soon(state: &AppState, listing: &str) -> GResult<()> {
    let id = listing.to_string();
    let soon = state
        .store
        .group_read(move |c| -> GResult<bool> {
            Ok(crate::store::marketplace_look::coming_soon_in(c, &id)?)
        })
        .await?;
    if soon {
        return Err(GErr::conflict(
            "coming_soon",
            "this listing is not available yet; nothing can be bought, installed or run from it",
        ));
    }
    Ok(())
}

/// Why a logo upload was refused, as the API says it.
fn logo_error(e: crate::avatars::AvatarError) -> GErr {
    use crate::avatars::AvatarError as A;
    match e {
        A::TooLarge => GErr::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "logo_too_large",
            "a logo is at most 1 MB",
        ),
        A::Unsupported => logo_unsupported(),
        A::Bad(why) => GErr::new(StatusCode::UNPROCESSABLE_ENTITY, "logo_unreadable", why),
        A::Internal => GErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "logo_failed",
            "the logo could not be processed",
        ),
    }
}

fn logo_unsupported() -> GErr {
    GErr::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "logo_unsupported",
        "a logo is a PNG, JPEG or WebP image; SVG is not accepted",
    )
}

/// The checks before decoding: the declared type, the size, the container and a square of at
/// least 256 px, read from the header alone. Decoding itself happens in the picture worker.
pub(crate) fn check_logo(content_type: &str, bytes: &[u8]) -> GResult<()> {
    use image::ImageFormat;
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        ct.as_str(),
        "image/png" | "image/jpeg" | "image/webp" | "application/octet-stream"
    ) {
        return Err(logo_unsupported());
    }
    if bytes.is_empty() {
        return Err(GErr::bad("logo_unreadable", "the body was empty"));
    }
    if bytes.len() > LOGO_MAX {
        return Err(logo_error(crate::avatars::AvatarError::TooLarge));
    }
    // An SVG (or any markup) is text: refuse it before a decoder is asked to guess.
    let head = bytes
        .iter()
        .copied()
        .skip_while(|b| b.is_ascii_whitespace() || *b == 0xEF || *b == 0xBB || *b == 0xBF)
        .take(1)
        .next();
    if head == Some(b'<') {
        return Err(logo_unsupported());
    }
    let format = image::guess_format(bytes).map_err(|_| logo_unsupported())?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(logo_unsupported());
    }
    let (w, h) = image::ImageReader::with_format(std::io::Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| {
            GErr::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "logo_unreadable",
                "the image could not be read",
            )
        })?;
    if w != h {
        return Err(GErr::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "logo_not_square",
            format!("a logo is square; this one is {w}×{h}"),
        ));
    }
    if w < LOGO_MIN_SIDE {
        return Err(GErr::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "logo_too_small",
            format!("a logo is at least {LOGO_MIN_SIDE}×{LOGO_MIN_SIDE}; 512×512 looks best"),
        ));
    }
    Ok(())
}

fn logo_store(state: &AppState) -> GResult<std::sync::Arc<crate::blobs::Blobs>> {
    state.logo_blobs.clone().ok_or_else(|| {
        GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "logos_unavailable",
            "this postbox has nowhere to keep logos",
        )
    })
}

/// Point `listing` at `variants` (or the default with `None`); the digests nothing else uses any
/// more, to remove after the commit.
fn set_logo_in(
    tx: &rusqlite::Transaction<'_>,
    listing: &str,
    publisher: &str,
    variants: Option<&BTreeMap<String, String>>,
    now: i64,
) -> GResult<Vec<String>> {
    let old: Option<Option<String>> = tx
        .query_row(
            "SELECT logo_variants FROM nest_listings WHERE id = ?1 AND publisher_id = ?2",
            params![listing, publisher],
            |r| r.get(0),
        )
        .optional()?;
    let Some(old) = old else {
        return Err(not_found());
    };
    let hash = variants.and_then(|v| v.get("512").cloned());
    let encoded = variants.map(|v| serde_json::to_string(v).unwrap_or_default());
    tx.execute(
        "UPDATE nest_listings SET logo_hash = ?2, logo_variants = ?3, updated_at = ?4 WHERE id = ?1",
        params![listing, hash, encoded, now],
    )?;
    let old: BTreeMap<String, String> = old
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let keep: Vec<&String> = variants.map(|v| v.values().collect()).unwrap_or_default();
    let mut unused = Vec::new();
    for digest in old.values() {
        if keep.contains(&digest) {
            continue;
        }
        // Content-addressed: another listing with the same picture shares the blob.
        let shared: bool = tx.query_row(
            "SELECT COUNT(*) > 0 FROM nest_listings WHERE id <> ?1 AND logo_variants LIKE ?2",
            params![listing, format!("%\"{digest}\"%")],
            |r| r.get(0),
        )?;
        if !shared {
            unused.push(digest.clone());
        }
    }
    Ok(unused)
}

/// `PUT /v1/publishers/{publisher}/listings/{listing}/logo` — the raw image as the body.
async fn upload_logo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Admin,
        )
        .await?;
        crate::marketplace_m2::certified(&ctx.p)?;
        let ct = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        check_logo(ct, &body)?;
        crate::groups::rate_limit(&format!("listinglogo:{}", ctx.p.id), 30, 3600)?;
        let blobs = logo_store(&state)?;
        let processed = state
            .avatar_processor
            .run(body, None)
            .await
            .map_err(logo_error)?;
        let mut variants = BTreeMap::new();
        for (size, bytes) in &processed.webp {
            if !LOGO_SIZES.contains(size) {
                continue;
            }
            let digest = blobs.put(bytes).map_err(|e| {
                tracing::error!(error = %e, "logo blob write failed");
                GErr::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    "the logo could not be stored",
                )
            })?;
            variants.insert(size.to_string(), digest);
        }
        if variants.len() != LOGO_SIZES.len() {
            return Err(logo_error(crate::avatars::AvatarError::Internal));
        }
        let now = now_unix() as i64;
        let (pid, lid) = (ctx.p.id.clone(), listing.clone());
        let stored = variants.clone();
        let unused = state
            .store
            .group_tx(move |tx| set_logo_in(tx, &lid, &pid, Some(&stored), now))
            .await?;
        for d in unused {
            blobs.remove(&d);
        }
        Ok(Json(json!({ "data": { "listing_id": listing,
            "logo": logo_json(&listing, variants.get("512").map(String::as_str)) } })))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/publishers/{publisher}/listings/{listing}/logo` — back to the default.
async fn remove_logo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Admin,
        )
        .await?;
        let now = now_unix() as i64;
        let (pid, lid) = (ctx.p.id.clone(), listing.clone());
        let unused = state
            .store
            .group_tx(move |tx| set_logo_in(tx, &lid, &pid, None, now))
            .await?;
        if let Some(blobs) = state.logo_blobs.as_ref() {
            for d in unused {
                blobs.remove(&d);
            }
        }
        Ok(Json(
            json!({ "data": { "listing_id": listing, "logo": null } }),
        ))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorefrontReq {
    /// One of the catalog's categories, or null to clear it.
    #[serde(default, with = "double_option")]
    category: Option<Option<String>>,
    #[serde(default, with = "double_option")]
    support_url: Option<Option<String>>,
    #[serde(default, with = "double_option")]
    privacy_url: Option<Option<String>>,
}

/// Absent, `null` and a value are three different things in a storefront update.
mod double_option {
    use serde::{Deserialize, Deserializer};
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<String>>, D::Error> {
        Option::<String>::deserialize(d).map(Some)
    }
}

/// An `https://` link of at most 300 characters, or an error naming `field`.
fn https_link(v: &str, field: &str) -> GResult<String> {
    let v = v.trim();
    let ok = v.len() <= 300
        && v.starts_with("https://")
        && v.len() > "https://".len()
        && !v
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '<' || c == '>');
    if !ok {
        return Err(GErr::bad(
            "invalid_request",
            format!("{field}: an https:// link of at most 300 characters"),
        ));
    }
    Ok(v.to_string())
}

/// `PUT /v1/publishers/{publisher}/listings/{listing}/storefront` — category and links. A field
/// left out is unchanged; `null` clears it.
async fn storefront(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Admin).await?;
        crate::marketplace_m2::certified(&ctx.p)?;
        let req: StorefrontReq = parse(body)?;
        let category = match &req.category {
            Some(Some(c)) => {
                if !crate::store::marketplace_look::CATEGORIES.iter().any(|(k, _)| k == c) {
                    let all: Vec<&str> = crate::store::marketplace_look::CATEGORIES
                        .iter()
                        .map(|(k, _)| *k)
                        .collect();
                    return Err(GErr::bad(
                        "invalid_request",
                        format!("category: one of {}", all.join(", ")),
                    ));
                }
                Some(Some(c.clone()))
            }
            other => other.clone(),
        };
        let support = match &req.support_url {
            Some(Some(u)) => Some(Some(https_link(u, "support_url")?)),
            other => other.clone(),
        };
        let privacy = match &req.privacy_url {
            Some(Some(u)) => Some(Some(https_link(u, "privacy_url")?)),
            other => other.clone(),
        };
        let now = now_unix() as i64;
        let pid = ctx.p.id.clone();
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let found: Option<String> = tx
                    .query_row(
                        "SELECT id FROM nest_listings WHERE id = ?1 AND publisher_id = ?2",
                        params![listing, pid],
                        |r| r.get(0),
                    )
                    .optional()?;
                if found.is_none() {
                    return Err(not_found());
                }
                for (column, value) in [
                    ("category", &category),
                    ("support_url", &support),
                    ("privacy_url", &privacy),
                ] {
                    if let Some(value) = value {
                        tx.execute(
                            &format!(
                                "UPDATE nest_listings SET {column} = ?2, updated_at = ?3 WHERE id = ?1"
                            ),
                            params![listing, value, now],
                        )?;
                    }
                }
                let v = crate::marketplace_m2::listing_in(tx, &listing, &pid)?.unwrap_or(Value::Null);
                Ok(v)
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/marketplace/listings/{listing}/logo/{hash}/{size}.webp` — public and immutable: the
/// hash names the picture, so a new logo is a new URL.
async fn logo_blob(
    State(state): State<AppState>,
    Path((listing, hash, file)): Path<(String, String, String)>,
) -> Response {
    let missing = || GErr::new(StatusCode::NOT_FOUND, "not_found", "no such logo");
    let result = async {
        let size = match file.as_str() {
            "512.webp" => "512",
            "128.webp" => "128",
            "64.webp" => "64",
            _ => return Err(missing()),
        };
        if listing.len() > 64 || hash.len() != LOGO_HASH_LEN {
            return Err(missing());
        }
        let blobs = state.logo_blobs.clone().ok_or_else(missing)?;
        let lid = listing.clone();
        let row: Option<(Option<String>, Option<String>)> = state
            .store
            .group_read(move |c| -> GResult<_> {
                Ok(c.query_row(
                    "SELECT logo_hash, logo_variants FROM nest_listings WHERE id = ?1",
                    [&lid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?)
            })
            .await?;
        let (Some(stored), Some(variants)) = row.unwrap_or((None, None)) else {
            return Err(missing());
        };
        // Holding the current URL is the only test: a logo is shown wherever its listing is,
        // including the publisher's console before the listing is published.
        let f = crate::policy::Facts {
            listing_visible: stored.starts_with(&hash),
            ..facts(&state)
        };
        authorize(
            &Actor::Anonymous,
            Action::NestDiscover,
            &Resource::NestListing(&listing),
            &f,
        )
        .map_err(|_| missing())?;
        let variants: BTreeMap<String, String> =
            serde_json::from_str(&variants).map_err(|_| missing())?;
        let bytes = variants
            .get(size)
            .and_then(|d| blobs.get(d))
            .ok_or_else(missing)?;
        let mut resp = bytes.into_response();
        let h = resp.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
        h.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; sandbox"),
        );
        h.insert(
            "cross-origin-resource-policy",
            HeaderValue::from_static("cross-origin"),
        );
        Ok(resp)
    }
    .await;
    into_response(result)
}

/// `POST /internal/v1/marketplace/samples` — seed the nine coming-soon samples under the platform
/// publisher. Idempotent: rows that exist are left as they are (a withdrawn sample stays
/// withdrawn).
async fn seed_samples(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::marketplace_m1::service_gate(&state, &headers) {
        return *resp;
    }
    let result = async {
        crate::marketplace_m1::flag_gate(&state).await?;
        let now = now_unix() as i64;
        let (publisher, seeded) = state
            .store
            .group_tx(move |tx| -> GResult<_> {
                let publisher = crate::marketplace_m1::platform_publisher_in(tx, now)?;
                let seeded = crate::store::marketplace_look::seed_samples_in(tx, &publisher, now)?;
                Ok((publisher, seeded))
            })
            .await?;
        Ok(Json(json!({ "data": {
            "publisher_id": publisher, "created": seeded.created, "existing": seeded.existing,
        } })))
    }
    .await;
    into_response(result)
}

/// `DELETE /internal/v1/marketplace/samples` — withdraw every sample still listed.
async fn withdraw_samples(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = crate::marketplace_m1::service_gate(&state, &headers) {
        return *resp;
    }
    let result = async {
        let now = now_unix() as i64;
        let n = state
            .store
            .group_tx(move |tx| -> GResult<usize> {
                Ok(crate::store::marketplace_look::withdraw_samples_in(
                    tx, now,
                )?)
            })
            .await?;
        Ok(Json(json!({ "data": { "withdrawn": n } })))
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
