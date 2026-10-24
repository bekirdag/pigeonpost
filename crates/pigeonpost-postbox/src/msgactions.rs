//! Message actions and receipts (SDS §14C, `docs/message-actions.md`): the shared `msg_ref`, edits,
//! replies, and the sent → delivered → seen (→ working → done | failed) receipts.
//!
//! A direct message is one sealed row per copy, so nothing that refers to a message can use a row
//! id: the sender's and the recipient's ids differ. Every new message therefore gets a `msg_ref`,
//! stored on both rows and sealed into a small per-copy metadata envelope beside the message. Edits
//! and replies point at a `msg_ref`; receipts travel from the recipient's row to the sender's row
//! through it.

use crate::capabilities::{AGENT_STATUS, MESSAGE_EDIT, MESSAGE_REPLY, RECEIPTS};
use crate::filtering::feature_on;
use crate::store::{self, MessageMeta, ReceiptKind, RefRow};
use crate::{now_unix, verbs, ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ed25519_dalek::VerifyingKey;
use pigeonpost_core::{envelope, Identity};
use serde_json::{json, Value};
use std::collections::HashMap;

/// How long after sending a message its sender may still edit it.
pub(crate) const EDIT_WINDOW_SECS: u64 = 24 * 3600;
/// Longest client-provided reply quote kept.
pub(crate) const QUOTE_MAX_CHARS: usize = 200;
/// Longest excerpt of a parent message shown with a reply.
const EXCERPT_MAX_CHARS: usize = 200;
/// References one `POST /v1/receipts` may name.
const RECEIPT_BATCH: usize = 100;
/// `POST /v1/receipts` calls per mailbox per minute.
const RECEIPT_CALLS_PER_MINUTE: u32 = 60;

const B32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// `mref_` + 26 lowercase base32 characters.
pub(crate) fn is_msg_ref(s: &str) -> bool {
    s.len() == 31 && s.starts_with("mref_") && s[5..].bytes().all(|b| B32.contains(&b))
}

/// A fresh random `msg_ref` (130 random bits; 256 is a multiple of 32, so `% 32` is unbiased).
pub(crate) fn new_msg_ref() -> String {
    let mut buf = [0u8; 26];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut buf);
    let mut out = String::from("mref_");
    for b in buf {
        out.push(B32[(b % 32) as usize] as char);
    }
    out
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// The words of a message for a one-line quote: a request's note or task, else the first non-empty
/// line of the text.
pub(crate) fn excerpt_of(body: &str) -> String {
    let text = match verbs::parse(body) {
        Some(req) => req
            .note
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| {
                ["task", "question"]
                    .iter()
                    .find_map(|k| req.args.get(*k).and_then(Value::as_str).map(str::to_string))
            })
            .unwrap_or_else(|| req.verb.clone()),
        None => body.to_string(),
    };
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    clip(line, EXCERPT_MAX_CHARS)
}

/// What an old client sees for an edit: the same message with `Edited: ` in front of the words. A
/// request envelope stays an envelope, with the prefix inside its text fields.
pub(crate) fn legacy_edit_body(body: &str) -> String {
    if let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(body) {
        if verbs::parse(body).is_some() {
            if let Some(Value::String(note)) = obj.get_mut("note") {
                *note = format!("Edited: {note}");
            }
            if let Some(Value::Object(args)) = obj.get_mut("args") {
                for key in ["task", "question"] {
                    if let Some(Value::String(v)) = args.get_mut(key) {
                        *v = format!("Edited: {v}");
                    }
                }
            }
            return Value::Object(obj).to_string();
        }
    }
    format!("Edited: {body}")
}

/// What an old client sees for a reply to prose: a quote line first. A request envelope is left as
/// it is, so a reply never stops an old agent from recognising a request.
pub(crate) fn legacy_reply_body(body: &str, quote: &str) -> String {
    if verbs::parse(body).is_some() || quote.is_empty() {
        return body.to_string();
    }
    format!("> {quote}\n\n{body}")
}

