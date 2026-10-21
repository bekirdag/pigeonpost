//! The MLS delivery service for groups (`docs/group-e2ee.md` §7): device credentials, key
//! packages, Welcomes, commit acceptance, epoch acknowledgements and pending operations.
//!
//! What the server can and cannot check is stated in P-2 §7.3: it authenticates the posting
//! device (envelope signature), binds the cleartext header to the request, serializes commits per
//! group (first valid commit for an epoch wins), and authorizes the *declared* adds and removes
//! against membership state. It cannot see inside a commit; honest clients compare each verified
//! commit outcome with the declared change and warn on a mismatch.

// Outcomes are built up step by step inside transactions; that reads better than one literal.
#![allow(clippy::field_reassign_with_default)]

use crate::groups::{
    account_of, active_member, add_notice, alloc_seq, b64, emit, emit_members, emit_one, finish,
    idempotent, live_group, load_group, load_member, parse, rand_b32, rate_limit,
    release_invitation, unb64, Ctx, GErr, GResult, IdentityQuery, Outcome,
};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::{now_unix, ApiError, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pigeonpost_groupcrypto as gc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Unclaimed single-use packages one device may hold.
const MAX_UNCLAIMED: i64 = 100;
/// Below this, the device is told to top up.
const LOW_WATER: i64 = 10;
/// Active devices per mailbox.
const MAX_DEVICES: i64 = 10;
/// A claim not consumed by a commit within this window is void (P-2 §7.1).
const CLAIM_TTL_SECS: u64 = 600;
/// Handshakes are kept at most this long (P-2 §7.6).
const HANDSHAKE_RETENTION_SECS: u64 = 90 * 86_400;
/// Devices silent this long are reported as lagging.
const LAGGING_SECS: u64 = 30 * 86_400;

fn hex(bytes: &[u8]) -> String {
    crate::hex_str(bytes)
}

/// One operation an online member device should commit next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub kind: &'static str,
    pub mailbox: String,
    pub device_id: String,
    pub since: i64,
}

