//! Org rooms: groups with `org_id` (SDS §4.1, §4.2, §4.4 "Rooms"; `docs/orgs.md` §5).
//!
//! A room is an ordinary end-to-end-encrypted group. What the org adds is who may be in it (only
//! live members and agents of that org, whatever the link or listing says), who may create one
//! (`rooms_creation`), the org directory listing, and auto-join: a new member is queued as
//! `keying` into every active auto-join room, and an auto-join room that activates later queues the
//! members who joined meanwhile. Queuing is all the server can do — a member's device adds the
//! newcomer cryptographically (the server holds no group keys).

use crate::groups::{
    self, add_notice, emit_members, load_group, load_member, new_group_id, GErr, GResult, GroupRow,
    Outcome,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// Whether `mailbox` may be in rooms of `org_id`: a live member mailbox or an active agent of a
/// live org.
pub(crate) fn member_mailbox_in(c: &Connection, org_id: &str, mailbox: &str) -> GResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM orgs o WHERE o.id = ?1 AND o.status IN ('active','past_due','read_only'))
            AND (EXISTS(SELECT 1 FROM org_members WHERE org_id = ?1 AND identity = ?2 AND status = 'active')
              OR EXISTS(SELECT 1 FROM org_agents WHERE org_id = ?1 AND identity = ?2 AND status = 'active'))",
        params![org_id, mailbox],
        |r| r.get(0),
    )?)
}

/// May the person behind member mailbox `mailbox` create a room in `org_id` (§6.4 "Rooms: create")?
pub(crate) fn may_create_in(c: &Connection, org_id: &str, mailbox: &str) -> GResult<()> {
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT m.id, o.status, o.settings FROM org_members m JOIN orgs o ON o.id = m.org_id
              WHERE m.org_id = ?1 AND m.identity = ?2 AND m.status = 'active'",
            params![org_id, mailbox],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((member, status, settings)) = row else {
        return Err(GErr::forbidden(
            "not_org_member",
            "act as your organization mailbox to create a room there",
        ));
    };
    if !matches!(status.as_str(), "active" | "past_due") {
        return Err(GErr::conflict(
            "org_read_only",
            "this organization cannot create rooms right now",
        ));
    }
    let settings: crate::org::Settings = serde_json::from_str(&settings).unwrap_or_default();
    let roles = crate::org::roles_of(c, org_id, &member)?;
    if !crate::org::permits(
        &roles,
        crate::org::Perm::CreateRoom,
        settings.rooms_creation != "admins",
    ) {
        return Err(GErr::forbidden(
            "forbidden",
            "only organization admins can create rooms here",
        ));
    }
    Ok(())
}

pub(crate) struct NewRoom<'a> {
    pub org_id: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub join_policy: &'a str,
    pub post_policy: &'a str,
    pub listed: bool,
    pub auto_join: bool,
    pub creator: &'a str,
    pub creator_account: Option<&'a str>,
}

/// Create a room in `provisioning` with its creator as keying owner. Their device activates it
/// (the web inbox does so automatically for groups it created).
pub(crate) fn create_room_in(c: &Connection, r: &NewRoom<'_>, now: u64) -> GResult<String> {
    let gid = new_group_id();
    c.execute(
        "INSERT INTO groups (id, org_id, name, description, join_policy, post_policy, visibility,
                auto_join, created_by, created_by_account, created_at, last_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        params![
            gid,
            r.org_id,
            r.name,
            r.description,
            r.join_policy,
            r.post_policy,
            if r.listed { "listed" } else { "unlisted" },
            r.auto_join as i64,
            r.creator,
            r.creator_account,
            now as i64
        ],
    )?;
    c.execute(
        "INSERT INTO group_members (group_id, member, role, state, joined_at, updated_at)
         VALUES (?1, ?2, 'owner', 'keying', ?3, ?3)",
        params![gid, r.creator, now as i64],
    )?;
    Ok(gid)
}