/// Open a sealed blob addressed to `me`.
pub(crate) fn open_text(me: &Identity, blob: &[u8]) -> Option<String> {
    let wrap: envelope::Wrap = serde_json::from_slice(blob).ok()?;
    envelope::open(me, &wrap)
        .ok()
        .map(|(_, b)| b.as_str().to_string())
}

/// The client quote sealed in a copy's metadata envelope.
pub(crate) fn sealed_quote(me: &Identity, meta: &MessageMeta) -> Option<String> {
    let text = open_text(me, meta.meta_blob.as_deref()?)?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["quote"].as_str().map(str::to_string)
}

// ---- sending -----------------------------------------------------------------------------------

/// The message-action fields of `POST /v1/send`.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub(crate) struct SendExtras {
    #[serde(default)]
    pub msg_ref: Option<String>,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub edit_of: Option<String>,
}

/// A send's references, resolved and checked.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    pub msg_ref: String,
    pub reply_to: Option<String>,
    pub quote: Option<String>,
    pub edit_of: Option<String>,
    /// For an edit: the original's thread on the sender's side.
    pub edit_thread: Option<String>,
    /// An edit inherits the original's disappearance deadline, including no timer.
    pub edit_expires_at: Option<u64>,
    /// Rechecked under the store's writer lock in case the window closes in flight.
    pub edit_deadline: Option<u64>,
    /// For an edit: the original was filed in the recipient's Filtered list.
    pub force_filtered: bool,
    /// A retry of a send that already happened: its original answer.
    pub duplicate: Option<Value>,
}

fn not_found_ref() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "reference_not_found",
        "no such message in this conversation",
    )
}

fn feature_off(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "feature_disabled",
        format!("{what} is not enabled for this account"),
    )
}

/// The peer a copy in `owner`'s mailbox is with.
fn peer_of(row: &RefRow) -> &str {
    if row.outgoing {
        &row.recipient
    } else {
        &row.sender
    }
}

/// Resolve and check a send's message-action fields, before anything is stored.
pub(crate) async fn prepare(
    state: &AppState,
    sender: &store::StoredIdentity,
    recipient: &str,
    extras: &SendExtras,
    has_attachments: bool,
) -> Result<Prepared, ApiError> {
    let store_err = |_| ApiError::server("store_error");
    let mut out = Prepared::default();

    out.msg_ref = match &extras.msg_ref {
        None => new_msg_ref(),
        Some(r) if !is_msg_ref(r) => {
            return Err(ApiError::bad(
                "invalid_msg_ref",
                "msg_ref must be mref_ followed by 26 characters of a-z and 2-7",
            ))
        }
        Some(r) => {
            let existing = state
                .store
                .rows_by_ref(sender.address.clone(), r.clone())
                .await
                .map_err(store_err)?;
            let mine: Vec<&RefRow> = existing.iter().filter(|row| row.msg_ref == *r).collect();
            if let Some(row) = mine
                .iter()
                .find(|row| row.outgoing && row.recipient == recipient)
            {
                // The same message again: answer as the first send did, without sending twice.
                let delivered = state
                    .store
                    .copy_in(recipient.to_string(), r.clone())
                    .await
                    .map_err(store_err)?;
                out.duplicate = Some(json!({
                    "message_id": delivered,
                    "sent_copy_id": row.id,
                    "recipient": recipient,
                    "msg_ref": r,
                    "duplicate": true,
                }));
                return Ok(out);
            }
            // A reference is unique within each mailbox it lands in, so one conversation's
            // reference can never be borrowed to make a message pass for part of another.
            let theirs = state
                .store
                .rows_by_ref(recipient.to_string(), r.clone())
                .await
                .map_err(store_err)?
                .into_iter()
                .any(|row| row.msg_ref == *r);
            if !mine.is_empty() || theirs {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "msg_ref_in_use",
                    "this msg_ref already names a different message",
                ));
            }
            r.clone()
        }
    };

    if let Some(key) = &extras.edit_of {
        if !feature_on(state, sender.account_id.as_deref(), MESSAGE_EDIT).await {
            return Err(feature_off("editing messages"));
        }
        if has_attachments {
            return Err(ApiError::bad(
                "edit_has_attachments",
                "an edit changes the text only; send files in a new message",
            ));
        }
        let rows = state
            .store
            .rows_by_ref(sender.address.clone(), key.clone())
            .await
            .map_err(store_err)?;
        let named = rows
            .iter()
            .find(|row| row.outgoing && row.recipient == recipient)
            .ok_or_else(not_found_ref)?;
        let root_ref = named
            .edit_of
            .clone()
            .unwrap_or_else(|| named.msg_ref.clone());
        let roots = state
            .store
            .rows_by_ref(sender.address.clone(), root_ref.clone())
            .await
            .map_err(store_err)?;
        let root = roots
            .iter()
            .find(|row| {
                row.outgoing
                    && row.edit_of.is_none()
                    && row.msg_ref == root_ref
                    && row.recipient == recipient
            })
            .ok_or_else(not_found_ref)?;
        if now_unix().saturating_sub(root.created_at) > EDIT_WINDOW_SECS {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "edit_window_closed",
                "a message can be edited for 24 hours after it was sent",
            ));
        }
        out.edit_thread = Some(root.thread_id.clone());
        out.edit_expires_at = root.expires_at;
        out.edit_deadline = Some(root.created_at.saturating_add(EDIT_WINDOW_SECS));
        out.force_filtered = state
            .store
            .rows_by_ref(recipient.to_string(), root_ref.clone())
            .await
            .map_err(store_err)?
            .iter()
            .any(|row| !row.outgoing && row.msg_ref == root_ref && row.filtered);
        out.edit_of = Some(root_ref);
        // An edit keeps the original's place in the conversation, reply included.
        return Ok(out);
    }

    if let Some(key) = &extras.reply_to {
        if !feature_on(state, sender.account_id.as_deref(), MESSAGE_REPLY).await {
            return Err(feature_off("replying to a message"));
        }
        let rows = state
            .store
            .rows_by_ref(sender.address.clone(), key.clone())
            .await
            .map_err(store_err)?;
        let parent = rows
            .iter()
            .find(|row| peer_of(row) == recipient)
            .ok_or_else(not_found_ref)?;
        out.reply_to = Some(
            parent
                .edit_of
                .clone()
                .unwrap_or_else(|| parent.msg_ref.clone()),
        );
        out.quote = extras
            .quote
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| q.chars().take(QUOTE_MAX_CHARS).collect());
    }
    Ok(out)
}

