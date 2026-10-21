//! Organization trust and permission rules (SDS §9; `docs/orgs.md` §6).
//!
//! An org rule is a contact entry applied to many owners at once. Evaluation, used by `do_send`,
//! call admission and the inbox's per-message verdict:
//!
//! 1. A personal block always wins (D-02) — decided by the caller from `store::Relationship`.
//! 2. Org rules whose `owners` match the receiving org mailbox and whose `peers` match the sender,
//!    ordered by priority (lower first), then specificity (exact > wildcard > members/agents >
//!    `*`), then age. The first rule to set a field wins, field by field.
//! 3. `locked` binds **agent** mailboxes only; a member's own contacts still apply to theirs.
//! 4. `autonomy: auto` from an org rule applies to agents only, never to a member's mailbox, and
//!    its verbs exclude every never-auto action.
//!
//! Rule sets are compiled once per `orgs.rules_revision` and cached (`Arc<RuleSet>`), so the hot
//! path is a revision read plus precompiled matching.

// Row tuples read straight from SQL are clearer inline than as one-use type aliases.
#![allow(clippy::type_complexity)]

use crate::groups::{GErr, GResult};
use crate::org::{self, Perm};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Verbs an org rule may never make automatic (SDS §9.2): the denied class, and the privileged
/// verbs whose wrappers would otherwise reach them.
pub(crate) const NEVER_AUTO: &[&str] = &[
    "git_push",
    "deploy",
    "read_credentials",
    "spend",
    "delete_files",
    "run_shell",
    "full_access",
    "make_change",
];

/// Most rules one org may hold.
pub(crate) const MAX_RULES: i64 = 500;

// ---- patterns ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pat {
    Any,
    Members,
    Agents,
    Exact(String),
    /// Everything that starts with this (`/acme/` for `/acme/*`, `/acme/build-` for `/acme/build-*`).
    Prefix(String),
}

impl Pat {
    fn specificity(&self) -> u8 {
        match self {
            Pat::Exact(_) => 3,
            Pat::Prefix(_) => 2,
            Pat::Members | Pat::Agents => 1,
            Pat::Any => 0,
        }
    }
}

/// Parse a pattern. `owners` patterns must stay inside the org namespace; `peers` may name anyone.
pub(crate) fn parse_pat(raw: &str, namespace: &str, owners: bool) -> Result<Pat, String> {
    let s = raw.trim().to_ascii_lowercase();
    match s.as_str() {
        "members" => return Ok(Pat::Members),
        "agents" => return Ok(Pat::Agents),
        "*" if !owners => return Ok(Pat::Any),
        "*" => {
            return Err(
                "owners must name members, agents or addresses inside the organization".into(),
            )
        }
        _ => {}
    }
    if !s.starts_with('/')
        || s.len() > 200
        || s.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!(
            "'{raw}' is not a pattern: use members, agents, /ns/name, /ns/* or /ns/prefix-*"
        ));
    }
    let segments: Vec<&str> = s.trim_start_matches('/').split('/').collect();
    if segments.iter().any(|seg| seg.is_empty()) {
        return Err(format!("'{raw}' has an empty segment"));
    }
    let wildcards = s.matches('*').count();
    if wildcards > 1 || (wildcards == 1 && !s.ends_with('*')) {
        return Err(format!(
            "'{raw}': only one trailing wildcard on the last segment is allowed"
        ));
    }
    if owners && segments[0] != namespace {
        return Err(format!("owners must stay inside /{namespace}"));
    }
    if wildcards == 1 {
        if segments.len() < 2 {
            return Err(format!("'{raw}': a wildcard needs a namespace before it"));
        }
        Ok(Pat::Prefix(s.trim_end_matches('*').to_string()))
    } else {
        let canonical = pigeonpost_core::Destination::for_handle(&s)
            .ok()
            .and_then(|d| d.handle().map(str::to_string))
            .ok_or_else(|| format!("'{raw}' is not a handle"))?;
        Ok(Pat::Exact(canonical))
    }
}

