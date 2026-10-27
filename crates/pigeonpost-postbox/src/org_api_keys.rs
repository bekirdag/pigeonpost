//! Org API keys (SDS §18.2; contract `docs/org-api.md` §1).
//!
//! A key is an automation credential for one organization: `pk_org_` + 32 base32 characters,
//! stored only as its SHA-256, shown once. What it may do is the intersection of
//!
//! * its **scopes** (fixed at creation),
//! * the **live authority of the person who created it** — they must still be an active owner or
//!   admin, and their current roles must permit the operation (`org::permits`), and
//! * the org's own state (live, not read-only for writes),
//!
//! decided by `policy::authorize` with `Actor::OrgKey` and `Action::OrgApi`. Nothing here can make
//! a key into a person: every human-only or step-up action refuses it with `human_required`.
//!
//! Creating a key is itself a §3.4 step-up action (`require_step_up`); when the deployment has no
//! strong-auth `acr` configured the answer is the documented `403 step_up_unavailable`, never a
//! bypass.

use crate::audit::ActorKind;
use crate::groups::{finish, parse, rate_limit, GErr, GResult, Outcome};
use crate::org::{self, load_member, load_org, roles_of, OrgRow, Perm, Roles};
use crate::policy::{authorize, Action, Actor, DenyReason, Facts, Resource};
use crate::{capabilities, now_unix, AppState, ClientIp};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::IpAddr;

/// Every key starts with this.
pub(crate) const KEY_PREFIX: &str = "pk_org_";
/// Characters after the prefix (base32, 160 bits).
const KEY_BODY_LEN: usize = 32;
/// Live (unrevoked, unexpired) keys one org may hold.
pub(crate) const MAX_KEYS_PER_ORG: i64 = 25;
/// Entries in one key's IP allowlist.
pub(crate) const MAX_ALLOWLIST: usize = 20;
/// The longest expiry a key may be given.
pub(crate) const MAX_EXPIRY_DAYS: u64 = 730;

/// The scopes, in the order the portal and the docs list them, with what each allows.
pub(crate) const SCOPES: &[(&str, &str)] = &[
    (
        "org:read",
        "Read the organization, members, roles, agents, rules, rooms, usage, billing status and webhooks",
    ),
    (
        "members:write",
        "Invite, rename, suspend, reactivate and remove ordinary members",
    ),
    (
        "agents:write",
        "Create and delete agents, disconnect their devices, revoke unused enrollment codes",
    ),
    (
        "rules:write",
        "Create, change and delete restrictive org rules (block, review)",
    ),
    (
        "rooms:write",
        "Create rooms, change listing and auto-join, invite org mailboxes, delete rooms",
    ),
    (
        "messages:send",
        "Send direct messages to org mailboxes as the agent chosen for this key",
    ),
    ("audit:read", "Read the organization's audit log"),
    ("webhooks:write", "Create, rotate and delete webhooks"),
];

/// The §6.4 permissions the key's creator must hold for a scope, at creation and at every use.
pub(crate) fn scope_perms(scope: &str) -> &'static [Perm] {
    match scope {
        "org:read" => &[Perm::View],
        "members:write" => &[Perm::ManagePeople],
        "agents:write" => &[Perm::ManageAgents],
        "rules:write" => &[Perm::EditMemberRules, Perm::EditAgentRules],
        "rooms:write" => &[Perm::CreateRoom, Perm::DeleteAnyRoom],
        "messages:send" => &[Perm::ManageAgents],
        "audit:read" | "webhooks:write" => &[Perm::Security],
        _ => &[],
    }
}

fn known_scope(s: &str) -> bool {
    SCOPES.iter().any(|(name, _)| *name == s)
}

pub(crate) fn is_org_key(token: &str) -> bool {
    token.starts_with(KEY_PREFIX)
}

/// A fresh key: the secret (shown once) and its last four characters.
fn new_key() -> (String, String) {
    let body = crate::groups::rand_b32(20);
    let body = &body[..KEY_BODY_LEN];
    (
        format!("{KEY_PREFIX}{body}"),
        body[KEY_BODY_LEN - 4..].to_string(),
    )
}

// ---- IP allowlists -----------------------------------------------------------------------------

/// One allowlist entry: an IPv4 or IPv6 network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cidr {
    net: IpAddr,
    prefix: u8,
}