/// Commit both copies with their metadata, then announce an edit to the recipient.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_copies(
    state: &AppState,
    sender_identity: &Identity,
    sender: &store::StoredIdentity,
    sender_vk: &VerifyingKey,
    recipient: &store::StoredIdentity,
    recipient_vk: &VerifyingKey,
    prepared: &Prepared,
    received: store::Message,
    sent: store::Message,
    expires_at: Option<u64>,
    now: u64,
) -> Result<store::DeliveryWrite, ApiError> {
    let sealed = json!({
        "v": 1,
        "msg_ref": prepared.msg_ref,
        "reply_to": prepared.reply_to,
        "edit_of": prepared.edit_of,
        "quote": prepared.quote,
    })
    .to_string();
    let seal = |vk: &VerifyingKey| -> Result<Vec<u8>, ApiError> {
        let wrap = envelope::wrap(sender_identity, vk, &sealed, now)
            .map_err(|_| ApiError::server("seal_error"))?;
        serde_json::to_vec(&wrap).map_err(|_| ApiError::server("encode_error"))
    };
    let row = |id: &str, owner: &str, blob: Option<Vec<u8>>| MessageMeta {
        id: id.to_string(),
        owner: owner.to_string(),
        msg_ref: prepared.msg_ref.clone(),
        reply_to: prepared.reply_to.clone(),
        edit_of: prepared.edit_of.clone(),
        meta_blob: blob,
        ..Default::default()
    };
    let message_id = received.id.clone();
    let rows = [
        row(&received.id, &recipient.address, Some(seal(recipient_vk)?)),
        row(&sent.id, &sender.address, Some(seal(sender_vk)?)),
    ];
    let result = state
        .store
        .enqueue_copies(received, sent, rows, expires_at, prepared.edit_deadline)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "sealed message copies not committed");
            ApiError::server("store_error")
        })?;
    if !matches!(result, store::DeliveryWrite::Stored) {
        return Ok(result);
    }
    if let (Some(root), Some(account)) = (&prepared.edit_of, recipient.account_id.as_deref()) {
        let original = state
            .store
            .rows_by_ref(recipient.address.clone(), root.clone())
            .await
            .ok()
            .and_then(|rows| {
                rows.into_iter()
                    .find(|r| !r.outgoing && r.edit_of.is_none() && r.msg_ref == *root)
                    .map(|r| r.id)
            });
        let data = json!({
            "msg_ref": prepared.msg_ref,
            "edit_of": root,
            "message_id": message_id,
            "original_message_id": original,
            "mailbox": recipient.address,
        });
        if state
            .store
            .record_account_event(
                account.to_string(),
                "message.edited".into(),
                Some(recipient.address.clone()),
                data,
                now,
            )
            .await
            .is_ok()
        {
            state.event_hub.wake(account);
        }
    }
    Ok(result)
}