/// One side of a delivery, with how it relates to this org.
#[derive(Debug, Clone, Default)]
pub(crate) struct Party {
    pub address: String,
    pub handle: Option<String>,
    pub member: bool,
    pub agent: bool,
}

impl Pat {
    fn matches(&self, p: &Party) -> bool {
        match self {
            Pat::Any => true,
            Pat::Members => p.member,
            Pat::Agents => p.agent,
            Pat::Exact(h) => p.handle.as_deref() == Some(h.as_str()) || p.address == *h,
            Pat::Prefix(pre) => p
                .handle
                .as_deref()
                .is_some_and(|h| h.starts_with(pre.as_str())),
        }
    }
}

// ---- compiled rule sets ------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub id: String,
    pub owners: Pat,
    pub peers: Pat,
    pub admission: Option<String>,
    pub autonomy: Option<String>,
    pub verbs: Option<Vec<String>>,
    pub known: bool,
    pub locked: bool,
    pub priority: i64,
    pub description: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Default)]
pub(crate) struct RuleSet {
    pub rules: Vec<Rule>,
}

impl RuleSet {
    pub(crate) fn compile(mut rules: Vec<Rule>) -> Self {
        rules.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(b.peers.specificity().cmp(&a.peers.specificity()))
                .then(b.owners.specificity().cmp(&a.owners.specificity()))
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
        RuleSet { rules }
    }

