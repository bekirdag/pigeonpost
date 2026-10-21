//! Organizations: the role matrix, the acting member, seat accounting, lifecycle and the gate that
//! every legacy mailbox route passes through (SDS §6; client contract `docs/orgs.md`).
//!
//! # Where the rules live
//!
//! * [`permits`] is the §6.4 matrix as one pure function, and [`can_manage`] the target-rank rule.
//!   Handlers resolve the caller's live roles inside the transaction that commits the change and
//!   pass the answer to `policy::authorize` as `Facts::org_permitted`, so the decision is still the
//!   policy's intersection (credential kind ∧ mandatory denies ∧ grant).
//! * [`mailbox_gate_in`] answers "may this org mailbox be used, and may it receive" for every
//!   pre-existing route (send, inbox, attachments, tokens). A suspended member, a removed member's
//!   detached mailbox, a deleted agent or a suspended org is closed there, once.
//! * Seat and agent counters are read inside the same IMMEDIATE transaction that reserves capacity
//!   (`Store::group_tx`), so two invitations racing for the last seat cannot both succeed.
//!
//! Members' message content is never reachable from here (D-04): nothing in this module or the
//! org routes reads `messages`.

use crate::audit::ActorKind;
use crate::groups::{GErr, GResult};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::principal::{require_human, require_step_up, AuthDenial, MemberSession};
use crate::store::{self, AuditRow};
use crate::{now_unix, AccountCredential, AppState, Principal};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

// ---- limits ------------------------------------------------------------------------------------

/// Invitations live seven days (SDS §7.2).
pub(crate) const INVITE_TTL_SECS: u64 = 7 * 86_400;
/// Rows per invitation request (SDS §7.4).
pub(crate) const INVITES_PER_REQUEST: usize = 200;
/// Payment lapse timeline (D-14): past due for 7 days, then read-only for 14, then suspended.
pub(crate) const PAST_DUE_SECS: i64 = 7 * 86_400;
pub(crate) const READ_ONLY_SECS: i64 = 14 * 86_400;

// ---- roles and the permission matrix (SDS §6.2, §6.4) ------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    Owner,
    Admin,
    PeopleAdmin,
    AgentAdmin,
    BillingAdmin,
}

impl Role {
    pub(crate) const ALL: [Role; 5] = [
        Role::Owner,
        Role::Admin,
        Role::PeopleAdmin,
        Role::AgentAdmin,
        Role::BillingAdmin,
    ];
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::PeopleAdmin => "people_admin",
            Role::AgentAdmin => "agent_admin",
            Role::BillingAdmin => "billing_admin",
        }
    }
    pub(crate) fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// The roles one member holds. Every active member is also a plain member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Roles(pub BTreeSet<Role>);

impl Roles {
    pub(crate) fn of(list: &[Role]) -> Self {
        Roles(list.iter().copied().collect())
    }
    pub(crate) fn has(&self, r: Role) -> bool {
        self.0.contains(&r)
    }
    fn any(&self, rs: &[Role]) -> bool {
        rs.iter().any(|r| self.has(*r))
    }
    /// 3 owner, 2 full admin, 1 a scoped admin role, 0 a plain member.
    pub(crate) fn rank(&self) -> u8 {
        if self.has(Role::Owner) {
            3
        } else if self.has(Role::Admin) {
            2
        } else if !self.0.is_empty() {
            1
        } else {
            0
        }
    }
    pub(crate) fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(|r| r.as_str()).collect()
    }
}

/// What a member may do on the org's administrative surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Perm {
    View,
    ManagePeople,
    GrantAdmin,
    GrantScopedAdmin,
    ManageAgents,
    Delegate,
    ApproveDevice,
    EditMemberRules,
    EditAgentRules,
    CreateRoom,
    DeleteAnyRoom,
    ViewBilling,
    ManageBilling,
    Security,
    ExportAudit,
    DeleteOrg,
    TransferOwnership,
}

impl Perm {
    #[cfg(test)]
    pub(crate) const ALL: [Perm; 17] = [
        Perm::View,
        Perm::ManagePeople,
        Perm::GrantAdmin,
        Perm::GrantScopedAdmin,
        Perm::ManageAgents,
        Perm::Delegate,
        Perm::ApproveDevice,
        Perm::EditMemberRules,
        Perm::EditAgentRules,
        Perm::CreateRoom,
        Perm::DeleteAnyRoom,
        Perm::ViewBilling,
        Perm::ManageBilling,
        Perm::Security,
        Perm::ExportAudit,
        Perm::DeleteOrg,
        Perm::TransferOwnership,
    ];

    /// The §3.4 actions that need a recent strong sign-in on top of the role.
    pub(crate) fn needs_step_up(self) -> bool {
        matches!(
            self,
            Perm::GrantAdmin | Perm::ExportAudit | Perm::DeleteOrg | Perm::TransferOwnership
        )
    }
}