/// IPv4-mapped IPv6 (`::ffff:a.b.c.d`) is the IPv4 address it carries.
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let m = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits & m).into())
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let m = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits & m).into())
        }
    }
}

impl Cidr {
    /// `203.0.113.7`, `203.0.113.0/24`, `2001:db8::/32`. Host bits are cleared.
    pub(crate) fn parse(raw: &str) -> Option<Cidr> {
        let raw = raw.trim();
        let (addr, prefix) = match raw.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (raw, None),
        };
        let ip = canonical_ip(addr.parse::<IpAddr>().ok()?);
        let max = if ip.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok()?,
            Some(_) => return None,
            None => max,
        };
        if prefix > max {
            return None;
        }
        Some(Cidr {
            net: mask(ip, prefix),
            prefix,
        })
    }

    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical_ip(ip);
        ip.is_ipv4() == self.net.is_ipv4() && mask(ip, self.prefix) == self.net
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.net, self.prefix)
    }
}

// ---- the acting key ----------------------------------------------------------------------------

/// An authenticated org API key, with its org and its creator's live authority.
#[derive(Debug, Clone)]
pub(crate) struct KeyCtx {
    pub key_id: String,
    pub name: String,
    pub org: OrgRow,
    pub scopes: Vec<String>,
    pub last4: String,
    /// The org agent mailbox `messages:send` sends as.
    pub send_as: Option<String>,
    pub ip_allowlist: Vec<Cidr>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    /// The creator's roles now. Empty when they are no longer an active member.
    pub creator_roles: Roles,
}

/// Is the org in a state where its API may be used at all?
fn org_live(status: &str) -> bool {
    matches!(status, "active" | "past_due" | "read_only")
}

impl KeyCtx {
    pub(crate) fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    fn rooms_everyone(&self) -> bool {
        self.org.settings.rooms_creation != "admins"
    }

    /// Whether the creator's live roles allow every one of `perms` (and they still administer the
    /// org at all: only owners and admins can hold keys).
    pub(crate) fn creator_permits(&self, perms: &[Perm]) -> bool {
        org::permits(&self.creator_roles, Perm::Security, false)
            && perms
                .iter()
                .all(|p| org::permits(&self.creator_roles, *p, self.rooms_everyone()))
    }

    /// Require `scope`, plus the creator's authority for `perms`, through the policy.
    pub(crate) fn require(&self, scope: &str, perms: &[Perm]) -> GResult<()> {
        let scope_granted = self.has_scope(scope);
        let facts = Facts {
            now: now_unix(),
            org_permitted: self.creator_permits(perms),
            scope_granted,
            org_inactive: !org_live(&self.org.status),
            ..Facts::default()
        };
        authorize(
            &Actor::OrgKey {
                key_id: &self.key_id,
                org_id: &self.org.id,
            },
            Action::OrgApi,
            &Resource::Org(&self.org.id),
            &facts,
        )
        .map_err(|deny| deny_error(deny, scope_granted, scope))
    }

    /// Refuse changes on a read-only org (§6.6). Reads stay available.
    pub(crate) fn require_writable(&self) -> GResult<()> {
        match self.org.status.as_str() {
            "active" | "past_due" => Ok(()),
            "read_only" => Err(GErr::conflict(
                "org_read_only",
                "this organization is read-only until its payment is settled",
            )),
            _ => Err(GErr::conflict(
                "org_inactive",
                "this organization is not active",
            )),
        }
    }

    /// The key as `GET /org/v1/key` shows it.
    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.key_id,
            "name": self.name,
            "org_id": self.org.id,
            "scopes": self.scopes,
            "last4": self.last4,
            "send_as": self.send_as,
            "ip_allowlist": self.ip_allowlist.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            "created_at": crate::org_api::iso(Some(self.created_at)),
            "expires_at": crate::org_api::iso(self.expires_at),
        })
    }
}