    /// Field-by-field first match.
    pub(crate) fn evaluate(&self, owner: &Party, peer: &Party) -> Verdict {
        let mut v = Verdict {
            owner_is_agent: owner.agent,
            ..Verdict::default()
        };
        for r in &self.rules {
            if !(r.owners.matches(owner) && r.peers.matches(peer)) {
                continue;
            }
            let mut used = false;
            if v.admission.is_none() {
                if let Some(a) = &r.admission {
                    v.admission = Some(Field::new(a.clone(), r));
                    used = true;
                }
            }
            if v.autonomy.is_none() {
                if let Some(a) = &r.autonomy {
                    v.autonomy = Some(Field::new(a.clone(), r));
                    used = true;
                }
            }
            if v.verbs.is_none() {
                if let Some(list) = &r.verbs {
                    v.verbs = Some(Field::new(list.clone(), r));
                    used = true;
                }
            }
            if v.known.is_none() && r.known {
                v.known = Some(r.id.clone());
                used = true;
            }
            if used && v.matched.is_none() {
                v.matched = Some((r.id.clone(), r.description.clone()));
            }
        }
        v
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field<T> {
    pub value: T,
    pub rule: String,
    pub locked: bool,
}

impl<T> Field<T> {
    fn new(value: T, r: &Rule) -> Self {
        Field {
            value,
            rule: r.id.clone(),
            locked: r.locked,
        }
    }
}

/// What the org says about one (owner, peer) pair.
#[derive(Debug, Clone, Default)]
pub(crate) struct Verdict {
    pub owner_is_agent: bool,
    pub admission: Option<Field<String>>,
    pub autonomy: Option<Field<String>>,
    pub verbs: Option<Field<Vec<String>>>,
    pub known: Option<String>,
    pub matched: Option<(String, Option<String>)>,
}

impl Verdict {
    /// An admission the receiving mailbox cannot override: locked, and only for agents.
    pub(crate) fn locked_admission(&self) -> Option<bool> {
        self.admission
            .as_ref()
            .filter(|f| f.locked && self.owner_is_agent)
            .map(|f| f.value == "allow")
    }
    pub(crate) fn admission(&self) -> Option<bool> {
        self.admission.as_ref().map(|f| f.value == "allow")
    }
    pub(crate) fn known(&self) -> bool {
        self.known.is_some()
    }
    /// A locked `review` on an agent forbids automation whatever the personal contact says.
    pub(crate) fn locked_review(&self) -> bool {
        self.owner_is_agent
            && self
                .autonomy
                .as_ref()
                .is_some_and(|f| f.locked && f.value == "review")
    }
    /// Automation the org grants on its own: agents only, never-auto verbs removed.
    pub(crate) fn org_auto_verbs(&self) -> Option<Vec<String>> {
        if !self.owner_is_agent {
            return None;
        }
        let auto = self.autonomy.as_ref().is_some_and(|f| f.value == "auto");
        if !auto {
            return None;
        }
        let verbs = self
            .verbs
            .as_ref()
            .map(|f| f.value.clone())
            .unwrap_or_default();
        Some(
            verbs
                .into_iter()
                .filter(|v| !NEVER_AUTO.contains(&v.as_str()))
                .collect(),
        )
    }
    pub(crate) fn matched_json(&self) -> Value {
        match &self.matched {
            Some((id, description)) => json!({ "id": id, "description": description }),
            None => Value::Null,
        }
    }
}

fn cache() -> &'static Mutex<HashMap<String, (i64, Arc<RuleSet>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (i64, Arc<RuleSet>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn read_rule(r: &rusqlite::Row<'_>, namespace: &str) -> rusqlite::Result<Option<Rule>> {
    let owners: String = r.get(1)?;
    let peers: String = r.get(2)?;
    let verbs: Option<String> = r.get(5)?;
    let (Ok(owners), Ok(peers)) = (
        parse_pat(&owners, namespace, true),
        parse_pat(&peers, namespace, false),
    ) else {
        return Ok(None);
    };
    Ok(Some(Rule {
        id: r.get(0)?,
        owners,
        peers,
        admission: r.get(3)?,
        autonomy: r.get(4)?,
        verbs: verbs.and_then(|v| serde_json::from_str(&v).ok()),
        known: r.get::<_, i64>(6)? != 0,
        locked: r.get::<_, i64>(7)? != 0,
        priority: r.get(8)?,
        description: r.get(9)?,
        created_at: r.get(10)?,
    }))
}

const RULE_COLS: &str =
    "id, owners, peers, admission, autonomy, verbs, known, locked, priority, description, created_at";

/// The org's compiled rules, from the cache when its revision has not moved.
pub(crate) fn rules_for(c: &Connection, org_id: &str) -> rusqlite::Result<Arc<RuleSet>> {
    let (revision, namespace): (i64, String) = c.query_row(
        "SELECT rules_revision, namespace FROM orgs WHERE id = ?1",
        [org_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if let Some((rev, set)) = cache().lock().expect("rule cache").get(org_id) {
        if *rev == revision {
            return Ok(set.clone());
        }
    }
    let mut stmt = c.prepare(&format!(
        "SELECT {RULE_COLS} FROM org_rules WHERE org_id = ?1"
    ))?;
    let rules: Vec<Rule> = stmt
        .query_map([org_id], |r| read_rule(r, &namespace))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let set = Arc::new(RuleSet::compile(rules));
    cache()
        .lock()
        .expect("rule cache")
        .insert(org_id.to_string(), (revision, set.clone()));
    Ok(set)
}

/// Which org (if any) an address is a live mailbox of, and as what.
fn org_party(c: &Connection, address: &str) -> rusqlite::Result<Option<(String, bool, bool)>> {
    if let Some(org) = c
        .query_row(
            "SELECT org_id FROM org_members WHERE identity = ?1 AND status = 'active'",
            [address],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(Some((org, true, false)));
    }
    Ok(c.query_row(
        "SELECT org_id FROM org_agents WHERE identity = ?1 AND status = 'active'",
        [address],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|org| (org, false, true)))
}

/// The org verdict for mail from `sender` to `recipient`, or `None` when the recipient is not a
/// live org mailbox (personal mailboxes are untouched by org rules).
pub(crate) fn verdict_in(
    c: &Connection,
    recipient: &str,
    recipient_handle: Option<&str>,
    sender: &str,
    sender_handle: Option<&str>,
) -> rusqlite::Result<Option<Verdict>> {
    let Some((org_id, member, agent)) = org_party(c, recipient)? else {
        return Ok(None);
    };
    let status: String = c.query_row("SELECT status FROM orgs WHERE id = ?1", [&org_id], |r| {
        r.get(0)
    })?;
    if !matches!(status.as_str(), "active" | "past_due" | "read_only") {
        return Ok(None);
    }
    let owner = Party {
        address: recipient.to_string(),
        handle: recipient_handle.map(str::to_string),
        member,
        agent,
    };
    let (peer_member, peer_agent) = match org_party(c, sender)? {
        Some((o, m, a)) if o == org_id => (m, a),
        _ => (false, false),
    };
    let peer = Party {
        address: sender.to_string(),
        handle: sender_handle.map(str::to_string),
        member: peer_member,
        agent: peer_agent,
    };
    Ok(Some(rules_for(c, &org_id)?.evaluate(&owner, &peer)))
}

/// [`verdict_in`] on its own connection. A read failure yields no org verdict (the personal
/// contact and inbox policy still apply, which is the conservative fallback for admission).
pub(crate) async fn verdict(
    state: &crate::AppState,
    recipient: &crate::store::StoredIdentity,
    sender_address: &str,
    sender_handle: Option<&str>,
) -> Option<Verdict> {
    let r = recipient.address.clone();
    let rh = recipient.handle.clone();
    let s = sender_address.to_string();
    let sh = sender_handle.map(str::to_string);
    state
        .store
        .group_read(move |c: &Connection| -> Result<Option<Verdict>, GErr> {
            Ok(verdict_in(c, &r, rh.as_deref(), &s, sh.as_deref())?)
        })
        .await
        .unwrap_or(None)
}

// ---- API (`/v1/orgs/{id}/rules…`) --------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuleBody {
    #[serde(default)]
    pub owners: Option<String>,
    #[serde(default)]
    pub peers: Option<String>,
    #[serde(default, deserialize_with = "crate::org_rules::nullable")]
    pub admission: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::org_rules::nullable")]
    pub autonomy: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::org_rules::nullable")]
    pub verbs: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub known: Option<bool>,
    #[serde(default)]
    pub locked: Option<bool>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default, deserialize_with = "crate::org_rules::nullable")]
    pub description: Option<Option<String>>,
}

