//! Self-set display names (SDS §14D, D-14D-9; docs/contacts-and-phone.md §6).
//!
//! A display name is what a person calls themselves. It is free, per mailbox, and **not** a
//! verified identity: clients render it as `~Name`, keep the address or handle one tap away and
//! never use it to address a message. Visibility works exactly like a profile picture's
//! (`everyone`, `known` — the default — or `nobody`), decided in `policy.rs` on
//! [`Resource::Profile`]. Handles are unaffected and stay paid.

use crate::audit::ActorKind;
use crate::capabilities;
use crate::filtering::{feature_on, mailbox_for};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::{StoredIdentity, Visibility};
use crate::{bearer, now_unix, principal_for_token, ApiError, AppState, Principal};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

/// Longest display name, in characters.
pub(crate) const MAX_CHARS: usize = 40;
/// Changes one mailbox may make per day.
const CHANGES_PER_DAY: usize = 10;
/// Addresses per lookup.
const LOOKUP_MAX: usize = 100;
/// Names nobody may take, whatever the case.
const RESERVED: &[&str] = &[
    "admin",
    "administrator",
    "support",
    "official",
    "verified",
    "security",
    "system",
    "moderator",
];

/// Characters that are invisible, reorder text or impersonate structure. `char::is_control`
/// covers C0/C1; these are the format (Cf) and filler characters that matter for names.
fn forbidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '/' | '@'
            | '\u{00AD}'
            | '\u{034F}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{115F}' | '\u{1160}'
            | '\u{17B4}' | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}')
}

/// Validate and tidy a name: trimmed, inner whitespace collapsed to single spaces, 1..=`max`
/// characters, no forbidden character; with `reserved`, no reserved word and nothing containing
/// "pigeonpost".
pub(crate) fn validate(raw: &str, max: usize, reserved: bool) -> Result<String, &'static str> {
    if raw.chars().any(forbidden) {
        return Err("names cannot contain /, @, or invisible or control characters");
    }
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = name.chars().count();
    if len == 0 {
        return Err("the name is empty");
    }
    if len > max {
        return Err("the name is too long");
    }
    if !name.chars().any(char::is_alphanumeric) {
        return Err("the name needs at least one letter or digit");
    }
    if reserved {
        let lower = name.to_lowercase();
        let squashed: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
        if squashed.contains("pigeonpost") || RESERVED.contains(&squashed.as_str()) {
            return Err("that name is reserved");
        }
    }
    Ok(name)
}

/// A mailbox's own name, as an invitation SMS may use it (the sender's own choice).
pub(crate) async fn own_name(state: &AppState, address: &str) -> Option<String> {
    let address = address.to_string();
    state
        .store
        .group_read(move |c| -> Result<Option<String>, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT display_name FROM profile_names WHERE address = ?1",
                params![address],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
        .ok()
        .flatten()
}

async fn name_row_of(state: &AppState, address: &str) -> Option<(String, Visibility, i64)> {
    let address = address.to_string();
    state
        .store
        .group_read(
            move |c| -> Result<Option<(String, String, i64)>, crate::groups::GErr> {
                Ok(c.query_row(
                    "SELECT display_name, visibility, updated_at FROM profile_names WHERE address = ?1",
                    params![address],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?)
            },
        )
        .await
        .ok()
        .flatten()
        .map(|(n, v, t)| (n, Visibility::parse(&v).unwrap_or(Visibility::Known), t))
}

/// `owner`'s display name if `viewer` (with its mailboxes) may see it; `None` otherwise, with no
/// difference between "no name", "not visible" and "feature off".
pub(crate) async fn visible_name(
    state: &AppState,
    viewer: &Principal,
    viewer_mailboxes: &[(String, Option<String>)],
    owner: &StoredIdentity,
) -> Option<String> {
    if !feature_on(
        state,
        owner.account_id.as_deref(),
        capabilities::DISPLAY_NAMES,
    )
    .await
    {
        return None;
    }
    let (name, visibility, _) = name_row_of(state, &owner.address).await?;
    let mut known = false;
    if visibility == Visibility::Known {
        for (address, handle) in viewer_mailboxes {
            if address == &owner.address {
                continue;
            }
            if state
                .store
                .relationship(owner.address.clone(), address.clone(), handle.clone())
                .await
                .is_ok_and(|rel| rel.known())
            {
                known = true;
                break;
            }
        }
    }
    authorize(
        &Actor::Principal(viewer),
        Action::ViewProfile,
        &Resource::Profile {
            visibility,
            owner_account: owner.account_id.as_deref(),
            owner_address: &owner.address,
            viewer_known: known,
        },
        &Facts {
            now: now_unix(),
            ..Facts::default()
        },
    )
    .ok()
    .map(|_| name)
}

fn names_off() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "feature_disabled",
        "display names are not enabled for this account",
    )
}

