//! Live streams in groups (SDS §14A, client contract and threat model in `docs/streaming.md`).
//!
//! The postbox decides who may go live, watch and speak, and keeps the stream's lifecycle: one live
//! stream per group, at most four hours, ended when the streamer stops, leaves for good, loses
//! the right to stream, or the calls service stops reporting it. It is also the **key directory**
//! for the stream's end-to-end encryption: every watching device registers the public half of a
//! fresh P-256 key pair here, and the streamer wraps the stream key only to keys this directory
//! lists for current members. The calls service (signaling, media tickets, capacity) and the
//! media server never hold a key that opens the media; a malicious calls service cannot add a
//! listener of its own, because it cannot register keys here.
//!
//! What the postbox stores is metadata: group, streamer, title, start/end, viewer counts, the stage
//! and public keys. Ended streams' rows are deleted after [`RETAIN_ENDED_SECS`]; the group keeps a
//! `stream` notice ("Jane streamed for 34 min · 18 viewers").
//!
//! Every decision goes through `policy::authorize` with [`Resource::GroupStream`]; the group rules
//! are resolved into `Facts::stream_permitted` inside the transaction that acts on them.

// Outcomes are built up step by step inside transactions; that reads better than one literal.
#![allow(clippy::field_reassign_with_default)]