// ---- reading -----------------------------------------------------------------------------------

/// Which shape of the inbox a client asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct View {
    /// The client understands `edit_of` and `reply_to` (raw bodies).
    pub actions: bool,
    /// Fold edits into their original (people's clients).
    pub fold: bool,
}

impl View {
    pub(crate) fn from_query(actions: Option<&str>, fold: Option<&str>) -> Self {
        let on = |v: Option<&str>| matches!(v, Some("1" | "true"));
        let actions = on(actions);
        View {
            actions,
            fold: actions && on(fold),
        }
    }
}

/// What the viewer may see about receipts on its own sent copies.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ReceiptView {
    pub receipts: bool,
    pub agent_status: bool,
    /// The viewer's own read-receipt setting; off masks seen.
    pub read_receipts: bool,
}

impl ReceiptView {
    pub(crate) async fn for_mailbox(state: &AppState, me: &store::StoredIdentity) -> Self {
        let account = me.account_id.as_deref();
        ReceiptView {
            receipts: feature_on(state, account, RECEIPTS).await,
            agent_status: feature_on(state, account, AGENT_STATUS).await,
            read_receipts: state
                .store
                .read_receipts(me.address.clone())
                .await
                .unwrap_or(false),
        }
    }

    pub(crate) fn annotate(&self, out: &mut Value, meta: &MessageMeta) {
        if self.receipts {
            let seen = meta.seen_at.filter(|_| self.read_receipts);
            let state = if seen.is_some() {
                "seen"
            } else if meta.delivered_at.is_some() {
                "delivered"
            } else {
                "sent"
            };
            out["receipt"] = json!({
                "state": state,
                "delivered_at": meta.delivered_at,
                "seen_at": seen,
            });
        }
        if self.agent_status {
            out["agent_status"] = match (&meta.agent_status, meta.agent_status_at) {
                (Some(s), at) => json!({ "state": s, "at": at }),
                _ => Value::Null,
            };
        }
    }
}

/// The `reply_to` object for a copy whose parent is `parent_ref`.
pub(crate) fn reply_json(
    parent_ref: &str,
    parent: Option<(&str, &str, bool, &str)>,
    quote: Option<String>,
) -> Value {
    match parent {
        Some((id, from, outgoing, body)) => {
            let req = verbs::parse(body);
            json!({
                "msg_ref": parent_ref,
                "message_id": id,
                "from": from,
                "direction": if outgoing { "out" } else { "in" },
                "excerpt": excerpt_of(body),
                "quote": quote,
                "verb": req.as_ref().map(|r| r.verb.clone()),
                "args": req.as_ref().map(|r| r.args.clone()),
            })
        }
        None => json!({
            "msg_ref": parent_ref,
            "message_id": null,
            "from": null,
            "direction": null,
            "excerpt": null,
            "quote": quote,
            "verb": null,
            "args": null,
        }),
    }
}

// ---- receipts ----------------------------------------------------------------------------------

/// Sender details looked up once per batch.
struct SenderFacts {
    account: Option<String>,
    receipts: bool,
    agent_status: bool,
    read_receipts: bool,
    blocked: bool,
}