/// The §6.4 matrix. `rooms_everyone` is the org's `rooms_creation = everyone` setting.
pub(crate) fn permits(roles: &Roles, perm: Perm, rooms_everyone: bool) -> bool {
    use Role::*;
    match perm {
        Perm::View => true,
        Perm::ManagePeople => roles.any(&[Owner, Admin, PeopleAdmin]),
        Perm::GrantAdmin => roles.has(Owner),
        Perm::GrantScopedAdmin => roles.any(&[Owner, Admin]),
        Perm::ManageAgents | Perm::Delegate | Perm::ApproveDevice => {
            roles.any(&[Owner, Admin, AgentAdmin])
        }
        Perm::EditMemberRules => roles.any(&[Owner, Admin, PeopleAdmin]),
        Perm::EditAgentRules => roles.any(&[Owner, Admin, AgentAdmin]),
        Perm::CreateRoom => rooms_everyone || !roles.0.is_empty(),
        Perm::DeleteAnyRoom => roles.any(&[Owner, Admin, PeopleAdmin]),
        Perm::ViewBilling => roles.any(&[Owner, Admin, BillingAdmin]),
        Perm::ManageBilling => roles.any(&[Owner, BillingAdmin]),
        Perm::Security | Perm::ExportAudit => roles.any(&[Owner, Admin]),
        Perm::DeleteOrg | Perm::TransferOwnership => roles.has(Owner),
    }
}

/// Target-rank rule (SDS §6.4): who may suspend, remove, rename or re-role whom.
///
/// The owner manages everyone but cannot remove or suspend themselves (ownership moves by
/// transfer). A full admin manages people below full admin. A people admin manages plain members
/// only. Nobody manages a peer or someone above them.
pub(crate) fn can_manage(actor: &Roles, target: &Roles, same_person: bool) -> bool {
    if same_person {
        return false;
    }
    match actor.rank() {
        3 => true,
        2 => target.rank() < 2,
        _ => actor.has(Role::PeopleAdmin) && target.rank() == 0,
    }
}

/// Whether `actor` may hand out (or take back) `role` at all. `owner` moves only by transfer.
pub(crate) fn may_grant(actor: &Roles, role: Role) -> bool {
    match role {
        Role::Owner => false,
        Role::Admin => permits(actor, Perm::GrantAdmin, false),
        _ => permits(actor, Perm::GrantScopedAdmin, false),
    }
}

// ---- rows --------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Settings {
    #[serde(default)]
    pub require_mfa: bool,
    #[serde(default)]
    pub invite_domains_only: bool,
    #[serde(default = "everyone")]
    pub rooms_creation: String,
}

