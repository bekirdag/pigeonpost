//! Phone numbers on mailboxes, contact discovery and SMS invitations (SDS §14D,
//! docs/contacts-and-phone.md).
//!
//! # What the server holds
//!
//! - A verified number as a keyed **tag** (`HMAC(K_disc, h)[..16]`, where `h` is the client hash
//!   below) for matching and uniqueness, plus the number **sealed** under the vault for key
//!   rotation and the masked display. The full number is never returned, not even to its owner.
//! - Nothing of a discovery upload: hashes are matched in memory. Rate limiting keeps a daily
//!   count per account and a 12-byte keyed mark per checked hash for 7 days.
//! - Invitations for 30 days (code, sender, the invitee's tag); opt-outs as a tag only.
//!
//! # The client hash and its limits
//!
//! `h = SHA-256("pigeonpost:phone:v1:" || E.164)[..16]`. Phone numbers are a small space, so `h`
//! is reversible by anyone who sees it — including this server. The design answer is to keep
//! nothing to reverse, to key everything stored, to make discoverability opt-in and to bound how
//! many numbers any one account can test (SDS §14D.3, D-14D-1/-3).
//!
//! # SMS
//!
//! Codes and invitations go through saas_be's `POST /v1/internal/notifications/sms` (Novu →
//! Twilio). Every send is first reserved in the durable `sms_ledger` inside the transaction that
//! decides it, so the hourly, daily, per-country and invite budgets hold across restarts and
//! concurrent requests. Audit rows hold metadata only: never a number, a hash, a code or a name.

use crate::audit::ActorKind;
use crate::capabilities;
use crate::groups::{GErr, GResult};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::{bearer, now_unix, principal_for_token, AppState, ClientIp, Principal};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use hmac::{Hmac, Mac};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

/// The domain-separation prefix of the client hash. Changing it changes every client.
pub(crate) const HASH_PREFIX: &str = "pigeonpost:phone:v1:";

const TAG_LABEL: &str = "contacts/discovery-tag/v1";
const MARK_LABEL: &str = "contacts/discovery-mark/v1";
const CODE_LABEL: &str = "contacts/phone-code/v1";
const NUMBER_LABEL: &str = "contacts/phone/v1";
const CHALLENGE_LABEL: &str = "contacts/phone-challenge/v1";

// ---- limits (docs/contacts-and-phone.md §3.3, §4, §5) --------------------------------------------

/// Hashes per discovery request.
pub(crate) const DISCOVER_BATCH_MAX: usize = 500;
const DISCOVER_REQUESTS_PER_HOUR: usize = 20;
const DISCOVER_REQUESTS_PER_IP_HOUR: usize = 30;
const DISCOVER_NEW_PER_DAY: i64 = 1_000;
const DISCOVER_NEW_PER_DAY_YOUNG: i64 = 200;
const DISCOVER_NEW_PER_30_DAYS: i64 = 5_000;
/// Days in 30 that may hit the daily cap before discovery locks.
const DISCOVER_CAPPED_DAYS_TO_LOCK: i64 = 3;
const DISCOVER_LOCK_SECS: u64 = 30 * 86_400;
const MARK_TTL: u64 = 7 * 86_400;
const YOUNG_ACCOUNT_SECS: u64 = 86_400;

/// Verified numbers per account.
pub(crate) const MAX_NUMBERS: i64 = 3;
const CODE_TTL: u64 = 600;
const CODE_ATTEMPTS: i64 = 5;
const RESEND_GAP: u64 = 60;
const CODES_PER_NUMBER_DAY: i64 = 5;
const CODES_PER_ACCOUNT_DAY: i64 = 10;
const PENDING_PER_ACCOUNT: i64 = 3;
const CODES_PER_IP_DAY: usize = 20;

const INVITES_PER_SENDER_DAY: i64 = 5;
const INVITES_PER_SENDER_30_DAYS: i64 = 20;
const INVITES_PER_NUMBER_7_DAYS: i64 = 1;
const INVITES_PER_NUMBER_30_DAYS: i64 = 3;
const INVITES_PER_IP_DAY: usize = 20;
const INVITE_TTL: u64 = 30 * 86_400;
const OPT_OUTS_PER_IP_HOUR: usize = 20;

const DAY: u64 = 86_400;

/// Premium-rate, shared-cost and international/satellite network ranges refused by default (digits
/// after the `+`). `POSTBOX_PHONE_BLOCKED_PREFIXES` adds to this list; it never replaces it.
const BUILTIN_BLOCKED: &[&str] = &[
    // International networks, satellite, international freephone/shared cost (no SMS, toll fraud).
    "800", "808", "870", "878", "881", "882", "883", "888", "979",
    // North America premium.
    "1900", "1976", // UK premium, revenue-sharing and personal numbering.
    "449", "4470", "4484", "4487",
    // Germany, France, Italy, Spain, Netherlands, Belgium, Switzerland, Austria, Poland premium /
    // shared cost.
    "49900", "49137", "49180", "3381", "3382", "3389", "39892", "39899", "34803", "34806", "34807",
    "34905", "31900", "31906", "31909", "32900", "32902", "32903", "32904", "32905", "32906",
    "32907", "32909", "41900", "41901", "41906", "43900", "43930", "48700", "48701", "48703",
    // Australia and Türkiye premium / shared-cost.
    "61190", "90444", "90900",
];

/// Two-digit country calling codes (ITU-T E.164). `1` and `7` are one digit; everything else is
/// three. The code space is prefix-free, so this is enough to split any E.164 number.
const TWO_DIGIT_CODES: &[&str] = &[
    "20", "27", "30", "31", "32", "33", "34", "36", "39", "40", "41", "43", "44", "45", "46", "47",
    "48", "49", "51", "52", "53", "54", "55", "56", "57", "58", "60", "61", "62", "63", "64", "65",
    "66", "81", "82", "84", "86", "90", "91", "92", "93", "94", "95", "98",
];

// ---- runtime -------------------------------------------------------------------------------------

/// Platform SMS budgets and the discovery circuit breaker (SDS §14D, D-14D-6).
#[derive(Debug, Clone)]
pub(crate) struct Budgets {
    pub sms_hourly: i64,
    pub sms_daily: i64,
    pub sms_country_daily: i64,
    pub sms_invite_daily: i64,
    pub discover_global_per_hour: i64,
}

impl Default for Budgets {
    fn default() -> Self {
        Budgets {
            sms_hourly: 60,
            sms_daily: 300,
            sms_country_daily: 60,
            sms_invite_daily: 150,
            discover_global_per_hour: 100_000,
        }
    }
}

/// One SMS as handed to the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sms {
    pub to: String,
    pub message: String,
}

/// Where SMS go.
pub(crate) enum Transport {
    /// No SMS credential configured: everything that would send answers `503 sms_unavailable`.
    Disabled,
    /// saas_be's internal notifications API.
    Http {
        url: String,
        key: String,
        client: reqwest::Client,
    },
    /// Tests: keep what would have been sent; optionally fail every send.
    #[cfg_attr(not(test), allow(dead_code))]
    Capture {
        sent: Mutex<Vec<Sms>>,
        fail: std::sync::atomic::AtomicBool,
    },
}

pub(crate) struct Runtime {
    pub(crate) budgets: Budgets,
    pub(crate) transport: Transport,
    blocked: Vec<String>,
    allowed: Vec<String>,
    /// `(hour, new hashes)` across all accounts, for the discovery circuit breaker. In memory: a
    /// restart resets it, which costs at most one hour's ceiling.
    global: Mutex<(u64, i64)>,
}

fn prefixes(var: &str) -> Vec<String> {
    std::env::var(var)
        .unwrap_or_default()
        .split(',')
        .map(|p| p.chars().filter(char::is_ascii_digit).collect::<String>())
        .filter(|p| !p.is_empty())
        .collect()
}

fn env_i64(var: &str, default: i64) -> i64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(default)
}

