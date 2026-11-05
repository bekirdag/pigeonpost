//! Typed Nest tools with action-bound approvals (Nests Phase B4, R-72).
//!
//! A Nest never performs an external write itself. Its runner may only *propose* one typed tool
//! call; the postbox stores it as an approval card, and the owner (a person) approves exactly that
//! call — tool, arguments and expiry, bound by a hash the client echoes back. Only then does the
//! postbox carry it out, once, as the tool broker, under its own destination policy.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `POST /v1/nest-approvals` | `NestToolRequest` | the Nest's own mailbox credential |
//! | `GET /v1/nests/{nest}/approvals` | `NestView` | the owner's account |
//! | `POST /v1/nests/{nest}/approvals/{approval}/approve` | `NestApprove` | the owner, a person |
//! | `POST /v1/nests/{nest}/approvals/{approval}/reject` | `NestApprove` | the owner, a person |
//!
//! Rules:
//! - Closed tool vocabulary ([`TOOLS`]). Nothing in the never-auto set (spending, credentials,
//!   deleting files, shell) exists as a tool at all.
//! - `send_message`: the recipient must be one of the owner's own mailboxes or a contact the owner
//!   has allowed (never a wildcard, never the Nest itself); the body is bounded.
//! - The approval binds `args_hash` = SHA-256 of the canonical call. A newer proposal from the same
//!   Nest for the same tool and recipient supersedes the older one; approving needs the hash the
//!   person saw, so a changed proposal cannot ride on an old approval.
//! - Single use, 24-hour expiry, at most 20 open cards per Nest, 50 executed sends per Nest a day.
//! - Text the model or a file put in the arguments is shown to the person verbatim; it is never
//!   interpreted here.

use crate::groups::{GErr, GResult};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::{capabilities, now_unix, principal_for_token, rand_hex, sha256, AppState, Principal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nest_approvals (
    id          TEXT PRIMARY KEY,
    nest_id     TEXT NOT NULL,
    account_id  TEXT NOT NULL,
    tool        TEXT NOT NULL,
    args        TEXT NOT NULL,
    args_hash   TEXT NOT NULL,
    subject     TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('pending','approved','rejected','superseded','expired','failed')),
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    decided_at  INTEGER,
    result      TEXT
);
CREATE INDEX IF NOT EXISTS nest_approvals_by_nest ON nest_approvals(nest_id, status, created_at);
";

/// The whole tool vocabulary. Keep it small: each entry is a way a Nest's words become an effect.
pub(crate) const TOOLS: &[&str] = &["send_message"];
const APPROVAL_SECS: i64 = 24 * 3600;
const MAX_OPEN: i64 = 20;
const MAX_SENDS_PER_DAY: i64 = 50;
const MAX_BODY_CHARS: usize = 4000;
/// Internal marker: the destination policy refused the call at approval time.
const DENIED: &str = "__denied__";

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nest-approvals", post(propose))
        .route("/v1/nests/{nest}/approvals", get(list))
        .route(
            "/v1/nests/{nest}/approvals/{approval}/{decision}",
            post(decide),
        )
}

/// The canonical form of one call and its hash: the binding an approval is for.
pub(crate) fn canonical(tool: &str, args: &Value) -> (String, String) {
    // Keys sorted, no whitespace: serde_json's Map is ordered by key (no preserve_order feature).
    let text = json!({ "tool": tool, "args": args }).to_string();
    let hash = crate::hex_str(&sha256(text.as_bytes()));
    (text, hash)
}

/// Validate a proposed call against its schema. Returns the normalized arguments and the
/// "subject" used for superseding (same tool + same subject = the same pending action).
pub(crate) fn validate(tool: &str, args: &Value) -> Result<(Value, String), &'static str> {
    if !TOOLS.contains(&tool) {
        return Err("unknown_tool");
    }
    let obj = args.as_object().ok_or("bad_args")?;
    match tool {
        "send_message" => {
            if obj.keys().any(|k| k != "to" && k != "body") {
                return Err("bad_args");
            }
            let to = obj
                .get("to")
                .and_then(Value::as_str)
                .ok_or("bad_args")?
                .trim();
            let body = obj.get("body").and_then(Value::as_str).ok_or("bad_args")?;
            if !to.starts_with('/')
                || to.contains('*')
                || to.len() > 200
                || to.chars().any(char::is_whitespace)
            {
                return Err("bad_recipient");
            }
            if body.trim().is_empty() || body.chars().count() > MAX_BODY_CHARS {
                return Err("bad_body");
            }
            Ok((
                json!({ "to": to, "body": body }),
                format!("send_message:{to}"),
            ))
        }
        _ => Err("unknown_tool"),
    }
}