fn everyone() -> String {
    "everyone".into()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            require_mfa: false,
            invite_domains_only: false,
            rooms_creation: everyone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OrgRow {
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub owner_account: String,
    pub internal_account: Option<String>,
    pub status: String,
    pub seats: i64,
    pub agent_limit: i64,
    pub storage_bytes: i64,
    pub plan_code: Option<String>,
    pub period_end: Option<i64>,
    pub settings: Settings,
    pub created_at: i64,
    pub revision: i64,
    #[allow(dead_code)]
    pub rules_revision: i64,
}

const ORG_COLS: &str = "id, namespace, name, owner_account, internal_account_id, status, seats, \
     agent_limit, storage_bytes, plan_code, period_end, settings, created_at, revision, rules_revision";

fn map_org(r: &rusqlite::Row<'_>) -> rusqlite::Result<OrgRow> {
    let settings: String = r.get(11)?;
    Ok(OrgRow {
        id: r.get(0)?,
        namespace: r.get(1)?,
        name: r.get(2)?,
        owner_account: r.get(3)?,
        internal_account: r.get(4)?,
        status: r.get(5)?,
        seats: r.get(6)?,
        agent_limit: r.get(7)?,
        storage_bytes: r.get(8)?,
        plan_code: r.get(9)?,
        period_end: r.get(10)?,
        settings: serde_json::from_str(&settings).unwrap_or_default(),
        created_at: r.get(12)?,
        revision: r.get(13)?,
        rules_revision: r.get(14)?,
    })
}

pub(crate) fn load_org(c: &Connection, id: &str) -> GResult<Option<OrgRow>> {
    Ok(c.query_row(
        &format!("SELECT {ORG_COLS} FROM orgs WHERE id = ?1"),
        [id],
        map_org,
    )
    .optional()?)
}

/// An org that exists and has been provisioned (has members, a namespace and a lifecycle).
pub(crate) fn provisioned(status: &str) -> bool {
    matches!(status, "active" | "past_due" | "read_only" | "suspended")
}

#[derive(Debug, Clone)]
pub(crate) struct MemberRow {
    pub id: String,
    pub org_id: String,
    pub account_id: Option<String>,
    pub identity: Option<String>,
    pub handle: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub contact_masked: Option<String>,
    pub status: String,
    pub authority_revision: i64,
    #[allow(dead_code)]
    pub invited_by: Option<String>,
    pub invited_at: Option<i64>,
    pub joined_at: Option<i64>,
    pub last_active_at: Option<i64>,
}

pub(crate) const MEMBER_COLS: &str = "id, org_id, account_id, identity, handle, first_name, \
     last_name, contact_masked, status, authority_revision, invited_by, invited_at, joined_at, last_active_at";

pub(crate) fn map_member(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemberRow> {
    Ok(MemberRow {
        id: r.get(0)?,
        org_id: r.get(1)?,
        account_id: r.get(2)?,
        identity: r.get(3)?,
        handle: r.get(4)?,
        first_name: r.get(5)?,
        last_name: r.get(6)?,
        contact_masked: r.get(7)?,
        status: r.get(8)?,
        authority_revision: r.get(9)?,
        invited_by: r.get(10)?,
        invited_at: r.get(11)?,
        joined_at: r.get(12)?,
        last_active_at: r.get(13)?,
    })
}

pub(crate) fn load_member(c: &Connection, org: &str, id: &str) -> GResult<Option<MemberRow>> {
    Ok(c.query_row(
        &format!("SELECT {MEMBER_COLS} FROM org_members WHERE org_id = ?1 AND id = ?2"),
        params![org, id],
        map_member,
    )
    .optional()?)
}

/// The caller's live (not removed) membership in `org`.
pub(crate) fn member_for_account(
    c: &Connection,
    org: &str,
    account: &str,
) -> GResult<Option<MemberRow>> {
    Ok(c.query_row(
        &format!(
            "SELECT {MEMBER_COLS} FROM org_members
              WHERE org_id = ?1 AND account_id = ?2 AND status <> 'removed'"
        ),
        params![org, account],
        map_member,
    )
    .optional()?)
}

pub(crate) fn roles_of(c: &Connection, org: &str, member: &str) -> GResult<Roles> {
    let mut stmt = c.prepare("SELECT role FROM org_roles WHERE org_id = ?1 AND member_id = ?2")?;
    let rows = stmt.query_map(params![org, member], |r| r.get::<_, String>(0))?;
    let mut out = BTreeSet::new();
    for role in rows {
        if let Some(r) = Role::parse(&role?) {
            out.insert(r);
        }
    }
    Ok(Roles(out))
}

pub(crate) fn member_json(c: &Connection, m: &MemberRow, now: u64) -> GResult<Value> {
    let roles = roles_of(c, &m.org_id, &m.id)?;
    let invite: Option<(String, i64)> = if m.status == "invited" {
        c.query_row(
            "SELECT id, expires_at FROM org_invites
              WHERE member_id = ?1 AND used_at IS NULL AND revoked_at IS NULL
              ORDER BY created_at DESC LIMIT 1",
            [&m.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    } else {
        None
    };
    Ok(json!({
        "id": m.id,
        "first_name": m.first_name,
        "last_name": m.last_name,
        "handle": m.handle,
        "address": m.identity,
        "roles": roles.names(),
        "status": m.status,
        "contact_masked": m.contact_masked,
        "invited_at": m.invited_at,
        "joined_at": m.joined_at,
        "last_active_at": m.last_active_at,
        "invite": invite.map(|(id, exp)| json!({
            "id": id,
            "expires_at": exp,
            "state": if exp > now as i64 { "pending" } else { "expired" },
        })),
    }))
}

// ---- accounting (SDS §6.3) ---------------------------------------------------------------------

/// Owner plus active and suspended people plus live invitation reservations.
pub(crate) fn seats_used(c: &Connection, org: &str, now: u64) -> GResult<i64> {
    Ok(c.query_row(
        "SELECT
            (SELECT COUNT(*) FROM org_members WHERE org_id = ?1 AND status IN ('active','suspended'))
          + (SELECT COUNT(*) FROM org_members m WHERE m.org_id = ?1 AND m.status = 'invited'
               AND EXISTS (SELECT 1 FROM org_invites i WHERE i.member_id = m.id AND i.used_at IS NULL
                            AND i.revoked_at IS NULL AND i.expires_at > ?2))",
        params![org, now as i64],
        |r| r.get(0),
    )?)
}

/// Active and suspended org agents.
pub(crate) fn agents_used(c: &Connection, org: &str) -> GResult<i64> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM org_agents WHERE org_id = ?1 AND status IN ('active','suspended')",
        [org],
        |r| r.get(0),
    )?)
}

pub(crate) fn org_json(c: &Connection, o: &OrgRow, now: u64) -> GResult<Value> {
    Ok(json!({
        "id": o.id,
        "name": o.name,
        "namespace": format!("/{}", o.namespace),
        "status": o.status,
        "seats": o.seats,
        "seats_used": seats_used(c, &o.id, now)?,
        "agent_limit": o.agent_limit,
        "agents_used": agents_used(c, &o.id)?,
        "storage_bytes": o.storage_bytes,
        "plan_code": o.plan_code,
        "period_end": o.period_end,
        "settings": o.settings,
        "created_at": o.created_at,
        "revision": o.revision,
    }))
}

// ---- lifecycle (SDS §6.6, D-14) ----------------------------------------------------------------

/// The provisioned status an org should have, given its best entitlement.
///
/// Access follows verified paid access, not a raw provider string (§8.5): an entitlement that is
/// live is `active` (or `past_due` while the provider reports a collection problem); once paid
/// access has lapsed the org is `past_due` for seven days, `read_only` for fourteen more, then
/// `suspended`.
pub(crate) fn lifecycle_status(
    best: Option<&store::Entitlement>,
    current: &str,
    now: u64,
) -> &'static str {
    let Some(e) = best else {
        return match current {
            "active" => "active",
            "past_due" => "past_due",
            "read_only" => "read_only",
            _ => "suspended",
        };
    };
    if e.active {
        return if matches!(e.status.as_str(), "past_due" | "grace") {
            "past_due"
        } else {
            "active"
        };
    }
    let lapsed_at = e.paid_through.or(e.period_end).unwrap_or(e.updated_at);
    let since = now as i64 - lapsed_at;
    if since < PAST_DUE_SECS {
        "past_due"
    } else if since < PAST_DUE_SECS + READ_ONLY_SECS {
        "read_only"
    } else {
        "suspended"
    }
}