/// `Some(None)` for an explicit `null`, `None` for an absent field.
pub(crate) fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(d)?))
}

/// A rule as stored, validated against the org and the editor's roles.
#[derive(Debug, Clone)]
pub(crate) struct Draft {
    pub owners: String,
    pub peers: String,
    pub admission: Option<String>,
    pub autonomy: Option<String>,
    pub verbs: Option<Vec<String>>,
    pub known: bool,
    pub locked: bool,
    pub priority: i64,
    pub description: Option<String>,
}

/// Which rule owners an editor may touch: people admins edit members' rules, agent admins agents'
/// rules, owners and admins everything (SDS §6.4).
pub(crate) fn editor_may(roles: &org::Roles, owners: &Pat) -> bool {
    let members = org::permits(roles, Perm::EditMemberRules, false);
    let agents = org::permits(roles, Perm::EditAgentRules, false);
    match owners {
        Pat::Members => members,
        Pat::Agents => agents,
        _ => members && agents,
    }
}

pub(crate) fn validate(d: &Draft, namespace: &str) -> GResult<(Pat, Pat)> {
    let owners = parse_pat(&d.owners, namespace, true).map_err(|m| GErr::bad("invalid_rule", m))?;
    let peers = parse_pat(&d.peers, namespace, false).map_err(|m| GErr::bad("invalid_rule", m))?;
    if let Some(a) = &d.admission {
        if !matches!(a.as_str(), "allow" | "block") {
            return Err(GErr::bad(
                "invalid_rule",
                "admission is allow, block or null",
            ));
        }
    }
    if let Some(a) = &d.autonomy {
        if !matches!(a.as_str(), "review" | "auto") {
            return Err(GErr::bad(
                "invalid_rule",
                "autonomy is review, auto or null",
            ));
        }
    }
    if let Some(verbs) = &d.verbs {
        if verbs.len() > 32 {
            return Err(GErr::bad("invalid_rule", "at most 32 verbs"));
        }
        if let Some(v) = verbs.iter().find(|v| NEVER_AUTO.contains(&v.as_str())) {
            return Err(GErr::bad(
                "never_auto_verb",
                format!("'{v}' always needs a person to approve it and cannot be in an org rule"),
            ));
        }
        crate::verbs::validate_grant(verbs).map_err(|m| GErr::bad("invalid_rule", m))?;
    }
    if d.autonomy.as_deref() == Some("auto") && matches!(owners, Pat::Members) {
        return Err(GErr::bad(
            "invalid_rule",
            "an org rule cannot make a member's mailbox act automatically",
        ));
    }
    if d.locked && matches!(owners, Pat::Members) {
        return Err(GErr::bad(
            "invalid_rule",
            "locked rules apply to agents; members keep control of their own contacts",
        ));
    }
    if d.locked && d.admission.as_deref() == Some("allow") && !d.known && d.autonomy.is_none() {
        // A locked allow is fine; this guard is only a reminder that "locked" can never force an
        // unblock: personal blocks are evaluated before any org rule.
    }
    if !(1..=1000).contains(&d.priority) {
        return Err(GErr::bad("invalid_rule", "priority is 1 to 1000"));
    }
    if d.admission.is_none() && d.autonomy.is_none() && d.verbs.is_none() && !d.known {
        return Err(GErr::bad(
            "invalid_rule",
            "a rule must set at least one of admission, autonomy, verbs or known",
        ));
    }
    if let Some(desc) = &d.description {
        if desc.chars().count() > 200 || desc.chars().any(char::is_control) {
            return Err(GErr::bad(
                "invalid_rule",
                "description is at most 200 printable characters",
            ));
        }
    }
    Ok((owners, peers))
}