use crate::capabilities;
use crate::filtering::{feature_on, mailbox_for};
use crate::groups::{
    active_member, add_notice, emit_members, finish, header_value, is_admin, live_group,
    load_group, load_member, rand_b32, rate_limit, GErr, GResult, GroupRow, IdentityQuery,
    MemberRow, Outcome,
};
use crate::policy::{authorize, Action, Actor, DenyReason, Facts, Resource};
use crate::{now_unix, ApiError, AppState, Principal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

// ---- limits ------------------------------------------------------------------------------------

/// The longest a stream may run (SDS §14A.5).
pub(crate) const MAX_STREAM_SECS: u64 = 4 * 3_600;
/// A live stream the calls service has not reported for this long is over (its streamer never
/// connected, or the calls service lost it).
pub(crate) const BEAT_TIMEOUT_SECS: u64 = 120;
/// Ended streams' rows (title, stage, keys) are deleted this long after the end.
pub(crate) const RETAIN_ENDED_SECS: u64 = 7 * 86_400;
/// Keys one member may hold for one stream (devices watching at once).
const KEYS_PER_MEMBER: i64 = 4;
const TITLE_CHARS: usize = 100;
/// Streams one mailbox may start per hour.
const STARTS_PER_HOUR: usize = 6;
/// People on the stage at once, besides the streamer.
const STAGE_MAX: i64 = 6;

/// Viewers per stream (SDS §14A.5: 200, configurable). The calls service may admit fewer when the
/// media server's measured budget requires it.
pub(crate) fn max_viewers() -> i64 {
    crate::env_num::<i64>("PIGEONPOST_STREAM_MAX_VIEWERS", 200).clamp(1, 10_000)
}

const SOURCES: [&str; 2] = ["camera", "screen"];

/// An uncompressed P-256 public key, base64url without padding (65 bytes, first byte 4).
pub(crate) fn valid_public_key(k: &str) -> bool {
    k.len() == 87
        && k.starts_with('B')
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_stream_id(id: &str) -> bool {
    id.len() == 30
        && id.starts_with("str_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

fn new_stream_id() -> String {
    format!("str_{}", rand_b32(16))
}

fn clean_title(raw: &str) -> GResult<String> {
    let t: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(*c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if t.is_empty() || t.chars().count() > TITLE_CHARS {
        return Err(GErr::bad(
            "bad_request",
            format!("title must be 1-{TITLE_CHARS} characters"),
        ));
    }
    Ok(t)
}

// ---- rows --------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct StreamRow {
    pub id: String,
    pub group_id: String,
    pub host: String,
    pub title: String,
    pub sources: Vec<String>,
    pub host_key: Option<String>,
    pub state: String,
    pub started_at: i64,
    pub ends_by: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<String>,
    pub viewers_now: i64,
    pub viewers_peak: i64,
    pub revision: i64,
}

const STREAM_COLS: &str =
    "id, group_id, host, title, sources, host_key, state, started_at, ends_by,
    ended_at, end_reason, viewers_now, viewers_peak, revision";

fn map_stream(r: &rusqlite::Row<'_>) -> rusqlite::Result<StreamRow> {
    let sources: String = r.get(4)?;
    Ok(StreamRow {
        id: r.get(0)?,
        group_id: r.get(1)?,
        host: r.get(2)?,
        title: r.get(3)?,
        sources: serde_json::from_str(&sources).unwrap_or_default(),
        host_key: r.get(5)?,
        state: r.get(6)?,
        started_at: r.get(7)?,
        ends_by: r.get(8)?,
        ended_at: r.get(9)?,
        end_reason: r.get(10)?,
        viewers_now: r.get(11)?,
        viewers_peak: r.get(12)?,
        revision: r.get(13)?,
    })
}

pub(crate) fn load_stream(c: &Connection, id: &str) -> GResult<Option<StreamRow>> {
    if !valid_stream_id(id) {
        return Ok(None);
    }
    Ok(c.query_row(
        &format!("SELECT {STREAM_COLS} FROM group_streams WHERE id = ?1"),
        params![id],
        map_stream,
    )
    .optional()?)
}

fn live_stream_of(c: &Connection, gid: &str) -> GResult<Option<StreamRow>> {
    Ok(c.query_row(
        &format!("SELECT {STREAM_COLS} FROM group_streams WHERE group_id = ?1 AND state = 'live'"),
        params![gid],
        map_stream,
    )
    .optional()?)
}

fn no_stream() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "no_stream", "no such stream")
}

fn stage_of(c: &Connection, sid: &str) -> GResult<Vec<(String, String)>> {
    let mut stmt = c.prepare(
        "SELECT member, state FROM group_stream_stage WHERE stream_id = ?1 ORDER BY updated_at",
    )?;
    let rows = stmt.query_map(params![sid], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn stage_state(c: &Connection, sid: &str, who: &str) -> GResult<Option<String>> {
    Ok(c.query_row(
        "SELECT state FROM group_stream_stage WHERE stream_id = ?1 AND member = ?2",
        params![sid, who],
        |r| r.get(0),
    )
    .optional()?)
}

fn handle_of(c: &Connection, mailbox: &str) -> GResult<Option<String>> {
    Ok(c.query_row(
        "SELECT handle FROM identities WHERE address = ?1",
        params![mailbox],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten())
}

/// The role of `who` in a live stream: `host`, `speaker`, `invited` or `viewer`.
fn role_of(c: &Connection, s: &StreamRow, who: &str) -> GResult<&'static str> {
    if s.host == who {
        return Ok("host");
    }
    Ok(match stage_state(c, &s.id, who)?.as_deref() {
        Some("speaker") => "speaker",
        Some("invited") => "invited",
        _ => "viewer",
    })
}

/// Distinct members (other than the streamer) who registered a key to watch.
fn distinct_viewers(c: &Connection, s: &StreamRow) -> GResult<i64> {
    Ok(c.query_row(
        "SELECT COUNT(DISTINCT member) FROM group_stream_keys WHERE stream_id = ?1 AND member != ?2",
        params![s.id, s.host],
        |r| r.get(0),
    )?)
}

pub(crate) fn stream_view(c: &Connection, s: &StreamRow) -> GResult<Value> {
    let stage: Vec<Value> = stage_of(c, &s.id)?
        .into_iter()
        .map(|(m, st)| json!({ "address": m, "state": st }))
        .collect();
    Ok(json!({
        "id": s.id,
        "group_id": s.group_id,
        "title": s.title,
        "sources": s.sources,
        "by": s.host,
        "by_handle": handle_of(c, &s.host)?,
        "state": s.state,
        "started_at": s.started_at,
        "ends_by": s.ends_by,
        "ended_at": s.ended_at,
        "end_reason": s.end_reason,
        "viewers": s.viewers_now,
        "peak_viewers": s.viewers_peak,
        "stage": stage,
        "max_viewers": max_viewers(),
        "revision": s.revision,
    }))
}

/// Where a client gets its media ticket: the calls service, on the inbox's own origin.
fn media(sid: &str) -> Value {
    json!({ "join": format!("/calls/v1/streams/{sid}/join") })
}

// ---- the rules (SDS §14A.1) --------------------------------------------------------------------

/// The stream actions the group rules decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    /// List, read and watch: any active member (restricted members too).
    Watch,
    /// Go live: an active, non-restricted member that the group's `stream_policy` allows.
    Start,
    /// Accept (or step down from) the stage: an active, non-restricted member who was invited.
    Stage,
    /// Invite to or remove from the stage, read the audience keys, stop: the streamer, or a group
    /// owner/admin.
    Moderate,
}

/// Whether the group's rules let `member` take `rule` on `stream`, with the precise refusal.
pub(crate) fn rule_allows(
    rule: Rule,
    group: &GroupRow,
    member: Option<&MemberRow>,
    stream: Option<&StreamRow>,
    stage: Option<&str>,
) -> GResult<()> {
    let m = match member {
        Some(m) if m.state == "active" => m,
        Some(m) if m.state == "keying" => {
            return Err(GErr::conflict(
                "key_activation_pending",
                "your membership is waiting for a member's device to add yours",
            ))
        }
        _ => {
            return Err(GErr::forbidden(
                "not_member",
                "you are not a member of this group",
            ))
        }
    };
    let restricted = || {
        GErr::forbidden(
            "stream_restricted",
            "you can watch this group's streams but not go live or speak",
        )
    };
    match rule {
        Rule::Watch => Ok(()),
        Rule::Start => {
            if group.activation_state != "active" {
                return Err(GErr::conflict(
                    "group_not_ready",
                    "the group's encryption is still being set up",
                ));
            }
            if m.role == "restricted" || (group.stream_policy != "everyone" && !is_admin(m)) {
                return Err(GErr::forbidden(
                    "stream_restricted",
                    "only admins can go live in this group",
                ));
            }
            Ok(())
        }
        Rule::Stage => {
            if m.role == "restricted" {
                return Err(restricted());
            }
            match stage {
                Some("invited" | "speaker") => Ok(()),
                _ => Err(GErr::forbidden(
                    "not_invited",
                    "the streamer has not invited you onto the stage",
                )),
            }
        }
        Rule::Moderate => {
            let host = stream.is_some_and(|s| s.host == m.member);
            if host || is_admin(m) {
                Ok(())
            } else {
                Err(GErr::forbidden(
                    "not_host",
                    "only the streamer or a group admin can do this",
                ))
            }
        }
    }
}

/// The streamer may keep streaming only while the rules would still let them start (a policy
/// change, a restriction or a removal ends the stream).
fn host_still_allowed(c: &Connection, s: &StreamRow) -> GResult<Option<&'static str>> {
    let Some(g) = load_group(c, &s.group_id)? else {
        return Ok(Some("group_deleted"));
    };
    if g.deleted_at.is_some() {
        return Ok(Some("group_deleted"));
    }
    let m = load_member(c, &s.group_id, &s.host)?;
    Ok(
        match rule_allows(Rule::Start, &g, m.as_ref(), Some(s), None) {
            Ok(()) => None,
            Err(e) if e.code == "stream_restricted" => Some("policy"),
            Err(_) => Some("host_removed"),
        },
    )
}

// ---- request context ---------------------------------------------------------------------------

/// The acting mailbox, resolved for `action`, with `groups` and `streaming` both on.
struct SCtx {
    principal: Principal,
    me: crate::store::StoredIdentity,
}

async fn sctx(
    state: &AppState,
    headers: &HeaderMap,
    identity: Option<&str>,
    action: Action,
) -> GResult<SCtx> {
    let explicit = identity.or_else(|| header_value(headers, "x-pigeonpost-identity"));
    let (principal, me) = mailbox_for(state, headers, explicit, action)
        .await
        .map_err(|e| match e.code {
            // `mailbox_for` names the credential problem; for a stream it is "a person only".
            "human_required" | "credential_not_accepted" => GErr::forbidden(
                "human_required",
                "only a person signed in to Pigeonpost can do this; agents and API keys cannot",
            ),
            _ => GErr::from(e),
        })?;
    for flag in [capabilities::GROUPS, capabilities::STREAMING] {
        if !feature_on(state, me.account_id.as_deref(), flag).await {
            return Err(GErr::new(
                StatusCode::NOT_FOUND,
                "feature_disabled",
                "live streams are not enabled for this account",
            ));
        }
    }
    Ok(SCtx { principal, me })
}

/// Decide through the policy: the credential holds the mailbox, the mandatory denies pass, and the
/// group rules (already evaluated into `rule`) grant this action.
fn decide(
    principal: &Principal,
    me: &crate::store::StoredIdentity,
    action: Action,
    rule: GResult<()>,
) -> GResult<()> {
    let facts = Facts {
        now: now_unix(),
        stream_permitted: rule.is_ok(),
        ..Facts::default()
    };
    match authorize(
        &Actor::Principal(principal),
        action,
        &Resource::GroupStream {
            mailbox: &me.address,
            account: me.account_id.as_deref(),
        },
        &facts,
    ) {
        Ok(()) => Ok(()),
        Err(DenyReason::NotPermitted) => Err(rule.err().unwrap_or_else(|| {
            GErr::forbidden("forbidden", "this credential may not act for this mailbox")
        })),
        Err(DenyReason::Auth(_)) | Err(DenyReason::CredentialNotAccepted) => Err(GErr::forbidden(
            "human_required",
            "only a person signed in to Pigeonpost can do this; agents and API keys cannot",
        )),
        Err(d) => Err(GErr::forbidden(d.code(), "this action is not allowed")),
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: Result<Json<Value>, JsonRejection>) -> GResult<T> {
    let Json(value) = body.map_err(|e| GErr::bad("bad_request", e.body_text()))?;
    serde_json::from_value(value).map_err(|e| GErr::bad("bad_request", e.to_string()))
}

fn stream_event(s: &StreamRow, extra: Value) -> Value {
    let mut v = json!({
        "group_id": s.group_id,
        "stream_id": s.id,
        "state": s.state,
        "title": s.title,
        "by": s.host,
        "revision": s.revision,
    });
    if let (Value::Object(base), Value::Object(more)) = (&mut v, extra) {
        base.extend(more);
    }
    v
}

fn bump(c: &Connection, sid: &str) -> GResult<i64> {
    Ok(c.query_row(
        "UPDATE group_streams SET revision = revision + 1 WHERE id = ?1 RETURNING revision",
        params![sid],
        |r| r.get(0),
    )?)
}

/// End a live stream inside the caller's transaction: mark it, post the `stream` notice and tell
/// the group. Returns the accounts to wake. Ending an ended stream does nothing.
pub(crate) fn end_in(
    c: &Connection,
    sid: &str,
    reason: &str,
    by: Option<&str>,
    now: u64,
) -> GResult<Vec<String>> {
    let Some(s) = load_stream(c, sid)? else {
        return Ok(Vec::new());
    };
    if s.state != "live" {
        return Ok(Vec::new());
    }
    let viewers = distinct_viewers(c, &s)?;
    c.execute(
        "UPDATE group_streams SET state = 'ended', ended_at = ?2, end_reason = ?3, ended_by = ?4,
                viewers_now = 0, revision = revision + 1
          WHERE id = ?1",
        params![sid, now as i64, reason, by],
    )?;
    let s = load_stream(c, sid)?.ok_or_else(no_stream)?;
    let duration = (now as i64 - s.started_at).max(0);
    let mut wake = Vec::new();
    if let Some(g) = load_group(c, &s.group_id)? {
        if g.deleted_at.is_none() {
            // Authenticated metadata in the group's sequence, rendered by each client in its own
            // words ("Jane streamed for 34 min · 18 viewers"). No title: the notice outlives the
            // stream's own rows.
            add_notice(
                c,
                &s.group_id,
                "stream",
                Some(&s.host),
                json!({ "stream_id": s.id, "duration": duration, "viewers": viewers, "reason": reason }),
                now,
            )?;
            wake = emit_members(
                c,
                &s.group_id,
                "group.stream",
                &stream_event(
                    &s,
                    json!({ "reason": reason, "duration": duration, "viewers": viewers }),
                ),
                now,
            )?;
        }
    }
    Ok(wake)
}

// ---- member routes -----------------------------------------------------------------------------

#[derive(Deserialize)]
struct StartReq {
    title: String,
    #[serde(default)]
    sources: Vec<String>,
    public_key: String,
}

/// `POST /v1/groups/{id}/streams` — go live.
pub(crate) async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(gid): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let cx = sctx(
            &state,
            &headers,
            q.identity.as_deref(),
            Action::StreamPublish,
        )
        .await?;
        let req: StartReq = parse(body)?;
        let title = clean_title(&req.title)?;
        let mut sources: Vec<String> = Vec::new();
        for src in &req.sources {
            if !SOURCES.contains(&src.as_str()) {
                return Err(GErr::bad("bad_request", "sources may be camera and screen"));
            }
            if !sources.contains(src) {
                sources.push(src.clone());
            }
        }
        if sources.is_empty() {
            return Err(GErr::bad("bad_request", "choose camera, screen or both"));
        }
        if !valid_public_key(&req.public_key) {
            return Err(GErr::bad(
                "bad_key",
                "public_key must be a P-256 public key",
            ));
        }
        let me = cx.me.address.clone();
        rate_limit(&format!("stream-start:{me}"), STARTS_PER_HOUR, 3_600)?;
        let now = now_unix();
        let key = req.public_key;
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let g = live_group(tx, &gid)?;
                let m = load_member(tx, &gid, &me)?;
                let rule = rule_allows(Rule::Start, &g, m.as_ref(), None, None);
                decide(&cx.principal, &cx.me, Action::StreamPublish, rule)?;
                if let Some(live) = live_stream_of(tx, &gid)? {
                    return Err(GErr::conflict(
                        "stream_in_progress",
                        "someone is already live in this group",
                    )
                    .with("stream_id", live.id));
                }
                let sid = new_stream_id();
                tx.execute(
                    "INSERT INTO group_streams (id, group_id, host, title, sources, host_key, state,
                            started_at, ends_by, last_beat)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'live', ?7, ?8, ?7)",
                    params![
                        sid,
                        gid,
                        me,
                        title,
                        json!(sources).to_string(),
                        key,
                        now as i64,
                        (now + MAX_STREAM_SECS) as i64
                    ],
                )?;
                let s = load_stream(tx, &sid)?.ok_or_else(no_stream)?;
                let mut out = Outcome::default();
                out.wake =
                    emit_members(tx, &gid, "group.stream", &stream_event(&s, json!({})), now)?;
                out.status = 201;
                out.body = json!({
                    "stream": stream_view(tx, &s)?,
                    "role": "host",
                    "host_key": s.host_key,
                    "media": media(&sid),
                });
                Ok(out)
            })
            .await
    }
    .await;
    if result.as_ref().is_ok_and(|o| o.status == 201) {
        crate::groups::audit(&state, &headers, "group.stream.start", None).await;
    }
    finish(&state, result)
}