/// Set up a just-activated org inside the activation transaction: the owner's membership and role
/// (the owner holds one purchased seat), and the default "everyone is known" rule (SDS §9.4).
/// Idempotent.
pub(crate) fn provision_in(c: &Connection, org_id: &str, now: u64) -> Result<(), rusqlite::Error> {
    let owner: Option<String> = c
        .query_row(
            "SELECT owner_account FROM orgs WHERE id = ?1",
            [org_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(owner) = owner else { return Ok(()) };
    let has_owner: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_roles WHERE org_id = ?1 AND role = 'owner')",
        [org_id],
        |r| r.get(0),
    )?;
    if has_owner {
        return Ok(());
    }
    let mid = new_id("mem");
    c.execute(
        "INSERT INTO org_members (id, org_id, account_id, first_name, last_name, status, joined_at)
         VALUES (?1, ?2, ?3, '', '', 'active', ?4)",
        params![mid, org_id, owner, now as i64],
    )?;
    c.execute(
        "INSERT INTO org_roles (org_id, member_id, role, granted_by, granted_at)
         VALUES (?1, ?2, 'owner', 'system', ?3)",
        params![org_id, mid, now as i64],
    )?;
    let ns: String = c.query_row("SELECT namespace FROM orgs WHERE id = ?1", [org_id], |r| {
        r.get(0)
    })?;
    let everyone = format!("/{ns}/*");
    c.execute(
        "INSERT INTO org_rules (id, org_id, owners, peers, admission, known, priority, description,
                created_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3, 'allow', 1, 100, 'Everyone at the company is known to each other',
                 'system', ?4, ?4)",
        params![new_id("rule"), org_id, everyone, now as i64],
    )?;
    Ok(())
}

// ---- the gate on every legacy mailbox route ----------------------------------------------------

/// Whether an address may be used and may receive, as far as organizations are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Not an org mailbox, or a live one.
    Open,
    /// The org is read-only (§6.6): the mailbox can read but not send.
    ReadOnly,
    /// Closed: cannot be used, and receives nothing (senders see "address not found").
    Closed(&'static str),
}

/// The gate for one address, read from the org tables.
pub(crate) fn mailbox_gate_in(c: &Connection, address: &str) -> rusqlite::Result<Gate> {
    let member: Option<(String, String)> = c
        .query_row(
            "SELECT m.status, o.status FROM org_members m JOIN orgs o ON o.id = m.org_id
              WHERE m.identity = ?1 ORDER BY CASE m.status WHEN 'removed' THEN 1 ELSE 0 END LIMIT 1",
            [address],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (own, org) = match member {
        Some(found) => found,
        None => match c
            .query_row(
                "SELECT a.status, o.status FROM org_agents a JOIN orgs o ON o.id = a.org_id
                  WHERE a.identity = ?1",
                [address],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            Some(found) => found,
            None => return Ok(Gate::Open),
        },
    };
    Ok(match (own.as_str(), org.as_str()) {
        ("removed" | "deleted", _) => Gate::Closed("org_mailbox_removed"),
        ("suspended", _) => Gate::Closed("org_mailbox_suspended"),
        (_, "suspended" | "deleted" | "pending" | "paid_unprovisioned") => {
            Gate::Closed("org_inactive")
        }
        (_, "read_only") => Gate::ReadOnly,
        _ => Gate::Open,
    })
}

/// [`mailbox_gate_in`] on its own connection. A store error closes the mailbox: an org whose
/// state cannot be read must not be assumed live.
pub(crate) async fn mailbox_gate(state: &AppState, address: &str) -> Gate {
    let address = address.to_string();
    state
        .store
        .group_read(move |c: &Connection| -> Result<Gate, GErr> {
            Ok(mailbox_gate_in(c, &address)?)
        })
        .await
        .unwrap_or(Gate::Closed("org_state_unavailable"))
}

/// The refusal for acting as a closed mailbox.
pub(crate) fn closed_error(code: &'static str) -> crate::ApiError {
    let message = match code {
        "org_mailbox_suspended" => "this organization mailbox is suspended",
        "org_mailbox_removed" => "this organization mailbox is no longer available",
        "org_inactive" => "this organization is not active",
        _ => "this mailbox is not available",
    };
    crate::ApiError::new(StatusCode::FORBIDDEN, code, message)
}

/// Whether `account` holds a live `read_inbox` delegation for the agent at `address`.
pub(crate) fn operates_in(
    c: &Connection,
    account: &str,
    address: &str,
    now: u64,
) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_delegations d
            JOIN org_members m ON m.org_id = d.org_id AND m.id = d.grantee_member_id
            JOIN org_agents a ON a.org_id = d.org_id AND a.identity = d.target_identity
            JOIN orgs o ON o.id = d.org_id
           WHERE d.target_identity = ?2 AND m.account_id = ?1 AND m.status = 'active'
             AND a.status = 'active' AND d.revoked_at IS NULL
             AND (d.expires_at IS NULL OR d.expires_at > ?3)
             AND o.status IN ('active','past_due','read_only')
             AND EXISTS (SELECT 1 FROM json_each(d.capabilities) WHERE value = 'read_inbox'))",
        params![account, address, now as i64],
        |r| r.get(0),
    )
}