impl Runtime {
    pub(crate) fn from_env() -> Self {
        let d = Budgets::default();
        let budgets = Budgets {
            sms_hourly: env_i64("POSTBOX_SMS_BUDGET_HOURLY", d.sms_hourly),
            sms_daily: env_i64("POSTBOX_SMS_BUDGET_DAILY", d.sms_daily),
            sms_country_daily: env_i64("POSTBOX_SMS_COUNTRY_DAILY", d.sms_country_daily),
            sms_invite_daily: env_i64("POSTBOX_SMS_INVITE_DAILY", d.sms_invite_daily),
            discover_global_per_hour: env_i64(
                "POSTBOX_DISCOVERY_GLOBAL_HOURLY",
                d.discover_global_per_hour,
            ),
        };
        let url = std::env::var("POSTBOX_SMS_URL")
            .or_else(|_| std::env::var("POSTBOX_NOTIFY_URL"))
            .unwrap_or_default()
            .trim()
            .trim_end_matches('/')
            .to_string();
        let key = std::env::var("POSTBOX_SMS_KEY")
            .unwrap_or_default()
            .trim()
            .to_string();
        let url_ok = url.starts_with("https://") || url.starts_with("http://127.0.0.1");
        let transport = match (url_ok, key.is_empty()) {
            (true, false) => match reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
            {
                Ok(client) => {
                    tracing::info!("SMS configured (saas_be internal notifications)");
                    Transport::Http { url, key, client }
                }
                Err(_) => Transport::Disabled,
            },
            _ => Transport::Disabled,
        };
        Runtime {
            budgets,
            transport,
            blocked: prefixes("POSTBOX_PHONE_BLOCKED_PREFIXES"),
            allowed: prefixes("POSTBOX_PHONE_ALLOWED_PREFIXES"),
            global: Mutex::new((0, 0)),
        }
    }

    /// Tests: capture SMS instead of sending them.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Runtime {
            budgets: Budgets::default(),
            transport: Transport::Capture {
                sent: Mutex::new(Vec::new()),
                fail: std::sync::atomic::AtomicBool::new(false),
            },
            blocked: Vec::new(),
            allowed: Vec::new(),
            global: Mutex::new((0, 0)),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }

    #[cfg(test)]
    pub(crate) fn sent(&self) -> Vec<Sms> {
        match &self.transport {
            Transport::Capture { sent, .. } => sent.lock().expect("sms log").clone(),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_sends(&self, on: bool) {
        if let Transport::Capture { fail, .. } = &self.transport {
            fail.store(on, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn configured(&self) -> bool {
        !matches!(self.transport, Transport::Disabled)
    }

    /// Whether SMS may go to this number at all.
    pub(crate) fn supported(&self, number: &E164) -> bool {
        let d = number.digits.as_str();
        if BUILTIN_BLOCKED.iter().any(|p| d.starts_with(p))
            || self.blocked.iter().any(|p| d.starts_with(p.as_str()))
        {
            return false;
        }
        self.allowed.is_empty() || self.allowed.iter().any(|p| d.starts_with(p.as_str()))
    }

    /// Reserve `n` new hashes in this hour's platform-wide discovery allowance.
    fn reserve_global(&self, n: i64, now: u64) -> bool {
        let mut g = self.global.lock().expect("discovery window");
        let hour = now / 3_600;
        if g.0 != hour {
            *g = (hour, 0);
        }
        if g.1 + n > self.budgets.discover_global_per_hour {
            return false;
        }
        g.1 += n;
        true
    }

    /// Send one SMS. `transaction` is the ledger id (Novu deduplicates on it); `subscriber` is a
    /// keyed id, so Novu never gets the raw number as an identifier.
    async fn send(&self, sms: Sms, transaction: &str, subscriber: &str) -> Result<(), String> {
        match &self.transport {
            Transport::Disabled => Err("sms not configured".into()),
            Transport::Capture { sent, fail } => {
                if fail.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err("capture: failing on purpose".into());
                }
                sent.lock().expect("sms log").push(sms);
                Ok(())
            }
            Transport::Http { url, key, client } => {
                let body = json!({
                    "tenant_id": "pigeonpost",
                    "to": sms.to,
                    "message": sms.message,
                    "workflow_id": "pigeonpost-sms",
                    "transaction_id": transaction,
                    "subscriber_id": subscriber,
                });
                match client
                    .post(format!("{url}/v1/internal/notifications/sms"))
                    .bearer_auth(key)
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => Ok(()),
                    Ok(r) => Err(format!("http {}", r.status().as_u16())),
                    Err(e) => Err(format!("request failed: {}", e.without_url())),
                }
            }
        }
    }
}

// ---- numbers and hashes --------------------------------------------------------------------------

/// A syntactically valid E.164 number: the digits after `+`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct E164 {
    pub digits: String,
}

impl E164 {
    pub(crate) fn text(&self) -> String {
        format!("+{}", self.digits)
    }

    pub(crate) fn country_code(&self) -> &str {
        country_code(&self.digits)
    }
}

/// `+` then 8–15 digits (the first 1–9). Spaces, dots, dashes and parentheses are tolerated;
/// anything else is refused. Normalising national formats is the client's job (contract §2).
pub(crate) fn parse_e164(raw: &str) -> Option<E164> {
    let mut digits = String::new();
    let mut plus = false;
    for ch in raw.trim().chars() {
        match ch {
            '+' if !plus && digits.is_empty() => plus = true,
            '0'..='9' if plus => digits.push(ch),
            ' ' | '-' | '.' | '(' | ')' if plus => {}
            _ => return None,
        }
    }
    (plus && (8..=15).contains(&digits.len()) && !digits.starts_with('0'))
        .then_some(E164 { digits })
}

fn country_code(digits: &str) -> &str {
    if digits.starts_with('1') || digits.starts_with('7') {
        &digits[..1]
    } else if digits.len() >= 2 && TWO_DIGIT_CODES.contains(&&digits[..2]) {
        &digits[..2]
    } else {
        &digits[..3.min(digits.len())]
    }
}

/// `+90 ••• ••• ••12`: the country code and the last two digits, nothing else.
pub(crate) fn masked(number: &str) -> String {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return "•••".into();
    }
    format!(
        "+{} ••• ••• ••{}",
        country_code(&digits),
        &digits[digits.len() - 2..]
    )
}

/// The client hash (contract §3.1).
pub(crate) fn client_hash(e164: &str) -> [u8; 16] {
    let d = Sha256::digest(format!("{HASH_PREFIX}{e164}").as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&d[..16]);
    out
}

fn mac(vault: &crate::vault::Vault, label: &str, parts: &[&[u8]]) -> [u8; 32] {
    let key = vault.subkey(label);
    let mut m = <HmacSha256 as Mac>::new_from_slice(&key).expect("HMAC takes any key length");
    if let [only] = parts {
        m.update(only);
    } else {
        for p in parts {
            m.update(&(p.len() as u32).to_be_bytes());
            m.update(p);
        }
    }
    m.finalize().into_bytes().into()
}

/// The stored, matchable form of a number: `HMAC(K_disc, h)[..16]`.
pub(crate) fn tag(vault: &crate::vault::Vault, h: &[u8; 16]) -> Vec<u8> {
    mac(vault, TAG_LABEL, &[h])[..16].to_vec()
}

/// A per-account rate-limit mark for one checked hash: not comparable across accounts or with
/// the phone table.
fn mark(vault: &crate::vault::Vault, account: &str, h: &[u8; 16]) -> Vec<u8> {
    mac(vault, MARK_LABEL, &[account.as_bytes(), h])[..12].to_vec()
}