pub(crate) fn rule_json(c: &Connection, id: &str) -> GResult<Value> {
    Ok(c.query_row(
        "SELECT id, owners, peers, admission, autonomy, verbs, known, locked, priority, description,
                created_by, created_at, updated_at FROM org_rules WHERE id = ?1",
        [id],
        |r| {
            let verbs: Option<String> = r.get(5)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "owners": r.get::<_, String>(1)?,
                "peers": r.get::<_, String>(2)?,
                "admission": r.get::<_, Option<String>>(3)?,
                "autonomy": r.get::<_, Option<String>>(4)?,
                "verbs": verbs.and_then(|v| serde_json::from_str::<Value>(&v).ok()),
                "known": r.get::<_, i64>(6)? != 0,
                "locked": r.get::<_, i64>(7)? != 0,
                "priority": r.get::<_, i64>(8)?,
                "description": r.get::<_, Option<String>>(9)?,
                "created_by": r.get::<_, Option<String>>(10)?,
                "created_at": r.get::<_, i64>(11)?,
                "updated_at": r.get::<_, i64>(12)?,
            }))
        },
    )?)
}

pub(crate) fn bump_rules(c: &Connection, org: &str) -> GResult<i64> {
    c.execute(
        "UPDATE orgs SET rules_revision = rules_revision + 1 WHERE id = ?1",
        [org],
    )?;
    Ok(c.query_row(
        "SELECT rules_revision FROM orgs WHERE id = ?1",
        [org],
        |r| r.get(0),
    )?)
}

pub(crate) fn insert(c: &Connection, org: &str, d: &Draft, by: &str, now: u64) -> GResult<String> {
    let count: i64 = c.query_row(
        "SELECT COUNT(*) FROM org_rules WHERE org_id = ?1",
        [org],
        |r| r.get(0),
    )?;
    if count >= MAX_RULES {
        return Err(GErr::conflict(
            "too_many_rules",
            format!("an organization holds at most {MAX_RULES} rules"),
        ));
    }
    let id = org::new_id("rule");
    c.execute(
        "INSERT INTO org_rules (id, org_id, owners, peers, admission, autonomy, verbs, known, locked,
                priority, description, created_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        params![
            id,
            org,
            d.owners.trim().to_ascii_lowercase(),
            d.peers.trim().to_ascii_lowercase(),
            d.admission,
            d.autonomy,
            d.verbs.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
            d.known as i64,
            d.locked as i64,
            d.priority,
            d.description,
            by,
            now as i64
        ],
    )?;
    Ok(id)
}