/// A policy refusal as the org API's error.
pub(crate) fn deny_error(deny: DenyReason, scope_granted: bool, scope: &str) -> GErr {
    match deny {
        DenyReason::NotPermitted if !scope_granted => GErr::forbidden(
            "insufficient_scope",
            format!("this key does not have the {scope} scope"),
        ),
        DenyReason::NotPermitted => GErr::forbidden(
            "forbidden",
            "the person who created this key no longer has the authority for this",
        ),
        DenyReason::Mandatory("org_inactive") => {
            GErr::conflict("org_inactive", "this organization is not active")
        }
        DenyReason::Mandatory(_) => unauthorized(),
        DenyReason::Auth(_) => GErr::forbidden(
            "human_required",
            "only a person signed in to Pigeonpost can do this; API keys cannot",
        ),
        DenyReason::CredentialNotAccepted => {
            GErr::forbidden("forbidden", "this credential may not perform this action")
        }
    }
}

/// The one answer for a missing, unknown, revoked or expired key.
pub(crate) fn unauthorized() -> GErr {
    let mut e = GErr::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "a valid org API key is required (Authorization: Bearer pk_org_…)",
    );
    e.challenge = Some("Bearer realm=\"pigeonpost-org\"".into());
    e
}

struct KeyRow {
    id: String,
    org_id: String,
    name: String,
    scopes: String,
    last4: String,
    send_as: Option<String>,
    ip_allowlist: Option<String>,
    created_by_member: String,
    created_at: i64,
    expires_at: Option<i64>,
    revoked_at: Option<i64>,
}

fn parse_allowlist(raw: Option<&str>) -> Vec<Cidr> {
    raw.and_then(|r| serde_json::from_str::<Vec<String>>(r).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| Cidr::parse(c))
        .collect()
}

/// Resolve a bearer token to the acting key. Unknown, revoked and expired keys, and keys of a
/// deleted org, all get the same `401 unauthorized`.
pub(crate) async fn authenticate(
    state: &AppState,
    token: Option<&str>,
    client_ip: &str,
) -> GResult<KeyCtx> {
    let token = token.ok_or_else(unauthorized)?;
    let body = token.strip_prefix(KEY_PREFIX).ok_or_else(unauthorized)?;
    if body.len() != KEY_BODY_LEN || !body.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(unauthorized());
    }
    let hash = crate::sha256(token.as_bytes());
    let now = now_unix();
    let found = state
        .store
        .group_read(
            move |c: &Connection| -> GResult<Option<(KeyRow, OrgRow, Roles)>> {
                let row = c
                    .query_row(
                        "SELECT id, org_id, name, scopes, send_as, ip_allowlist, created_by_member,
                            created_at, expires_at, revoked_at, last4
                       FROM org_api_keys WHERE key_hash = ?1",
                        [&hash[..]],
                        |r| {
                            Ok(KeyRow {
                                id: r.get(0)?,
                                org_id: r.get(1)?,
                                name: r.get(2)?,
                                scopes: r.get(3)?,
                                send_as: r.get(4)?,
                                ip_allowlist: r.get(5)?,
                                created_by_member: r.get(6)?,
                                created_at: r.get(7)?,
                                expires_at: r.get(8)?,
                                revoked_at: r.get(9)?,
                                last4: r.get(10)?,
                            })
                        },
                    )
                    .optional()?;
                let Some(row) = row else { return Ok(None) };
                if row.revoked_at.is_some() || row.expires_at.is_some_and(|e| e <= now as i64) {
                    return Ok(None);
                }
                let Some(o) = load_org(c, &row.org_id)? else {
                    return Ok(None);
                };
                if !org::provisioned(&o.status) {
                    return Ok(None);
                }
                // The creator's authority is read live on every request (SDS §18.2).
                let roles = match load_member(c, &o.id, &row.created_by_member)? {
                    Some(m) if m.status == "active" => roles_of(c, &o.id, &m.id)?,
                    _ => Roles::default(),
                };
                Ok(Some((row, o, roles)))
            },
        )
        .await?;
    let (row, o, roles) = found.ok_or_else(unauthorized)?;
    if !crate::filtering::feature_on(state, Some(&o.owner_account), capabilities::ORG_API).await {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "the enterprise API is not enabled for this organization",
        ));
    }
    let allowlist = parse_allowlist(row.ip_allowlist.as_deref());
    if !allowlist.is_empty() {
        let ip = client_ip.parse::<IpAddr>().ok();
        if !ip.is_some_and(|ip| allowlist.iter().any(|c| c.contains(ip))) {
            return Err(GErr::forbidden(
                "ip_not_allowed",
                "this key may not be used from this address",
            ));
        }
    }
    Ok(KeyCtx {
        key_id: row.id,
        name: row.name,
        org: o,
        scopes: serde_json::from_str(&row.scopes).unwrap_or_default(),
        last4: row.last4,
        send_as: row.send_as,
        ip_allowlist: allowlist,
        created_at: row.created_at,
        expires_at: row.expires_at,
        creator_roles: roles,
    })
}