fn code_mac(vault: &crate::vault::Vault, challenge: &str, code: &str) -> Vec<u8> {
    mac(vault, CODE_LABEL, &[challenge.as_bytes(), code.as_bytes()]).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hash(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Six uniformly random digits.
fn six_digits() -> String {
    loop {
        let v = rand_core::RngCore::next_u32(&mut rand_core::OsRng);
        if v < 4_294_000_000 {
            return format!("{:06}", v % 1_000_000);
        }
    }
}

const CODE_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// An invitation link code: 12 characters from a 31-letter alphabet (about 59 bits).
fn invite_code() -> String {
    let mut out = String::with_capacity(12);
    while out.len() < 12 {
        let b = (rand_core::RngCore::next_u32(&mut rand_core::OsRng) & 0xff) as u8;
        if (b as usize) < CODE_ALPHABET.len() * 8 {
            out.push(CODE_ALPHABET[(b as usize) % CODE_ALPHABET.len()] as char);
        }
    }
    out
}

fn valid_invite_code(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| CODE_ALPHABET.contains(&b))
}

/// The sender's name as an SMS may carry it: letters, digits, spaces and `.'-`, at most 24
/// characters, nothing that reads as a link or as Pigeonpost itself. Otherwise "A friend".
pub(crate) fn sms_name(name: Option<&str>) -> String {
    let Some(name) = name else {
        return "A friend".into();
    };
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '\'' | '-'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned: String = cleaned.chars().take(24).collect();
    let lower = cleaned.to_lowercase();
    // A dot between two letters or digits ("evil.example") reads as a domain; "J. Smith" does not.
    let chars: Vec<char> = lower.chars().collect();
    let dotted = chars
        .windows(3)
        .any(|w| w[0].is_alphanumeric() && w[1] == '.' && w[2].is_alphanumeric());
    let looks_like_link = lower.contains("www") || lower.contains("http") || dotted;
    if cleaned.chars().filter(|c| c.is_alphabetic()).count() < 2
        || looks_like_link
        || lower.contains("pigeonpost")
    {
        return "A friend".into();
    }
    cleaned.trim().to_string()
}

// ---- shared request plumbing ----------------------------------------------------------------------

fn respond_contacts(r: GResult<Value>) -> Response {
    match r {
        Ok(v) => ([(axum::http::header::CACHE_CONTROL, "no-store")], Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

fn feature_off() -> GErr {
    GErr::new(
        StatusCode::NOT_FOUND,
        "feature_disabled",
        "this feature is not enabled for this account",
    )
}

fn sms_unavailable() -> GErr {
    GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "sms_unavailable",
        "text messages cannot be sent right now; try again later",
    )
}

fn limited(limit: &'static str, retry_after: u64) -> GErr {
    let mut e = GErr::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "too many requests; slow down",
    )
    .with("limit", limit);
    e.retry_after = Some(retry_after.max(1));
    e
}

fn vault_err<E>(_: E) -> GErr {
    GErr::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "vault_error",
        "vault_error",
    )
}

/// The in-memory limiter, with its refusal relabelled.
fn contacts_hit(key: &str, max: usize, window: u64, limit: &'static str) -> GResult<()> {
    crate::groups::rate_limit(key, max, window).map_err(|e| {
        let mut out = limited(limit, e.retry_after.unwrap_or(window));
        out.message = e.message;
        out
    })
}

fn ip_of(client_ip: &Option<Extension<ClientIp>>) -> String {
    client_ip
        .as_ref()
        .map(|Extension(ip)| ip.as_str().to_string())
        .unwrap_or_else(|| "unknown".into())
}

async fn flag_on(state: &AppState, account: &str, name: &'static str) -> bool {
    crate::filtering::feature_on(state, Some(account), name).await
}

/// The person acting, for `action` on their own account: an interactive member session only.
async fn require_person(
    state: &AppState,
    headers: &HeaderMap,
    action: Action,
) -> GResult<(Principal, String)> {
    let principal = principal_for_token(state, bearer(headers)).await?;
    let account = capabilities::principal_account(&principal)
        .unwrap_or_default()
        .to_string();
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Account(&account),
        &Facts {
            now: now_unix(),
            ..Facts::default()
        },
    )
    .map_err(|deny| {
        let status = deny.clone().into_response().status();
        GErr::new(
            status,
            deny.code(),
            "only a person signed in to their own account can do this",
        )
    })?;
    Ok((principal, account))
}

/// The person plus one of their own mailboxes (not an operated agent's).
async fn person_mailbox(
    state: &AppState,
    headers: &HeaderMap,
    identity: Option<&str>,
    action: Action,
) -> GResult<(Principal, String, crate::store::StoredIdentity)> {
    let (principal, account) = require_person(state, headers, action).await?;
    // `person` admits only an interactive member session, so this is always an account principal.
    let Principal::Account(acct, credential) = principal else {
        return Err(GErr::forbidden(
            "credential_not_accepted",
            "a person is required",
        ));
    };
    let view = credential.clone();
    let me = crate::resolve_acting_identity(
        state,
        Principal::Account(acct.clone(), credential),
        identity,
    )
    .await?;
    let principal = Principal::Account(acct, view);
    if me.account_id.as_deref() != Some(account.as_str()) {
        return Err(GErr::forbidden(
            "forbidden",
            "a phone number can only be linked to your own mailbox",
        ));
    }
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Mailbox {
            address: &me.address,
            account: me.account_id.as_deref(),
        },
        &Facts {
            now: now_unix(),
            ..Facts::default()
        },
    )
    .map_err(|deny| GErr::forbidden(deny.code(), "not permitted for this mailbox"))?;
    Ok((principal, account, me))
}

/// Refuse when a platform SMS budget is spent. Counts every reservation, sent or failed.
fn budget_ok(
    c: &rusqlite::Connection,
    b: &Budgets,
    purpose: &str,
    country: &str,
    now: u64,
) -> GResult<()> {
    let now = now as i64;
    let count = |sql: &str, args: &[&dyn rusqlite::ToSql]| -> rusqlite::Result<i64> {
        c.query_row(sql, args, |r| r.get(0))
    };
    let hour = count(
        "SELECT COUNT(*) FROM sms_ledger WHERE created_at > ?1",
        &[&(now - 3_600)],
    )?;
    let day = count(
        "SELECT COUNT(*) FROM sms_ledger WHERE created_at > ?1",
        &[&(now - DAY as i64)],
    )?;
    let country_day = count(
        "SELECT COUNT(*) FROM sms_ledger WHERE created_at > ?1 AND country_code = ?2",
        &[&(now - DAY as i64), &country],
    )?;
    let invite_day = count(
        "SELECT COUNT(*) FROM sms_ledger WHERE created_at > ?1 AND purpose = 'invite'",
        &[&(now - DAY as i64)],
    )?;
    if hour >= b.sms_hourly
        || day >= b.sms_daily
        || country_day >= b.sms_country_daily
        || (purpose == "invite" && invite_day >= b.sms_invite_daily)
    {
        tracing::warn!(purpose, country, "SMS budget exhausted");
        return Err(sms_unavailable());
    }
    Ok(())
}

async fn set_ledger(state: &AppState, id: String, status: &'static str) {
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "UPDATE sms_ledger SET status = ?2 WHERE id = ?1",
                params![id, status],
            )?;
            Ok(())
        })
        .await;
}

fn phone_event(
    c: &rusqlite::Connection,
    account: &str,
    kind: &str,
    data: Value,
    now: u64,
) -> GResult<()> {
    crate::store::append_event(c, account, kind, None, &data, now)?;
    Ok(())
}

// ---- phone numbers -------------------------------------------------------------------------------

fn number_json(
    vault: &crate::vault::Vault,
    id: &str,
    identity: &str,
    sealed: &[u8],
    country: &str,
    discoverable: bool,
    verified_at: i64,
) -> Value {
    let masked_number = vault
        .open_labeled(NUMBER_LABEL, id.as_bytes(), sealed)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|n| masked(&n))
        .unwrap_or_else(|| format!("+{country} •••"));
    json!({
        "id": id,
        "identity": identity,
        "country_code": country,
        "masked": masked_number,
        "discoverable": discoverable,
        "verified_at": verified_at,
    })
}

/// `GET /v1/phone` — the account's verified numbers, masked.
pub(crate) async fn list_numbers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    respond_contacts(list_numbers_inner(&state, &headers).await)
}