// ---- the acting member -------------------------------------------------------------------------

/// A person acting on one org's administrative surface.
pub(crate) struct OrgCtx {
    pub principal: Principal,
    #[allow(dead_code)]
    pub session: MemberSession,
    pub account: String,
    pub org: OrgRow,
    pub member: MemberRow,
    pub roles: Roles,
}

/// Turn a human/step-up denial into the org error shape, with the RFC 9470 challenge in both the
/// header and the body (browsers may not expose `WWW-Authenticate` across origins).
pub(crate) fn auth_denial(d: AuthDenial) -> GErr {
    let mut e = match &d {
        AuthDenial::HumanRequired => GErr::forbidden(
            "human_required",
            "only a person signed in to Pigeonpost can do this; agents and API keys cannot",
        ),
        AuthDenial::StepUpUnconfigured => GErr::forbidden(
            "step_up_unavailable",
            "this action needs strong sign-in, which this server has not been configured to accept",
        ),
        AuthDenial::StepUpRequired {
            acr_values,
            max_age,
        } => GErr::new(
            StatusCode::UNAUTHORIZED,
            "step_up_required",
            "sign in again with two-step verification, then retry",
        )
        .with("acr_values", acr_values.join(" "))
        .with("max_age", *max_age),
    };
    e.challenge = d.challenge();
    e
}

/// The member session behind a principal, or `human_required`.
pub(crate) fn person(principal: &Principal) -> GResult<(String, MemberSession)> {
    let session = require_human(principal).map_err(auth_denial)?.clone();
    match principal {
        Principal::Account(account, AccountCredential::Member(_)) => Ok((account.clone(), session)),
        _ => Err(auth_denial(AuthDenial::HumanRequired)),
    }
}

/// Resolve a signed-in person and the `orgs` flag; no org yet.
pub(crate) async fn person_ctx(
    state: &AppState,
    headers: &HeaderMap,
) -> GResult<(Principal, String, MemberSession)> {
    let principal = crate::principal_for_token(state, crate::bearer(headers)).await?;
    let (account, session) = person(&principal)?;
    if !crate::filtering::feature_on(state, Some(&account), crate::capabilities::ORGS).await {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "organizations are not enabled for this account",
        ));
    }
    Ok((principal, account, session))
}

/// Resolve the acting member of `org_id`. Non-members get the same 404 as an unknown org, so the
/// route does not reveal which orgs exist.
pub(crate) async fn org_ctx(
    state: &AppState,
    headers: &HeaderMap,
    org_id: &str,
) -> GResult<OrgCtx> {
    let (principal, account, session) = person_ctx(state, headers).await?;
    let org_id = org_id.to_string();
    let acct = account.clone();
    let now = now_unix();
    let (org, member, roles) = state
        .store
        .group_tx(move |tx| -> GResult<(OrgRow, MemberRow, Roles)> {
            let not_found =
                || GErr::new(StatusCode::NOT_FOUND, "not_found", "no such organization");
            let org = load_org(tx, &org_id)?.ok_or_else(not_found)?;
            if !provisioned(&org.status) {
                return Err(not_found());
            }
            // Orgs activated before members existed get their owner on first visit.
            provision_in(tx, &org.id, now)?;
            let member = member_for_account(tx, &org.id, &acct)?.ok_or_else(not_found)?;
            if member.status != "active" {
                return Err(GErr::forbidden(
                    "not_org_member",
                    "your membership of this organization is not active",
                ));
            }
            if member.last_active_at.is_none_or(|t| t + 300 < now as i64) {
                tx.execute(
                    "UPDATE org_members SET last_active_at = ?2 WHERE id = ?1",
                    params![member.id, now as i64],
                )?;
            }
            let roles = roles_of(tx, &org.id, &member.id)?;
            Ok((org, member, roles))
        })
        .await?;
    let ctx = OrgCtx {
        principal,
        session,
        account,
        org,
        member,
        roles,
    };
    // Org-required two-step sign-in applies to every person on the org's surface (§3.4).
    if ctx.org.settings.require_mfa {
        require_step_up(&ctx.principal, u64::MAX / 4, &state.step_up.acr_values, now).map_err(
            |d| match d {
                AuthDenial::StepUpRequired { acr_values, .. } => {
                    auth_denial(AuthDenial::StepUpRequired {
                        acr_values,
                        max_age: state.step_up.max_age,
                    })
                    .with("reason", "mfa_required")
                }
                other => auth_denial(other),
            },
        )?;
    }
    Ok(ctx)
}