/// Revoke one key inside a transaction. `false` when it was not live.
pub(crate) fn revoke_in(
    c: &Connection,
    org_id: &str,
    key_id: &str,
    by: &str,
    now: u64,
) -> GResult<bool> {
    Ok(c.execute(
        "UPDATE org_api_keys SET revoked_at = ?3, revoked_by = ?4
          WHERE id = ?1 AND org_id = ?2 AND revoked_at IS NULL",
        params![key_id, org_id, now as i64, by],
    )? > 0)
}

// ---- dashboard routes (a person signed in) -----------------------------------------------------

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/orgs/{id}/api-keys", get(list).post(create))
        .route("/v1/orgs/{id}/api-keys/{key_id}", delete(revoke))
}

type Body = Result<Json<Value>, JsonRejection>;

/// The dashboard key routes follow the `org_api` flag on the org owner's account or the caller's.
async fn dashboard_flag(state: &AppState, ctx: &org::OrgCtx) -> GResult<()> {
    let on =
        crate::filtering::feature_on(state, Some(&ctx.org.owner_account), capabilities::ORG_API)
            .await
            || crate::filtering::feature_on(state, Some(&ctx.account), capabilities::ORG_API).await;
    if on {
        Ok(())
    } else {
        Err(GErr::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "the enterprise API is not enabled for this organization",
        ))
    }
}

fn key_json(c: &Connection, id: &str) -> GResult<Value> {
    let v = c.query_row(
        "SELECT k.id, k.name, k.scopes, k.last4, k.send_as, k.ip_allowlist, k.created_at,
                COALESCE(m.handle, trim(m.first_name || ' ' || m.last_name)), k.expires_at,
                k.last_used_at, k.revoked_at, a.handle
           FROM org_api_keys k
           LEFT JOIN org_members m ON m.id = k.created_by_member
           LEFT JOIN org_agents a ON a.org_id = k.org_id AND a.identity = k.send_as
          WHERE k.id = ?1",
        [id],
        |r| {
            let scopes: String = r.get(2)?;
            let allow: Option<String> = r.get(5)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "scopes": serde_json::from_str::<Value>(&scopes).unwrap_or(Value::Null),
                "last4": r.get::<_, String>(3)?,
                "send_as": r.get::<_, Option<String>>(11)?.or(r.get::<_, Option<String>>(4)?),
                "ip_allowlist": allow.and_then(|a| serde_json::from_str::<Value>(&a).ok()).unwrap_or(json!([])),
                "created_at": r.get::<_, i64>(6)?,
                "created_by": r.get::<_, Option<String>>(7)?,
                "expires_at": r.get::<_, Option<i64>>(8)?,
                "last_used_at": r.get::<_, Option<i64>>(9)?,
                "revoked_at": r.get::<_, Option<i64>>(10)?,
            }))
        },
    )?;
    Ok(v)
}

/// `GET /v1/orgs/{id}/api-keys`
async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        dashboard_flag(&state, &ctx).await?;
        ctx.require(&state, Perm::Security)?;
        let keys = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT id FROM org_api_keys WHERE org_id = ?1 ORDER BY created_at DESC, id",
                )?;
                let ids: Vec<String> = stmt
                    .query_map([&id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                ids.iter().map(|k| key_json(c, k)).collect()
            })
            .await?;
        Ok(Outcome::ok(json!({ "keys": keys })))
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    name: String,
    scopes: Vec<String>,
    #[serde(default)]
    expires_in_days: Option<u64>,
    #[serde(default)]
    ip_allowlist: Vec<String>,
    #[serde(default)]
    send_as: Option<String>,
}

/// Validated creation input, before the transaction.
struct Checked {
    name: String,
    scopes: Vec<String>,
    expires_at: Option<i64>,
    allowlist: Vec<String>,
    send_as: Option<String>,
}

