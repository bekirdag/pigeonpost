//! Who may act on a Nest (Nests Phase B7, R-74).
//!
//! A Nest belongs to a person (its account) or to an organization (the org's internal account,
//! with the Nest's mailbox registered as an org agent, so it counts against the org's agent
//! addresses: 10 × seats). Access is resolved from live server state on every request; nothing is
//! cached, so removing a member or revoking a delegation ends their access at once, everywhere.
//!
//! | Role | Person-owned Nest | Org-owned Nest |
//! |---|---|---|
//! | `Owner` (manage: pause, resume, rename, delete, export) | the owning account | an active member with the owner, admin or agent_admin role, while the org is live |
//! | `Operator` (chat, files, approvals, usage) | — | an active member with a live `operate_nest` delegation on the Nest's mailbox |

use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    Operator,
    Owner,
}

#[derive(Debug, Clone)]
pub(crate) struct NestRef {
    pub nest_id: String,
    /// The holding account (a person's, or the org's internal one).
    pub account_id: String,
    pub org_id: Option<String>,
    pub address: Option<String>,
    pub label: String,
    pub state: String,
    pub funding: String,
    pub paused_reason: Option<String>,
}

type Row = (
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
);

/// The caller's role on `nest`, if any. `None` answers 404 upstream, the same as a missing Nest.
pub(crate) fn access_in(
    c: &Connection,
    nest: &str,
    caller: &str,
    now: u64,
) -> rusqlite::Result<Option<(Role, NestRef)>> {
    let row: Option<Row> = c
        .query_row(
            "SELECT account_id, address, label, state, funding, paused_reason FROM nests WHERE id = ?1 AND deleted_at IS NULL",
            [nest],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let Some((account_id, address, label, state, funding, paused_reason)) = row else {
        return Ok(None);
    };
    let org_id: Option<String> = c
        .query_row(
            "SELECT id FROM orgs WHERE internal_account_id = ?1",
            [&account_id],
            |r| r.get(0),
        )
        .optional()?;
    let nref = NestRef {
        nest_id: nest.to_string(),
        account_id: account_id.clone(),
        org_id: org_id.clone(),
        address: address.clone(),
        label,
        state,
        funding,
        paused_reason,
    };
    let Some(org) = org_id else {
        return Ok((account_id == caller).then_some((Role::Owner, nref)));
    };
    let admin: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_roles r JOIN org_members m ON m.org_id = r.org_id AND m.id = r.member_id
                         JOIN orgs o ON o.id = r.org_id
            WHERE r.org_id = ?1 AND m.account_id = ?2 AND m.status = 'active'
              AND r.role IN ('owner','admin','agent_admin') AND o.status IN ('active','past_due','read_only'))",
        params![org, caller],
        |r| r.get(0),
    )?;
    if admin {
        return Ok(Some((Role::Owner, nref)));
    }
    let Some(address) = address else {
        return Ok(None);
    };
    Ok(operates_in(c, caller, &address, now)?.then_some((Role::Operator, nref)))
}

/// A live `operate_nest` delegation for `account` on the org agent at `address`.
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
             AND EXISTS (SELECT 1 FROM json_each(d.capabilities) WHERE value = 'operate_nest'))",
        params![account, address, now as i64],
        |r| r.get(0),
    )
}

/// Whether a message from `sender` (a mailbox) to the org-owned Nest at `nest_address` may be
/// acted on: the sender's account is the org's Nest admin or a live operator. Computed live.
pub(crate) fn org_sender_ok(
    c: &Connection,
    nest_address: &str,
    sender: &str,
    now: u64,
) -> rusqlite::Result<bool> {
    let nest: Option<String> = c
        .query_row(
            "SELECT id FROM nests WHERE address = ?1 AND deleted_at IS NULL",
            [nest_address],
            |r| r.get(0),
        )
        .optional()?;
    let account: Option<Option<String>> = c
        .query_row(
            "SELECT account_id FROM identities WHERE address = ?1",
            [sender],
            |r| r.get(0),
        )
        .optional()?;
    let (Some(nest), Some(Some(mut account))) = (nest, account) else {
        return Ok(false);
    };
    // A member's org mailbox is held by the org's internal account: the person behind it is the
    // member it belongs to, if that membership is active.
    let member_account: Option<Option<String>> = c
        .query_row(
            "SELECT m.account_id FROM org_members m JOIN orgs o ON o.id = m.org_id
              WHERE m.identity = ?1 AND m.status = 'active' AND o.internal_account_id = ?2",
            params![sender, account],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(Some(person)) = member_account {
        account = person;
    }
    // A Nest's own mailbox never drives another Nest.
    let sender_is_nest: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM nests WHERE address = ?1 AND deleted_at IS NULL)",
        [sender],
        |r| r.get(0),
    )?;
    if sender_is_nest {
        return Ok(false);
    }
    Ok(access_in(c, &nest, &account, now)?.is_some())
}

/// Whether the Nest at `address` is owned by an organization.
pub(crate) fn is_org_nest(c: &Connection, address: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM nests n JOIN orgs o ON o.internal_account_id = n.account_id
            WHERE n.address = ?1 AND n.deleted_at IS NULL)",
        [address],
        |r| r.get(0),
    )
}

/// The common gate of every per-Nest route: an account credential allowed `action`, the `nests`
/// flag on for the caller, and at least `min` on this Nest. Anything less is 404.
pub(crate) async fn authorize_nest(
    state: &crate::AppState,
    headers: &axum::http::HeaderMap,
    action: crate::policy::Action,
    nest: &str,
    min: Role,
) -> crate::groups::GResult<(String, Role, NestRef)> {
    use crate::groups::GErr;
    use crate::policy::{authorize, Actor, Facts, Resource};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let principal = crate::principal_for_token(state, crate::bearer(headers)).await?;
    let account = crate::capabilities::principal_account(&principal)
        .map(str::to_string)
        .ok_or_else(|| {
            GErr::forbidden(
                "credential_not_accepted",
                "an account credential is required",
            )
        })?;
    let facts = Facts {
        now: crate::now_unix(),
        ..Facts::default()
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Account(&account),
        &facts,
    )
    .map_err(|deny| {
        let code = deny.code();
        GErr::new(
            deny.into_response().status(),
            code,
            "this credential may not do that",
        )
    })?;
    if !crate::filtering::feature_on(state, Some(&account), crate::capabilities::NESTS).await {
        return Err(GErr::forbidden(
            "feature_disabled",
            "Nests are not enabled for this account",
        ));
    }
    let missing = || GErr::new(StatusCode::NOT_FOUND, "no_nest", "no such Nest");
    if !crate::nests::nest_id_ok(nest) {
        return Err(missing());
    }
    let (n, a) = (nest.to_string(), account.clone());
    let now = crate::now_unix();
    let found = state
        .store
        .group_read(
            move |c| -> crate::groups::GResult<Option<(Role, NestRef)>> {
                Ok(access_in(c, &n, &a, now)?)
            },
        )
        .await?;
    match found {
        Some((role, nref)) if role >= min => Ok((account, role, nref)),
        _ => Err(missing()),
    }
}