impl OrgCtx {
    pub(crate) fn rooms_everyone(&self) -> bool {
        self.org.settings.rooms_creation != "admins"
    }

    /// Require `perm` through the policy (and step-up for the §3.4 ones).
    pub(crate) fn require(&self, state: &AppState, perm: Perm) -> GResult<()> {
        let permitted = permits(&self.roles, perm, self.rooms_everyone());
        let facts = Facts {
            now: now_unix(),
            step_up: (*state.step_up).clone(),
            org_permitted: permitted,
            ..Facts::default()
        };
        let action = if perm.needs_step_up() {
            Action::OrgSensitive
        } else {
            Action::OrgAdminister
        };
        authorize(
            &Actor::Principal(&self.principal),
            action,
            &Resource::Org(&self.org.id),
            &facts,
        )
        .map_err(|deny| match deny {
            crate::policy::DenyReason::Auth(d) => auth_denial(d),
            crate::policy::DenyReason::NotPermitted => GErr::forbidden(
                "forbidden",
                "your role in this organization does not allow this",
            ),
            other => GErr::forbidden(other.code(), "this action is not allowed"),
        })
    }

    /// Require a recent strong sign-in on top of whatever role check already passed.
    pub(crate) fn step_up(&self, state: &AppState) -> GResult<()> {
        require_step_up(
            &self.principal,
            state.step_up.max_age,
            &state.step_up.acr_values,
            now_unix(),
        )
        .map(|_| ())
        .map_err(auth_denial)
    }

    /// Refuse capacity-creating changes on a read-only or suspended org (§6.6).
    pub(crate) fn require_writable(&self) -> GResult<()> {
        match self.org.status.as_str() {
            "active" | "past_due" => Ok(()),
            "read_only" => Err(GErr::conflict(
                "org_read_only",
                "this organization is read-only until its payment is settled",
            )),
            _ => Err(GErr::conflict(
                "org_inactive",
                "this organization is suspended",
            )),
        }
    }
}

// ---- audit and events --------------------------------------------------------------------------

/// Append an org audit record inside the caller's transaction, so it commits with the change.
#[allow(clippy::too_many_arguments)]
pub(crate) fn audit_in(
    c: &Connection,
    state_auditor: &crate::audit::Auditor,
    org: &str,
    actor_kind: ActorKind,
    actor_account: Option<&str>,
    action: &str,
    target: Option<&str>,
    details: Value,
    ip: Option<&str>,
    now: u64,
) -> GResult<()> {
    let ip = ip.and_then(|ip| ip.parse::<std::net::IpAddr>().ok());
    let mut row: AuditRow = state_auditor.row(
        actor_kind,
        actor_account.map(str::to_string),
        action,
        target.map(str::to_string),
        details,
        ip,
        None,
        now,
    );
    row.org_id = Some(org.to_string());
    store::audit::append_in(c, &row)?;
    Ok(())
}

/// Emit one typed event to one account (durable outbox, same transaction).
pub(crate) fn emit_to(
    c: &Connection,
    account: &str,
    kind: &str,
    data: &Value,
    now: u64,
) -> GResult<()> {
    store::append_event(c, account, kind, None, data, now)?;
    Ok(())
}

/// Emit to every active member's account.
pub(crate) fn emit_members(
    c: &Connection,
    org: &str,
    kind: &str,
    data: &Value,
    now: u64,
) -> GResult<Vec<String>> {
    let mut stmt = c.prepare(
        "SELECT DISTINCT account_id FROM org_members
          WHERE org_id = ?1 AND status = 'active' AND account_id IS NOT NULL",
    )?;
    let accounts: Vec<String> = stmt
        .query_map([org], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for a in &accounts {
        emit_to(c, a, kind, data, now)?;
    }
    Ok(accounts)
}

// ---- identifiers, tokens, contacts, handles ----------------------------------------------------

/// `<prefix>_<26 base32>` (130 bits).
pub(crate) fn new_id(prefix: &str) -> String {
    let raw = crate::groups::rand_b32(17);
    format!("{prefix}_{}", &raw[..26])
}

/// URL-safe base64 without padding.
pub(crate) fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// A 256-bit secret from the OS CSPRNG, base64url (43 characters) (SDS §7.2).
pub(crate) fn new_secret() -> String {
    let mut raw = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut raw);
    b64url(&raw)
}

/// SHA-256 of a presented secret, for lookups.
pub(crate) fn secret_hash(secret: &str) -> Vec<u8> {
    crate::sha256(secret.trim().as_bytes()).to_vec()
}

/// The one normalization applied to a contact address, at creation and at acceptance alike:
/// trim, lower-case. Dots and `+tags` are significant (SDS §7.2).
pub(crate) fn canonical_contact(raw: &str) -> Option<String> {
    let c = raw.trim().to_lowercase();
    let (local, domain) = c.split_once('@')?;
    let ok = !local.is_empty()
        && local.len() <= 64
        && domain.len() >= 3
        && domain.len() <= 253
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !c.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        && c.matches('@').count() == 1;
    ok.then_some(c)
}