fn check(req: CreateReq, ctx: &org::OrgCtx, now: u64) -> GResult<Checked> {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(GErr::bad(
            "bad_request",
            "name is 1 to 80 printable characters",
        ));
    }
    let mut scopes: Vec<String> = Vec::new();
    for s in &req.scopes {
        let s = s.trim();
        if !known_scope(s) {
            return Err(GErr::bad("invalid_scope", format!("unknown scope '{s}'")));
        }
        if !scopes.iter().any(|x| x == s) {
            scopes.push(s.to_string());
        }
    }
    if scopes.is_empty() {
        return Err(GErr::bad("invalid_scope", "choose at least one scope"));
    }
    // A key can never carry authority its creator does not have (SDS §18.2).
    for s in &scopes {
        if !scope_perms(s)
            .iter()
            .all(|p| org::permits(&ctx.roles, *p, ctx.rooms_everyone()))
        {
            return Err(GErr::forbidden(
                "forbidden",
                format!("your role cannot grant the {s} scope"),
            ));
        }
    }
    let expires_at = match req.expires_in_days {
        None => None,
        Some(d) if (1..=MAX_EXPIRY_DAYS).contains(&d) => Some(now as i64 + d as i64 * 86_400),
        Some(_) => {
            return Err(GErr::bad(
                "bad_request",
                format!("expires_in_days is 1 to {MAX_EXPIRY_DAYS}"),
            ))
        }
    };
    if req.ip_allowlist.len() > MAX_ALLOWLIST {
        return Err(GErr::bad(
            "bad_request",
            format!("at most {MAX_ALLOWLIST} allowlist entries"),
        ));
    }
    let mut allowlist = Vec::new();
    for raw in &req.ip_allowlist {
        let c = Cidr::parse(raw).ok_or_else(|| {
            GErr::bad(
                "invalid_ip_allowlist",
                format!("'{raw}' is not an IP address or network"),
            )
        })?;
        let s = c.to_string();
        if !allowlist.contains(&s) {
            allowlist.push(s);
        }
    }
    let sends = scopes.iter().any(|s| s == "messages:send");
    let send_as = match (
        sends,
        req.send_as
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (true, Some(h)) => Some(h.to_string()),
        (true, None) => {
            return Err(GErr::bad(
                "send_as_required",
                "messages:send needs the org agent mailbox the key sends as",
            ))
        }
        (false, Some(_)) => {
            return Err(GErr::bad(
                "bad_request",
                "send_as is only for keys with the messages:send scope",
            ))
        }
        (false, None) => None,
    };
    Ok(Checked {
        name,
        scopes,
        expires_at,
        allowlist,
        send_as,
    })
}

/// `POST /v1/orgs/{id}/api-keys` — owner/admin, with a recent strong sign-in (§3.4).
async fn create(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        dashboard_flag(&state, &ctx).await?;
        ctx.require(&state, Perm::Security)?;
        // Issuing an org API key is a §3.4 sensitive action: never without step-up.
        ctx.step_up(&state)?;
        rate_limit(&format!("orgkeycreate:{id}"), 30, 86_400)?;
        let (req, _): (CreateReq, String) = parse(body)?;
        let now = now_unix();
        let checked = check(req, &ctx, now)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let member = ctx.member.id.clone();
        let namespace = ctx.org.namespace.clone();
        let ip = Some(cip.as_str().to_string());
        let (secret, last4) = new_key();
        let hash = crate::sha256(secret.as_bytes());
        let body = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let live: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM org_api_keys WHERE org_id = ?1 AND revoked_at IS NULL
                        AND (expires_at IS NULL OR expires_at > ?2)",
                    params![id, now as i64],
                    |r| r.get(0),
                )?;
                if live >= MAX_KEYS_PER_ORG {
                    return Err(GErr::conflict(
                        "key_limit",
                        format!("an organization holds at most {MAX_KEYS_PER_ORG} live API keys"),
                    ));
                }
                let send_as = match &checked.send_as {
                    None => None,
                    Some(raw) => {
                        let handle = if raw.starts_with('/') {
                            raw.to_ascii_lowercase()
                        } else {
                            format!("/{namespace}/{}", raw.to_ascii_lowercase())
                        };
                        let found: Option<String> = tx
                            .query_row(
                                "SELECT identity FROM org_agents WHERE org_id = ?1
                                    AND (handle = ?2 OR identity = ?3) AND status = 'active'",
                                params![id, handle, raw],
                                |r| r.get(0),
                            )
                            .optional()?;
                        Some(found.ok_or_else(|| {
                            GErr::bad(
                                "invalid_send_as",
                                "send_as must be an active agent of this organization",
                            )
                        })?)
                    }
                };
                let kid = org::new_id("okey");
                let allow = (!checked.allowlist.is_empty()).then(|| json!(checked.allowlist).to_string());
                tx.execute(
                    "INSERT INTO org_api_keys (id, org_id, name, key_hash, last4, scopes, send_as, ip_allowlist,
                            created_by_member, created_by_account, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        kid,
                        id,
                        checked.name,
                        &hash[..],
                        last4,
                        json!(checked.scopes).to_string(),
                        send_as,
                        allow,
                        member,
                        account,
                        now as i64,
                        checked.expires_at
                    ],
                )?;
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "api_key.create",
                    Some(&kid),
                    json!({ "name": checked.name, "scopes": checked.scopes, "expires_at": checked.expires_at,
                            "allowlist_entries": checked.allowlist.len(), "send_as": send_as }),
                    ip.as_deref(),
                    now,
                )?;
                key_json(tx, &kid)
            })
            .await?;
        Ok(Outcome::created(json!({ "key": body, "secret": secret })))
    }
    .await;
    no_store(finish(&state, result))
}