async fn sender_facts(
    state: &AppState,
    me: &store::StoredIdentity,
    sender: &str,
    cache: &mut HashMap<String, Option<SenderFacts>>,
) -> bool {
    if cache.contains_key(sender) {
        return cache[sender].is_some();
    }
    let facts = match state.store.get(sender.to_string()).await {
        Ok(Some(identity)) => {
            let account = identity.account_id.clone();
            let blocked = state
                .store
                .relationship(
                    me.address.clone(),
                    sender.to_string(),
                    identity.handle.clone(),
                )
                .await
                .map(|r| r.blocked)
                .unwrap_or(true);
            Some(SenderFacts {
                receipts: feature_on(state, account.as_deref(), RECEIPTS).await,
                agent_status: feature_on(state, account.as_deref(), AGENT_STATUS).await,
                read_receipts: state
                    .store
                    .read_receipts(sender.to_string())
                    .await
                    .unwrap_or(false),
                account,
                blocked,
            })
        }
        _ => None,
    };
    let found = facts.is_some();
    cache.insert(sender.to_string(), facts);
    found
}

async fn announce(
    state: &AppState,
    facts: &SenderFacts,
    sender: &str,
    msg_ref: &str,
    copy_id: &str,
    receipt_state: &str,
    now: u64,
) {
    let Some(account) = facts.account.as_deref() else {
        return;
    };
    let data = json!({
        "msg_ref": msg_ref,
        "message_id": copy_id,
        "mailbox": sender,
        "state": receipt_state,
        "at": now,
    });
    if state
        .store
        .record_account_event(
            account.to_string(),
            "message.receipt".into(),
            Some(sender.to_string()),
            data,
            now,
        )
        .await
        .is_ok()
    {
        state.event_hub.wake(account);
    }
}

/// Record delivered or seen for some of `me`'s received copies and pass it to their senders.
/// Returns how many copies changed.
pub(crate) async fn receipt_for_rows(
    state: &AppState,
    me: &store::StoredIdentity,
    ids: Vec<String>,
    kind: ReceiptKind,
) -> usize {
    if ids.is_empty() {
        return 0;
    }
    if kind == ReceiptKind::Seen
        && !state
            .store
            .read_receipts(me.address.clone())
            .await
            .unwrap_or(false)
    {
        return 0;
    }
    let now = now_unix();
    let changed = match state
        .store
        .mark_received(me.address.clone(), ids, kind, now)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "receipt write failed");
            return 0;
        }
    };
    let mut cache = HashMap::new();
    let count = changed.len();
    for (_, msg_ref, sender, filtered) in changed {
        if filtered && kind == ReceiptKind::Seen {
            continue;
        }
        if !sender_facts(state, me, &sender, &mut cache).await {
            continue;
        }
        let facts = cache[&sender].as_ref().expect("checked");
        if facts.blocked || !facts.receipts {
            continue;
        }
        let copy = match state
            .store
            .apply_receipt(sender.clone(), msg_ref.clone(), kind, now)
            .await
        {
            Ok(Some(copy)) => copy,
            _ => continue,
        };
        // A sender who turned read receipts off is not told it was seen, now or later.
        if kind == ReceiptKind::Seen && !facts.read_receipts {
            continue;
        }
        let label = match kind {
            ReceiptKind::Delivered => "delivered",
            ReceiptKind::Seen => "seen",
        };
        announce(state, facts, &sender, &msg_ref, &copy, label, now).await;
    }
    count
}