/// `j***@example.com`.
pub(crate) fn mask_contact(canonical: &str) -> String {
    match canonical.split_once('@') {
        Some((local, domain)) => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".into(),
    }
}

pub(crate) fn contact_domain(canonical: &str) -> &str {
    canonical.split_once('@').map(|(_, d)| d).unwrap_or("")
}

/// Canonical `/<ns>/<name>` inside this org's namespace, or `invalid_handle`.
pub(crate) fn org_handle(org: &OrgRow, raw: &str) -> GResult<String> {
    let raw = raw.trim();
    let raw = if raw.starts_with('/') {
        raw.to_string()
    } else {
        format!("/{}/{raw}", org.namespace)
    };
    let destination = pigeonpost_core::Destination::for_handle(&raw)
        .map_err(|_| GErr::bad("invalid_handle", "expected /<namespace>/<name>"))?;
    let canonical = destination
        .handle()
        .ok_or_else(|| GErr::bad("invalid_handle", "not a handle"))?
        .to_string();
    let segments: Vec<&str> = canonical.trim_start_matches('/').split('/').collect();
    if segments.len() != 2 || segments[0] != org.namespace {
        return Err(GErr::bad(
            "invalid_handle",
            format!(
                "addresses in this organization look like /{}/<name>",
                org.namespace
            ),
        ));
    }
    Ok(canonical)
}

/// Whether `handle` is free for a new member or agent in this org. A held address (left by a
/// removed member) is free only when `use_hold` — the explicit reassignment path.
pub(crate) fn handle_free_in(
    c: &Connection,
    org: &str,
    handle: &str,
    use_hold: bool,
) -> GResult<bool> {
    let taken: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM identities WHERE handle = ?2)
             OR EXISTS(SELECT 1 FROM org_members WHERE org_id = ?1 AND handle = ?2 AND status <> 'removed')
             OR EXISTS(SELECT 1 FROM org_agents WHERE org_id = ?1 AND handle = ?2 AND status <> 'deleted')",
        params![org, handle],
        |r| r.get(0),
    )?;
    if taken {
        return Ok(false);
    }
    let held: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_handle_holds WHERE org_id = ?1 AND handle = ?2)",
        params![org, handle],
        |r| r.get(0),
    )?;
    Ok(!held || use_hold)
}