/// `DELETE /v1/orgs/{id}/api-keys/{key_id}`
async fn revoke(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, key_id)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        dashboard_flag(&state, &ctx).await?;
        ctx.require(&state, Perm::Security)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let member = ctx.member.id.clone();
        let ip = Some(cip.as_str().to_string());
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                if !revoke_in(tx, &id, &key_id, &member, now)? {
                    return Err(GErr::new(
                        StatusCode::NOT_FOUND,
                        "not_found",
                        "no such live key",
                    ));
                }
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "api_key.revoke",
                    Some(&key_id),
                    json!({}),
                    ip.as_deref(),
                    now,
                )?;
                Ok(Outcome::ok(json!({ "key": key_json(tx, &key_id)? })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_have_the_documented_shape() {
        let (secret, last4) = new_key();
        assert!(secret.starts_with("pk_org_"));
        assert_eq!(secret.len(), KEY_PREFIX.len() + KEY_BODY_LEN);
        assert!(secret.ends_with(&last4));
        assert_ne!(new_key().0, secret);
    }

    #[test]
    fn allowlists_match_networks_exactly() {
        let net = Cidr::parse("203.0.113.0/24").unwrap();
        assert!(net.contains("203.0.113.9".parse().unwrap()));
        assert!(net.contains("::ffff:203.0.113.9".parse().unwrap()));
        assert!(!net.contains("203.0.114.9".parse().unwrap()));
        assert!(!net.contains("2001:db8::1".parse().unwrap()));
        // Host bits are cleared; a bare address is a /32.
        assert_eq!(Cidr::parse("203.0.113.77/24").unwrap(), net);
        let one = Cidr::parse("198.51.100.4").unwrap();
        assert!(one.contains("198.51.100.4".parse().unwrap()));
        assert!(!one.contains("198.51.100.5".parse().unwrap()));
        let v6 = Cidr::parse("2001:db8:1::/48").unwrap();
        assert!(v6.contains("2001:db8:1:2::3".parse().unwrap()));
        assert!(!v6.contains("2001:db8:2::3".parse().unwrap()));
        for bad in [
            "",
            "nonsense",
            "10.0.0.0/33",
            "10.0.0.0/",
            "10.0.0.0/-1",
            "::/129",
            "10.0.0.0/8x",
        ] {
            assert!(Cidr::parse(bad).is_none(), "{bad}");
        }
        assert_eq!(Cidr::parse("0.0.0.0/0").unwrap().to_string(), "0.0.0.0/0");
        assert!(Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn every_scope_maps_to_creator_permissions() {
        for (scope, _) in SCOPES {
            assert!(!scope_perms(scope).is_empty(), "{scope}");
        }
        assert!(scope_perms("org:write").is_empty());
    }
}