pub(crate) fn load_draft(c: &Connection, org: &str, id: &str) -> GResult<Option<Draft>> {
    Ok(c.query_row(
        "SELECT owners, peers, admission, autonomy, verbs, known, locked, priority, description
           FROM org_rules WHERE org_id = ?1 AND id = ?2",
        params![org, id],
        |r| {
            let verbs: Option<String> = r.get(4)?;
            Ok(Draft {
                owners: r.get(0)?,
                peers: r.get(1)?,
                admission: r.get(2)?,
                autonomy: r.get(3)?,
                verbs: verbs.and_then(|v| serde_json::from_str(&v).ok()),
                known: r.get::<_, i64>(5)? != 0,
                locked: r.get::<_, i64>(6)? != 0,
                priority: r.get(7)?,
                description: r.get(8)?,
            })
        },
    )
    .optional()?)
}

pub(crate) fn apply(d: &mut Draft, b: RuleBody) {
    if let Some(v) = b.owners {
        d.owners = v;
    }
    if let Some(v) = b.peers {
        d.peers = v;
    }
    if let Some(v) = b.admission {
        d.admission = v;
    }
    if let Some(v) = b.autonomy {
        d.autonomy = v;
    }
    if let Some(v) = b.verbs {
        d.verbs = v;
    }
    if let Some(v) = b.known {
        d.known = v;
    }
    if let Some(v) = b.locked {
        d.locked = v;
    }
    if let Some(v) = b.priority {
        d.priority = v;
    }
    if let Some(v) = b.description {
        d.description = v;
    }
}