fn deny_err(deny: crate::policy::DenyReason) -> GErr {
    let code = deny.code();
    GErr::new(
        deny.into_response().status(),
        code,
        "this credential may not do that",
    )
}

/// `POST /v1/nest-approvals {tool, args}` — a Nest proposes one call. Its own mailbox credential
/// only; nothing happens until its owner approves.
async fn propose(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let principal = principal_for_token(&state, crate::bearer(&headers)).await?;
        let Principal::Identity(me) = &principal else {
            return Err(GErr::forbidden("credential_not_accepted", "a Nest's mailbox credential is required"));
        };
        let address = me.address.clone();
        let facts = Facts { now: now_unix(), ..Facts::default() };
        authorize(
            &Actor::Principal(&principal),
            Action::NestToolRequest,
            &Resource::Mailbox { address: &address, account: me.account_id.as_deref() },
            &facts,
        )
        .map_err(deny_err)?;
        let Json(req) = body.map_err(|_| GErr::bad("invalid_json", "expected {tool, args}"))?;
        let tool = req["tool"].as_str().unwrap_or("").to_string();
        let (args, subject) = validate(&tool, &req["args"]).map_err(|code| GErr::bad(code, "that tool call is not allowed"))?;
        let (args_text, hash) = canonical(&tool, &args);
        let now = now_unix() as i64;
        let a = address.clone();
        let card = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                // Only a live Nest proposes, and only for its own owner.
                let nest: Option<(String, String)> = tx
                    .query_row(
                        "SELECT id, account_id FROM nests WHERE address = ?1 AND deleted_at IS NULL",
                        [&a],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let Some((nest_id, account)) = nest else {
                    return Err(GErr::forbidden("not_a_nest", "only a Nest proposes tool calls"));
                };
                tx.execute(
                    "UPDATE nest_approvals SET status = 'expired' WHERE nest_id = ?1 AND status = 'pending' AND expires_at <= ?2",
                    params![nest_id, now],
                )?;
                // The newest proposal for the same action replaces the older one.
                tx.execute(
                    "UPDATE nest_approvals SET status = 'superseded', decided_at = ?3
                      WHERE nest_id = ?1 AND subject = ?2 AND status = 'pending'",
                    params![nest_id, subject, now],
                )?;
                let open: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM nest_approvals WHERE nest_id = ?1 AND status = 'pending'",
                    [&nest_id],
                    |r| r.get(0),
                )?;
                if open >= MAX_OPEN {
                    return Err(GErr::new(StatusCode::TOO_MANY_REQUESTS, "too_many_pending", "too many approvals are waiting"));
                }
                let id = format!("nap_{}", rand_hex(12));
                tx.execute(
                    "INSERT INTO nest_approvals (id, nest_id, account_id, tool, args, args_hash, subject, status, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9)",
                    params![id, nest_id, account, tool, args_text, hash, subject, now, now + APPROVAL_SECS],
                )?;
                Ok(json!({ "approval_id": id, "status": "pending", "args_hash": hash, "expires_at": now + APPROVAL_SECS }))
            })
            .await?;
        Ok((StatusCode::CREATED, Json(card)).into_response())
    }
    .await;
    result.unwrap_or_else(|e: GErr| e.into_response())
}