/// Queue a new member into every active auto-join room of the org.
pub(crate) fn auto_join_in(
    c: &Connection,
    org_id: &str,
    mailbox: &str,
    now: u64,
    out: &mut Outcome,
) -> GResult<Vec<String>> {
    let rooms: Vec<String> = {
        let mut stmt = c.prepare(
            "SELECT id FROM groups WHERE org_id = ?1 AND auto_join = 1 AND deleted_at IS NULL
                AND activation_state = 'active'",
        )?;
        let rows = stmt.query_map([org_id], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut joined = Vec::new();
    for gid in rooms {
        let g = groups::live_group(c, &gid)?;
        let existing = load_member(c, &gid, mailbox)?;
        if existing
            .as_ref()
            .is_some_and(|m| matches!(m.state.as_str(), "active" | "keying" | "banned"))
        {
            continue;
        }
        groups::start_keying(c, &g, mailbox, existing.as_ref(), now, out)?;
        add_notice(
            c,
            &gid,
            "member_invited",
            None,
            json!({ "address": mailbox, "auto_join": true }),
            now,
        )?;
        joined.push(g.name.clone());
    }
    Ok(joined)
}

/// When an auto-join org room activates, queue every live member who is not yet in it.
pub(crate) fn on_room_activated(
    c: &Connection,
    g: &GroupRow,
    now: u64,
    out: &mut Outcome,
) -> GResult<()> {
    let Some(org_id) = g.org_id.as_deref() else {
        return Ok(());
    };
    let auto: bool = c.query_row("SELECT auto_join FROM groups WHERE id = ?1", [&g.id], |r| {
        r.get::<_, i64>(0).map(|v| v != 0)
    })?;
    if !auto {
        return Ok(());
    }
    let mailboxes: Vec<String> = {
        let mut stmt = c.prepare(
            "SELECT identity FROM org_members WHERE org_id = ?1 AND status = 'active' AND identity IS NOT NULL",
        )?;
        let rows = stmt.query_map([org_id], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let g = load_group(c, &g.id)?.ok_or_else(GErr::not_found)?;
    for m in mailboxes {
        let existing = load_member(c, &g.id, &m)?;
        if existing.is_none() {
            groups::start_keying(c, &g, &m, None, now, out)?;
        }
    }
    Ok(())
}

/// Take a departing mailbox out of every group it is in (offboarding, SDS §6.7).
pub(crate) fn remove_everywhere_in(
    c: &Connection,
    mailbox: &str,
    actor: &str,
    now: u64,
    out: &mut Outcome,
) -> GResult<()> {
    let rows: Vec<String> = {
        let mut stmt = c.prepare(
            "SELECT m.group_id FROM group_members m JOIN groups g ON g.id = m.group_id
              WHERE m.member = ?1 AND g.deleted_at IS NULL AND m.state IN ('active','keying','invited')",
        )?;
        let rows = stmt.query_map([mailbox], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    for gid in rows {
        let g = groups::live_group(c, &gid)?;
        let Some(m) = load_member(c, &gid, mailbox)? else {
            continue;
        };
        if m.state == "invited" {
            c.execute(
                "DELETE FROM group_members WHERE group_id = ?1 AND member = ?2",
                params![gid, mailbox],
            )?;
            continue;
        }
        if m.role == "owner" {
            // Ownership passes to the oldest admin, else the oldest active member (SDS §4.4).
            let heir: Option<String> = c
                .query_row(
                    "SELECT member FROM group_members WHERE group_id = ?1 AND member <> ?2
                        AND state = 'active' ORDER BY CASE role WHEN 'admin' THEN 0 ELSE 1 END, joined_at
                      LIMIT 1",
                    params![gid, mailbox],
                    |r| r.get(0),
                )
                .optional()?;
            match heir {
                Some(heir) => {
                    c.execute(
                        "UPDATE group_members SET role = 'member' WHERE group_id = ?1 AND member = ?2",
                        params![gid, mailbox],
                    )?;
                    c.execute(
                        "UPDATE group_members SET role = 'owner', revision = revision + 1 WHERE group_id = ?1 AND member = ?2",
                        params![gid, heir],
                    )?;
                    add_notice(
                        c,
                        &gid,
                        "ownership_transferred",
                        None,
                        json!({ "from": mailbox, "to": heir }),
                        now,
                    )?;
                }
                None => {
                    out.wake.extend(groups::delete_in(c, &g, actor, now)?);
                    continue;
                }
            }
        }
        let m = load_member(c, &gid, mailbox)?.ok_or_else(GErr::not_found)?;
        groups::take_out(c, &g, &m, "removed", actor, now, out)?;
    }
    Ok(())
}

/// One room for the org's admin views.
pub(crate) fn room_json(c: &Connection, gid: &str) -> GResult<Value> {
    Ok(c.query_row(
        "SELECT g.id, g.name, g.description, g.join_policy, g.post_policy, g.visibility, g.auto_join,
                g.activation_state, g.created_by,
                (SELECT COUNT(*) FROM group_members m WHERE m.group_id = g.id AND m.state IN ('active','keying'))
           FROM groups g WHERE g.id = ?1",
        [gid],
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "description": r.get::<_, Option<String>>(2)?,
                "join_policy": r.get::<_, String>(3)?,
                "post_policy": r.get::<_, String>(4)?,
                "listed": r.get::<_, String>(5)? == "listed",
                "auto_join": r.get::<_, i64>(6)? != 0,
                "activation_state": r.get::<_, String>(7)?,
                "created_by": r.get::<_, String>(8)?,
                "member_count": r.get::<_, i64>(9)?,
            }))
        },
    )?)
}

/// Emit `group.settings` after an admin changes a room from the org surface.
pub(crate) fn settings_changed(
    c: &Connection,
    gid: &str,
    changed: &[&str],
    now: u64,
) -> GResult<Vec<String>> {
    groups::bump_group_revision(c, gid)?;
    emit_members(
        c,
        gid,
        "group.settings",
        &json!({ "group_id": gid, "changed": changed }),
        now,
    )
}