async fn list_numbers_inner(state: &AppState, headers: &HeaderMap) -> GResult<Value> {
    let (_, account) = require_person(state, headers, Action::ManagePhone).await?;
    if !flag_on(state, &account, capabilities::PHONE_NUMBERS).await {
        return Err(feature_off());
    }
    let vault = state.vault.clone();
    state
        .store
        .group_read(move |c| -> GResult<Value> {
            let mut stmt = c.prepare(
                "SELECT id, identity, number_sealed, country_code, discoverable, verified_at
                   FROM phone_numbers WHERE account_id = ?1 ORDER BY verified_at",
            )?;
            let numbers = stmt
                .query_map(params![account], |r| {
                    Ok(number_json(
                        &vault,
                        &r.get::<_, String>(0)?,
                        &r.get::<_, String>(1)?,
                        &r.get::<_, Vec<u8>>(2)?,
                        &r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)? != 0,
                        r.get::<_, i64>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "numbers": numbers, "max_numbers": MAX_NUMBERS }))
        })
        .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartReq {
    phone: String,
    #[serde(default)]
    identity: Option<String>,
}

/// `POST /v1/phone/verify` — send a code to a number, for one of the person's mailboxes.
pub(crate) async fn verify_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    client_ip: Option<Extension<ClientIp>>,
    body: Option<Json<StartReq>>,
) -> Response {
    match verify_start_inner(&state, &headers, ip_of(&client_ip), body).await {
        Ok(v) => (
            StatusCode::ACCEPTED,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Json(v),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

async fn verify_start_inner(
    state: &AppState,
    headers: &HeaderMap,
    ip: String,
    body: Option<Json<StartReq>>,
) -> GResult<Value> {
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "expected {phone}"))?;
    let (principal, account, me) =
        person_mailbox(state, headers, req.identity.as_deref(), Action::ManagePhone).await?;
    if !flag_on(state, &account, capabilities::PHONE_NUMBERS).await {
        return Err(feature_off());
    }
    let number = parse_e164(&req.phone).ok_or_else(|| {
        GErr::bad(
            "invalid_phone",
            "phone must be in international format, e.g. +15555550100",
        )
    })?;
    if !state.contacts.supported(&number) {
        return Err(GErr::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "phone_not_supported",
            "this number cannot receive Pigeonpost codes",
        ));
    }
    if !state.contacts.configured() {
        return Err(sms_unavailable());
    }
    contacts_hit(&format!("phone-ip:{ip}"), CODES_PER_IP_DAY, DAY, "ip")?;

    let text = number.text();
    let h = client_hash(&text);
    let tag_v = tag(&state.vault, &h);
    let country = number.country_code().to_string();
    let challenge = format!("pvc_{}", crate::rand_hex(16));
    let code = six_digits();
    let code_mac_v = code_mac(&state.vault, &challenge, &code);
    let sealed = state
        .vault
        .seal_labeled(CHALLENGE_LABEL, challenge.as_bytes(), text.as_bytes())
        .map_err(vault_err)?;
    let ledger = format!("sms_{}", crate::rand_hex(12));
    let now = now_unix();
    let audit = state.auditor.row(
        ActorKind::of(&principal),
        Some(account.clone()),
        "phone.verify_started",
        Some(format!("mailbox:{}", me.address)),
        json!({ "country_code": country }),
        None,
        None,
        now,
    );
    let budgets = state.contacts.budgets.clone();
    let (acct, identity, t, cc, ch, lid) = (
        account.clone(),
        me.address.clone(),
        tag_v.clone(),
        country.clone(),
        challenge.clone(),
        ledger.clone(),
    );
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            let n = now as i64;
            let held: i64 = tx.query_row(
                "SELECT COUNT(*) FROM phone_numbers WHERE account_id = ?1 AND identity <> ?2 AND tag <> ?3",
                params![acct, identity, t],
                |r| r.get(0),
            )?;
            if held >= MAX_NUMBERS {
                return Err(GErr::conflict(
                    "phone_limit",
                    "an account can link at most 3 numbers; unlink one first",
                ));
            }
            let pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM phone_challenges WHERE account_id = ?1 AND expires_at > ?2 AND tag <> ?3",
                params![acct, n, t],
                |r| r.get(0),
            )?;
            if pending >= PENDING_PER_ACCOUNT {
                return Err(limited("pending", CODE_TTL));
            }
            let last: Option<i64> = tx.query_row(
                "SELECT MAX(created_at) FROM sms_ledger WHERE tag = ?1 AND purpose = 'verify'",
                params![t],
                |r| r.get(0),
            )?;
            if let Some(last) = last.filter(|l| *l > n - RESEND_GAP as i64) {
                return Err(limited("resend", (last + RESEND_GAP as i64 - n) as u64));
            }
            let per_number: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sms_ledger WHERE tag = ?1 AND purpose = 'verify' AND created_at > ?2",
                params![t, n - DAY as i64],
                |r| r.get(0),
            )?;
            if per_number >= CODES_PER_NUMBER_DAY {
                return Err(limited("number", DAY));
            }
            let per_account: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sms_ledger WHERE account_id = ?1 AND purpose = 'verify' AND created_at > ?2",
                params![acct, n - DAY as i64],
                |r| r.get(0),
            )?;
            if per_account >= CODES_PER_ACCOUNT_DAY {
                return Err(limited("account", DAY));
            }
            budget_ok(tx, &budgets, "verify", &cc, now)?;
            tx.execute(
                "DELETE FROM phone_challenges WHERE account_id = ?1 AND tag = ?2",
                params![acct, t],
            )?;
            tx.execute(
                "INSERT INTO phone_challenges
                   (id, account_id, identity, tag, number_sealed, country_code, code_mac, attempts, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9)",
                params![ch, acct, identity, t, sealed, cc, code_mac_v, n, n + CODE_TTL as i64],
            )?;
            tx.execute(
                "INSERT INTO sms_ledger (id, purpose, account_id, tag, country_code, status, created_at)
                 VALUES (?1, 'verify', ?2, ?3, ?4, 'pending', ?5)",
                params![lid, acct, t, cc, n],
            )?;
            crate::store::audit::append_in(tx, &audit)?;
            Ok(())
        })
        .await?;

    let sms = Sms {
        to: text,
        message: format!(
            "Pigeonpost: your verification code is {code}. It expires in 10 minutes. Never share it.\n\n@inbox.pigeonpost.dev #{code}"
        ),
    };
    if let Err(e) = state
        .contacts
        .send(sms, &ledger, &format!("pigeonpost:sms:{}", hex(&tag_v)))
        .await
    {
        tracing::warn!(error = %e, "verification SMS not sent");
        set_ledger(state, ledger, "failed").await;
        let ch = challenge.clone();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute("DELETE FROM phone_challenges WHERE id = ?1", params![ch])?;
                Ok(())
            })
            .await;
        return Err(sms_unavailable());
    }
    set_ledger(state, ledger, "sent").await;
    Ok(json!({
        "challenge_id": challenge,
        "expires_at": now + CODE_TTL,
        "resend_after": now + RESEND_GAP,
        "code_length": 6,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckReq {
    code: String,
    #[serde(default)]
    discoverable: bool,
}

/// `(identity, tag, sealed number, country code, code MAC, attempts, expires_at)`.
type ChallengeRow = (String, Vec<u8>, Vec<u8>, String, Vec<u8>, i64, i64);

enum Checked {
    Dead,
    Wrong(i64),
    Verified {
        number: Value,
        previous: Option<(String, String, String)>,
    },
}

fn challenge_expired() -> GErr {
    GErr::new(
        StatusCode::GONE,
        "challenge_expired",
        "this code has expired or was tried too many times; request a new one",
    )
}

/// `POST /v1/phone/verify/{challenge_id}` — prove the code and link the number.
pub(crate) async fn verify_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(challenge): Path<String>,
    body: Option<Json<CheckReq>>,
) -> Response {
    respond_contacts(verify_check_inner(&state, &headers, challenge, body).await)
}

async fn verify_check_inner(
    state: &AppState,
    headers: &HeaderMap,
    challenge: String,
    body: Option<Json<CheckReq>>,
) -> GResult<Value> {
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "expected {code}"))?;
    let (principal, account) = require_person(state, headers, Action::ManagePhone).await?;
    if !flag_on(state, &account, capabilities::PHONE_NUMBERS).await {
        return Err(feature_off());
    }
    if !challenge.starts_with("pvc_") || challenge.len() != 36 {
        return Err(challenge_expired());
    }
    let code: String = req.code.chars().filter(|c| !c.is_whitespace()).collect();
    let given = code_mac(&state.vault, &challenge, &code);
    let vault = state.vault.clone();
    let now = now_unix();
    let kind = ActorKind::of(&principal);
    let auditor = state.auditor.clone();
    let acct = account.clone();
    let discoverable = req.discoverable;
    let outcome = state
        .store
        .group_tx(move |tx| -> GResult<Checked> {
            let n = now as i64;
            let row: Option<ChallengeRow> = tx
                .query_row(
                    "SELECT identity, tag, number_sealed, country_code, code_mac, attempts, expires_at
                       FROM phone_challenges WHERE id = ?1 AND account_id = ?2",
                    params![challenge, acct],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )
                .optional()?;
            let Some((identity, t, sealed, cc, stored_mac, attempts, expires)) = row else {
                return Ok(Checked::Dead);
            };
            if expires <= n || attempts >= CODE_ATTEMPTS {
                tx.execute("DELETE FROM phone_challenges WHERE id = ?1", params![challenge])?;
                return Ok(Checked::Dead);
            }
            // The attempt counts before the comparison, and is committed either way.
            let attempts = attempts + 1;
            tx.execute(
                "UPDATE phone_challenges SET attempts = ?2 WHERE id = ?1",
                params![challenge, attempts],
            )?;
            let ok = code.len() == 6
                && code.bytes().all(|b| b.is_ascii_digit())
                && bool::from(subtle::ConstantTimeEq::ct_eq(
                    given.as_slice(),
                    stored_mac.as_slice(),
                ));
            if !ok {
                if attempts >= CODE_ATTEMPTS {
                    tx.execute("DELETE FROM phone_challenges WHERE id = ?1", params![challenge])?;
                }
                return Ok(Checked::Wrong(CODE_ATTEMPTS - attempts));
            }
            tx.execute("DELETE FROM phone_challenges WHERE id = ?1", params![challenge])?;
            let still_mine: bool = tx
                .query_row(
                    "SELECT 1 FROM identities WHERE address = ?1 AND account_id = ?2",
                    params![identity, acct],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !still_mine {
                return Ok(Checked::Dead);
            }
            let number = vault
                .open_labeled(CHALLENGE_LABEL, challenge.as_bytes(), &sealed)
                .map_err(vault_err)?;
            // Whoever proves the number last holds it (recycled numbers).
            let existing: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT id, account_id, identity FROM phone_numbers WHERE tag = ?1",
                    params![t],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let mut previous = None;
            if let Some((pid, pacct, pident)) = existing {
                tx.execute("DELETE FROM phone_numbers WHERE id = ?1", params![pid])?;
                if pacct != acct {
                    previous = Some((pacct, pident, pid));
                }
            }
            // A mailbox has one number: verifying a new one replaces the old.
            tx.execute(
                "DELETE FROM phone_numbers WHERE identity = ?1",
                params![identity],
            )?;
            let held: i64 = tx.query_row(
                "SELECT COUNT(*) FROM phone_numbers WHERE account_id = ?1",
                params![acct],
                |r| r.get(0),
            )?;
            if held >= MAX_NUMBERS {
                return Err(GErr::conflict(
                    "phone_limit",
                    "an account can link at most 3 numbers; unlink one first",
                ));
            }
            let id = format!("ph_{}", crate::rand_hex(12));
            let resealed = vault
                .seal_labeled(NUMBER_LABEL, id.as_bytes(), &number)
                .map_err(vault_err)?;
            tx.execute(
                "INSERT INTO phone_numbers
                   (id, account_id, identity, tag, number_sealed, country_code, discoverable, verified_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![id, acct, identity, t, resealed, cc, discoverable as i64, n],
            )?;
            let row = auditor.row(
                kind,
                Some(acct.clone()),
                "phone.verified",
                Some(format!("phone:{id}")),
                json!({ "mailbox": identity, "country_code": cc, "discoverable": discoverable }),
                None,
                None,
                now,
            );
            crate::store::audit::append_in(tx, &row)?;
            phone_event(tx, &acct, "phone.verified", json!({ "id": id, "identity": identity }), now)?;
            if let Some((pacct, pident, pid)) = &previous {
                let row = auditor.row(
                    ActorKind::System,
                    Some(pacct.clone()),
                    "phone.unlinked",
                    Some(format!("phone:{pid}")),
                    json!({ "mailbox": pident, "reason": "verified_elsewhere" }),
                    None,
                    None,
                    now,
                );
                crate::store::audit::append_in(tx, &row)?;
                phone_event(
                    tx,
                    pacct,
                    "phone.unlinked",
                    json!({ "id": pid, "identity": pident, "reason": "verified_elsewhere" }),
                    now,
                )?;
            }
            Ok(Checked::Verified {
                number: number_json(&vault, &id, &identity, &resealed, &cc, discoverable, n),
                previous,
            })
        })
        .await?;
    match outcome {
        Checked::Dead => Err(challenge_expired()),
        Checked::Wrong(left) => {
            if left <= 0 {
                Err(challenge_expired())
            } else {
                Err(GErr::bad("invalid_code", "that code is not right").with("attempts_left", left))
            }
        }
        Checked::Verified { number, previous } => {
            state.event_hub.wake(&account);
            if let Some((pacct, _, _)) = previous {
                state.event_hub.wake(&pacct);
            }
            Ok(json!({ "number": number }))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchReq {
    #[serde(default)]
    discoverable: Option<bool>,
    #[serde(default)]
    identity: Option<String>,
}

/// `PATCH /v1/phone/{id}` — discoverability on/off, or move the number to another own mailbox.
pub(crate) async fn update_number(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<PatchReq>>,
) -> Response {
    respond_contacts(update_number_inner(&state, &headers, id, body).await)
}

async fn update_number_inner(
    state: &AppState,
    headers: &HeaderMap,
    id: String,
    body: Option<Json<PatchReq>>,
) -> GResult<Value> {
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "expected a JSON body"))?;
    if req.discoverable.is_none() && req.identity.is_none() {
        return Err(GErr::bad("bad_request", "nothing to change"));
    }
    let (principal, account, target) = match req.identity.as_deref() {
        Some(identity) => {
            let (p, a, me) =
                person_mailbox(state, headers, Some(identity), Action::ManagePhone).await?;
            (p, a, Some(me.address))
        }
        None => {
            let (p, a) = require_person(state, headers, Action::ManagePhone).await?;
            (p, a, None)
        }
    };
    if !flag_on(state, &account, capabilities::PHONE_NUMBERS).await {
        return Err(feature_off());
    }
    let vault = state.vault.clone();
    let now = now_unix();
    let audit = state.auditor.row(
        ActorKind::of(&principal),
        Some(account.clone()),
        "phone.updated",
        Some(format!("phone:{id}")),
        json!({ "discoverable": req.discoverable, "moved": target.is_some() }),
        None,
        None,
        now,
    );
    let acct = account.clone();
    let discoverable = req.discoverable;
    let v = state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let row: Option<(String, Vec<u8>, String, i64, i64)> = tx
                .query_row(
                    "SELECT identity, number_sealed, country_code, discoverable, verified_at
                       FROM phone_numbers WHERE id = ?1 AND account_id = ?2",
                    params![id, acct],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let Some((mut identity, sealed, cc, mut disc, verified_at)) = row else {
                return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "no such number"));
            };
            if let Some(to) = target.filter(|t| *t != identity) {
                let taken: bool = tx
                    .query_row(
                        "SELECT 1 FROM phone_numbers WHERE identity = ?1",
                        params![to],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if taken {
                    return Err(GErr::conflict(
                        "mailbox_has_number",
                        "that mailbox already has a number; unlink it first",
                    ));
                }
                identity = to;
            }
            if let Some(d) = discoverable {
                disc = d as i64;
            }
            tx.execute(
                "UPDATE phone_numbers SET identity = ?2, discoverable = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, identity, disc, now as i64],
            )?;
            crate::store::audit::append_in(tx, &audit)?;
            phone_event(
                tx,
                &acct,
                "phone.updated",
                json!({ "id": id, "identity": identity, "discoverable": disc != 0 }),
                now,
            )?;
            Ok(json!({ "number": number_json(&vault, &id, &identity, &sealed, &cc, disc != 0, verified_at) }))
        })
        .await?;
    state.event_hub.wake(&account);
    Ok(v)
}

/// `DELETE /v1/phone/{id}` — unlink. Works even with the flag off, so nobody is stuck with a link.
pub(crate) async fn remove_number(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    respond_contacts(remove_number_inner(&state, &headers, id).await)
}

async fn remove_number_inner(state: &AppState, headers: &HeaderMap, id: String) -> GResult<Value> {
    let (principal, account) = require_person(state, headers, Action::ManagePhone).await?;
    let now = now_unix();
    let audit = state.auditor.row(
        ActorKind::of(&principal),
        Some(account.clone()),
        "phone.unlinked",
        Some(format!("phone:{id}")),
        json!({ "reason": "removed" }),
        None,
        None,
        now,
    );
    let acct = account.clone();
    let v = state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let identity: Option<String> = tx
                .query_row(
                    "SELECT identity FROM phone_numbers WHERE id = ?1 AND account_id = ?2",
                    params![id, acct],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(identity) = identity else {
                return Err(GErr::new(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "no such number",
                ));
            };
            tx.execute("DELETE FROM phone_numbers WHERE id = ?1", params![id])?;
            crate::store::audit::append_in(tx, &audit)?;
            phone_event(
                tx,
                &acct,
                "phone.unlinked",
                json!({ "id": id, "identity": identity, "reason": "removed" }),
                now,
            )?;
            Ok(json!({ "deleted": true }))
        })
        .await?;
    state.event_hub.wake(&account);
    Ok(v)
}