/// The caller's account for an owner route, authorized for `action`, flag on, owning `nest`.
async fn owner(
    state: &AppState,
    headers: &HeaderMap,
    action: Action,
    nest: &str,
) -> GResult<(String, String)> {
    let principal = principal_for_token(state, crate::bearer(headers)).await?;
    let account = capabilities::principal_account(&principal)
        .map(str::to_string)
        .ok_or_else(|| {
            GErr::forbidden(
                "credential_not_accepted",
                "an account credential is required",
            )
        })?;
    let facts = Facts {
        now: now_unix(),
        ..Facts::default()
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Account(&account),
        &facts,
    )
    .map_err(deny_err)?;
    if !crate::filtering::feature_on(state, Some(&account), capabilities::NESTS).await {
        return Err(GErr::forbidden(
            "feature_disabled",
            "Nests are not enabled for this account",
        ));
    }
    let missing = || GErr::new(StatusCode::NOT_FOUND, "no_nest", "no such Nest");
    if !crate::nests::nest_id_ok(nest) {
        return Err(missing());
    }
    let (n, acct) = (nest.to_string(), account.clone());
    let address: Option<String> = state
        .store
        .group_read(move |c| -> GResult<Option<String>> {
            Ok(c.query_row(
                "SELECT address FROM nests WHERE id = ?1 AND account_id = ?2 AND deleted_at IS NULL",
                params![n, acct],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
        })
        .await?;
    address.map(|a| (account, a)).ok_or_else(missing)
}

fn card_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let args: String = r.get(2)?;
    Ok(json!({
        "approval_id": r.get::<_, String>(0)?,
        "tool": r.get::<_, String>(1)?,
        "call": serde_json::from_str::<Value>(&args).unwrap_or(Value::Null),
        "args_hash": r.get::<_, String>(3)?,
        "status": r.get::<_, String>(4)?,
        "created_at": r.get::<_, i64>(5)?,
        "expires_at": r.get::<_, i64>(6)?,
        "decided_at": r.get::<_, Option<i64>>(7)?,
        "result": r.get::<_, Option<String>>(8)?,
    }))
}

/// `GET /v1/nests/{nest}/approvals` — newest first, the last 100.
async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
) -> Response {
    let result = async {
        owner(&state, &headers, Action::NestView, &nest).await?;
        let now = now_unix() as i64;
        let cards = state
            .store
            .group_tx(move |tx| -> GResult<Vec<Value>> {
                tx.execute(
                    "UPDATE nest_approvals SET status = 'expired' WHERE nest_id = ?1 AND status = 'pending' AND expires_at <= ?2",
                    params![nest, now],
                )?;
                let mut stmt = tx.prepare(
                    "SELECT id, tool, args, args_hash, status, created_at, expires_at, decided_at, result
                       FROM nest_approvals WHERE nest_id = ?1 ORDER BY created_at DESC, id LIMIT 100",
                )?;
                let rows = stmt.query_map([&nest], card_json)?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok::<_, GErr>(Json(json!({ "approvals": cards })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// Whether the owner may be messaged at `to` through their Nest: one of their own mailboxes (not
/// a Nest), or a peer one of their own mailboxes allows.
fn recipient_allowed(
    c: &rusqlite::Connection,
    account: &str,
    nest_address: &str,
    to: &str,
) -> rusqlite::Result<bool> {
    if to == nest_address {
        return Ok(false);
    }
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM identities i WHERE (i.address = ?2 OR i.handle = ?2) AND i.account_id = ?1
                         AND i.address NOT IN (SELECT address FROM nests WHERE address IS NOT NULL))
             OR EXISTS(SELECT 1 FROM contacts ct JOIN identities o ON o.address = ct.owner
                        WHERE o.account_id = ?1 AND ct.peer = ?2 AND ct.admission = 'allow'
                          AND o.address NOT IN (SELECT address FROM nests WHERE address IS NOT NULL))",
        params![account, to],
        |r| r.get(0),
    )
}

/// `POST /v1/nests/{nest}/approvals/{approval}/{approve|reject} {args_hash}` — a person decides
/// on exactly the call they were shown.
async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((nest, approval, decision)): Path<(String, String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        if decision != "approve" && decision != "reject" {
            return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "unknown decision"));
        }
        let (account, nest_address) = owner(&state, &headers, Action::NestApprove, &nest).await?;
        let Json(req) = body.map_err(|_| GErr::bad("invalid_json", "expected {args_hash}"))?;
        let shown = req["args_hash"].as_str().unwrap_or("").to_string();
        let now = now_unix() as i64;
        let approve = decision == "approve";
        let (acct, n, a, addr) = (account.clone(), nest.clone(), approval.clone(), nest_address.clone());
        // Claim the card in one transaction: pending, unexpired, this hash, this Nest. Only then is
        // anything executed, so a double click or a race executes once.
        let claimed: (String, Value) = state
            .store
            .group_tx(move |tx| -> GResult<(String, Value)> {
                let row: Option<(String, String, String, String, i64)> = tx
                    .query_row(
                        "SELECT tool, args, args_hash, status, expires_at FROM nest_approvals WHERE id = ?1 AND nest_id = ?2 AND account_id = ?3",
                        params![a, n, acct],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?;
                let Some((tool, args, hash, status, expires)) = row else {
                    return Err(GErr::new(StatusCode::NOT_FOUND, "no_approval", "no such approval"));
                };
                if status != "pending" {
                    return Err(GErr::conflict("approval_closed", "this approval was already decided, replaced or expired").with("status", status));
                }
                if expires <= now {
                    tx.execute("UPDATE nest_approvals SET status = 'expired' WHERE id = ?1", [&a])?;
                    return Err(GErr::conflict("approval_closed", "this approval expired").with("status", "expired"));
                }
                if hash != shown {
                    return Err(GErr::conflict("approval_changed", "the action changed since it was shown; review it again"));
                }
                let call: Value = serde_json::from_str(&args).map_err(|_| GErr::bad("corrupt", "stored call is unreadable"))?;
                if approve && tool == "send_message" {
                    let to = call["args"]["to"].as_str().unwrap_or("");
                    if !recipient_allowed(tx, &acct, &addr, to)? {
                        // Recorded (committed) as failed, then refused below.
                        tx.execute("UPDATE nest_approvals SET status = 'failed', decided_at = ?2, result = 'recipient_not_allowed' WHERE id = ?1", params![a, now])?;
                        return Ok((DENIED.to_string(), Value::Null));
                    }
                    let sent_today: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM nest_approvals WHERE nest_id = ?1 AND tool = 'send_message' AND status = 'approved' AND decided_at > ?2",
                        params![n, now - 86400],
                        |r| r.get(0),
                    )?;
                    if sent_today >= MAX_SENDS_PER_DAY {
                        return Err(GErr::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "this Nest sent its daily maximum of messages"));
                    }
                }
                tx.execute(
                    "UPDATE nest_approvals SET status = ?2, decided_at = ?3 WHERE id = ?1",
                    params![a, if approve { "approved" } else { "rejected" }, now],
                )?;
                Ok((tool, call))
            })
            .await?;
        if claimed.0 == DENIED {
            return Err(GErr::forbidden("recipient_not_allowed", "a Nest may only message you or people you have allowed"));
        }
        if !approve {
            return Ok(Json(json!({ "approval_id": approval, "status": "rejected" })).into_response());
        }
        // Execute as the tool broker: the Nest's own mailbox sends, the postbox does it.
        let (tool, call) = claimed;
        let outcome = match tool.as_str() {
            "send_message" => {
                let me = state
                    .store
                    .get(nest_address.clone())
                    .await
                    .map_err(GErr::from)?
                    .ok_or_else(|| GErr::new(StatusCode::GONE, "no_nest", "the Nest's mailbox is gone"))?;
                let to = call["args"]["to"].as_str().unwrap_or("").to_string();
                let text = call["args"]["body"].as_str().unwrap_or("").to_string();
                crate::do_send(&state, &me, &to, &text, crate::ThreadRequest::Default)
                    .await
                    .map(|_| "sent".to_string())
                    .map_err(|e| e.code.to_string())
            }
            _ => Err("unknown_tool".to_string()),
        };
        let (status, result_text) = match &outcome {
            Ok(r) => ("approved", r.clone()),
            Err(code) => ("failed", code.clone()),
        };
        let a = approval.clone();
        let rt = result_text.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute("UPDATE nest_approvals SET status = ?2, result = ?3 WHERE id = ?1", params![a, status, rt])?;
                Ok(())
            })
            .await?;
        match outcome {
            Ok(_) => Ok(Json(json!({ "approval_id": approval, "status": "approved", "result": result_text })).into_response()),
            Err(code) => Err(GErr::new(StatusCode::BAD_GATEWAY, "tool_failed", "the action could not be carried out").with("result", code)),
        }
    }
    .await;
    result.unwrap_or_else(|e: GErr| e.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_validated_and_hashed_canonically() {
        let ok = json!({"to": "/bob", "body": "hi"});
        let (args, subject) = validate("send_message", &ok).unwrap();
        assert_eq!(subject, "send_message:/bob");
        let (_, h1) = canonical("send_message", &args);
        let (_, h2) = canonical("send_message", &json!({"body": "hi", "to": "/bob"}));
        assert_eq!(h1, h2, "key order never changes the binding");
        let (_, h3) = canonical("send_message", &json!({"to": "/bob", "body": "hi!"}));
        assert_ne!(h1, h3, "any change to the call changes the binding");
        for (tool, args, code) in [
            ("spend", json!({}), "unknown_tool"),
            ("run_shell", json!({}), "unknown_tool"),
            (
                "send_message",
                json!({"to": "/bob/*", "body": "x"}),
                "bad_recipient",
            ),
            (
                "send_message",
                json!({"to": "bob", "body": "x"}),
                "bad_recipient",
            ),
            (
                "send_message",
                json!({"to": "/bob", "body": " "}),
                "bad_body",
            ),
            (
                "send_message",
                json!({"to": "/bob", "body": "x", "cc": "/eve"}),
                "bad_args",
            ),
            ("send_message", json!("to /bob"), "bad_args"),
        ] {
            assert_eq!(validate(tool, &args).unwrap_err(), code, "{tool} {args}");
        }
        assert!(validate(
            "send_message",
            &json!({"to": "/bob", "body": "x".repeat(MAX_BODY_CHARS + 1)})
        )
        .is_err());
    }
}