fn no_store_name(v: Value) -> Response {
    ([(axum::http::header::CACHE_CONTROL, "no-store")], Json(v)).into_response()
}

#[derive(Deserialize)]
pub(crate) struct IdentityQuery {
    #[serde(default)]
    identity: Option<String>,
}

/// `GET /v1/profile/name` — the acting mailbox's own name.
pub(crate) async fn get_own_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        q.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if !feature_on(
        &state,
        me.account_id.as_deref(),
        capabilities::DISPLAY_NAMES,
    )
    .await
    {
        return names_off().into_response();
    }
    let name = name_row_of(&state, &me.address)
        .await
        .map(|(n, v, t)| json!({ "display_name": n, "visibility": v.as_str(), "updated_at": t }));
    no_store_name(json!({ "name": name }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutReq {
    display_name: String,
    #[serde(default)]
    visibility: Option<String>,
}

/// `PUT /v1/profile/name` — set the acting mailbox's name (and optionally who may see it).
pub(crate) async fn put_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
    body: Option<Json<PutReq>>,
) -> Response {
    match put_name_inner(&state, &headers, q, body).await {
        Ok(v) => no_store_name(v),
        Err(e) => e.into_response(),
    }
}

async fn put_name_inner(
    state: &AppState,
    headers: &HeaderMap,
    q: IdentityQuery,
    body: Option<Json<PutReq>>,
) -> Result<Value, ApiError> {
    let (principal, me) = mailbox_for(
        state,
        headers,
        q.identity.as_deref(),
        Action::SetDisplayName,
    )
    .await?;
    if !feature_on(state, me.account_id.as_deref(), capabilities::DISPLAY_NAMES).await {
        return Err(names_off());
    }
    let Json(req) = body.ok_or_else(|| ApiError::bad("bad_request", "expected {display_name}"))?;
    let name = validate(&req.display_name, MAX_CHARS, true)
        .map_err(|m| ApiError::bad("invalid_name", m))?;
    let visibility = match req.visibility.as_deref() {
        None => None,
        Some(v) => Some(Visibility::parse(v).ok_or_else(|| {
            ApiError::bad(
                "bad_request",
                "visibility must be everyone, known or nobody",
            )
        })?),
    };
    crate::groups::rate_limit(
        &format!("display-name:{}", me.address),
        CHANGES_PER_DAY,
        86_400,
    )
    .map_err(|e| {
        ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", e.message)
            .with_retry_after(e.retry_after.unwrap_or(3_600))
    })?;
    let now = now_unix();
    let audit = state.auditor.row(
        ActorKind::of(&principal),
        me.account_id.clone(),
        "profile.name_set",
        Some(format!("mailbox:{}", me.address)),
        json!({ "visibility": visibility.map(Visibility::as_str) }),
        None,
        None,
        now,
    );
    let (address, account, n) = (me.address.clone(), me.account_id.clone(), name.clone());
    let stored = state
        .store
        .group_tx(move |tx| -> Result<(String, String, i64), crate::groups::GErr> {
            tx.execute(
                "INSERT INTO profile_names (address, account_id, display_name, visibility, updated_at)
                 VALUES (?1, ?2, ?3, COALESCE(?4, 'known'), ?5)
                 ON CONFLICT(address) DO UPDATE SET display_name = excluded.display_name,
                   visibility = COALESCE(?4, profile_names.visibility), updated_at = excluded.updated_at",
                params![address, account, n, visibility.map(Visibility::as_str), now as i64],
            )?;
            crate::store::audit::append_in(tx, &audit)?;
            Ok(tx.query_row(
                "SELECT display_name, visibility, updated_at FROM profile_names WHERE address = ?1",
                params![address],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .map_err(|e| ApiError::new(e.status, e.code, e.message))?;
    announce_name(state, &me).await;
    Ok(
        json!({ "name": { "display_name": stored.0, "visibility": stored.1, "updated_at": stored.2 } }),
    )
}

/// `DELETE /v1/profile/name` — remove the acting mailbox's name.
pub(crate) async fn remove_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let (principal, me) = match mailbox_for(
        &state,
        &headers,
        q.identity.as_deref(),
        Action::SetDisplayName,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let now = now_unix();
    let audit = state.auditor.row(
        ActorKind::of(&principal),
        me.account_id.clone(),
        "profile.name_removed",
        Some(format!("mailbox:{}", me.address)),
        json!({}),
        None,
        None,
        now,
    );
    let address = me.address.clone();
    let removed = state
        .store
        .group_tx(move |tx| -> Result<usize, crate::groups::GErr> {
            let n = tx.execute(
                "DELETE FROM profile_names WHERE address = ?1",
                params![address],
            )?;
            if n > 0 {
                crate::store::audit::append_in(tx, &audit)?;
            }
            Ok(n)
        })
        .await;
    match removed {
        Ok(n) => {
            if n > 0 {
                announce_name(&state, &me).await;
            }
            no_store_name(json!({ "deleted": n > 0 }))
        }
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct LookupQuery {
    #[serde(default)]
    addresses: String,
}

/// `GET /v1/profiles?addresses=a,b` — display names the caller may see. A credential is required.
pub(crate) async fn lookup_names(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LookupQuery>,
) -> Response {
    let principal = match principal_for_token(&state, bearer(&headers)).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let names: Vec<String> = q
        .addresses
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if names.len() > LOOKUP_MAX {
        return ApiError::bad("bad_request", "at most 100 addresses per lookup").into_response();
    }
    let mailboxes: Vec<(String, Option<String>)> = match &principal {
        Principal::Identity(id) => vec![(id.address.clone(), id.handle.clone())],
        Principal::Account(account, _) => {
            match state.store.list_by_account(account.clone()).await {
                Ok(list) => list
                    .into_iter()
                    .take(50)
                    .map(|m| (m.address, m.handle))
                    .collect(),
                Err(_) => return ApiError::server("store_error").into_response(),
            }
        }
    };
    let mut out = serde_json::Map::new();
    for name in names {
        let entry = match crate::resolve_recipient(&state, &name).await {
            Ok(Some(owner)) => {
                let display_name = visible_name(&state, &principal, &mailboxes, &owner).await;
                json!({ "address": owner.address, "handle": owner.handle, "display_name": display_name })
            }
            Ok(None) => Value::Null,
            Err(e) => return e.into_response(),
        };
        out.insert(name, entry);
    }
    (
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Json(json!({ "profiles": out })),
    )
        .into_response()
}

/// Tell the owner, and everyone who corresponds with them, that the name changed; each recipient
/// learns only what it may see.
async fn announce_name(state: &AppState, owner: &StoredIdentity) {
    let now = now_unix();
    let row = name_row_of(state, &owner.address).await;
    let data = |visible: bool| {
        json!({
            "address": owner.address,
            "handle": owner.handle,
            "display_name": row.as_ref().filter(|_| visible).map(|r| r.0.clone()),
        })
    };
    let mut sent = std::collections::BTreeSet::new();
    if let Some(account) = owner.account_id.as_deref() {
        if state
            .store
            .record_account_event(
                account.to_string(),
                "profile.name".into(),
                Some(owner.address.clone()),
                data(true),
                now,
            )
            .await
            .is_ok()
        {
            state.event_hub.wake(account);
        }
        sent.insert(account.to_string());
    }
    let audience = state
        .store
        .avatar_audience(owner.address.clone())
        .await
        .unwrap_or_default();
    for (address, handle, account) in audience {
        if !sent.insert(account.clone()) {
            continue;
        }
        let visible = match &row {
            None => false,
            Some((_, v, _)) => match v {
                Visibility::Everyone => true,
                Visibility::Nobody => false,
                Visibility::Known => state
                    .store
                    .relationship(owner.address.clone(), address.clone(), handle)
                    .await
                    .is_ok_and(|rel| rel.known()),
            },
        };
        if state
            .store
            .record_account_event(
                account.clone(),
                "profile.name".into(),
                Some(address),
                data(visible),
                now,
            )
            .await
            .is_ok()
        {
            state.event_hub.wake(&account);
        }
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn names_are_validated() {
        assert_eq!(
            validate("  Bekir   Dağ ", 40, true).as_deref(),
            Ok("Bekir Dağ")
        );
        assert_eq!(validate("李小龍", 40, true).as_deref(), Ok("李小龍"));
        for bad in [
            "",
            "   ",
            "/bekir",
            "bekir@x",
            "Bek\u{202E}ir",
            "Be\u{200B}kir",
            "\u{3164}",
            "a\u{0007}b",
            "---",
            "Pigeon Post",
            "pigeonpost team",
            "ADMIN",
            "Support",
        ] {
            assert!(validate(bad, 40, true).is_err(), "{bad:?}");
        }
        assert!(validate(&"x".repeat(41), 40, true).is_err());
        assert!(validate(&"x".repeat(40), 40, true).is_ok());
        // Aliases are private: the reserved words do not apply.
        assert!(validate("Support", 64, false).is_ok());
    }
}