pub(crate) fn update(c: &Connection, id: &str, d: &Draft, now: u64) -> GResult<()> {
    c.execute(
        "UPDATE org_rules SET owners = ?2, peers = ?3, admission = ?4, autonomy = ?5, verbs = ?6,
                known = ?7, locked = ?8, priority = ?9, description = ?10, updated_at = ?11
          WHERE id = ?1",
        params![
            id,
            d.owners.trim().to_ascii_lowercase(),
            d.peers.trim().to_ascii_lowercase(),
            d.admission,
            d.autonomy,
            d.verbs
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_default()),
            d.known as i64,
            d.locked as i64,
            d.priority,
            d.description,
            now as i64
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, owners: &str, peers: &str, priority: i64) -> Rule {
        Rule {
            id: id.into(),
            owners: parse_pat(owners, "acme", true).unwrap(),
            peers: parse_pat(peers, "acme", false).unwrap(),
            admission: None,
            autonomy: None,
            verbs: None,
            known: false,
            locked: false,
            priority,
            description: None,
            created_at: 0,
        }
    }

    fn member(h: &str) -> Party {
        Party {
            address: format!("/k/{h}"),
            handle: Some(format!("/acme/{h}")),
            member: true,
            agent: false,
        }
    }
    fn agent(h: &str) -> Party {
        Party {
            agent: true,
            member: false,
            ..member(h)
        }
    }
    fn stranger() -> Party {
        Party {
            address: "/k/x".into(),
            handle: Some("/other/x".into()),
            ..Party::default()
        }
    }

    #[test]
    fn pattern_grammar() {
        assert_eq!(parse_pat("members", "acme", true), Ok(Pat::Members));
        assert_eq!(
            parse_pat("/acme/*", "acme", true),
            Ok(Pat::Prefix("/acme/".into()))
        );
        assert_eq!(
            parse_pat("/Acme/Build-*", "acme", true),
            Ok(Pat::Prefix("/acme/build-".into()))
        );
        assert_eq!(
            parse_pat("/acme/agent1", "acme", true),
            Ok(Pat::Exact("/acme/agent1".into()))
        );
        assert_eq!(parse_pat("*", "acme", false), Ok(Pat::Any));
        assert!(
            parse_pat("*", "acme", true).is_err(),
            "owners never * (outside the org)"
        );
        assert!(
            parse_pat("/other/*", "acme", true).is_err(),
            "owners stay in the namespace"
        );
        assert!(
            parse_pat("/other/*", "acme", false).is_ok(),
            "peers may be anyone"
        );
        for bad in [
            "/acme/*/x",
            "/acme/a*b",
            "/acme/**",
            "acme/x",
            "/acme//x",
            "/*",
            "",
        ] {
            assert!(parse_pat(bad, "acme", false).is_err(), "{bad}");
        }
    }

    #[test]
    fn first_rule_to_set_a_field_wins_by_priority_then_specificity() {
        let mut everyone = rule("r1", "/acme/*", "/acme/*", 100);
        everyone.admission = Some("allow".into());
        everyone.known = true;
        let mut block_bob = rule("r2", "/acme/*", "/acme/bob", 100);
        block_bob.admission = Some("block".into());
        let mut late = rule("r3", "/acme/*", "*", 500);
        late.admission = Some("allow".into());
        late.autonomy = Some("review".into());
        let set = RuleSet::compile(vec![everyone, block_bob, late]);

        // Same priority: the exact peer beats the wildcard for admission…
        let v = set.evaluate(&member("jane"), &member("bob"));
        assert_eq!(v.admission.as_ref().unwrap().rule, "r2");
        assert_eq!(v.admission(), Some(false));
        // …but `known` was set only by the wildcard rule, so that field still comes from it.
        assert_eq!(v.known.as_deref(), Some("r1"));
        // The lower-priority rule fills only what is still unset.
        assert_eq!(v.autonomy.as_ref().unwrap().rule, "r3");
        assert_eq!(v.matched.as_ref().unwrap().0, "r2");

        let v = set.evaluate(&member("jane"), &stranger());
        assert_eq!(v.admission.as_ref().unwrap().rule, "r3");
        assert!(!v.known());
    }

    #[test]
    fn locks_and_automation_apply_to_agents_only() {
        let mut lock = rule("l", "agents", "*", 10);
        lock.admission = Some("block".into());
        lock.locked = true;
        let mut auto = rule("a", "/acme/*", "/acme/john", 20);
        auto.autonomy = Some("auto".into());
        auto.verbs = Some(vec!["report_status".into(), "git_push".into()]);
        let set = RuleSet::compile(vec![lock, auto]);
        let to_agent = set.evaluate(&agent("build"), &stranger());
        assert_eq!(to_agent.locked_admission(), Some(false));
        let to_member = set.evaluate(&member("jane"), &stranger());
        assert_eq!(
            to_member.locked_admission(),
            None,
            "members are never bound by locked rules"
        );
        // Automation: the agent gets the safe verbs only; a member never gets automation.
        let a = set.evaluate(&agent("build"), &member("john"));
        assert_eq!(a.org_auto_verbs(), Some(vec!["report_status".into()]));
        let m = set.evaluate(&member("jane"), &member("john"));
        assert_eq!(m.org_auto_verbs(), None);
    }

    #[test]
    fn validation_refuses_unsafe_rules() {
        let base = Draft {
            owners: "agents".into(),
            peers: "*".into(),
            admission: Some("allow".into()),
            autonomy: None,
            verbs: None,
            known: false,
            locked: false,
            priority: 100,
            description: None,
        };
        assert!(validate(&base, "acme").is_ok());
        let mut d = base.clone();
        d.verbs = Some(vec!["run_shell".into()]);
        assert_eq!(validate(&d, "acme").unwrap_err().code, "never_auto_verb");
        let mut d = base.clone();
        d.verbs = Some(vec!["make_change".into()]);
        assert_eq!(validate(&d, "acme").unwrap_err().code, "never_auto_verb");
        let mut d = base.clone();
        d.owners = "members".into();
        d.autonomy = Some("auto".into());
        assert!(
            validate(&d, "acme").is_err(),
            "no forced automation for members"
        );
        let mut d = base.clone();
        d.owners = "members".into();
        d.locked = true;
        assert!(validate(&d, "acme").is_err(), "no locks on members");
        let mut d = base.clone();
        d.admission = None;
        assert!(validate(&d, "acme").is_err(), "a rule must say something");
        let mut d = base;
        d.owners = "/other/*".into();
        assert!(validate(&d, "acme").is_err());
    }

    /// Property: whatever the rule order, an `auto` verdict never carries a never-auto verb, and a
    /// member owner never gets automation or a lock.
    #[test]
    fn property_no_unsafe_outcome_under_any_ordering() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let owners = [
            "members",
            "agents",
            "/acme/*",
            "/acme/build-*",
            "/acme/build-1",
            "/acme/jane",
        ];
        let peers = [
            "*",
            "members",
            "agents",
            "/acme/*",
            "/acme/john",
            "/other/*",
        ];
        let verbs = [
            "report_status",
            "answer_question",
            "run_tests",
            "git_push",
            "deploy",
        ];
        for round in 0..300 {
            let mut rules = Vec::new();
            for i in 0..(1 + next() % 12) {
                let mut r = rule(
                    &format!("r{round}-{i}"),
                    owners[(next() % owners.len() as u64) as usize],
                    peers[(next() % peers.len() as u64) as usize],
                    1 + (next() % 5) as i64 * 50,
                );
                r.created_at = (next() % 1000) as i64;
                if next() % 2 == 0 {
                    r.admission = Some(if next() % 2 == 0 { "allow" } else { "block" }.into());
                }
                if next() % 2 == 0 {
                    r.autonomy = Some(if next() % 2 == 0 { "auto" } else { "review" }.into());
                }
                if next() % 2 == 0 {
                    r.verbs = Some(
                        verbs
                            .iter()
                            .filter(|_| next() % 2 == 0)
                            .map(|v| v.to_string())
                            .collect(),
                    );
                }
                r.known = next() % 3 == 0;
                r.locked = next() % 4 == 0;
                rules.push(r);
            }
            // Evaluate the same rules in two different input orders: the compiled order decides.
            let mut shuffled = rules.clone();
            shuffled.reverse();
            let a = RuleSet::compile(rules);
            let b = RuleSet::compile(shuffled);
            for owner in [member("jane"), agent("build-1"), member("build-x")] {
                for peer in [member("john"), agent("build-2"), stranger()] {
                    let va = a.evaluate(&owner, &peer);
                    let vb = b.evaluate(&owner, &peer);
                    assert_eq!(va.admission, vb.admission, "order-independent");
                    assert_eq!(va.autonomy, vb.autonomy);
                    if let Some(list) = va.org_auto_verbs() {
                        assert!(owner.agent);
                        assert!(list.iter().all(|v| !NEVER_AUTO.contains(&v.as_str())));
                    }
                    if !owner.agent {
                        assert_eq!(va.locked_admission(), None);
                        assert!(!va.locked_review());
                        assert_eq!(va.org_auto_verbs(), None);
                    }
                }
            }
        }
    }

    /// SDS §9.3 target: < 50 µs per evaluation with 500 rules (release builds; debug is slower, so
    /// the assertion here is generous and the measured figure is printed).
    #[test]
    fn evaluation_is_fast_with_500_rules() {
        let mut rules = Vec::new();
        for i in 0..500 {
            let mut r = rule(&format!("r{i}"), "/acme/*", &format!("/acme/user{i}"), 100);
            r.admission = Some("allow".into());
            rules.push(r);
        }
        let set = RuleSet::compile(rules);
        let owner = member("jane");
        let peer = stranger();
        let start = std::time::Instant::now();
        let n = 2_000;
        for _ in 0..n {
            std::hint::black_box(set.evaluate(&owner, &peer));
        }
        let per = start.elapsed() / n;
        eprintln!("org rule evaluation, 500 rules: {per:?} per call");
        assert!(per < std::time::Duration::from_micros(2_000), "{per:?}");
    }
}