/// Mint a mailbox inside the caller's transaction: keypair, sealed seed, and a capability hash
/// nobody holds (org mailboxes are reached by their owner's account or by device credentials).
pub(crate) fn mint_identity_in(
    c: &Connection,
    vault: &crate::vault::Vault,
    handle: &str,
    account: &str,
    label: Option<&str>,
    now: u64,
) -> GResult<String> {
    use zeroize::Zeroize;
    let identity = pigeonpost_core::Identity::generate();
    let address = identity.address().as_str().to_string();
    let ed = identity.verifying_key().to_bytes();
    let x = pigeonpost_core::keys::x25519_public(&identity);
    let mut seed = identity.to_seed();
    let wrapped = vault.wrap(&seed);
    seed.zeroize();
    let wrapped = wrapped.map_err(|_| {
        GErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "vault_error",
            "vault_error",
        )
    })?;
    let unheld = crate::sha256(new_secret().as_bytes());
    c.execute(
        "INSERT INTO identities
           (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at, account_id, handle)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            address,
            &ed[..],
            &x[..],
            &wrapped.nonce[..],
            wrapped.ct,
            &unheld[..],
            label,
            now as i64,
            account,
            handle,
        ],
    )?;
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(list: &[Role]) -> Roles {
        Roles::of(list)
    }

    /// The §6.4 matrix, row by row, for every role and a plain member.
    #[test]
    fn permission_matrix_matches_the_sds() {
        use Role::*;
        let owner = r(&[Owner]);
        let admin = r(&[Admin]);
        let people = r(&[PeopleAdmin]);
        let agents = r(&[AgentAdmin]);
        let billing = r(&[BillingAdmin]);
        let member = r(&[]);
        let all = [&owner, &admin, &people, &agents, &billing, &member];
        let expect = |perm: Perm, want: [bool; 6]| {
            for (roles, want) in all.iter().zip(want) {
                assert_eq!(
                    permits(roles, perm, false),
                    want,
                    "{perm:?} for {:?}",
                    roles.names()
                );
            }
        };
        expect(Perm::View, [true; 6]);
        expect(Perm::ManagePeople, [true, true, true, false, false, false]);
        expect(Perm::GrantAdmin, [true, false, false, false, false, false]);
        expect(
            Perm::GrantScopedAdmin,
            [true, true, false, false, false, false],
        );
        expect(Perm::ManageAgents, [true, true, false, true, false, false]);
        expect(Perm::Delegate, [true, true, false, true, false, false]);
        expect(Perm::ApproveDevice, [true, true, false, true, false, false]);
        expect(
            Perm::EditMemberRules,
            [true, true, true, false, false, false],
        );
        expect(
            Perm::EditAgentRules,
            [true, true, false, true, false, false],
        );
        expect(Perm::CreateRoom, [true, true, true, true, true, false]);
        expect(Perm::DeleteAnyRoom, [true, true, true, false, false, false]);
        expect(Perm::ViewBilling, [true, true, false, false, true, false]);
        expect(
            Perm::ManageBilling,
            [true, false, false, false, true, false],
        );
        expect(Perm::Security, [true, true, false, false, false, false]);
        expect(Perm::ExportAudit, [true, true, false, false, false, false]);
        expect(Perm::DeleteOrg, [true, false, false, false, false, false]);
        expect(
            Perm::TransferOwnership,
            [true, false, false, false, false, false],
        );
        // rooms_creation = everyone lets plain members create rooms.
        assert!(permits(&member, Perm::CreateRoom, true));
        // Every perm is covered above (a new one must be added to the matrix test).
        assert_eq!(Perm::ALL.len(), 17);
        for p in Perm::ALL {
            assert!(permits(&owner, p, false), "owner holds {p:?}");
        }
    }

    #[test]
    fn target_rank_rules() {
        use Role::*;
        let owner = r(&[Owner]);
        let admin = r(&[Admin]);
        let people = r(&[PeopleAdmin]);
        let agents = r(&[AgentAdmin]);
        let member = r(&[]);
        assert!(can_manage(&owner, &admin, false));
        assert!(
            !can_manage(&owner, &owner, true),
            "owner cannot remove themselves"
        );
        assert!(can_manage(&admin, &people, false));
        assert!(can_manage(&admin, &member, false));
        assert!(!can_manage(&admin, &admin, false), "no peer full admins");
        assert!(!can_manage(&admin, &owner, false));
        assert!(can_manage(&people, &member, false));
        assert!(
            !can_manage(&people, &agents, false),
            "people admins manage plain members only"
        );
        assert!(!can_manage(&people, &admin, false));
        assert!(!can_manage(&agents, &member, false));
        assert!(!can_manage(&member, &member, false));
        // Nobody re-roles themselves.
        assert!(!can_manage(&admin, &admin, true));
        // Grants.
        assert!(may_grant(&owner, Admin));
        assert!(!may_grant(&admin, Admin));
        assert!(may_grant(&admin, PeopleAdmin));
        assert!(
            !may_grant(&people, PeopleAdmin),
            "a people admin cannot promote anyone"
        );
        assert!(
            !may_grant(&owner, Owner),
            "ownership moves by transfer only"
        );
    }

    #[test]
    fn contacts_normalize_one_way() {
        assert_eq!(
            canonical_contact("  Jane.Doe+x@Example.COM "),
            Some("jane.doe+x@example.com".into())
        );
        assert_ne!(
            canonical_contact("janedoe@example.com"),
            canonical_contact("jane.doe@example.com")
        );
        for bad in [
            "",
            "a@b",
            "@example.com",
            "a@@example.com",
            "a b@example.com",
            "a@.com",
            "a@example.",
        ] {
            assert_eq!(canonical_contact(bad), None, "{bad}");
        }
        assert_eq!(mask_contact("jane@example.com"), "j***@example.com");
    }

    #[test]
    fn secrets_are_256_bit_base64url() {
        let s = new_secret();
        assert_eq!(s.len(), 43);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(s, new_secret());
    }

    fn ent(active: bool, status: &str, paid_through: Option<i64>) -> store::Entitlement {
        store::Entitlement {
            id: "e".into(),
            subject_kind: "org".into(),
            subject_id: "o".into(),
            source: "grant".into(),
            plan_code: "ent-5-monthly-usd".into(),
            plan_version: "1".into(),
            names: None,
            seats: Some(5),
            agent_limit: Some(50),
            storage_bytes: None,
            status: status.into(),
            period_end: paid_through,
            paid_through,
            provider_ref: "r".into(),
            provider_environment: "live".into(),
            cancel_requested_at: None,
            pending_plan_code: None,
            pending_effective_at: None,
            updated_at: 0,
            reconciliation_version: 1,
            active,
        }
    }

    #[test]
    fn lifecycle_follows_paid_access() {
        let now: u64 = 10_000_000;
        let n = now as i64;
        assert_eq!(
            lifecycle_status(Some(&ent(true, "active", Some(n + 10))), "active", now),
            "active"
        );
        assert_eq!(
            lifecycle_status(Some(&ent(true, "past_due", Some(n + 10))), "active", now),
            "past_due"
        );
        assert_eq!(
            lifecycle_status(
                Some(&ent(false, "expired", Some(n - 86_400))),
                "active",
                now
            ),
            "past_due"
        );
        assert_eq!(
            lifecycle_status(
                Some(&ent(false, "expired", Some(n - 8 * 86_400))),
                "active",
                now
            ),
            "read_only"
        );
        assert_eq!(
            lifecycle_status(
                Some(&ent(false, "expired", Some(n - 20 * 86_400))),
                "active",
                now
            ),
            "read_only"
        );
        assert_eq!(
            lifecycle_status(
                Some(&ent(false, "expired", Some(n - 22 * 86_400))),
                "active",
                now
            ),
            "suspended"
        );
        // A renewal brings a suspended org straight back.
        assert_eq!(
            lifecycle_status(
                Some(&ent(true, "active", Some(n + 86_400))),
                "suspended",
                now
            ),
            "active"
        );
    }
}