/// Record an agent's run status for some of `me`'s received copies.
pub(crate) async fn agent_status_for_rows(
    state: &AppState,
    me: &store::StoredIdentity,
    ids: Vec<String>,
    status: &'static str,
) -> usize {
    let now = now_unix();
    let mut cache = HashMap::new();
    let mut count = 0;
    for id in ids {
        let rows = match state
            .store
            .rows_by_ref(me.address.clone(), id.clone())
            .await
        {
            Ok(rows) => rows,
            Err(_) => continue,
        };
        let Some(row) = rows.into_iter().find(|r| r.id == id && !r.outgoing) else {
            continue;
        };
        // Filtered mail never reports more than delivered.
        if row.filtered {
            continue;
        }
        if !sender_facts(state, me, &row.sender, &mut cache).await {
            continue;
        }
        let facts = cache[&row.sender].as_ref().expect("checked");
        if facts.blocked || !facts.agent_status {
            continue;
        }
        // The status of an edit belongs to the request it edits.
        let target = row.edit_of.clone().unwrap_or_else(|| row.msg_ref.clone());
        if let Ok(Some(copy)) = state
            .store
            .apply_agent_status(row.sender.clone(), target.clone(), status, now)
            .await
        {
            count += 1;
            announce(state, facts, &row.sender, &target, &copy, status, now).await;
        }
    }
    count
}

/// A fixed one-minute window per mailbox. In memory: a restart forgets it, which costs at most one
/// extra minute's allowance.
fn allow_receipt_call(mailbox: &str, now: u64) -> bool {
    use std::sync::{Mutex, OnceLock};
    static WINDOWS: OnceLock<Mutex<HashMap<String, (u64, u32)>>> = OnceLock::new();
    let map = WINDOWS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().expect("receipt limiter");
    let window = now / 60;
    if map.len() > 10_000 {
        map.retain(|_, (w, _)| *w == window);
    }
    let entry = map.entry(mailbox.to_string()).or_insert((window, 0));
    if entry.0 != window {
        *entry = (window, 0);
    }
    entry.1 += 1;
    entry.1 <= RECEIPT_CALLS_PER_MINUTE
}

#[derive(serde::Deserialize)]
pub(crate) struct ReceiptsReq {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    message_ids: Vec<String>,
    #[serde(default)]
    msg_refs: Vec<String>,
    kind: String,
    #[serde(default)]
    status: Option<String>,
}

/// `POST /v1/receipts`.
pub(crate) async fn receipts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ReceiptsReq>,
) -> Response {
    let me = match crate::acting_identity(&state, &headers, req.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    if req.message_ids.len() + req.msg_refs.len() > RECEIPT_BATCH {
        return ApiError::bad("too_many", "name at most 100 messages per call").into_response();
    }
    let status: Option<&'static str> = match (req.kind.as_str(), req.status.as_deref()) {
        ("delivered" | "seen", _) => None,
        ("agent_status", Some("working")) => Some("working"),
        ("agent_status", Some("done")) => Some("done"),
        ("agent_status", Some("failed")) => Some("failed"),
        ("agent_status", _) => {
            return ApiError::bad("invalid_status", "status must be working, done or failed")
                .into_response()
        }
        _ => {
            return ApiError::bad(
                "invalid_kind",
                "kind must be delivered, seen or agent_status",
            )
            .into_response()
        }
    };
    if !allow_receipt_call(&me.address, now_unix()) {
        return ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many receipt writes; try again in a minute",
        )
        .with_retry_after(60)
        .into_response();
    }
    let mut ids = req.message_ids.clone();
    for r in &req.msg_refs {
        if let Ok(rows) = state.store.rows_by_ref(me.address.clone(), r.clone()).await {
            ids.extend(
                rows.into_iter()
                    .filter(|row| !row.outgoing && row.msg_ref == *r)
                    .map(|row| row.id),
            );
        }
    }
    ids.sort();
    ids.dedup();
    let updated = match status {
        Some(s) => agent_status_for_rows(&state, &me, ids, s).await,
        None => {
            let kind = if req.kind == "seen" {
                ReceiptKind::Seen
            } else {
                ReceiptKind::Delivered
            };
            receipt_for_rows(&state, &me, ids, kind).await
        }
    };
    Json(json!({ "updated": updated })).into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct InfoQuery {
    #[serde(default)]
    identity: Option<String>,
}