/// `GET /v1/groups/{id}/streams` — the group's live stream, if any.
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(gid): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let cx = sctx(&state, &headers, q.identity.as_deref(), Action::StreamWatch).await?;
        let me = cx.me.address.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                let g = live_group(c, &gid)?;
                let m = load_member(c, &gid, &me)?;
                decide(
                    &cx.principal,
                    &cx.me,
                    Action::StreamWatch,
                    rule_allows(Rule::Watch, &g, m.as_ref(), None, None),
                )?;
                let mut streams = Vec::new();
                if let Some(s) = live_stream_of(c, &gid)? {
                    streams.push(stream_view(c, &s)?);
                }
                Ok(Outcome::ok(json!({ "streams": streams })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// The stream, its group and the caller's membership, for routes addressed by stream id.
fn stream_and_member(
    c: &Connection,
    sid: &str,
    me: &str,
) -> GResult<(StreamRow, GroupRow, Option<MemberRow>)> {
    let s = load_stream(c, sid)?.ok_or_else(no_stream)?;
    let g = live_group(c, &s.group_id)?;
    let m = load_member(c, &s.group_id, me)?;
    // Somebody with no relationship to the group learns nothing about the stream.
    if m.as_ref()
        .is_none_or(|m| !matches!(m.state.as_str(), "active" | "keying"))
    {
        return Err(no_stream());
    }
    Ok((s, g, m))
}

/// `GET /v1/streams/{id}`.
pub(crate) async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let cx = sctx(&state, &headers, q.identity.as_deref(), Action::StreamWatch).await?;
        let me = cx.me.address.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                let (s, g, m) = stream_and_member(c, &sid, &me)?;
                decide(
                    &cx.principal,
                    &cx.me,
                    Action::StreamWatch,
                    rule_allows(Rule::Watch, &g, m.as_ref(), Some(&s), None),
                )?;
                let role = if s.state == "live" {
                    role_of(c, &s, &me)?
                } else {
                    "viewer"
                };
                Ok(Outcome::ok(
                    json!({ "stream": stream_view(c, &s)?, "role": role }),
                ))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
struct WatchReq {
    public_key: String,
}

/// `POST /v1/streams/{id}/watch` — register this device's receive key and learn the streamer's.
/// The streamer uses it too, after a reload, to replace the stream's sending key.
pub(crate) async fn watch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let cx = sctx(&state, &headers, q.identity.as_deref(), Action::StreamWatch).await?;
        let req: WatchReq = parse(body)?;
        if !valid_public_key(&req.public_key) {
            return Err(GErr::bad("bad_key", "public_key must be a P-256 public key"));
        }
        let me = cx.me.address.clone();
        rate_limit(&format!("stream-watch:{me}"), 60, 60)?;
        let now = now_unix();
        let key = req.public_key;
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let (s, g, m) = stream_and_member(tx, &sid, &me)?;
                decide(
                    &cx.principal,
                    &cx.me,
                    Action::StreamWatch,
                    rule_allows(Rule::Watch, &g, m.as_ref(), Some(&s), None),
                )?;
                if s.state != "live" {
                    return Err(GErr::new(StatusCode::GONE, "stream_ended", "this stream has ended")
                        .with("stream", stream_view(tx, &s)?));
                }
                let mut out = Outcome::default();
                if s.host == me {
                    // The streamer's device changed key (a reload): it is the only sending key.
                    // Replacing it needs the same person who went live.
                    decide(
                        &cx.principal,
                        &cx.me,
                        Action::StreamPublish,
                        rule_allows(Rule::Moderate, &g, m.as_ref(), Some(&s), None),
                    )?;
                    if s.host_key.as_deref() != Some(key.as_str()) {
                        tx.execute(
                            "UPDATE group_streams SET host_key = ?2 WHERE id = ?1",
                            params![sid, key],
                        )?;
                        bump(tx, &sid)?;
                        let s = load_stream(tx, &sid)?.ok_or_else(no_stream)?;
                        out.wake = emit_members(
                            tx,
                            &s.group_id,
                            "group.stream",
                            &stream_event(&s, json!({ "host_key_changed": true })),
                            now,
                        )?;
                    }
                } else {
                    tx.execute(
                        "INSERT OR IGNORE INTO group_stream_keys (stream_id, member, public_key, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![sid, me, key, now as i64],
                    )?;
                    // A few devices at once; the oldest key beyond that stops being listed.
                    tx.execute(
                        "DELETE FROM group_stream_keys WHERE stream_id = ?1 AND member = ?2 AND public_key NOT IN
                           (SELECT public_key FROM group_stream_keys WHERE stream_id = ?1 AND member = ?2
                             ORDER BY created_at DESC, rowid DESC LIMIT ?3)",
                        params![sid, me, KEYS_PER_MEMBER],
                    )?;
                }
                let s = load_stream(tx, &sid)?.ok_or_else(no_stream)?;
                out.status = 200;
                out.body = json!({
                    "stream": stream_view(tx, &s)?,
                    "role": role_of(tx, &s, &me)?,
                    "host_key": s.host_key,
                    "media": media(&sid),
                });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `GET /v1/streams/{id}/audience` — the key directory, for the streamer: every current active
/// member who registered a receive key, with those keys. The streamer wraps the stream key to
/// these keys and no others.
pub(crate) async fn audience(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let cx = sctx(&state, &headers, q.identity.as_deref(), Action::StreamModerate).await?;
        let me = cx.me.address.clone();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                let (s, g, m) = stream_and_member(c, &sid, &me)?;
                // The streamer only: the key holder is the one who needs the directory.
                let rule = if s.host == me {
                    rule_allows(Rule::Moderate, &g, m.as_ref(), Some(&s), None)
                } else {
                    Err(GErr::forbidden("not_host", "only the streamer reads the audience"))
                };
                decide(&cx.principal, &cx.me, Action::StreamModerate, rule)?;
                if s.state != "live" {
                    return Err(GErr::new(StatusCode::GONE, "stream_ended", "this stream has ended"));
                }
                let mut stmt = c.prepare(
                    "SELECT k.member, k.public_key, i.handle FROM group_stream_keys k
                       JOIN group_members gm ON gm.group_id = ?2 AND gm.member = k.member AND gm.state = 'active'
                       LEFT JOIN identities i ON i.address = k.member
                      WHERE k.stream_id = ?1 AND k.member != ?3
                      ORDER BY k.member, k.created_at",
                )?;
                let rows: Vec<(String, String, Option<String>)> = stmt
                    .query_map(params![s.id, s.group_id, s.host], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })?
                    .collect::<Result<_, _>>()?;
                let mut audience: Vec<Value> = Vec::new();
                for (member, key, handle) in rows {
                    match audience.last_mut() {
                        Some(last) if last["address"] == member.as_str() => {
                            last["keys"].as_array_mut().expect("keys").push(json!(key));
                        }
                        _ => {
                            let role = role_of(c, &s, &member)?;
                            audience.push(json!({ "address": member, "handle": handle, "role": role, "keys": [key] }));
                        }
                    }
                }
                Ok(Outcome::ok(json!({ "stream_id": s.id, "revision": s.revision, "audience": audience })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
struct StageReq {
    address: String,
    on: bool,
}

/// `POST /v1/streams/{id}/stage {address, on}`.
///
/// * The streamer or a group admin, about someone else: `on` invites (they must accept), `off`
///   takes them off the stage or withdraws the invitation.
/// * The invited member, about themself: `on` accepts (from now on the calls service gives them a
///   publishing ticket), `off` declines or steps down.
pub(crate) async fn stage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    Query(q): Query<IdentityQuery>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: StageReq = parse(body)?;
        // Stepping down needs no more than watching does; everything else opens a microphone or
        // decides for someone else, which is a person's.
        let explicit = q.identity.clone();
        let cx = sctx(&state, &headers, explicit.as_deref(), Action::StreamWatch).await?;
        let me = cx.me.address.clone();
        rate_limit(&format!("stream-stage:{me}"), 30, 60)?;
        let target = req.address.trim().to_string();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let (s, g, m) = stream_and_member(tx, &sid, &me)?;
                if s.state != "live" {
                    return Err(GErr::new(StatusCode::GONE, "stream_ended", "this stream has ended"));
                }
                let target = if target.eq_ignore_ascii_case(&me) { me.clone() } else {
                    // A handle or an address of a member of this group.
                    tx.query_row(
                        "SELECT m.member FROM group_members m LEFT JOIN identities i ON i.address = m.member
                          WHERE m.group_id = ?1 AND (lower(m.member) = lower(?2) OR lower(i.handle) = lower(?2))",
                        params![s.group_id, target],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or_else(|| GErr::bad("not_member", "that mailbox is not a member of this group"))?
                };
                let current = stage_state(tx, &sid, &target)?;
                let changed: Option<&str>;
                if target == me {
                    if s.host == me {
                        return Err(GErr::bad("bad_request", "the streamer is always on the stage"));
                    }
                    if req.on {
                        decide(
                            &cx.principal,
                            &cx.me,
                            Action::StreamPublish,
                            rule_allows(Rule::Stage, &g, m.as_ref(), Some(&s), current.as_deref()),
                        )?;
                        tx.execute(
                            "UPDATE group_stream_stage SET state = 'speaker', updated_at = ?3 WHERE stream_id = ?1 AND member = ?2",
                            params![sid, me, now as i64],
                        )?;
                        changed = (current.as_deref() != Some("speaker")).then_some("speaker");
                    } else {
                        decide(
                            &cx.principal,
                            &cx.me,
                            Action::StreamWatch,
                            rule_allows(Rule::Watch, &g, m.as_ref(), Some(&s), None),
                        )?;
                        tx.execute(
                            "DELETE FROM group_stream_stage WHERE stream_id = ?1 AND member = ?2",
                            params![sid, me],
                        )?;
                        changed = current.is_some().then_some("viewer");
                    }
                } else {
                    decide(
                        &cx.principal,
                        &cx.me,
                        Action::StreamModerate,
                        rule_allows(Rule::Moderate, &g, m.as_ref(), Some(&s), None),
                    )?;
                    if target == s.host {
                        return Err(GErr::bad("bad_request", "the streamer is always on the stage"));
                    }
                    if req.on {
                        let tm = active_member(tx, &s.group_id, &target).map_err(|_| {
                            GErr::bad("not_member", "that mailbox is not an active member of this group")
                        })?;
                        if tm.role == "restricted" {
                            return Err(GErr::forbidden(
                                "stream_restricted",
                                "a restricted member cannot be invited onto the stage",
                            ));
                        }
                        if current.is_none() {
                            let on_stage: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM group_stream_stage WHERE stream_id = ?1",
                                params![sid],
                                |r| r.get(0),
                            )?;
                            if on_stage >= STAGE_MAX {
                                return Err(GErr::conflict("stage_full", "the stage is full"));
                            }
                            tx.execute(
                                "INSERT INTO group_stream_stage (stream_id, member, state, invited_by, updated_at)
                                 VALUES (?1, ?2, 'invited', ?3, ?4)",
                                params![sid, target, me, now as i64],
                            )?;
                            changed = Some("invited");
                        } else {
                            changed = None;
                        }
                    } else {
                        tx.execute(
                            "DELETE FROM group_stream_stage WHERE stream_id = ?1 AND member = ?2",
                            params![sid, target],
                        )?;
                        changed = current.is_some().then_some("viewer");
                    }
                }
                let mut out = Outcome::default();
                if let Some(now_state) = changed {
                    bump(tx, &sid)?;
                    let s = load_stream(tx, &sid)?.ok_or_else(no_stream)?;
                    let data = stream_event(&s, json!({ "stage": { "address": target, "state": now_state, "by": me } }));
                    out.wake = emit_members(tx, &s.group_id, "group.stream", &data, now)?;
                }
                let s = load_stream(tx, &sid)?.ok_or_else(no_stream)?;
                out.status = 200;
                out.body = json!({ "stream": stream_view(tx, &s)?, "address": target, "role": role_of(tx, &s, &target)? });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/streams/{id}` — stop: the streamer or a group admin.
pub(crate) async fn stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let result = async {
        let cx = sctx(
            &state,
            &headers,
            q.identity.as_deref(),
            Action::StreamModerate,
        )
        .await?;
        let me = cx.me.address.clone();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let (s, g, m) = stream_and_member(tx, &sid, &me)?;
                decide(
                    &cx.principal,
                    &cx.me,
                    Action::StreamModerate,
                    rule_allows(Rule::Moderate, &g, m.as_ref(), Some(&s), None),
                )?;
                let mut out = Outcome::default();
                if s.state == "live" {
                    let reason = if s.host == me {
                        "stopped"
                    } else {
                        "stopped_by_admin"
                    };
                    out.wake = end_in(tx, &sid, reason, Some(&me), now)?;
                }
                let s = load_stream(tx, &sid)?.ok_or_else(no_stream)?;
                out.status = 200;
                out.body = json!({ "stream": stream_view(tx, &s)? });
                Ok(out)
            })
            .await
    }
    .await;
    if result.as_ref().is_ok_and(|o| o.status == 200) {
        crate::groups::audit(&state, &headers, "group.stream.stop", None).await;
    }
    finish(&state, result)
}

// ---- the calls service (internal) --------------------------------------------------------------

fn internal_auth(state: &AppState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    crate::internal_api::authorize_service(
        state,
        headers,
        crate::internal_api::Endpoint::StreamAdmission,
        Action::StreamAdmission,
    )
    .map(|_| ())
}

#[derive(Deserialize)]
pub(crate) struct Mailbox {
    mailbox: String,
}

#[derive(Deserialize)]
pub(crate) struct AdmissionReq {
    stream_id: String,
    from: Mailbox,
    #[serde(default)]
    public_key: Option<String>,
}

/// `POST /internal/v1/stream-admission` — may this mailbox join this stream's media room, and as
/// what? Asked by the calls service for every join and role refresh, with the caller's own mailbox
/// (which it already proved owns the member token).
pub(crate) async fn admission(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<AdmissionReq>, JsonRejection>,
) -> Response {
    if let Err(r) = internal_auth(&state, &headers) {
        return *r;
    }
    let Ok(Json(req)) = body else {
        return ApiError::bad(
            "bad_request",
            "expected {stream_id, from:{mailbox}, public_key?}",
        )
        .into_response();
    };
    let deny = |reason: &str| Json(json!({ "allowed": false, "reason": reason })).into_response();
    let caller = match state.store.get(req.from.mailbox.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => return deny("caller_invalid"),
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    for flag in [capabilities::GROUPS, capabilities::STREAMING] {
        if !feature_on(&state, caller.account_id.as_deref(), flag).await {
            return deny("feature_disabled");
        }
    }
    let me = caller.address.clone();
    let sid = req.stream_id.clone();
    let key = req.public_key.clone();
    let looked = state
        .store
        .group_read(move |c: &Connection| -> GResult<Value> {
            let (s, g, m) = stream_and_member(c, &sid, &me)?;
            rule_allows(Rule::Watch, &g, m.as_ref(), Some(&s), None)?;
            if s.state != "live" {
                return Ok(json!({ "allowed": false, "reason": "stream_ended" }));
            }
            let role = role_of(c, &s, &me)?;
            // The calls service lists only participants whose key the postbox knows for them.
            let key_attested = match &key {
                None => false,
                Some(k) if s.host == me => s.host_key.as_deref() == Some(k.as_str()),
                Some(k) => c
                    .query_row(
                        "SELECT 1 FROM group_stream_keys WHERE stream_id = ?1 AND member = ?2 AND public_key = ?3",
                        params![s.id, me, k],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some(),
            };
            // A restricted speaker cannot publish, whatever the stage row says.
            let role = if role == "speaker" && m.as_ref().is_some_and(|m| m.role == "restricted") {
                "viewer"
            } else {
                role
            };
            Ok(json!({
                "allowed": true,
                "role": role,
                "group_id": s.group_id,
                "host": s.host,
                "host_key": s.host_key,
                "sources": s.sources,
                "state": s.state,
                "ends_by": s.ends_by,
                "max_viewers": max_viewers(),
                "key_attested": key_attested,
            }))
        })
        .await;
    match looked {
        Ok(v) => Json(v).into_response(),
        Err(e) if e.status.is_server_error() => ApiError::server("store_error").into_response(),
        Err(e) => deny(if e.code == "no_stream" {
            "no_stream"
        } else {
            "not_member"
        }),
    }
}

#[derive(Deserialize)]
pub(crate) struct CheckReq {
    stream_id: String,
    #[serde(default)]
    viewers: i64,
    #[serde(default)]
    members: Vec<String>,
}

/// `POST /internal/v1/stream-check` — the calls service's heartbeat for a live stream: the current
/// viewer count in, and back the stream's state plus the current role of every mailbox it has in
/// the media room (`null`: no longer allowed; remove them and rotate the key).
pub(crate) async fn check(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CheckReq>, JsonRejection>,
) -> Response {
    if let Err(r) = internal_auth(&state, &headers) {
        return *r;
    }
    let Ok(Json(req)) = body else {
        return ApiError::bad("bad_request", "expected {stream_id, viewers, members:[]}")
            .into_response();
    };
    if req.members.len() > 2_000 || !(0..=100_000).contains(&req.viewers) {
        return ApiError::bad("bad_request", "too many members").into_response();
    }
    let now = now_unix();
    let result = state
        .store
        .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
            let Some(s) = load_stream(tx, &req.stream_id)? else {
                return Ok((json!({ "state": "ended", "reason": "no_stream", "roles": {} }), Vec::new()));
            };
            let mut wake = Vec::new();
            if s.state == "live" {
                let reason = if now as i64 >= s.ends_by {
                    Some("max_duration")
                } else {
                    host_still_allowed(tx, &s)?
                };
                if let Some(reason) = reason {
                    wake = end_in(tx, &s.id, reason, None, now)?;
                } else {
                    tx.execute(
                        "UPDATE group_streams SET last_beat = ?2, viewers_now = ?3,
                                viewers_peak = MAX(viewers_peak, ?3) WHERE id = ?1",
                        params![s.id, now as i64, req.viewers],
                    )?;
                }
            }
            let s = load_stream(tx, &req.stream_id)?.ok_or_else(no_stream)?;
            let mut roles = serde_json::Map::new();
            for who in &req.members {
                let role = if s.state != "live" {
                    Value::Null
                } else {
                    let m = load_member(tx, &s.group_id, who)?;
                    match (m.as_ref().map(|m| (m.state.as_str(), m.role.as_str())), s.host == *who) {
                        (Some(("active", _)), true) => json!("host"),
                        (Some(("active", "restricted")), false) => json!("viewer"),
                        (Some(("active", _)), false) => json!(role_of(tx, &s, who)?),
                        _ => Value::Null,
                    }
                };
                roles.insert(who.clone(), role);
            }
            Ok((
                json!({ "state": s.state, "reason": s.end_reason, "ends_by": s.ends_by, "host_key": s.host_key, "revision": s.revision, "roles": roles }),
                wake,
            ))
        })
        .await;
    match result {
        Ok((v, wake)) => {
            for a in wake {
                state.event_hub.wake(&a);
            }
            Json(v).into_response()
        }
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct EndReq {
    stream_id: String,
    reason: String,
}

/// `POST /internal/v1/stream-end` — the calls service ends a stream (the streamer left and did not
/// come back, or the media room could not continue).
pub(crate) async fn end_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<EndReq>, JsonRejection>,
) -> Response {
    if let Err(r) = internal_auth(&state, &headers) {
        return *r;
    }
    let Ok(Json(req)) = body else {
        return ApiError::bad("bad_request", "expected {stream_id, reason}").into_response();
    };
    let reason = match req.reason.as_str() {
        "host_left" | "media_failed" | "max_duration" | "capacity" => req.reason.clone(),
        _ => return ApiError::bad("bad_request", "unknown reason").into_response(),
    };
    let now = now_unix();
    let sid = req.stream_id;
    let result = state
        .store
        .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
            let wake = end_in(tx, &sid, &reason, None, now)?;
            let state = load_stream(tx, &sid)?.map(|s| s.state);
            Ok((json!({ "ok": true, "state": state }), wake))
        })
        .await;
    match result {
        Ok((v, wake)) => {
            for a in wake {
                state.event_hub.wake(&a);
            }
            Json(v).into_response()
        }
        Err(e) => e.into_response(),
    }
}

// ---- maintenance -------------------------------------------------------------------------------

/// End streams past their four hours or no longer reported by the calls service, and delete the
/// rows of streams that ended more than [`RETAIN_ENDED_SECS`] ago. Returns accounts to wake.
pub(crate) fn sweep_in(tx: &Connection, now: u64) -> GResult<Vec<String>> {
    let due: Vec<(String, &'static str)> = {
        let mut stmt = tx.prepare(
            "SELECT id, ends_by <= ?1 FROM group_streams
              WHERE state = 'live' AND (ends_by <= ?1 OR last_beat <= ?2) LIMIT 100",
        )?;
        let rows = stmt.query_map(
            params![now as i64, now as i64 - BEAT_TIMEOUT_SECS as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    if r.get::<_, bool>(1)? {
                        "max_duration"
                    } else {
                        "lost"
                    },
                ))
            },
        )?;
        rows.collect::<Result<_, _>>()?
    };
    let mut wake = Vec::new();
    for (sid, reason) in due {
        wake.extend(end_in(tx, &sid, reason, None, now)?);
    }
    // Streams whose group went away or whose streamer may no longer stream.
    let live: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM group_streams WHERE state = 'live' LIMIT 500")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    for sid in live {
        if let Some(s) = load_stream(tx, &sid)? {
            if let Some(reason) = host_still_allowed(tx, &s)? {
                wake.extend(end_in(tx, &sid, reason, None, now)?);
            }
        }
    }
    let old = now as i64 - RETAIN_ENDED_SECS as i64;
    tx.execute(
        "DELETE FROM group_stream_keys WHERE stream_id IN
           (SELECT id FROM group_streams WHERE state = 'ended' AND ended_at <= ?1)",
        params![old],
    )?;
    tx.execute(
        "DELETE FROM group_stream_stage WHERE stream_id IN
           (SELECT id FROM group_streams WHERE state = 'ended' AND ended_at <= ?1)",
        params![old],
    )?;
    tx.execute(
        "DELETE FROM group_streams WHERE state = 'ended' AND ended_at <= ?1",
        params![old],
    )?;
    // Stage rows of anyone no longer an active member are meaningless; drop them now.
    tx.execute(
        "DELETE FROM group_stream_stage WHERE EXISTS (
            SELECT 1 FROM group_streams s WHERE s.id = group_stream_stage.stream_id AND s.state = 'live'
              AND NOT EXISTS (SELECT 1 FROM group_members m WHERE m.group_id = s.group_id
                                AND m.member = group_stream_stage.member AND m.state = 'active'))",
        [],
    )?;
    Ok(wake)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(policy: &str) -> GroupRow {
        GroupRow {
            id: "grp_x".into(),
            org_id: None,
            name: "g".into(),
            description: None,
            avatar_id: None,
            join_policy: "private".into(),
            post_policy: "everyone".into(),
            visibility: "unlisted".into(),
            members_can_invite: false,
            members_can_change_timer: false,
            revision: 1,
            activation_state: "active".into(),
            link_token_hash: None,
            ephemeral_secs: None,
            max_members: 1000,
            epoch: 1,
            last_seq: 1,
            pruned_through_seq: 0,
            created_by: "/k/o".into(),
            created_at: 0,
            last_at: 0,
            deleted_at: None,
            stream_policy: policy.into(),
        }
    }

    fn member(who: &str, role: &str, state: &str) -> MemberRow {
        MemberRow {
            member: who.into(),
            role: role.into(),
            state: state.into(),
            invited_by: None,
            filtered: false,
            joined_at: Some(0),
            version: 1,
            revision: 1,
            read_cursor: 0,
            muted_until: None,
            notify: "all".into(),
        }
    }

    fn stream(host: &str) -> StreamRow {
        StreamRow {
            id: "str_aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            group_id: "grp_x".into(),
            host: host.into(),
            title: "t".into(),
            sources: vec!["camera".into()],
            host_key: None,
            state: "live".into(),
            started_at: 0,
            ends_by: 1,
            ended_at: None,
            end_reason: None,
            viewers_now: 0,
            viewers_peak: 0,
            revision: 1,
        }
    }

    fn code(r: GResult<()>) -> &'static str {
        match r {
            Ok(()) => "ok",
            Err(e) => e.code,
        }
    }

    #[test]
    fn who_may_go_live_follows_the_stream_policy() {
        let admins = group("admins");
        let everyone = group("everyone");
        for (role, by_admins, by_everyone) in [
            ("owner", "ok", "ok"),
            ("admin", "ok", "ok"),
            ("member", "stream_restricted", "ok"),
            ("restricted", "stream_restricted", "stream_restricted"),
        ] {
            let m = member("/k/m", role, "active");
            assert_eq!(
                code(rule_allows(Rule::Start, &admins, Some(&m), None, None)),
                by_admins,
                "{role}"
            );
            assert_eq!(
                code(rule_allows(Rule::Start, &everyone, Some(&m), None, None)),
                by_everyone,
                "{role}"
            );
        }
        let mut provisioning = group("everyone");
        provisioning.activation_state = "provisioning".into();
        let owner = member("/k/o", "owner", "active");
        assert_eq!(
            code(rule_allows(
                Rule::Start,
                &provisioning,
                Some(&owner),
                None,
                None
            )),
            "group_not_ready"
        );
    }

    #[test]
    fn only_active_members_watch() {
        let g = group("admins");
        for (state, want) in [
            ("active", "ok"),
            ("keying", "key_activation_pending"),
            ("invited", "not_member"),
            ("left", "not_member"),
            ("removed", "not_member"),
            ("banned", "not_member"),
        ] {
            let m = member("/k/m", "restricted", state);
            assert_eq!(
                code(rule_allows(Rule::Watch, &g, Some(&m), None, None)),
                want,
                "{state}"
            );
        }
        assert_eq!(
            code(rule_allows(Rule::Watch, &g, None, None, None)),
            "not_member"
        );
    }

    #[test]
    fn the_stage_is_for_invited_unrestricted_members_and_moderation_for_host_or_admins() {
        let g = group("admins");
        let s = stream("/k/host");
        let m = member("/k/m", "member", "active");
        assert_eq!(
            code(rule_allows(Rule::Stage, &g, Some(&m), Some(&s), None)),
            "not_invited"
        );
        assert_eq!(
            code(rule_allows(
                Rule::Stage,
                &g,
                Some(&m),
                Some(&s),
                Some("invited")
            )),
            "ok"
        );
        let r = member("/k/r", "restricted", "active");
        assert_eq!(
            code(rule_allows(
                Rule::Stage,
                &g,
                Some(&r),
                Some(&s),
                Some("invited")
            )),
            "stream_restricted"
        );
        assert_eq!(
            code(rule_allows(Rule::Moderate, &g, Some(&m), Some(&s), None)),
            "not_host"
        );
        let host = member("/k/host", "member", "active");
        assert_eq!(
            code(rule_allows(Rule::Moderate, &g, Some(&host), Some(&s), None)),
            "ok"
        );
        let admin = member("/k/a", "admin", "active");
        assert_eq!(
            code(rule_allows(
                Rule::Moderate,
                &g,
                Some(&admin),
                Some(&s),
                None
            )),
            "ok"
        );
        let gone = member("/k/host", "member", "removed");
        assert_eq!(
            code(rule_allows(Rule::Moderate, &g, Some(&gone), Some(&s), None)),
            "not_member"
        );
    }

    #[test]
    fn ids_keys_and_titles_are_checked() {
        assert!(valid_stream_id(&new_stream_id()));
        assert!(!valid_stream_id("str_AAAAAAAAAAAAAAAAAAAAAAAAAA"));
        assert!(!valid_stream_id("grp_aaaaaaaaaaaaaaaaaaaaaaaaaa"));
        let key = format!("B{}", "A".repeat(86));
        assert!(valid_public_key(&key));
        assert!(!valid_public_key(&key[1..]));
        assert!(!valid_public_key(&format!("B{}=", "A".repeat(85))));
        assert_eq!(
            clean_title("  Sprint \u{202e}demo\n ").unwrap(),
            "Sprint demo"
        );
        assert!(clean_title("   ").is_err());
        assert!(clean_title(&"x".repeat(101)).is_err());
    }
}