/// What the group's MLS tree should change, from membership and device state (P-2 §7.4).
pub(crate) fn pending_ops(c: &Connection, gid: &str, _now: u64) -> GResult<Vec<Pending>> {
    let mut out = Vec::new();
    {
        let mut stmt = c.prepare(
            "SELECT m.member, d.device_id, MAX(m.updated_at, d.created_at)
               FROM group_members m
               JOIN mls_devices d ON d.mailbox = m.member AND d.revoked_at IS NULL
               JOIN groups g ON g.id = m.group_id AND g.deleted_at IS NULL
              WHERE m.group_id = ?1 AND m.state IN ('active','keying')
                AND NOT EXISTS (SELECT 1 FROM group_leaves l
                                 WHERE l.group_id = m.group_id AND l.mailbox = m.member
                                   AND l.device_id = d.device_id)
              ORDER BY 3",
        )?;
        let rows = stmt.query_map(params![gid], |r| {
            Ok(Pending {
                kind: "add",
                mailbox: r.get(0)?,
                device_id: r.get(1)?,
                since: r.get(2)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    {
        let mut stmt = c.prepare(
            "SELECT l.mailbox, l.device_id, l.added_seq FROM group_leaves l
               LEFT JOIN group_members m ON m.group_id = l.group_id AND m.member = l.mailbox
               LEFT JOIN mls_devices d ON d.mailbox = l.mailbox AND d.device_id = l.device_id
              WHERE l.group_id = ?1
                AND (m.member IS NULL OR m.state NOT IN ('active','keying')
                     OR d.device_id IS NULL OR d.revoked_at IS NOT NULL OR l.stale = 1)",
        )?;
        let rows = stmt.query_map(params![gid], |r| {
            Ok(Pending {
                kind: "remove",
                mailbox: r.get(0)?,
                device_id: r.get(1)?,
                since: r.get(2)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

fn is_leaf(c: &Connection, gid: &str, mailbox: &str, device: &str) -> GResult<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM group_leaves WHERE group_id = ?1 AND mailbox = ?2 AND device_id = ?3",
        params![gid, mailbox, device],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

/// Groups a mailbox is live in, and every co-member there (for device-change visibility).
type CoMembers = (Vec<String>, Vec<(String, Option<String>)>);

fn co_members(c: &Connection, mailbox: &str) -> GResult<CoMembers> {
    let groups: Vec<String> = {
        let mut stmt = c.prepare(
            "SELECT m.group_id FROM group_members m JOIN groups g ON g.id = m.group_id
              WHERE m.member = ?1 AND m.state IN ('active','keying') AND g.deleted_at IS NULL",
        )?;
        let rows = stmt.query_map(params![mailbox], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    seen.insert(mailbox.to_string());
    targets.push((mailbox.to_string(), account_of(c, mailbox)?));
    for g in &groups {
        for (m, a) in crate::groups::members_in(c, g, &["active", "keying"])? {
            if seen.insert(m.clone()) {
                targets.push((m, a));
            }
        }
    }
    Ok((groups, targets))
}

// ---- devices -----------------------------------------------------------------------------------

#[derive(Deserialize)]
struct DeviceReq {
    device_id: String,
    signature_key: String,
    #[serde(default)]
    attestation: Option<String>,
}

/// `POST /v1/mls/devices`.
pub(crate) async fn register_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let (req, _): (DeviceReq, String) = parse(body)?;
        gc::credential::validate_device_id(&req.device_id)
            .map_err(|_| GErr::bad("bad_request", "device_id must be 1-64 of A-Z a-z 0-9 . _ -"))?;
        let key = unb64("signature_key", &req.signature_key)?;
        if key.len() != 32 {
            return Err(GErr::bad("bad_request", "signature_key must be 32 bytes"));
        }
        rate_limit(&format!("device:{}", ctx.address()), 10, 86_400)?;
        let me = ctx.address();
        let payload = gc::attestation_payload(&me, &req.device_id, &key)
            .map_err(|_| GErr::bad("bad_request", "cannot attest this device"))?;
        let mailbox_pk = ctx.me.ed25519_pub;
        let attestation: Vec<u8> = match &req.attestation {
            Some(a) => {
                // A self-custodied mailbox signed it; check it against the mailbox key.
                let sig = unb64("attestation", a)?;
                let vk = ed25519_verify(&mailbox_pk, &payload, &sig);
                if !vk {
                    return Err(GErr::bad("invalid_credential", "the attestation does not verify"));
                }
                sig
            }
            None => {
                let mut seed = state.vault.unwrap(&ctx.me.wrapped_seed).map_err(|_| {
                    GErr::from(ApiError::server("vault_error"))
                })?;
                let sig = gc::sign_attestation_with_seed(&seed, &payload);
                zeroize::Zeroize::zeroize(&mut seed);
                sig.map_err(|_| GErr::bad("bad_request", "cannot attest this device"))?.to_vec()
            }
        };
        // A further device is a person's decision: on a hosted mailbox the postbox can attest
        // devices, so adding one silently would be exactly the attack P-2 §12 describes.
        let existing = {
            let me = me.clone();
            state
                .store
                .group_read(move |c: &Connection| -> GResult<i64> {
                    Ok(c.query_row(
                        "SELECT COUNT(*) FROM mls_devices WHERE mailbox = ?1 AND revoked_at IS NULL",
                        params![me],
                        |r| r.get(0),
                    )?)
                })
                .await?
        };
        if existing > 0 {
            require_second_device(&state, &ctx)?;
        }
        if existing >= MAX_DEVICES {
            return Err(GErr::conflict("too_many_devices", "this mailbox has the maximum number of devices"));
        }
        let fingerprint = gc::device_fingerprint(&me, &req.device_id, &key);
        let now = now_unix();
        let device_id = req.device_id.clone();
        let att = attestation.clone();
        let out = state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let prior: Option<Option<i64>> = tx
                    .query_row(
                        "SELECT revoked_at FROM mls_devices WHERE mailbox = ?1 AND device_id = ?2",
                        params![me, device_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                match prior {
                    Some(None) => return Err(GErr::conflict("device_exists", "that device id is already registered")),
                    Some(Some(_)) => {
                        // A revoked id may be reused for a new key; its old leaves go first.
                        tx.execute(
                            "DELETE FROM mls_devices WHERE mailbox = ?1 AND device_id = ?2",
                            params![me, device_id],
                        )?;
                    }
                    None => {}
                }
                tx.execute(
                    "INSERT INTO mls_devices (mailbox, device_id, signature_key, fingerprint, attestation, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![me, device_id, key, fingerprint.to_vec(), att, now as i64],
                )?;
                let (groups, targets) = co_members(tx, &me)?;
                let mut out = Outcome::default();
                out.wake = emit(
                    tx,
                    &targets,
                    "group.device",
                    &json!({ "mailbox": me, "device_id": device_id, "fingerprint": hex(&fingerprint), "change": "added" }),
                    now,
                )?;
                for g in &groups {
                    add_notice(tx, g, "device_added", Some(&me), json!({ "mailbox": me, "device_id": device_id, "fingerprint": hex(&fingerprint) }), now)?;
                    out.wake.extend(emit_members(tx, g, "group.crypto_pending", &json!({ "group_id": g, "kinds": ["add"] }), now)?);
                }
                out.status = 201;
                out.body = json!({
                    "device_id": device_id,
                    "fingerprint": hex(&fingerprint),
                    "fingerprint_digits": gc::fingerprint_digits(&fingerprint),
                    "attestation": b64(&att),
                    "mailbox_public_key": b64(&mailbox_pk),
                    "created_at": now,
                });
                Ok(out)
            })
            .await;
        out
    }
    .await;
    if result.is_ok() {
        crate::groups::audit(&state, &headers, "mls.device.add", None).await;
    }
    finish(&state, result)
}

fn ed25519_verify(pk: &[u8; 32], msg: &[u8], sig: &[u8]) -> bool {
    let Ok(sig) = <[u8; 64]>::try_from(sig) else {
        return false;
    };
    let Ok(vk) = pigeonpost_core::keys::verifying_key_from_bytes(pk) else {
        return false;
    };
    vk.verify_strict(msg, &ed25519_dalek::Signature::from_bytes(&sig))
        .is_ok()
}

/// A second (or later) device needs a person, and step-up when the deployment has one configured.
fn require_second_device(state: &AppState, ctx: &Ctx) -> GResult<()> {
    ctx.require_human()?;
    if !state.step_up.acr_values.is_empty() {
        let facts = Facts {
            now: now_unix(),
            step_up: (*state.step_up).clone(),
            ..Facts::default()
        };
        authorize(
            &Actor::Principal(&ctx.principal),
            Action::SensitiveAccountChange,
            &Resource::Mailbox {
                address: &ctx.me.address,
                account: ctx.me.account_id.as_deref(),
            },
            &facts,
        )
        .map_err(|d| {
            GErr::new(
                StatusCode::UNAUTHORIZED,
                "step_up_required",
                "sign in again to add a device",
            )
            .with("reason", d.code())
        })?;
    }
    Ok(())
}

#[derive(Deserialize, Default)]
pub(crate) struct DevicesQuery {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    mailbox: Option<String>,
}

/// `GET /v1/mls/devices?mailbox=`.
pub(crate) async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DevicesQuery>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let me = ctx.address();
        let target = q.mailbox.clone().unwrap_or_else(|| me.clone());
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                if target != me {
                    let shared: bool = c
                        .query_row(
                            "SELECT 1 FROM group_members a JOIN group_members b ON a.group_id = b.group_id
                               JOIN groups g ON g.id = a.group_id AND g.deleted_at IS NULL
                              WHERE a.member = ?1 AND b.member = ?2
                                AND a.state IN ('active','keying') AND b.state IN ('active','keying','invited')
                              LIMIT 1",
                            params![me, target],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some();
                    if !shared {
                        return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "no shared group with that mailbox"));
                    }
                }
                let mut stmt = c.prepare(
                    "SELECT device_id, fingerprint, signature_key, created_at FROM mls_devices
                      WHERE mailbox = ?1 AND revoked_at IS NULL ORDER BY created_at",
                )?;
                let rows = stmt.query_map(params![target], |r| {
                    let fp: Vec<u8> = r.get(1)?;
                    let key: Vec<u8> = r.get(2)?;
                    let digits = <[u8; 32]>::try_from(fp.as_slice())
                        .map(|f| gc::fingerprint_digits(&f))
                        .unwrap_or_default();
                    Ok(json!({
                        "device_id": r.get::<_, String>(0)?,
                        "fingerprint": hex(&fp),
                        "fingerprint_digits": digits,
                        "signature_key": b64(&key),
                        "created_at": r.get::<_, i64>(3)?,
                    }))
                })?;
                Ok(Outcome::ok(json!({ "mailbox": target, "devices": rows.collect::<Result<Vec<_>, _>>()? })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/mls/devices/{device_id}`.
pub(crate) async fn revoke_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        if ctx.device.as_deref() != Some(device_id.as_str()) {
            // Revoking another device is a person's act (a stolen agent token must not lock the
            // owner's phone out).
            ctx.require_human()?;
        }
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let fp: Vec<u8> = tx
                    .query_row(
                        "SELECT fingerprint FROM mls_devices WHERE mailbox = ?1 AND device_id = ?2 AND revoked_at IS NULL",
                        params![me, device_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "unknown_device", "no such device"))?;
                tx.execute(
                    "UPDATE mls_devices SET revoked_at = ?3 WHERE mailbox = ?1 AND device_id = ?2",
                    params![me, device_id, now as i64],
                )?;
                tx.execute(
                    "DELETE FROM mls_key_packages WHERE mailbox = ?1 AND device_id = ?2",
                    params![me, device_id],
                )?;
                let (groups, targets) = co_members(tx, &me)?;
                let mut out = Outcome::ok(json!({ "revoked": true }));
                out.wake = emit(
                    tx,
                    &targets,
                    "group.device",
                    &json!({ "mailbox": me, "device_id": device_id, "fingerprint": hex(&fp), "change": "revoked" }),
                    now,
                )?;
                for g in groups {
                    if is_leaf(tx, &g, &me, &device_id)? {
                        add_notice(tx, &g, "device_removed", Some(&me), json!({ "mailbox": me, "device_id": device_id }), now)?;
                        out.wake.extend(emit_members(tx, &g, "group.crypto_pending", &json!({ "group_id": g, "kinds": ["remove"] }), now)?);
                    }
                }
                Ok(out)
            })
            .await
    }
    .await;
    if result.is_ok() {
        crate::groups::audit(&state, &headers, "mls.device.revoke", None).await;
    }
    finish(&state, result)
}

// ---- key packages ------------------------------------------------------------------------------

#[derive(Deserialize)]
struct KeyPackagesReq {
    request_key: String,
    #[serde(default)]
    key_packages: Vec<String>,
    #[serde(default)]
    last_resort: Option<String>,
}

/// Inspect and vet one uploaded package for the acting device.
fn vet_package(
    bytes: &[u8],
    me: &str,
    device: &str,
    key: &[u8],
    want_last_resort: bool,
    now: u64,
) -> GResult<gc::KeyPackageInfo> {
    let info = gc::inspect_key_package(bytes).map_err(|e| {
        GErr::bad(
            "invalid_key_package",
            format!("key package rejected: {}", e.code()),
        )
    })?;
    if info.mailbox != me || info.device_id != device || info.signature_key != key {
        return Err(GErr::bad(
            "invalid_key_package",
            "the key package is not for this device",
        ));
    }
    if info.last_resort != want_last_resort {
        return Err(GErr::bad(
            "invalid_key_package",
            "last-resort marking does not match",
        ));
    }
    if info.not_after < now + 86_400 {
        return Err(GErr::bad(
            "invalid_key_package",
            "the key package expires too soon",
        ));
    }
    Ok(info)
}

/// `POST /v1/mls/key-packages`.
pub(crate) async fn upload_key_packages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let (req, hash): (KeyPackagesReq, String) = parse(body)?;
        if req.key_packages.len() > gc::MAX_KEY_PACKAGES_PER_CALL as usize {
            return Err(GErr::bad("bad_request", "too many key packages in one call"));
        }
        rate_limit(&format!("kp:{}:{device}", ctx.address()), 30, 3_600)?;
        let me = ctx.address();
        let now = now_unix();
        let packages: Vec<Vec<u8>> = req
            .key_packages
            .iter()
            .map(|p| unb64("key_packages", p))
            .collect::<GResult<_>>()?;
        let last = req.last_resort.as_deref().map(|p| unb64("last_resort", p)).transpose()?;
        state
            .store
            .group_tx(move |tx| {
                idempotent(tx, &me, "", &format!("kp:{device}"), &req.request_key, &hash, now, |tx| {
                    let key: Vec<u8> = tx.query_row(
                        "SELECT signature_key FROM mls_devices WHERE mailbox = ?1 AND device_id = ?2",
                        params![me, device],
                        |r| r.get(0),
                    )?;
                    let unclaimed: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM mls_key_packages WHERE mailbox = ?1 AND device_id = ?2 AND last_resort = 0 AND claimed_at IS NULL",
                        params![me, device],
                        |r| r.get(0),
                    )?;
                    if unclaimed + packages.len() as i64 > MAX_UNCLAIMED {
                        return Err(GErr::conflict("too_many_key_packages", "this device already holds enough key packages"));
                    }
                    let mut stored = 0;
                    for bytes in &packages {
                        let info = vet_package(bytes, &me, &device, &key, false, now)?;
                        stored += tx.execute(
                            "INSERT OR IGNORE INTO mls_key_packages (ref, mailbox, device_id, bytes, last_resort, not_after, created_at)
                             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
                            params![hex(&info.key_package_ref), me, device, bytes, info.not_after as i64, now as i64],
                        )?;
                    }
                    if let Some(bytes) = &last {
                        let info = vet_package(bytes, &me, &device, &key, true, now)?;
                        tx.execute(
                            "DELETE FROM mls_key_packages WHERE mailbox = ?1 AND device_id = ?2 AND last_resort = 1",
                            params![me, device],
                        )?;
                        tx.execute(
                            "INSERT OR REPLACE INTO mls_key_packages (ref, mailbox, device_id, bytes, last_resort, not_after, created_at)
                             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
                            params![hex(&info.key_package_ref), me, device, bytes, info.not_after as i64, now as i64],
                        )?;
                    }
                    let (count, lr) = kp_count(tx, &me, &device, now)?;
                    Ok(Outcome::ok(json!({ "stored": stored, "unclaimed": count, "last_resort": lr })))
                })
            })
            .await
    }
    .await;
    finish(&state, result)
}

fn kp_count(c: &Connection, me: &str, device: &str, now: u64) -> GResult<(i64, bool)> {
    let count: i64 = c.query_row(
        "SELECT COUNT(*) FROM mls_key_packages WHERE mailbox = ?1 AND device_id = ?2 AND last_resort = 0
            AND claimed_at IS NULL AND not_after > ?3",
        params![me, device, now as i64],
        |r| r.get(0),
    )?;
    let lr: bool = c
        .query_row(
            "SELECT 1 FROM mls_key_packages WHERE mailbox = ?1 AND device_id = ?2 AND last_resort = 1 AND not_after > ?3",
            params![me, device, now as i64],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok((count, lr))
}

/// `GET /v1/mls/key-packages/count`.
pub(crate) async fn key_package_count(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let me = ctx.address();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                let (count, lr) = kp_count(c, &me, &device, now_unix())?;
                Ok(Outcome::ok(
                    json!({ "unclaimed": count, "last_resort": lr }),
                ))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
struct ClaimReq {
    request_key: String,
    mailboxes: Vec<String>,
}

/// May this device act on the group's tree: a current leaf, or the creator's first device while
/// the group is still provisioning.
fn may_commit(
    c: &Connection,
    g: &crate::groups::GroupRow,
    me: &str,
    device: &str,
) -> GResult<bool> {
    let m = load_member(c, &g.id, me)?;
    let Some(m) = m else { return Ok(false) };
    if g.activation_state == "provisioning" {
        return Ok(m.role == "owner" && m.state == "keying");
    }
    Ok(m.state == "active" && is_leaf(c, &g.id, me, device)?)
}

/// `POST /v1/groups/{id}/key-packages/claim`.
pub(crate) async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let (req, hash): (ClaimReq, String) = parse(body)?;
        if req.mailboxes.len() > 200 {
            return Err(GErr::bad("bad_request", "at most 200 mailboxes per claim"));
        }
        rate_limit(&format!("claim:{}:{device}", ctx.address()), 120, 3_600)?;
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| {
                idempotent(tx, &me, &id, &format!("claim:{device}"), &req.request_key, &hash, now, |tx| {
                    let g = live_group(tx, &id)?;
                    if !may_commit(tx, &g, &me, &device)? {
                        return Err(GErr::forbidden("not_member", "this device cannot change the group's keys"));
                    }
                    let wanted: HashSet<&String> = req.mailboxes.iter().collect();
                    let mut claims = Vec::new();
                    let mut exhausted = Vec::new();
                    let mut low = Vec::new();
                    for p in pending_ops(tx, &id, now)? {
                        if p.kind != "add" || !wanted.contains(&p.mailbox) || (p.mailbox == me && p.device_id == device) {
                            continue;
                        }
                        let pick: Option<(String, Vec<u8>, bool)> = tx
                            .query_row(
                                "SELECT ref, bytes, last_resort FROM mls_key_packages
                                  WHERE mailbox = ?1 AND device_id = ?2 AND not_after > ?3
                                    AND (last_resort = 1 OR claimed_at IS NULL)
                                  ORDER BY last_resort ASC, created_at ASC LIMIT 1",
                                params![p.mailbox, p.device_id, now as i64],
                                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
                            )
                            .optional()?;
                        let Some((kp_ref, bytes, last_resort)) = pick else {
                            exhausted.push(json!({ "mailbox": p.mailbox, "device_id": p.device_id }));
                            continue;
                        };
                        if !last_resort {
                            tx.execute("UPDATE mls_key_packages SET claimed_at = ?2 WHERE ref = ?1", params![kp_ref, now as i64])?;
                        }
                        tx.execute(
                            "INSERT INTO mls_kp_claims (ref, group_id, mailbox, device_id, claimed_by_mailbox, claimed_by_device, claimed_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![kp_ref, id, p.mailbox, p.device_id, me, device, now as i64],
                        )?;
                        let (left, _) = kp_count(tx, &p.mailbox, &p.device_id, now)?;
                        if left < LOW_WATER {
                            low.push((p.mailbox.clone(), p.device_id.clone(), left));
                        }
                        claims.push(json!({
                            "mailbox": p.mailbox,
                            "device_id": p.device_id,
                            "key_package": b64(&bytes),
                            "key_package_ref": kp_ref,
                            "last_resort": last_resort,
                        }));
                    }
                    let mut out = Outcome::ok(json!({ "claims": claims, "exhausted": exhausted }));
                    for (mailbox, device_id, left) in low {
                        out.wake.extend(emit_one(tx, &mailbox, "mls.key_packages_low", &json!({ "device_id": device_id, "unclaimed": left, "self": true }), now)?);
                    }
                    Ok(out)
                })
            })
            .await
    }
    .await;
    finish(&state, result)
}

// ---- commits -----------------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
struct AddRef {
    mailbox: String,
    device_id: String,
    key_package_ref: String,
}

#[derive(Deserialize, Clone)]
struct RemoveRef {
    mailbox: String,
    device_id: String,
}

#[derive(Deserialize)]
struct CommitReq {
    request_key: String,
    epoch: i64,
    commit_id: String,
    commit: String,
    envelope_signature: String,
    #[serde(default)]
    welcome: Option<String>,
    #[serde(default)]
    adds: Vec<AddRef>,
    #[serde(default)]
    removes: Vec<RemoveRef>,
    expected_revision: i64,
}

/// `POST /v1/groups/{id}/commits` — P-2 §7.3, one transaction.
pub(crate) async fn commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let (req, hash): (CommitReq, String) = parse(body)?;
        rate_limit(&format!("commit:{}:{device}", ctx.address()), 60, 60)?;
        let commit = unb64("commit", &req.commit)?;
        let signature = unb64("envelope_signature", &req.envelope_signature)?;
        let welcome = req
            .welcome
            .as_deref()
            .map(|w| unb64("welcome", w))
            .transpose()?;
        if commit.len() > 4 * 1024 * 1024
            || welcome.as_ref().is_some_and(|w| w.len() > 8 * 1024 * 1024)
        {
            return Err(GErr::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "commit too large",
            ));
        }
        if req.adds.len() > 1_000 || req.removes.len() > 1_000 {
            return Err(GErr::bad("bad_request", "too many changes in one commit"));
        }
        // Pure checks first, outside the transaction.
        let info = gc::inspect_envelope(&commit)
            .map_err(|e| GErr::bad("bad_envelope", format!("not a group commit: {}", e.code())))?;
        if info.kind != gc::MessageKind::Commit
            || info.group_id != id
            || info.sender_mailbox != ctx.me.address
            || info.sender_device != device
            || info.client_message_id != req.commit_id
            || info.epoch as i64 != req.epoch
        {
            return Err(GErr::bad(
                "binding_mismatch",
                "the commit header does not match this request",
            ));
        }
        let recipients: HashSet<String> = match &welcome {
            Some(w) => gc::welcome_recipients(w)
                .map_err(|_| GErr::bad("bad_welcome", "not a welcome"))?
                .iter()
                .map(|r| hex(r))
                .collect(),
            None => HashSet::new(),
        };
        let declared: HashSet<String> = req
            .adds
            .iter()
            .map(|a| a.key_package_ref.to_ascii_lowercase())
            .collect();
        if declared.len() != req.adds.len() || recipients != declared {
            return Err(GErr::bad(
                "binding_mismatch",
                "the welcome must be addressed to exactly the declared adds",
            ));
        }
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| {
                idempotent(
                    tx,
                    &me,
                    &id,
                    &format!("commit:{device}"),
                    &req.request_key.clone(),
                    &hash,
                    now,
                    |tx| {
                        apply_commit(
                            tx,
                            &id,
                            &me,
                            &device,
                            &req,
                            &commit,
                            &signature,
                            welcome.as_deref(),
                            now,
                        )
                    },
                )
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[allow(clippy::too_many_arguments)]
fn apply_commit(
    tx: &rusqlite::Transaction<'_>,
    gid: &str,
    me: &str,
    device: &str,
    req: &CommitReq,
    commit: &[u8],
    signature: &[u8],
    welcome: Option<&[u8]>,
    now: u64,
) -> GResult<Outcome> {
    let g = live_group(tx, gid)?;
    if !may_commit(tx, &g, me, device)? {
        return Err(GErr::forbidden(
            "not_member",
            "this device cannot change the group's keys",
        ));
    }
    let key: Vec<u8> = tx.query_row(
        "SELECT signature_key FROM mls_devices WHERE mailbox = ?1 AND device_id = ?2 AND revoked_at IS NULL",
        params![me, device],
        |r| r.get(0),
    )?;
    gc::verify_envelope_signature(&key, commit, signature)
        .map_err(|_| GErr::bad("bad_signature", "the envelope signature does not verify"))?;
    // Serialization: the first valid commit for an epoch wins; there is no merge.
    if req.epoch != g.epoch || req.expected_revision != g.revision {
        return Err(GErr::conflict(
            "revision_conflict",
            "another commit won; catch up and rebuild",
        )
        .with("current_epoch", g.epoch)
        .with("revision", g.revision));
    }
    if g.activation_state == "provisioning" && g.epoch != 0 {
        return Err(GErr::conflict(
            "revision_conflict",
            "the group is in an inconsistent state",
        ));
    }
    let pending = pending_ops(tx, gid, now)?;
    let pending_add: HashSet<(String, String)> = pending
        .iter()
        .filter(|p| p.kind == "add")
        .map(|p| (p.mailbox.clone(), p.device_id.clone()))
        .collect();
    let pending_remove: HashSet<(String, String)> = pending
        .iter()
        .filter(|p| p.kind == "remove")
        .map(|p| (p.mailbox.clone(), p.device_id.clone()))
        .collect();
    for a in &req.adds {
        let pair = (a.mailbox.clone(), a.device_id.clone());
        if !pending_add.contains(&pair) || (a.mailbox == me && a.device_id == device) {
            return Err(GErr::forbidden(
                "not_permitted",
                format!("{} {} is not waiting to be added", a.mailbox, a.device_id),
            ));
        }
        let claimed: bool = tx
            .query_row(
                "SELECT 1 FROM mls_kp_claims WHERE group_id = ?1 AND ref = ?2 AND mailbox = ?3 AND device_id = ?4
                    AND claimed_by_mailbox = ?5 AND claimed_by_device = ?6 AND claimed_at > ?7 AND used_at IS NULL",
                params![gid, a.key_package_ref.to_ascii_lowercase(), a.mailbox, a.device_id, me, device, now as i64 - CLAIM_TTL_SECS as i64],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !claimed {
            return Err(GErr::forbidden(
                "not_permitted",
                "adds must use key packages this device claimed in the last 10 minutes",
            ));
        }
    }
    for r in &req.removes {
        let pair = (r.mailbox.clone(), r.device_id.clone());
        let own_other =
            r.mailbox == me && r.device_id != device && is_leaf(tx, gid, &r.mailbox, &r.device_id)?;
        if !(pending_remove.contains(&pair) || own_other) {
            return Err(GErr::forbidden(
                "not_permitted",
                format!(
                    "{} {} is not pending removal; remove the member first",
                    r.mailbox, r.device_id
                ),
            ));
        }
    }
    let new_epoch = g.epoch + 1;
    let seq = alloc_seq(tx, gid, now)?;
    let adds_json = json!(req
        .adds
        .iter()
        .map(|a| json!({ "mailbox": a.mailbox, "device_id": a.device_id }))
        .collect::<Vec<_>>());
    let removes_json = json!(req
        .removes
        .iter()
        .map(|r| json!({ "mailbox": r.mailbox, "device_id": r.device_id }))
        .collect::<Vec<_>>());
    tx.execute(
        "INSERT INTO group_handshakes (group_id, seq, epoch, commit_id, sender, sender_device, commit_blob,
                welcome_blob, adds, removes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![gid, seq, g.epoch, req.commit_id, me, device, commit, welcome, adds_json.to_string(), removes_json.to_string(), now as i64],
    )?;
    let revision: i64 = tx.query_row(
        "UPDATE groups SET epoch = ?2, revision = revision + 1 WHERE id = ?1 RETURNING revision",
        params![gid, new_epoch],
        |r| r.get(0),
    )?;
    let mut out = Outcome::default();
    for r in &req.removes {
        tx.execute(
            "DELETE FROM group_leaves WHERE group_id = ?1 AND mailbox = ?2 AND device_id = ?3",
            params![gid, r.mailbox, r.device_id],
        )?;
    }
    if g.activation_state == "provisioning" {
        tx.execute(
            "INSERT OR IGNORE INTO group_leaves (group_id, mailbox, device_id, added_epoch, added_seq, acked_epoch, acked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6)",
            params![gid, me, device, new_epoch, seq, now as i64],
        )?;
    }
    for a in &req.adds {
        tx.execute(
            "INSERT INTO group_leaves (group_id, mailbox, device_id, added_epoch, added_seq) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![gid, a.mailbox, a.device_id, new_epoch, seq],
        )?;
        tx.execute(
            "UPDATE mls_kp_claims SET used_at = ?3 WHERE group_id = ?1 AND ref = ?2 AND used_at IS NULL",
            params![gid, a.key_package_ref.to_ascii_lowercase(), now as i64],
        )?;
        let wid = format!("wel_{}", rand_b32(12));
        tx.execute(
            "INSERT INTO mls_welcomes (id, group_id, seq, epoch, key_package_ref, mailbox, device_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![wid, gid, seq, new_epoch, a.key_package_ref.to_ascii_lowercase(), a.mailbox, a.device_id, now as i64],
        )?;
        out.wake.extend(emit_one(
            tx,
            &a.mailbox,
            "mls.welcome",
            &json!({ "welcome_id": wid, "group_id": gid, "device_id": a.device_id, "self": true }),
            now,
        )?);
    }
    // The committer has processed its own commit.
    tx.execute(
        "UPDATE group_leaves SET acked_epoch = MAX(acked_epoch, ?4), acked_at = ?5 WHERE group_id = ?1 AND mailbox = ?2 AND device_id = ?3",
        params![gid, me, device, new_epoch, now as i64],
    )?;
    // Members whose first device is now in the tree become active, with a fresh interval.
    let activated: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT m.member FROM group_members m WHERE m.group_id = ?1 AND m.state = 'keying'
               AND EXISTS (SELECT 1 FROM group_leaves l WHERE l.group_id = m.group_id AND l.mailbox = m.member)",
        )?;
        let rows = stmt.query_map(params![gid], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    for member in &activated {
        let version: i64 = tx.query_row(
            "UPDATE group_members SET state = 'active', membership_version = membership_version + 1,
                    joined_at = ?3, join_seq = ?4, read_cursor = MAX(read_cursor, ?4), revision = revision + 1, updated_at = ?3
              WHERE group_id = ?1 AND member = ?2 RETURNING membership_version",
            params![gid, member, now as i64, seq],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO group_membership_intervals (group_id, member, membership_version, first_seq, join_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![gid, member, version, seq + 1, new_epoch],
        )?;
    }
    let activating = g.activation_state == "provisioning";
    if activating {
        tx.execute(
            "UPDATE groups SET activation_state = 'active' WHERE id = ?1",
            params![gid],
        )?;
        add_notice(
            tx,
            gid,
            "group_created",
            Some(me),
            json!({ "name": g.name }),
            now,
        )?;
        let invited: Vec<(String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT member, invited_by FROM group_members WHERE group_id = ?1 AND state = 'invited'",
            )?;
            let rows = stmt.query_map(params![gid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let g = load_group(tx, gid)?.ok_or_else(GErr::not_found)?;
        for (member, by) in invited {
            release_invitation(tx, &g, &member, by.as_deref().unwrap_or(me), now, &mut out)?;
        }
        // An auto-join org room queues the members who joined the org meanwhile (SDS §4.2).
        crate::org_rooms::on_room_activated(tx, &g, now, &mut out)?;
    }
    for member in &activated {
        if !(activating && member == me) {
            add_notice(
                tx,
                gid,
                "member_joined",
                None,
                json!({ "address": member }),
                now,
            )?;
        }
        let data =
            json!({ "group_id": gid, "address": member, "state": "active", "change": "joined" });
        out.wake
            .extend(emit_members(tx, gid, "group.member", &data, now)?);
    }
    out.wake.extend(emit_members(
        tx,
        gid,
        "group.handshake",
        &json!({ "group_id": gid, "seq": seq, "epoch": new_epoch }),
        now,
    )?);
    out.status = 201;
    out.body = json!({ "seq": seq, "epoch": new_epoch, "revision": revision });
    Ok(out)
}

// ---- welcomes, acks, crypto state --------------------------------------------------------------

/// `GET /v1/mls/welcomes`.
pub(crate) async fn welcomes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let me = ctx.address();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                let mut stmt = c.prepare(
                    "SELECT w.id, w.group_id, w.epoch, w.seq, h.welcome_blob FROM mls_welcomes w
                       JOIN group_handshakes h ON h.group_id = w.group_id AND h.seq = w.seq
                       JOIN groups g ON g.id = w.group_id AND g.deleted_at IS NULL
                      WHERE w.mailbox = ?1 AND w.device_id = ?2 AND w.acked_at IS NULL
                        AND h.welcome_blob IS NOT NULL
                      ORDER BY w.seq",
                )?;
                let rows = stmt.query_map(params![me, device], |r| {
                    Ok(json!({
                        "welcome_id": r.get::<_, String>(0)?,
                        "group_id": r.get::<_, String>(1)?,
                        "epoch": r.get::<_, i64>(2)?,
                        "seq": r.get::<_, i64>(3)?,
                        "welcome": b64(&r.get::<_, Vec<u8>>(4)?),
                    }))
                })?;
                Ok(Outcome::ok(
                    json!({ "welcomes": rows.collect::<Result<Vec<_>, _>>()? }),
                ))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
struct WelcomeAckReq {
    request_key: String,
    joined: bool,
    #[serde(default)]
    error_code: Option<String>,
}

/// `POST /v1/mls/welcomes/{welcome_id}/ack`.
pub(crate) async fn welcome_ack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(wid): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let (req, hash): (WelcomeAckReq, String) = parse(body)?;
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| {
                idempotent(tx, &me, &wid, &format!("wack:{device}"), &req.request_key, &hash, now, |tx| {
                    let row: Option<(String, i64)> = tx
                        .query_row(
                            "SELECT group_id, epoch FROM mls_welcomes WHERE id = ?1 AND mailbox = ?2 AND device_id = ?3",
                            params![wid, me, device],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    let Some((gid, epoch)) = row else {
                        return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "no such welcome"));
                    };
                    tx.execute(
                        "UPDATE mls_welcomes SET acked_at = ?2, joined = ?3 WHERE id = ?1",
                        params![wid, now as i64, req.joined as i64],
                    )?;
                    let mut out = Outcome::ok(json!({ "acked": true }));
                    if req.joined {
                        tx.execute(
                            "UPDATE group_leaves SET acked_epoch = MAX(acked_epoch, ?4), acked_at = ?5
                              WHERE group_id = ?1 AND mailbox = ?2 AND device_id = ?3",
                            params![gid, me, device, epoch, now as i64],
                        )?;
                    } else {
                        tracing::info!(error = ?req.error_code, "a device could not use its welcome; re-adding");
                        tx.execute(
                            "UPDATE group_leaves SET stale = 1 WHERE group_id = ?1 AND mailbox = ?2 AND device_id = ?3",
                            params![gid, me, device],
                        )?;
                        out.wake = emit_members(tx, &gid, "group.crypto_pending", &json!({ "group_id": gid, "kinds": ["remove", "add"] }), now)?;
                    }
                    Ok(out)
                })
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
struct EpochAckReq {
    epoch: i64,
}

/// `POST /v1/groups/{id}/epoch-ack`.
pub(crate) async fn epoch_ack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let (req, _): (EpochAckReq, String) = parse(body)?;
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let g = live_group(tx, &id)?;
                let epoch = req.epoch.clamp(0, g.epoch);
                tx.execute(
                    "UPDATE group_leaves SET acked_epoch = MAX(acked_epoch, ?4), acked_at = ?5
                      WHERE group_id = ?1 AND mailbox = ?2 AND device_id = ?3",
                    params![id, me, device, epoch, now as i64],
                )?;
                Ok(Outcome::ok(json!({ "acked_epoch": epoch })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `GET /v1/groups/{id}/crypto`.
pub(crate) async fn crypto_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                let g = live_group(c, &id)?;
                let m = load_member(c, &id, &me)?
                    .filter(|m| m.state == "active" || (m.state == "keying" && m.role == "owner"))
                    .ok_or_else(|| GErr::forbidden("not_member", "you are not a member of this group"))?;
                let _ = m;
                let pending: Vec<Value> = pending_ops(c, &id, now)?
                    .into_iter()
                    .map(|p| json!({ "kind": p.kind, "mailbox": p.mailbox, "device_id": p.device_id, "since": p.since }))
                    .collect();
                let lagging: Vec<Value> = {
                    let mut stmt = c.prepare(
                        "SELECT mailbox, device_id, acked_epoch FROM group_leaves
                          WHERE group_id = ?1 AND acked_epoch < ?2 AND COALESCE(acked_at, 0) < ?3",
                    )?;
                    let rows = stmt.query_map(params![id, g.epoch, now as i64 - LAGGING_SECS as i64], |r| {
                        Ok(json!({ "mailbox": r.get::<_, String>(0)?, "device_id": r.get::<_, String>(1)?, "acked_epoch": r.get::<_, i64>(2)? }))
                    })?;
                    rows.collect::<Result<_, _>>()?
                };
                Ok(Outcome::ok(json!({
                    "epoch": g.epoch,
                    "revision": g.revision,
                    "activation_state": g.activation_state,
                    "in_group": is_leaf(c, &id, &me, &device)?,
                    "pending": pending,
                    "lagging_devices": lagging,
                })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
struct ResyncReq {
    request_key: String,
}

/// `POST /v1/groups/{id}/resync` — this device lost its group state.
pub(crate) async fn resync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::groups::ctx(&state, &headers, q.identity.as_deref()).await?;
        let device = ctx.device()?;
        let (req, hash): (ResyncReq, String) = parse(body)?;
        let me = ctx.address();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| {
                idempotent(tx, &me, &id, &format!("resync:{device}"), &req.request_key, &hash, now, |tx| {
                    live_group(tx, &id)?;
                    active_member(tx, &id, &me)?;
                    let n = tx.execute(
                        "UPDATE group_leaves SET stale = 1 WHERE group_id = ?1 AND mailbox = ?2 AND device_id = ?3",
                        params![id, me, device],
                    )?;
                    let mut out = Outcome::ok(json!({ "resync": true, "had_leaf": n > 0 }));
                    out.wake = emit_members(tx, &id, "group.crypto_pending", &json!({ "group_id": id, "kinds": ["remove", "add"] }), now)?;
                    Ok(out)
                })
            })
            .await
    }
    .await;
    finish(&state, result)
}

// ---- maintenance -------------------------------------------------------------------------------

/// Drop expired packages and old claims, and prune handshakes every leaf has moved past (or that
/// are older than 90 days; a device that needed one gets `resync_required`).
pub(crate) fn sweep_mls(tx: &Connection, now: u64) -> GResult<()> {
    tx.execute(
        "DELETE FROM mls_key_packages WHERE not_after < ?1 OR (claimed_at IS NOT NULL AND claimed_at < ?2)",
        params![now as i64, now as i64 - 86_400],
    )?;
    tx.execute(
        "DELETE FROM mls_kp_claims WHERE claimed_at < ?1",
        params![now as i64 - 86_400],
    )?;
    let groups: Vec<(String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT h.group_id, COALESCE((SELECT MIN(acked_epoch) FROM group_leaves l WHERE l.group_id = h.group_id), 0)
               FROM group_handshakes h GROUP BY h.group_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    for (gid, min_acked) in groups {
        let pruned: Option<i64> = tx.query_row(
            "SELECT MAX(seq) FROM group_handshakes
              WHERE group_id = ?1 AND (epoch < ?2 OR created_at < ?3)
                AND NOT EXISTS (SELECT 1 FROM mls_welcomes w WHERE w.group_id = ?1 AND w.seq = group_handshakes.seq AND w.acked_at IS NULL AND group_handshakes.created_at >= ?3)",
            params![gid, min_acked, now as i64 - HANDSHAKE_RETENTION_SECS as i64],
            |r| r.get(0),
        )?;
        if let Some(through) = pruned {
            tx.execute(
                "DELETE FROM group_handshakes WHERE group_id = ?1 AND seq <= ?2",
                params![gid, through],
            )?;
            tx.execute(
                "UPDATE groups SET pruned_through_seq = MAX(pruned_through_seq, ?2) WHERE id = ?1",
                params![gid, through],
            )?;
        }
    }
    Ok(())
}

// ---- group calls (internal) --------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct CallFrom {
    mailbox: String,
}

#[derive(Deserialize)]
pub(crate) struct GroupCallReq {
    group_id: String,
    from: CallFrom,
    kind: String,
    #[serde(default)]
    join: bool,
}

/// `POST /internal/v1/group-call-admission` — the calls service asks before it rings a group or
/// lets someone join a group call (SDS §4.7). Membership is live; each callee's own call settings
/// and blocks decide whether they ring.
pub(crate) async fn group_call_admission(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<GroupCallReq>, JsonRejection>,
) -> Response {
    if let Err(resp) = crate::internal_api::authorize_service(
        &state,
        &headers,
        crate::internal_api::Endpoint::GroupCallAdmission,
        Action::GroupCallAdmission,
    ) {
        return *resp;
    }
    let Ok(Json(req)) = body else {
        return ApiError::bad(
            "bad_request",
            "expected {group_id, from:{mailbox}, kind, join}",
        )
        .into_response();
    };
    if !matches!(req.kind.as_str(), "audio" | "video") {
        return ApiError::bad("bad_request", "kind must be audio or video").into_response();
    }
    let deny = |reason: &str| {
        Json(json!({ "allowed": false, "reason": reason, "members": [] })).into_response()
    };
    let caller = match state.store.get(req.from.mailbox.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => return deny("caller_invalid"),
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    if !crate::filtering::feature_on(
        &state,
        caller.account_id.as_deref(),
        crate::capabilities::GROUPS,
    )
    .await
    {
        return deny("feature_disabled");
    }
    let gid = req.group_id.clone();
    let me = caller.address.clone();
    let looked = state
        .store
        .group_read(move |c: &Connection| -> GResult<(String, Vec<String>)> {
            let g = live_group(c, &gid)?;
            active_member(c, &gid, &me)?;
            let others: Vec<String> = crate::groups::members_in(c, &gid, &["active"])?
                .into_iter()
                .map(|(m, _)| m)
                .filter(|m| *m != me)
                .collect();
            Ok((g.name, others))
        })
        .await;
    let (name, others) = match looked {
        Ok(x) => x,
        Err(_) => return deny("not_member"),
    };
    if req.join {
        return Json(json!({ "allowed": true, "group_name": name, "members": [] })).into_response();
    }
    let now = now_unix();
    let mut members = Vec::new();
    for address in others.into_iter().take(1_000) {
        let Ok(Some(callee)) = state.store.get(address.clone()).await else {
            continue;
        };
        let decision = match crate::filtering::decide_call(&state, &caller, &callee, now).await {
            Ok((rule, _)) => rule.as_str(),
            Err(_) => "block",
        };
        members.push(json!({ "address": address, "handle": callee.handle, "decision": decision }));
    }
    Json(json!({ "allowed": true, "group_name": name, "members": members })).into_response()
}

/// Record a group call's start or end as a notice in the group (from the calls service's
/// lifecycle events).
pub(crate) async fn call_notice(
    state: &AppState,
    group_id: &str,
    kind: &str,
    room: &str,
    by: Option<&str>,
) {
    let gid = group_id.to_string();
    let kind = kind.to_string();
    let room = room.to_string();
    let by = by.map(str::to_string);
    let now = now_unix();
    let result = state
        .store
        .group_tx(move |tx| -> GResult<Vec<String>> {
            let Some(g) = load_group(tx, &gid)? else { return Ok(Vec::new()) };
            if g.deleted_at.is_some() || g.activation_state != "active" {
                return Ok(Vec::new());
            }
            // One notice per room and kind, however often the event is retried.
            let dup: bool = tx
                .query_row(
                    "SELECT 1 FROM group_notices WHERE group_id = ?1 AND kind = ?2 AND json_extract(metadata, '$.room') = ?3",
                    params![gid, kind, room],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if dup {
                return Ok(Vec::new());
            }
            add_notice(tx, &gid, &kind, by.as_deref(), json!({ "room": room }), now)?;
            emit_members(tx, &gid, "group.settings", &json!({ "group_id": gid, "changed": ["call"], "revision": g.revision }), now)
        })
        .await;
    match result {
        Ok(wake) => {
            for a in wake {
                state.event_hub.wake(&a);
            }
        }
        Err(e) => tracing::warn!(code = e.code, "group call notice not recorded"),
    }
}