/// `GET /v1/messages/{id}/info` — receipts, versions and reply parent of one of the caller's copies.
pub(crate) async fn info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<InfoQuery>,
) -> Response {
    let me = match crate::acting_identity(&state, &headers, q.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_info(&state, &me, &id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

pub(crate) async fn do_info(
    state: &AppState,
    me: &store::StoredIdentity,
    id: &str,
) -> Result<Value, ApiError> {
    let not_found = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such message in your mailbox",
        )
    };
    let store_err = |_| ApiError::server("store_error");
    let identity = crate::open_identity(state, me)?;
    let rows = state
        .store
        .rows_by_ref(me.address.clone(), id.to_string())
        .await
        .map_err(store_err)?;
    let row = rows
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(not_found)?;
    let root_ref = row.edit_of.clone().unwrap_or_else(|| row.msg_ref.clone());
    let root = state
        .store
        .rows_by_ref(me.address.clone(), root_ref.clone())
        .await
        .map_err(store_err)?
        .into_iter()
        .find(|r| r.edit_of.is_none() && r.msg_ref == root_ref && r.outgoing == row.outgoing);
    let edits = state
        .store
        .edits_of(me.address.clone(), root_ref.clone())
        .await
        .map_err(store_err)?
        .into_iter()
        .filter(|r| r.outgoing == row.outgoing)
        .collect::<Vec<_>>();
    let mut version_rows: Vec<&RefRow> = Vec::new();
    if let Some(r) = &root {
        version_rows.push(r);
    }
    version_rows.extend(edits.iter());
    let wraps = state
        .store
        .wraps_of(
            me.address.clone(),
            version_rows.iter().map(|r| r.id.clone()).collect(),
        )
        .await
        .map_err(store_err)?;
    let versions: Vec<Value> = version_rows
        .iter()
        .filter_map(|r| {
            let body = open_text(&identity, wraps.get(&r.id)?)?;
            Some(json!({ "message_id": r.id, "body": body, "at": r.created_at }))
        })
        .collect();
    let anchor = root.as_ref().unwrap_or(&row);
    let metas = state
        .store
        .meta_for(me.address.clone(), vec![anchor.id.clone()])
        .await
        .map_err(store_err)?;
    let meta = metas.get(&anchor.id).cloned().unwrap_or_default();
    let reply_to = match &meta.reply_to {
        None => Value::Null,
        Some(parent_ref) => {
            let quote = sealed_quote(&identity, &meta);
            let parent = state
                .store
                .rows_by_ref(me.address.clone(), parent_ref.clone())
                .await
                .map_err(store_err)?
                .into_iter()
                .find(|r| r.edit_of.is_none() && r.msg_ref == *parent_ref);
            let body = match &parent {
                Some(p) => state
                    .store
                    .wraps_of(me.address.clone(), vec![p.id.clone()])
                    .await
                    .map_err(store_err)?
                    .get(&p.id)
                    .and_then(|blob| open_text(&identity, blob)),
                None => None,
            };
            match (&parent, &body) {
                (Some(p), Some(b)) => {
                    reply_json(parent_ref, Some((&p.id, &p.sender, p.outgoing, b)), quote)
                }
                _ => reply_json(parent_ref, None, quote),
            }
        }
    };
    let mut out = json!({
        "message_id": row.id,
        "msg_ref": root_ref,
        "direction": if row.outgoing { "out" } else { "in" },
        "sent_at": anchor.created_at,
        "versions": versions,
        "reply_to": reply_to,
        "receipt": null,
        "agent_status": null,
    });
    if anchor.outgoing {
        ReceiptView::for_mailbox(state, me)
            .await
            .annotate(&mut out, &meta);
    }
    Ok(out)
}

/// The ids of every version of `root` in `owner`'s mailbox other than the original, for deleting
/// with it.
pub(crate) async fn versions_to_delete(state: &AppState, owner: &str, id: &str) -> Vec<String> {
    match state
        .store
        .msg_ref_of(owner.to_string(), id.to_string())
        .await
    {
        Ok(Some((msg_ref, None))) => state
            .store
            .edits_of(owner.to_string(), msg_ref)
            .await
            .map(|rows| rows.into_iter().map(|r| r.id).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// ---- groups ------------------------------------------------------------------------------------

/// After a member's client lists a group page, record how far it has fetched.
pub(crate) async fn note_group_page(state: &AppState, group_id: &str, member: &str, body: &Value) {
    let max = body["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i["seq"].as_i64())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    if max > 0 {
        if let Err(e) = state
            .store
            .note_group_delivered(group_id.to_string(), member.to_string(), max)
            .await
        {
            tracing::warn!(error = %e, "group delivered cursor not recorded");
        }
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GroupReceiptsQuery {
    #[serde(default)]
    identity: Option<String>,
}

/// `GET /v1/groups/{id}/receipts`.
pub(crate) async fn group_receipts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<GroupReceiptsQuery>,
) -> Response {
    let ctx = match crate::groups::ctx(&state, &headers, q.identity.as_deref()).await {
        Ok(c) => c,
        Err(e) => return crate::groups::finish(&state, Err(e)),
    };
    if !feature_on(&state, ctx.me.account_id.as_deref(), RECEIPTS).await {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "receipts are not enabled for this account",
        )
        .into_response();
    }
    let me = ctx.address();
    let gid = id.clone();
    let who = me.clone();
    let member = state
        .store
        .group_read(
            move |c: &rusqlite::Connection| -> crate::groups::GResult<()> {
                crate::groups::live_group(c, &gid)?;
                crate::groups::active_member(c, &gid, &who)?;
                Ok(())
            },
        )
        .await;
    if let Err(e) = member {
        return crate::groups::finish(&state, Err(e));
    }
    let mine = state.store.read_receipts(me.clone()).await.unwrap_or(false);
    let rows = match state.store.group_receipt_rows(id, me).await {
        Ok(r) => r,
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    let members: Vec<Value> = rows
        .into_iter()
        .map(|(address, handle, delivered, read, shares)| {
            json!({
                "address": address,
                "handle": handle,
                "delivered_seq": delivered.max(read),
                "seen_seq": if mine && shares { Some(read) } else { None },
            })
        })
        .collect();
    Json(json!({ "members": members, "read_receipts": mine })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_refs_have_one_shape() {
        for _ in 0..100 {
            let r = new_msg_ref();
            assert!(is_msg_ref(&r), "{r}");
        }
        assert!(!is_msg_ref("mref_ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        assert!(!is_msg_ref("mref_abc"));
        assert!(!is_msg_ref("xref_abcdefghijklmnopqrstuvwxyz"));
        assert!(!is_msg_ref("mref_abcdefghijklmnopqrstuvwxy1"));
    }

    #[test]
    fn an_old_client_sees_an_edit_as_a_prefixed_message() {
        assert_eq!(legacy_edit_body("hello"), "Edited: hello");
        let env = r#"{"v":1,"verb":"full_access","args":{"task":"do x"},"note":"do x"}"#;
        let out: Value = serde_json::from_str(&legacy_edit_body(env)).unwrap();
        assert_eq!(out["note"], "Edited: do x");
        assert_eq!(out["args"]["task"], "Edited: do x");
        assert!(verbs::parse(&out.to_string()).is_some());
    }

    #[test]
    fn a_reply_to_prose_gets_a_quote_line_and_a_request_does_not() {
        assert_eq!(
            legacy_reply_body("yes", "are you there?"),
            "> are you there?\n\nyes"
        );
        let env = r#"{"v":1,"verb":"full_access","args":{"task":"x"}}"#;
        assert_eq!(legacy_reply_body(env, "q"), env);
    }

    #[test]
    fn an_excerpt_is_the_first_line_of_the_words() {
        assert_eq!(excerpt_of("\n  first line\nsecond"), "first line");
        let env = r#"{"v":1,"verb":"full_access","args":{"task":"run it"},"note":"please run it"}"#;
        assert_eq!(excerpt_of(env), "please run it");
        let long = "x".repeat(300);
        assert_eq!(excerpt_of(&long).chars().count(), EXCERPT_MAX_CHARS + 1);
    }

    #[test]
    fn the_receipt_limiter_counts_per_mailbox_per_minute() {
        let now = 6_000_000;
        for _ in 0..RECEIPT_CALLS_PER_MINUTE {
            assert!(allow_receipt_call("/k/limiter-test", now));
        }
        assert!(!allow_receipt_call("/k/limiter-test", now));
        assert!(allow_receipt_call("/k/limiter-other", now));
        assert!(allow_receipt_call("/k/limiter-test", now + 60));
    }
}