// ---- discovery -----------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoverReq {
    hashes: Vec<String>,
}

enum Discovered {
    Locked(u64),
    Limited(&'static str, u64),
    Busy,
    Done {
        new: i64,
        remaining: i64,
        /// `(index into the request's hashes, mailbox address)`.
        matches: Vec<(usize, String)>,
    },
}

/// `POST /v1/contacts/discover` — which of these hashed numbers are people who chose to be found.
pub(crate) async fn discover(
    State(state): State<AppState>,
    headers: HeaderMap,
    client_ip: Option<Extension<ClientIp>>,
    body: Option<Json<DiscoverReq>>,
) -> Response {
    respond_contacts(discover_inner(&state, &headers, ip_of(&client_ip), body).await)
}

async fn discover_inner(
    state: &AppState,
    headers: &HeaderMap,
    ip: String,
    body: Option<Json<DiscoverReq>>,
) -> GResult<Value> {
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "expected {hashes}"))?;
    let (principal, account) = require_person(state, headers, Action::DiscoverContacts).await?;
    if !flag_on(state, &account, capabilities::CONTACT_DISCOVERY).await {
        return Err(feature_off());
    }
    if req.hashes.is_empty() || req.hashes.len() > DISCOVER_BATCH_MAX {
        return Err(GErr::bad(
            "bad_request",
            format!("send 1 to {DISCOVER_BATCH_MAX} hashes"),
        )
        .with("batch_max", DISCOVER_BATCH_MAX));
    }
    let mut unique: BTreeMap<[u8; 16], String> = BTreeMap::new();
    for raw in &req.hashes {
        let h = parse_hash(raw)
            .ok_or_else(|| GErr::bad("bad_request", "each hash is 32 lowercase hex characters"))?;
        unique.entry(h).or_insert_with(|| raw.clone());
    }
    contacts_hit(
        &format!("discover:acct:{account}"),
        DISCOVER_REQUESTS_PER_HOUR,
        3_600,
        "requests",
    )?;
    contacts_hit(
        &format!("discover:ip:{ip}"),
        DISCOVER_REQUESTS_PER_IP_HOUR,
        3_600,
        "requests",
    )?;
    let entries: Vec<(String, Vec<u8>, Vec<u8>)> = unique
        .iter()
        .map(|(h, raw)| {
            (
                raw.clone(),
                mark(&state.vault, &account, h),
                tag(&state.vault, h),
            )
        })
        .collect();
    let submitted = entries.len();
    let now = now_unix();
    let kind = ActorKind::of(&principal);
    let auditor = state.auditor.clone();
    let runtime = state.contacts.clone();
    let acct = account.clone();
    let tags: Vec<Vec<u8>> = entries.iter().map(|e| e.2.clone()).collect();
    let marks: Vec<Vec<u8>> = entries.iter().map(|e| e.1.clone()).collect();
    let outcome = state
        .store
        .group_tx(move |tx| -> GResult<Discovered> {
            let n = now as i64;
            let locked: Option<i64> = tx
                .query_row(
                    "SELECT until FROM discovery_lockouts WHERE account_id = ?1 AND until > ?2",
                    params![acct, n],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(until) = locked {
                return Ok(Discovered::Locked(until as u64));
            }
            let mut fresh = Vec::new();
            for m in &marks {
                let seen = tx
                    .query_row(
                        "SELECT 1 FROM discovery_marks WHERE account_id = ?1 AND mark = ?2 AND expires_at > ?3",
                        params![acct, m, n],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !seen {
                    fresh.push(m.clone());
                }
            }
            let new = fresh.len() as i64;
            let day = (now / DAY) as i64;
            let today: i64 = tx
                .query_row(
                    "SELECT new_hashes FROM discovery_usage WHERE account_id = ?1 AND day = ?2",
                    params![acct, day],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            let month: i64 = tx.query_row(
                "SELECT COALESCE(SUM(new_hashes), 0) FROM discovery_usage WHERE account_id = ?1 AND day > ?2",
                params![acct, day - 30],
                |r| r.get(0),
            )?;
            let created: i64 = tx
                .query_row(
                    "SELECT created_at FROM accounts WHERE id = ?1",
                    params![acct],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(n);
            let cap = if now.saturating_sub(created as u64) < YOUNG_ACCOUNT_SECS {
                DISCOVER_NEW_PER_DAY_YOUNG
            } else {
                DISCOVER_NEW_PER_DAY
            };
            let until_tomorrow = (day as u64 + 1) * DAY - now;
            if today + new > cap {
                tx.execute(
                    "INSERT INTO discovery_usage (account_id, day, new_hashes, capped) VALUES (?1, ?2, 0, 1)
                     ON CONFLICT(account_id, day) DO UPDATE SET capped = 1",
                    params![acct, day],
                )?;
                let capped_days: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM discovery_usage WHERE account_id = ?1 AND day > ?2 AND capped = 1",
                    params![acct, day - 30],
                    |r| r.get(0),
                )?;
                if capped_days >= DISCOVER_CAPPED_DAYS_TO_LOCK {
                    let until = now + DISCOVER_LOCK_SECS;
                    tx.execute(
                        "INSERT INTO discovery_lockouts (account_id, until) VALUES (?1, ?2)
                         ON CONFLICT(account_id) DO UPDATE SET until = excluded.until",
                        params![acct, until as i64],
                    )?;
                    let row = auditor.row(
                        ActorKind::System,
                        Some(acct.clone()),
                        "contacts.discovery_locked",
                        Some(format!("account:{acct}")),
                        json!({ "capped_days": capped_days, "until": until }),
                        None,
                        None,
                        now,
                    );
                    crate::store::audit::append_in(tx, &row)?;
                    tracing::warn!("contact discovery locked for an account after repeated caps");
                    return Ok(Discovered::Locked(until));
                }
                return Ok(Discovered::Limited("daily", until_tomorrow));
            }
            if month + new > DISCOVER_NEW_PER_30_DAYS {
                return Ok(Discovered::Limited("monthly", until_tomorrow));
            }
            if new > 0 && !runtime.reserve_global(new, now) {
                tracing::warn!("contact discovery circuit breaker open");
                return Ok(Discovered::Busy);
            }
            for m in &fresh {
                tx.execute(
                    "INSERT INTO discovery_marks (account_id, mark, expires_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(account_id, mark) DO UPDATE SET expires_at = excluded.expires_at",
                    params![acct, m, n + MARK_TTL as i64],
                )?;
            }
            tx.execute(
                "INSERT INTO discovery_usage (account_id, day, new_hashes) VALUES (?1, ?2, ?3)
                 ON CONFLICT(account_id, day) DO UPDATE SET new_hashes = new_hashes + excluded.new_hashes",
                params![acct, day, new],
            )?;
            let mut matches = Vec::new();
            for (i, t) in tags.iter().enumerate() {
                let found: Option<String> = tx
                    .query_row(
                        "SELECT p.identity FROM phone_numbers p
                           JOIN identities i ON i.address = p.identity AND i.account_id = p.account_id
                          WHERE p.tag = ?1 AND p.discoverable = 1 AND p.account_id <> ?2",
                        params![t, acct],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(address) = found {
                    matches.push((i, address));
                }
            }
            let row = auditor.row(
                kind,
                Some(acct.clone()),
                "contacts.discovered",
                Some(format!("account:{acct}")),
                json!({ "submitted": submitted, "new": new, "matched": matches.len() }),
                None,
                None,
                now,
            );
            crate::store::audit::append_in(tx, &row)?;
            Ok(Discovered::Done {
                new,
                remaining: (cap - today - new).max(0),
                matches,
            })
        })
        .await?;
    let (new, remaining, found) = match outcome {
        Discovered::Locked(until) => {
            return Err(GErr::forbidden(
                "discovery_locked",
                "finding contacts is paused for this account",
            )
            .with("until", until))
        }
        Discovered::Limited(limit, retry) => return Err(limited(limit, retry)),
        Discovered::Busy => {
            let mut e = GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "discovery_busy",
                "finding contacts is busy; try again later",
            );
            e.retry_after = Some(600);
            return Err(e);
        }
        Discovered::Done {
            new,
            remaining,
            matches,
        } => (new, remaining, matches),
    };

    // Who may see what: the matched person must not have blocked any of the caller's mailboxes,
    // their mailbox must be open, and their name follows its own visibility.
    let mine: Vec<(String, Option<String>)> = state
        .store
        .list_by_account(account.clone())
        .await
        .map_err(GErr::from)?
        .into_iter()
        .take(50)
        .map(|m| (m.address, m.handle))
        .collect();
    let mut out = Vec::new();
    for (i, address) in found {
        let Some(owner) = crate::resolve_recipient(state, &address).await? else {
            continue;
        };
        let mut blocked = false;
        for (addr, handle) in &mine {
            if state
                .store
                .relationship(owner.address.clone(), addr.clone(), handle.clone())
                .await
                .map(|r| r.blocked)
                .unwrap_or(true)
            {
                blocked = true;
                break;
            }
        }
        if blocked {
            continue;
        }
        let display_name =
            crate::profile_names::visible_name(state, &principal, &mine, &owner).await;
        out.push(json!({
            "hash": entries[i].0,
            "address": owner.address,
            "handle": owner.handle,
            "display_name": display_name,
        }));
    }
    Ok(json!({
        "matches": out,
        "checked": submitted,
        "new": new,
        "remaining_today": remaining,
    }))
}

// ---- invitations ---------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InviteReq {
    phone: String,
    #[serde(default)]
    confirm: bool,
    #[serde(default)]
    identity: Option<String>,
}

enum Invited {
    NotSent(&'static str),
    Send,
}

/// `POST /v1/contacts/invites` — one SMS invitation, on a person's explicit tap.
pub(crate) async fn invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    client_ip: Option<Extension<ClientIp>>,
    body: Option<Json<InviteReq>>,
) -> Response {
    respond_contacts(invite_inner(&state, &headers, ip_of(&client_ip), body).await)
}

async fn invite_inner(
    state: &AppState,
    headers: &HeaderMap,
    ip: String,
    body: Option<Json<InviteReq>>,
) -> GResult<Value> {
    let Json(req) = body.ok_or_else(|| GErr::bad("bad_request", "expected {phone, confirm}"))?;
    let (principal, account, me) = person_mailbox(
        state,
        headers,
        req.identity.as_deref(),
        Action::SendSmsInvite,
    )
    .await?;
    if !flag_on(state, &account, capabilities::SMS_INVITES).await {
        return Err(feature_off());
    }
    if !req.confirm {
        return Err(GErr::bad(
            "confirm_required",
            "an invitation is sent only when the person confirms it",
        ));
    }
    let number = parse_e164(&req.phone).ok_or_else(|| {
        GErr::bad(
            "invalid_phone",
            "phone must be in international format, e.g. +15555550100",
        )
    })?;
    if !state.contacts.configured() {
        return Err(sms_unavailable());
    }
    contacts_hit(&format!("invite-ip:{ip}"), INVITES_PER_IP_DAY, DAY, "ip")?;
    let supported = state.contacts.supported(&number);
    let text = number.text();
    let h = client_hash(&text);
    let tag_v = tag(&state.vault, &h);
    let country = number.country_code().to_string();
    let invite_id = format!("inv_{}", crate::rand_hex(12));
    let code = invite_code();
    let ledger = format!("sms_{}", crate::rand_hex(12));
    let name = crate::profile_names::own_name(state, &me.address).await;
    let now = now_unix();
    let audit = state.auditor.row(
        ActorKind::of(&principal),
        Some(account.clone()),
        "sms.invite_sent",
        Some(format!("invite:{invite_id}")),
        json!({ "country_code": country }),
        None,
        None,
        now,
    );
    let budgets = state.contacts.budgets.clone();
    let (acct, sender, t, cc, iid, c, lid) = (
        account.clone(),
        me.address.clone(),
        tag_v.clone(),
        country.clone(),
        invite_id.clone(),
        code.clone(),
        ledger.clone(),
    );
    let decided = state
        .store
        .group_tx(move |tx| -> GResult<Invited> {
            let n = now as i64;
            let count = |sql: &str, a: &dyn rusqlite::ToSql, since: i64| -> rusqlite::Result<i64> {
                tx.query_row(sql, params![a, since], |r| r.get(0))
            };
            let has_phone = count(
                "SELECT COUNT(*) FROM phone_numbers WHERE account_id = ?1 AND verified_at > ?2",
                &acct,
                -1,
            )?;
            if has_phone == 0 {
                return Err(GErr::forbidden(
                    "phone_required",
                    "verify your own phone number before inviting people by SMS",
                ));
            }
            let sender_day = count(
                "SELECT COUNT(*) FROM sms_invites WHERE sender_account = ?1 AND created_at > ?2",
                &acct,
                n - DAY as i64,
            )?;
            if sender_day >= INVITES_PER_SENDER_DAY {
                return Err(limited("daily", DAY));
            }
            let sender_month = count(
                "SELECT COUNT(*) FROM sms_invites WHERE sender_account = ?1 AND created_at > ?2",
                &acct,
                n - 30 * DAY as i64,
            )?;
            if sender_month >= INVITES_PER_SENDER_30_DAYS {
                return Err(limited("monthly", DAY));
            }
            if !supported {
                return Ok(Invited::NotSent("unavailable"));
            }
            let own: bool = tx
                .query_row(
                    "SELECT 1 FROM phone_numbers WHERE tag = ?1 AND account_id = ?2",
                    params![t, acct],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if own {
                return Ok(Invited::NotSent("unavailable"));
            }
            let pair: bool = tx
                .query_row(
                    "SELECT 1 FROM sms_invites WHERE sender_account = ?1 AND tag = ?2 AND created_at > ?3",
                    params![acct, t, n - 30 * DAY as i64],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if pair {
                return Ok(Invited::NotSent("already_invited"));
            }
            let opted_out: bool = tx
                .query_row(
                    "SELECT 1 FROM sms_optouts WHERE tag = ?1",
                    params![t],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            let week = count(
                "SELECT COUNT(*) FROM sms_invites WHERE tag = ?1 AND created_at > ?2",
                &t,
                n - 7 * DAY as i64,
            )?;
            let month = count(
                "SELECT COUNT(*) FROM sms_invites WHERE tag = ?1 AND created_at > ?2",
                &t,
                n - 30 * DAY as i64,
            )?;
            if opted_out || week >= INVITES_PER_NUMBER_7_DAYS || month >= INVITES_PER_NUMBER_30_DAYS
            {
                return Ok(Invited::NotSent("unavailable"));
            }
            budget_ok(tx, &budgets, "invite", &cc, now)?;
            tx.execute(
                "INSERT INTO sms_invites (id, code, sender_account, sender_identity, tag, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![iid, c, acct, sender, t, n, n + INVITE_TTL as i64],
            )?;
            tx.execute(
                "INSERT INTO sms_ledger (id, purpose, account_id, tag, country_code, status, created_at)
                 VALUES (?1, 'invite', ?2, ?3, ?4, 'pending', ?5)",
                params![lid, acct, t, cc, n],
            )?;
            crate::store::audit::append_in(tx, &audit)?;
            Ok(Invited::Send)
        })
        .await?;
    if let Invited::NotSent(reason) = decided {
        return Ok(json!({ "status": "not_sent", "reason": reason }));
    }
    let sms = Sms {
        to: text,
        message: format!(
            "Pigeonpost: {} invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c={code} Reply STOP to opt out.",
            sms_name(name.as_deref())
        ),
    };
    if let Err(e) = state
        .contacts
        .send(sms, &ledger, &format!("pigeonpost:sms:{}", hex(&tag_v)))
        .await
    {
        tracing::warn!(error = %e, "invitation SMS not sent");
        set_ledger(state, ledger, "failed").await;
        let iid = invite_id.clone();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute("DELETE FROM sms_invites WHERE id = ?1", params![iid])?;
                Ok(())
            })
            .await;
        return Err(sms_unavailable());
    }
    set_ledger(state, ledger, "sent").await;
    Ok(json!({ "status": "sent", "invite_id": invite_id }))
}

#[derive(Deserialize)]
pub(crate) struct OptOutReq {
    #[serde(default)]
    code: String,
}

/// `POST /v1/sms/opt-out` — public: stop invitations to the number an invitation code was sent to.
/// Always the same answer, so a code reveals nothing.
pub(crate) async fn opt_out(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    body: Option<Json<OptOutReq>>,
) -> Response {
    let ip = ip_of(&client_ip);
    if let Err(e) = contacts_hit(
        &format!("optout-ip:{ip}"),
        OPT_OUTS_PER_IP_HOUR,
        3_600,
        "ip",
    ) {
        return e.into_response();
    }
    let code = body.map(|Json(b)| b.code).unwrap_or_default();
    // The code's shape is the proof checked here; whether it names an invitation is answered
    // nowhere, so every caller gets the same reply.
    let proof = Facts {
        now: now_unix(),
        device_proof: valid_invite_code(&code),
        ..Facts::default()
    };
    if authorize(
        &Actor::Anonymous,
        Action::SmsOptOut,
        &Resource::Platform,
        &proof,
    )
    .is_ok()
    {
        let now = now_unix();
        let audit = state.auditor.row(
            ActorKind::System,
            None,
            "sms.opt_out",
            None,
            json!({}),
            None,
            None,
            now,
        );
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let t: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT tag FROM sms_invites WHERE code = ?1",
                        params![code],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(t) = t {
                    tx.execute(
                        "INSERT OR IGNORE INTO sms_optouts (tag, created_at) VALUES (?1, ?2)",
                        params![t, now as i64],
                    )?;
                    tx.execute("DELETE FROM sms_invites WHERE tag = ?1", params![t])?;
                    crate::store::audit::append_in(tx, &audit)?;
                }
                Ok(())
            })
            .await;
    }
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

/// Reaper hook: retention for every contacts table.
pub(crate) async fn purge(store: &crate::store::Store, now: u64) {
    match store.sweep_contacts(now).await {
        Ok(s) if s.total() > 0 => tracing::info!(
            challenges = s.challenges,
            marks = s.marks,
            usage = s.usage,
            invites = s.invites,
            ledger = s.ledger,
            lockouts = s.lockouts,
            "contacts retention swept"
        ),
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "contacts retention failed"),
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn client_hash_matches_the_contract_vectors() {
        for (n, h) in [
            ("+15555550100", "7c0dc19d3eae376a630cee358d0f8912"),
            ("+447700900123", "5c4fd5bb0f19345b15863f865cb76cee"),
            ("+61491570156", "511ebc743c8a1d1ea8be5541f2fee95d"),
        ] {
            assert_eq!(hex(&client_hash(n)), h, "{n}");
            assert_eq!(parse_hash(h), Some(client_hash(n)));
        }
        assert_eq!(parse_hash("7C0DC19D3EAE376A630CEE358D0F8912"), None);
        assert_eq!(parse_hash("7c0d"), None);
    }

    #[test]
    fn e164_parsing_and_country_codes() {
        let p = |s| parse_e164(s).map(|n| n.text());
        assert_eq!(p("+1 (555) 555-0100").as_deref(), Some("+15555550100"));
        assert_eq!(p("+44 7700 900123").as_deref(), Some("+447700900123"));
        for bad in [
            "15555550100",
            "+0123456789",
            "+1234567",
            "+1234567890123456",
            "+1555abc0100",
            "++15555550100",
            "1+5555550100",
        ] {
            assert_eq!(p(bad), None, "{bad}");
        }
        assert_eq!(country_code("15555550100"), "1");
        assert_eq!(country_code("79001234567"), "7");
        assert_eq!(country_code("905321234567"), "90");
        assert_eq!(country_code("447700900123"), "44");
        assert_eq!(country_code("35312345678"), "353");
        assert_eq!(country_code("8801712345678"), "880");
        assert_eq!(masked("+905321234567"), "+90 ••• ••• ••67");
    }

    #[test]
    fn premium_and_network_prefixes_are_refused() {
        let rt = Runtime::for_test();
        let ok = |s: &str| rt.supported(&parse_e164(s).unwrap());
        assert!(ok("+15555550100"));
        assert!(ok("+447700900123"));
        assert!(!ok("+19005550100"));
        assert!(!ok("+881612345678"));
        assert!(!ok("+449012345678"));
        assert!(!ok("+909001234567"));
    }

    #[test]
    fn keyed_values_differ_by_purpose_and_account() {
        let v = crate::vault::Vault::new([7u8; 32]);
        let h = client_hash("+15555550100");
        assert_eq!(tag(&v, &h).len(), 16);
        assert_ne!(tag(&v, &h), mark(&v, "acct_a", &h)[..12].to_vec());
        assert_ne!(mark(&v, "acct_a", &h), mark(&v, "acct_b", &h));
        assert_ne!(
            tag(&v, &h),
            tag(&crate::vault::Vault::new([8u8; 32]), &h),
            "a different key gives unrelated tags"
        );
        assert_ne!(
            code_mac(&v, "pvc_a", "123456"),
            code_mac(&v, "pvc_b", "123456")
        );
    }

    #[test]
    fn codes_and_invite_codes_have_the_right_shape() {
        for _ in 0..200 {
            let c = six_digits();
            assert_eq!(c.len(), 6);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
            let i = invite_code();
            assert!(valid_invite_code(&i), "{i}");
        }
        assert!(!valid_invite_code("short"));
        assert!(
            !valid_invite_code("abcdefghijk1"),
            "1 is not in the alphabet"
        );
    }

    #[test]
    fn sms_names_are_plain() {
        assert_eq!(sms_name(Some("Bekir Dağ")), "Bekir Dağ");
        assert_eq!(sms_name(None), "A friend");
        assert_eq!(sms_name(Some("evil.example.com")), "A friend");
        assert_eq!(sms_name(Some("http://x")), "A friend");
        assert_eq!(sms_name(Some("Pigeonpost Team")), "A friend");
        assert_eq!(sms_name(Some("!!")), "A friend");
        assert_eq!(
            sms_name(Some("A very long name that goes on and on"))
                .chars()
                .count(),
            24
        );
        assert_eq!(sms_name(Some("Ann  <b>Lee</b>")), "Ann bLeeb");
        assert_eq!(sms_name(Some("J. Smith")), "J. Smith");
    }
}
