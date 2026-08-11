//! `pigeonpost loft submit` and `pigeonpost loft drain` — authenticated pool lifecycle.
//!
//! The directory accepts one strictly increasing sequence shared by submissions and drains. A
//! signed mutation is therefore persisted before network I/O. If the response is lost, the next
//! invocation sends the exact same signed object; it never guesses whether the directory committed
//! and never reuses that sequence for different content.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use pigeonpost_directory::{DirectoryEntry, DrainAuthorization, Health, LoftPolicy, LoftState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(all(unix, not(target_os = "redox")))]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, OpenAccess,
};

const MAX_SERVICE_URL_BYTES: usize = 2_048;
const MAX_OPERATOR_BYTES: usize = 256;
const MAX_MUTATION_BODY_BYTES: usize = 32 * 1024;
const MAX_STATE_BYTES: u64 = 64 * 1024;
const STATE_VERSION: u8 = 1;
#[cfg(all(unix, not(target_os = "redox")))]
const STATE_DIRECTORY: &str = ".pigeonpost-directory-mutations";

#[derive(Debug, Clone)]
struct DirectoryTarget {
    origin: String,
    submit_url: reqwest::Url,
    drain_url: reqwest::Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MutationTuple {
    directory_origin: String,
    endpoint: String,
    loft_pubkey: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationState {
    version: u8,
    directory_origin: String,
    endpoint: String,
    loft_pubkey: String,
    committed_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingMutation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum PendingMutation {
    Submit {
        request_url: String,
        entry: DirectoryEntry,
    },
    Drain {
        request_url: String,
        authorization: DrainAuthorization,
    },
}

impl PendingMutation {
    fn sequence(&self) -> u64 {
        match self {
            Self::Submit { entry, .. } => entry.sequence,
            Self::Drain { authorization, .. } => authorization.sequence,
        }
    }
}

struct MutationStore {
    storage: SecureMutationStorage,
    state: MutationState,
    state_exists: bool,
    key: SigningKey,
}

impl MutationStore {
    fn open(
        loft_dir: &Path,
        tuple: MutationTuple,
        key: SigningKey,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let state_id = mutation_state_id(&tuple);
        let storage = SecureMutationStorage::open(loft_dir, &state_id)?;
        let loaded = storage.read_state()?;
        let state_exists = loaded.is_some();
        let state = loaded.unwrap_or_else(|| MutationState {
            version: STATE_VERSION,
            directory_origin: tuple.directory_origin.clone(),
            endpoint: tuple.endpoint.clone(),
            loft_pubkey: tuple.loft_pubkey.clone(),
            committed_sequence: 0,
            pending: None,
        });
        validate_state(&state, &tuple, &key)?;
        storage.remove_stale_temp()?;
        Ok(Self {
            storage,
            state,
            state_exists,
            key,
        })
    }

    fn next_sequence(&self) -> Result<u64, Box<dyn std::error::Error>> {
        self.state
            .committed_sequence
            .checked_add(1)
            .ok_or_else(|| "directory mutation sequence is exhausted".into())
    }

    fn stage(&mut self, pending: PendingMutation) -> Result<(), Box<dyn std::error::Error>> {
        if self.state.pending.is_some() {
            return Err("an unresolved directory mutation must be retried exactly first".into());
        }
        if pending.sequence() != self.next_sequence()? {
            return Err("directory mutation sequence is not the next durable value".into());
        }
        let mut candidate = self.state.clone();
        candidate.pending = Some(pending);
        let tuple = self.tuple();
        validate_state(&candidate, &tuple, &self.key)?;
        self.persist(&candidate)?;
        self.state = candidate;
        Ok(())
    }

    fn accept_pending(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
        let sequence = self
            .state
            .pending
            .as_ref()
            .ok_or("there is no staged directory mutation to accept")?
            .sequence();
        let mut candidate = self.state.clone();
        candidate.committed_sequence = sequence;
        candidate.pending = None;
        let tuple = self.tuple();
        validate_state(&candidate, &tuple, &self.key)?;
        self.persist(&candidate)?;
        self.state = candidate;
        Ok(sequence)
    }

    fn persist(&mut self, state: &MutationState) -> Result<(), Box<dyn std::error::Error>> {
        let encoded = serde_json::to_vec(state)?;
        if encoded.len() as u64 > MAX_STATE_BYTES {
            return Err("directory mutation state exceeded its fixed bound".into());
        }
        self.storage.persist(&encoded, self.state_exists)?;
        self.state_exists = true;
        Ok(())
    }

    fn tuple(&self) -> MutationTuple {
        MutationTuple {
            directory_origin: self.state.directory_origin.clone(),
            endpoint: self.state.endpoint.clone(),
            loft_pubkey: self.state.loft_pubkey.clone(),
        }
    }
}

pub async fn submit(
    dir: &Path,
    directory_url: &str,
    endpoint: &str,
    operator: Option<String>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_operator(operator.as_deref())?;
    let target = directory_target(directory_url)?;
    let endpoint = loft_endpoint(endpoint)?;
    let seed = crate::loft_key::load_existing_seed(&dir.join("loft.key"))
        .map_err(|_| "no valid owner-only loft key — run `pigeonpost install` first")?;
    let key = SigningKey::from_bytes(&seed);
    let tuple = MutationTuple {
        directory_origin: target.origin.clone(),
        endpoint: endpoint.clone(),
        loft_pubkey: pigeonpost_directory::entry::hex(key.verifying_key().as_bytes()),
    };
    let mut store = MutationStore::open(dir, tuple, key)?;

    let (request_url, entry, exact_retry) = match store.state.pending.as_ref() {
        Some(PendingMutation::Submit { request_url, entry }) => {
            if request_url != target.submit_url.as_str() || entry.operator != operator {
                return Err(
                    "the pending submission differs; retry with the exact directory and operator"
                        .into(),
                );
            }
            (request_url.clone(), entry.clone(), true)
        }
        Some(PendingMutation::Drain { .. }) => {
            return Err("a pending drain must be retried exactly before another submission".into());
        }
        None => {
            // Ask the loft itself rather than trusting flags: advertised values must be the ones it
            // will honor, or directory weighting is built on fiction.
            let loft = match pigeonpost_loft::LoftClient::new(&endpoint) {
                Ok(local) => local,
                Err(_) => pigeonpost_loft::LoftClient::new_untrusted(&endpoint).await?,
            };
            let info = loft.info().await?;
            let live_pubkey = pigeonpost_directory::entry::parse_hex32(&info.pubkey)
                .ok_or("loft returned a malformed public key")?;
            if live_pubkey != store.key.verifying_key().to_bytes() {
                return Err("loft public key does not match the local owner key".into());
            }
            const GIB_BYTES: u64 = 1024 * 1024 * 1024;
            if info.capacity_bytes == 0 || info.capacity_bytes % GIB_BYTES != 0 {
                return Err("loft capacity must be an exact nonzero GiB value".into());
            }
            let entry = DirectoryEntry::signed_with_sequence(
                &store.key,
                &endpoint,
                operator,
                info.capacity_bytes / GIB_BYTES,
                info.retention_days,
                LoftPolicy {
                    open: info.open,
                    pow_floor: info.pow_floor,
                    max_event_bytes: info.max_event_bytes,
                },
                info.utilization,
                store.next_sequence()?,
            );
            let request_url = target.submit_url.to_string();
            store.stage(PendingMutation::Submit {
                request_url: request_url.clone(),
                entry: entry.clone(),
            })?;
            (request_url, entry, false)
        }
    };

    #[derive(Serialize)]
    struct SubmitRequest<'a> {
        entry: &'a DirectoryEntry,
    }

    let status = post_json(&request_url, &SubmitRequest { entry: &entry }).await?;
    if !status.is_success() {
        return Err(format!(
            "directory refused the submission ({status}); the exact mutation remains pending"
        )
        .into());
    }
    let sequence = store.accept_pending()?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "endpoint": endpoint,
                "directory": target.origin,
                "state": "pending",
                "sequence": sequence,
                "exact_retry": exact_retry,
            })
        );
    } else {
        println!(
            "submitted {endpoint} to {} at sequence {sequence}",
            target.origin
        );
        if exact_retry {
            println!("The previously staged signed submission was accepted exactly.");
        }
        println!();
        println!(
            "State is `pending`. The directory will probe this loft and promote it only after"
        );
        println!("24 continuous hours of clean measurements.");
    }
    Ok(())
}

pub async fn drain(
    dir: &Path,
    directory_url: &str,
    endpoint: &str,
    after_utc: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = directory_target(directory_url)?;
    let endpoint = loft_endpoint(endpoint)?;
    let after = parse_utc_deadline(after_utc)?;
    let seed = crate::loft_key::load_existing_seed(&dir.join("loft.key"))
        .map_err(|_| "no valid owner-only loft key — run `pigeonpost install` first")?;
    let key = SigningKey::from_bytes(&seed);
    let tuple = MutationTuple {
        directory_origin: target.origin.clone(),
        endpoint: endpoint.clone(),
        loft_pubkey: pigeonpost_directory::entry::hex(key.verifying_key().as_bytes()),
    };
    let mut store = MutationStore::open(dir, tuple, key)?;

    let (request_url, authorization, exact_retry) = match store.state.pending.as_ref() {
        Some(PendingMutation::Drain {
            request_url,
            authorization,
        }) => {
            if request_url != target.drain_url.as_str() || authorization.after != after {
                return Err(
                    "the pending drain differs; retry with the exact directory and UTC deadline"
                        .into(),
                );
            }
            (request_url.clone(), authorization.clone(), true)
        }
        Some(PendingMutation::Submit { .. }) => {
            return Err(
                "a pending submission must be retried exactly before announcing a drain".into(),
            );
        }
        None => {
            if after <= now_secs()? {
                return Err("drain deadline must be an absolute future UTC instant".into());
            }
            let authorization =
                DrainAuthorization::signed(&store.key, &endpoint, after, store.next_sequence()?);
            let request_url = target.drain_url.to_string();
            store.stage(PendingMutation::Drain {
                request_url: request_url.clone(),
                authorization: authorization.clone(),
            })?;
            (request_url, authorization, false)
        }
    };

    let status = post_json(&request_url, &authorization).await?;
    if !status.is_success() {
        return Err(format!(
            "directory refused the drain ({status}); the exact authorization remains pending"
        )
        .into());
    }
    let sequence = store.accept_pending()?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "endpoint": endpoint,
                "directory": target.origin,
                "state": "draining",
                "after": after,
                "after_utc": after_utc,
                "sequence": sequence,
                "exact_retry": exact_retry,
            })
        );
    } else {
        println!("announced drain for {endpoint} at {after_utc} (sequence {sequence})");
        if exact_retry {
            println!("The previously staged signed drain was accepted exactly.");
        }
        println!("Keep the loft serving reads through the full announced deadline.");
    }
    Ok(())
}

async fn post_json<T: Serialize>(
    request_url: &str,
    payload: &T,
) -> Result<reqwest::StatusCode, Box<dyn std::error::Error>> {
    let url = reqwest::Url::parse(request_url).map_err(|_| "stored mutation URL is invalid")?;
    let body = serde_json::to_vec(payload)?;
    if body.len() > MAX_MUTATION_BODY_BYTES {
        return Err("directory mutation exceeded its fixed request bound".into());
    }
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .pool_max_idle_per_host(1)
        .user_agent(concat!("pigeonpost-cli/", env!("CARGO_PKG_VERSION")));
    let response = builder
        .build()?
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await?;
    Ok(response.status())
}

fn directory_target(input: &str) -> Result<DirectoryTarget, Box<dyn std::error::Error>> {
    let base = service_base(input, false)?;
    let origin = base.origin().ascii_serialization();
    let submit_url = base
        .join("v1/directory/submit")
        .map_err(|_| "invalid directory submission URL")?;
    let drain_url = base
        .join("v1/directory/drain")
        .map_err(|_| "invalid directory drain URL")?;
    Ok(DirectoryTarget {
        origin,
        submit_url,
        drain_url,
    })
}

fn loft_endpoint(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let url = service_base(input, false)?;
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn service_base(input: &str, allow_path: bool) -> Result<reqwest::Url, Box<dyn std::error::Error>> {
    if input.is_empty() || input.len() > MAX_SERVICE_URL_BYTES {
        return Err("service URL is empty or too long".into());
    }
    let url = reqwest::Url::parse(input).map_err(|_| "invalid service URL")?;
    if url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port() == Some(0)
        || (!allow_path && !matches!(url.path(), "" | "/"))
    {
        return Err("service URL must be an origin without credentials or selectors".into());
    }
    let host = url.host_str().ok_or("service URL has no host")?;
    if pigeonpost_core::network::is_localhost_name(host) {
        return Err("service URL cannot use localhost names".into());
    }
    if url.scheme() != "https"
        && !(url.scheme() == "http" && pigeonpost_core::network::is_numeric_loopback_host(host))
    {
        return Err("service URL must use HTTPS (HTTP is loopback-only)".into());
    }
    Ok(url)
}

fn validate_operator(operator: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    if operator.is_some_and(|value| {
        value.is_empty()
            || value.len() > MAX_OPERATOR_BYTES
            || value.bytes().any(|byte| byte.is_ascii_control())
    }) {
        return Err("operator handle is empty, too long, or contains control bytes".into());
    }
    Ok(())
}

fn parse_utc_deadline(value: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return Err("drain deadline must use YYYY-MM-DDTHH:MM:SSZ".into());
    }
    let year = parse_decimal(&bytes[..4])? as i64;
    let month = parse_decimal(&bytes[5..7])?;
    let day = parse_decimal(&bytes[8..10])?;
    let hour = parse_decimal(&bytes[11..13])?;
    let minute = parse_decimal(&bytes[14..16])?;
    let second = parse_decimal(&bytes[17..19])?;
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err("drain deadline is not a valid UTC instant".into());
    }
    let days = u64::try_from(days_from_civil(year, month, day))?;
    days.checked_mul(86_400)
        .and_then(|value| value.checked_add(u64::from(hour) * 3_600))
        .and_then(|value| value.checked_add(u64::from(minute) * 60))
        .and_then(|value| value.checked_add(u64::from(second)))
        .ok_or_else(|| "drain deadline overflowed".into())
}

fn parse_decimal(bytes: &[u8]) -> Result<u32, Box<dyn std::error::Error>> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err("UTC deadline contains a non-decimal field".into());
    }
    bytes.iter().try_fold(0u32, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit - b'0')))
            .ok_or_else(|| "UTC deadline field overflowed".into())
    })
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn now_secs() -> Result<u64, Box<dyn std::error::Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn mutation_state_id(tuple: &MutationTuple) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"pigeonpost/directory-mutation-state/v1");
    for field in [
        tuple.directory_origin.as_bytes(),
        tuple.endpoint.as_bytes(),
        tuple.loft_pubkey.as_bytes(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    pigeonpost_directory::entry::hex(&hasher.finalize())
}

fn validate_state(
    state: &MutationState,
    tuple: &MutationTuple,
    key: &SigningKey,
) -> Result<(), Box<dyn std::error::Error>> {
    if state.version != STATE_VERSION
        || state.directory_origin != tuple.directory_origin
        || state.endpoint != tuple.endpoint
        || state.loft_pubkey != tuple.loft_pubkey
        || state.loft_pubkey != pigeonpost_directory::entry::hex(key.verifying_key().as_bytes())
        || state.directory_origin.len() > MAX_SERVICE_URL_BYTES
        || state.endpoint.len() > MAX_SERVICE_URL_BYTES
    {
        return Err("directory mutation state does not match this loft and origin".into());
    }
    if let Some(pending) = &state.pending {
        let expected = state
            .committed_sequence
            .checked_add(1)
            .ok_or("directory mutation sequence is exhausted")?;
        if pending.sequence() != expected {
            return Err("pending directory mutation has a non-contiguous sequence".into());
        }
        match pending {
            PendingMutation::Submit { request_url, entry } => {
                validate_stored_request(request_url, &state.directory_origin, "submit")?;
                let verified = entry.verify()?;
                if verified.to_bytes() != key.verifying_key().to_bytes()
                    || entry.endpoint != state.endpoint
                    || entry.pubkey != state.loft_pubkey
                    || entry.state != LoftState::Pending
                    || entry.health != Health::default()
                    || entry.drain_after.is_some()
                    || entry.last_mutation_sequence != 0
                    || entry.capacity_gb == 0
                    || entry.retention_days == 0
                    || entry.policy.max_event_bytes == 0
                    || entry.policy.max_event_bytes > 2 * 1024 * 1024
                    || !entry.utilization.is_finite()
                    || !(0.0..=1.0).contains(&entry.utilization)
                {
                    return Err("persisted directory submission is malformed".into());
                }
            }
            PendingMutation::Drain {
                request_url,
                authorization,
            } => {
                validate_stored_request(request_url, &state.directory_origin, "drain")?;
                if authorization.endpoint != state.endpoint || authorization.after == 0 {
                    return Err("persisted directory drain is malformed".into());
                }
                authorization.verify(&key.verifying_key())?;
            }
        }
    }
    Ok(())
}

fn validate_stored_request(
    request_url: &str,
    expected_origin: &str,
    operation: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = service_base(request_url, true)?;
    let expected_suffix = format!("/v1/directory/{operation}");
    if url.origin().ascii_serialization() != expected_origin || url.path() != expected_suffix {
        return Err("persisted directory request target is malformed".into());
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "redox")))]
struct SecureMutationStorage {
    directory: GuardedDir,
    state_name: LeafName,
    temp_name: LeafName,
    lock: GuardedFile,
    expected_state: std::sync::Mutex<Option<GuardedFile>>,
}

#[cfg(all(unix, not(target_os = "redox")))]
impl SecureMutationStorage {
    fn open(loft_dir: &Path, state_id: &str) -> io::Result<Self> {
        use fs2::FileExt;

        let directory = secure_state_directory(loft_dir)?;
        let state_name = LeafName::new(format!("{state_id}.json")).map_err(custody_state_error)?;
        let temp_name = LeafName::new(format!(".{state_id}.tmp")).map_err(custody_state_error)?;
        let lock_name = LeafName::new(format!("{state_id}.lock")).map_err(custody_state_error)?;
        let lock = directory
            .open_or_create_file(
                &lock_name,
                OpenAccess::ReadWrite,
                FilePolicy::private_exact(0),
            )
            .map_err(custody_state_error)?;
        if lock.metadata().map_err(custody_state_error)?.mode & 0o7777 != 0o600 {
            return Err(private_state_error(
                "mutation lock file mode must be exactly 0600",
            ));
        }
        lock.verify_named().map_err(custody_state_error)?;
        lock.file().try_lock_exclusive().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "another directory lifecycle mutation is already in progress",
            )
        })?;
        lock.verify_named().map_err(custody_state_error)?;
        Ok(Self {
            directory,
            state_name,
            temp_name,
            lock,
            expected_state: std::sync::Mutex::new(None),
        })
    }

    fn read_state(&self) -> io::Result<Option<MutationState>> {
        use std::io::Read;

        let mut expected = self
            .expected_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut file = match self
            .directory
            .open_file_optional(
                &self.state_name,
                OpenAccess::ReadOnly,
                FilePolicy::private(MAX_STATE_BYTES),
            )
            .map_err(custody_state_error)?
        {
            Some(file) => file,
            None => {
                *expected = None;
                return Ok(None);
            }
        };
        let metadata = file.metadata().map_err(custody_state_error)?;
        if metadata.mode & 0o7777 != 0o600 {
            return Err(private_state_error(
                "mutation state file mode must be exactly 0600",
            ));
        }
        let capacity = usize::try_from(metadata.len.min(MAX_STATE_BYTES))
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        let mut encoded = Vec::with_capacity(capacity);
        Read::by_ref(&mut file)
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut encoded)?;
        file.verify_named().map_err(custody_state_error)?;
        if encoded.len() as u64 > MAX_STATE_BYTES {
            return Err(private_state_error("state file exceeded its fixed bound"));
        }
        let state = serde_json::from_slice(&encoded)
            .map_err(|_| private_state_error("state file is malformed"))?;
        *expected = Some(file);
        Ok(Some(state))
    }

    fn remove_stale_temp(&self) -> io::Result<()> {
        let file = self
            .directory
            .open_file_optional(
                &self.temp_name,
                OpenAccess::ReadWrite,
                FilePolicy::private(MAX_STATE_BYTES),
            )
            .map_err(custody_state_error)?;
        if let Some(file) = file {
            if file.metadata().map_err(custody_state_error)?.mode & 0o7777 != 0o600 {
                return Err(private_state_error(
                    "mutation temporary file mode must be exactly 0600",
                ));
            }
            self.directory
                .unlink_file(file)
                .map_err(custody_state_error)?;
        }
        Ok(())
    }

    fn persist(&self, encoded: &[u8], expected_existing: bool) -> io::Result<()> {
        use std::io::Write;

        if encoded.len() as u64 > MAX_STATE_BYTES {
            return Err(private_state_error("state file exceeded its fixed bound"));
        }
        self.remove_stale_temp()?;
        let mut file = self
            .directory
            .create_file(&self.temp_name, FilePolicy::private(MAX_STATE_BYTES))
            .map_err(custody_state_error)?;
        let result = (|| -> io::Result<()> {
            if file.metadata().map_err(custody_state_error)?.mode & 0o7777 != 0o600 {
                return Err(private_state_error(
                    "mutation temporary file mode must be exactly 0600",
                ));
            }
            file.write_all(encoded)?;
            file.sync_all().map_err(custody_state_error)?;
            file.verify_named().map_err(custody_state_error)?;

            run_before_state_commit_hook();
            let mut expected = self
                .expected_state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.verify_expected_state(expected.as_ref(), expected_existing)?;
            let published = self
                .directory
                .rename_replace(file, &self.directory, &self.state_name)
                .map_err(custody_state_error)?;
            published.verify_named().map_err(custody_state_error)?;
            *expected = Some(published);
            Ok(())
        })();
        if result.is_err() {
            let cleanup = self.remove_stale_temp();
            self.directory.verify_named().map_err(custody_state_error)?;
            cleanup?;
        }
        result
    }

    fn verify_expected_state(
        &self,
        retained: Option<&GuardedFile>,
        expected_existing: bool,
    ) -> io::Result<()> {
        match (retained, expected_existing) {
            (Some(file), true) => file.verify_named().map_err(custody_state_error),
            (None, false) => {
                let appeared = self
                    .directory
                    .open_file_optional(
                        &self.state_name,
                        OpenAccess::ReadOnly,
                        FilePolicy::private(MAX_STATE_BYTES),
                    )
                    .map_err(custody_state_error)?;
                if appeared.is_some() {
                    Err(private_state_error(
                        "state file appeared during a locked mutation",
                    ))
                } else {
                    Ok(())
                }
            }
            (Some(_), false) => Err(private_state_error(
                "state file unexpectedly existed before mutation",
            )),
            (None, true) => Err(private_state_error(
                "state file disappeared during a locked mutation",
            )),
        }
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
impl Drop for SecureMutationStorage {
    fn drop(&mut self) {
        use fs2::FileExt;
        let _ = FileExt::unlock(self.lock.file());
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
fn secure_state_directory(loft_dir: &Path) -> io::Result<GuardedDir> {
    let root =
        GuardedDir::open_existing(loft_dir, DirPolicy::trusted()).map_err(custody_state_error)?;
    let state_path = root.absolute_path().join(STATE_DIRECTORY);
    let directory = GuardedDir::create_private(&state_path).map_err(custody_state_error)?;
    root.verify_named().map_err(custody_state_error)?;
    directory.verify_named().map_err(custody_state_error)?;
    Ok(directory)
}

#[cfg(not(all(unix, not(target_os = "redox"))))]
struct SecureMutationStorage;

#[cfg(not(all(unix, not(target_os = "redox"))))]
impl SecureMutationStorage {
    fn open(_loft_dir: &Path, _state_id: &str) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "secure directory mutation persistence is unavailable on this platform",
        ))
    }

    fn read_state(&self) -> io::Result<Option<MutationState>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "unavailable"))
    }

    fn remove_stale_temp(&self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "unavailable"))
    }

    fn persist(&self, _encoded: &[u8], _expected_existing: bool) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "unavailable"))
    }
}

#[cfg(all(unix, not(target_os = "redox")))]
fn private_state_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(all(unix, not(target_os = "redox")))]
fn custody_state_error(error: CustodyError) -> io::Error {
    match error {
        error @ CustodyError::NotFound => io::Error::new(io::ErrorKind::NotFound, error),
        error @ CustodyError::AlreadyExists => io::Error::new(io::ErrorKind::AlreadyExists, error),
        CustodyError::Io(error) => error,
        error => io::Error::new(io::ErrorKind::PermissionDenied, error),
    }
}

#[cfg(all(test, unix, not(target_os = "redox")))]
type BeforeStateCommitHook = Box<dyn FnOnce()>;

#[cfg(all(test, unix, not(target_os = "redox")))]
thread_local! {
    static BEFORE_STATE_COMMIT_HOOK: std::cell::RefCell<Option<BeforeStateCommitHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix, not(target_os = "redox")))]
fn install_before_state_commit_hook(hook: BeforeStateCommitHook) {
    BEFORE_STATE_COMMIT_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
}

#[cfg(all(test, unix, not(target_os = "redox")))]
fn run_before_state_commit_hook() {
    BEFORE_STATE_COMMIT_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(all(not(test), unix, not(target_os = "redox")))]
fn run_before_state_commit_hook() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_os = "redox")))]
    fn tuple(key: &SigningKey) -> MutationTuple {
        MutationTuple {
            directory_origin: "http://127.0.0.1:7719".into(),
            endpoint: "http://127.0.0.1:7717".into(),
            loft_pubkey: pigeonpost_directory::entry::hex(key.verifying_key().as_bytes()),
        }
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    fn entry(key: &SigningKey, sequence: u64) -> DirectoryEntry {
        DirectoryEntry::signed_with_sequence(
            key,
            "http://127.0.0.1:7717",
            Some("/github/operator".into()),
            20,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 2 * 1024 * 1024,
            },
            0.25,
            sequence,
        )
    }

    #[test]
    fn directory_mutations_use_bounded_https_or_exact_loopback_targets() {
        let target = directory_target("https://directory.example/").unwrap();
        assert_eq!(
            target.submit_url.as_str(),
            "https://directory.example/v1/directory/submit"
        );
        assert_eq!(
            target.drain_url.as_str(),
            "https://directory.example/v1/directory/drain"
        );
        assert_eq!(target.origin, "https://directory.example");
        assert!(directory_target("http://127.0.0.1:7719").is_ok());
        assert!(directory_target("http://[::1]:7719").is_ok());
        assert!(directory_target("http://localhost:7719").is_err());
        assert!(directory_target("http://localhost.:7719").is_err());
        assert!(directory_target("http://localhost.evil:7719").is_err());
        assert!(directory_target("https://localhost:7719").is_err());
        assert!(directory_target("https://api.localhost:7719").is_err());
        assert!(directory_target("https://API.LOCALHOST.:7719").is_err());
        assert!(directory_target("http://192.0.2.1:7719").is_err());
        assert!(directory_target("https://user@directory.example").is_err());
        assert!(directory_target("https://directory.example/base/").is_err());
        assert!(directory_target("https://directory.example?raw=selector").is_err());
        assert!(loft_endpoint("https://loft.example/path").is_err());
        assert!(loft_endpoint("https://api.localhost:7717").is_err());
        assert!(validate_stored_request(
            "https://directory.example/v1/directory/submit",
            "https://directory.example",
            "submit"
        )
        .is_ok());
        assert!(validate_stored_request(
            "https://directory.example/prefix/v1/directory/submit",
            "https://directory.example",
            "submit"
        )
        .is_err());
    }

    #[test]
    fn utc_deadline_parser_is_strict_and_calendar_correct() {
        assert_eq!(
            parse_utc_deadline("2030-01-01T00:00:00Z").unwrap(),
            1_893_456_000
        );
        assert!(parse_utc_deadline("2028-02-29T23:59:59Z").is_ok());
        assert!(parse_utc_deadline("2027-02-29T00:00:00Z").is_err());
        assert!(parse_utc_deadline("2030-01-01T00:00:00+00:00").is_err());
        assert!(parse_utc_deadline("2030-01-01t00:00:00z").is_err());
        assert!(parse_utc_deadline("2030-01-01T00:00:60Z").is_err());
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[test]
    fn staged_submit_survives_restart_and_drain_uses_the_same_sequence() {
        let dir = crate::test_support::private_tempdir();
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let request_url = "http://127.0.0.1:7719/v1/directory/submit".to_string();
        {
            let mut store = MutationStore::open(dir.path(), tuple(&key), key.clone()).unwrap();
            store
                .stage(PendingMutation::Submit {
                    request_url: request_url.clone(),
                    entry: entry(&key, 1),
                })
                .unwrap();
        }
        {
            let mut store = MutationStore::open(dir.path(), tuple(&key), key.clone()).unwrap();
            assert_eq!(
                store.state.pending,
                Some(PendingMutation::Submit {
                    request_url,
                    entry: entry(&key, 1),
                })
            );
            assert_eq!(store.accept_pending().unwrap(), 1);
            let authorization =
                DrainAuthorization::signed(&key, "http://127.0.0.1:7717", 1_893_456_000, 2);
            store
                .stage(PendingMutation::Drain {
                    request_url: "http://127.0.0.1:7719/v1/directory/drain".into(),
                    authorization: authorization.clone(),
                })
                .unwrap();
            assert_eq!(store.state.pending.unwrap().sequence(), 2);
        }
        let mut store = MutationStore::open(dir.path(), tuple(&key), key).unwrap();
        assert_eq!(store.accept_pending().unwrap(), 2);
        assert_eq!(store.state.committed_sequence, 2);
        assert!(store.state.pending.is_none());
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[test]
    fn unresolved_mutation_cannot_be_replaced_or_skip_a_sequence() {
        let dir = crate::test_support::private_tempdir();
        let key = SigningKey::from_bytes(&[8u8; 32]);
        let mut store = MutationStore::open(dir.path(), tuple(&key), key.clone()).unwrap();
        store
            .stage(PendingMutation::Submit {
                request_url: "http://127.0.0.1:7719/v1/directory/submit".into(),
                entry: entry(&key, 1),
            })
            .unwrap();
        let drain = PendingMutation::Drain {
            request_url: "http://127.0.0.1:7719/v1/directory/drain".into(),
            authorization: DrainAuthorization::signed(
                &key,
                "http://127.0.0.1:7717",
                1_893_456_000,
                2,
            ),
        };
        assert!(store.stage(drain).is_err());
        assert_eq!(store.state.pending.unwrap().sequence(), 1);
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[tokio::test]
    async fn lost_http_response_retries_the_exact_signed_drain() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn request_body(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
            let mut request = Vec::new();
            let (head_end, content_length) = loop {
                let mut chunk = [0u8; 2_048];
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "request ended before its headers");
                request.extend_from_slice(&chunk[..read]);
                assert!(request.len() <= MAX_MUTATION_BODY_BYTES + 8 * 1024);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head_end = index + 4;
                    let head = std::str::from_utf8(&request[..index]).unwrap();
                    let content_length = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        .expect("bounded JSON POST carries a content length");
                    break (head_end, content_length);
                }
            };
            while request.len() < head_end + content_length {
                let mut chunk = [0u8; 2_048];
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "request ended before its body");
                request.extend_from_slice(&chunk[..read]);
                assert!(request.len() <= MAX_MUTATION_BODY_BYTES + 8 * 1024);
            }
            request[head_end..head_end + content_length].to_vec()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let directory_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                bodies.push(request_body(&mut stream).await);
                if attempt == 1 {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .await
                        .unwrap();
                }
            }
            bodies
        });

        let dir = crate::test_support::private_tempdir();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let seed = crate::loft_key::load_or_create_seed(&dir.path().join("loft.key"))
            .unwrap()
            .0;
        let key = SigningKey::from_bytes(&seed);
        let deadline = "2099-01-01T00:00:00Z";
        assert!(drain(
            dir.path(),
            &directory_url,
            "http://127.0.0.1:7717",
            deadline,
            true,
        )
        .await
        .is_err());
        drain(
            dir.path(),
            &directory_url,
            "http://127.0.0.1:7717",
            deadline,
            true,
        )
        .await
        .unwrap();

        let bodies = server.await.unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0], bodies[1]);
        let authorization: DrainAuthorization = serde_json::from_slice(&bodies[0]).unwrap();
        assert_eq!(authorization.sequence, 1);
        assert_eq!(authorization.after, 4_070_908_800);
        authorization.verify(&key.verifying_key()).unwrap();

        let target = directory_target(&directory_url).unwrap();
        let state = MutationStore::open(
            dir.path(),
            MutationTuple {
                directory_origin: target.origin,
                endpoint: "http://127.0.0.1:7717".into(),
                loft_pubkey: pigeonpost_directory::entry::hex(key.verifying_key().as_bytes()),
            },
            key,
        )
        .unwrap();
        assert_eq!(state.state.committed_sequence, 1);
        assert!(state.state.pending.is_none());
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[test]
    fn lifecycle_state_is_owner_only_and_rejects_links_or_unsafe_modes() {
        use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

        let dir = crate::test_support::private_tempdir();
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let mutation_tuple = tuple(&key);
        let state_id = mutation_state_id(&mutation_tuple);
        {
            let mut store =
                MutationStore::open(dir.path(), mutation_tuple.clone(), key.clone()).unwrap();
            store
                .stage(PendingMutation::Submit {
                    request_url: "http://127.0.0.1:7719/v1/directory/submit".into(),
                    entry: entry(&key, 1),
                })
                .unwrap();
        }
        let state_dir = dir.path().join(STATE_DIRECTORY);
        let state_path = state_dir.join(format!("{state_id}.json"));
        assert_eq!(
            std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let hardlink = state_dir.join("extra-link");
        std::fs::hard_link(&state_path, &hardlink).unwrap();
        assert!(MutationStore::open(dir.path(), mutation_tuple.clone(), key.clone()).is_err());
        std::fs::remove_file(&hardlink).unwrap();

        std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(MutationStore::open(dir.path(), mutation_tuple.clone(), key.clone()).is_err());
        std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let original = state_dir.join("original-state");
        std::fs::rename(&state_path, &original).unwrap();
        symlink(&original, &state_path).unwrap();
        assert!(MutationStore::open(dir.path(), mutation_tuple, key).is_err());
        assert_eq!(std::fs::metadata(original).unwrap().nlink(), 1);
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[test]
    fn lifecycle_storage_rejects_lexical_escape_links_and_mutable_ancestors_without_effects() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = crate::test_support::private_tempdir();
        let state_id = "a".repeat(64);
        let escaped = root.path().join("escaped");
        let lexical = root
            .path()
            .join("would-be-created")
            .join("..")
            .join("escaped");
        assert!(SecureMutationStorage::open(&lexical, &state_id).is_err());
        assert!(!root.path().join("would-be-created").exists());
        assert!(!escaped.exists());

        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.path().join("alias");
        symlink(&real, &alias).unwrap();
        assert!(SecureMutationStorage::open(&alias, &state_id).is_err());
        assert!(!real.join(STATE_DIRECTORY).exists());

        let mutable = root.path().join("mutable");
        let loft = mutable.join("loft");
        std::fs::create_dir(&mutable).unwrap();
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::create_dir(&loft).unwrap();
        std::fs::set_permissions(&loft, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(SecureMutationStorage::open(&loft, &state_id).is_err());
        assert!(!loft.join(STATE_DIRECTORY).exists());
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[test]
    fn retained_destination_rejects_replacement_immediately_before_commit() {
        use std::os::unix::fs::PermissionsExt;

        let dir = crate::test_support::private_tempdir();
        let key = SigningKey::from_bytes(&[10u8; 32]);
        let mutation_tuple = tuple(&key);
        let state_id = mutation_state_id(&mutation_tuple);
        {
            let mut store =
                MutationStore::open(dir.path(), mutation_tuple.clone(), key.clone()).unwrap();
            store
                .stage(PendingMutation::Submit {
                    request_url: "http://127.0.0.1:7719/v1/directory/submit".into(),
                    entry: entry(&key, 1),
                })
                .unwrap();
        }

        let mut store = MutationStore::open(dir.path(), mutation_tuple, key).unwrap();
        let state_dir = dir.path().join(STATE_DIRECTORY);
        let state_path = state_dir.join(format!("{state_id}.json"));
        let displaced = state_dir.join("displaced.json");
        let hook_state = state_path.clone();
        let hook_displaced = displaced.clone();
        install_before_state_commit_hook(Box::new(move || {
            std::fs::rename(&hook_state, &hook_displaced).unwrap();
            std::fs::write(&hook_state, b"replacement").unwrap();
            std::fs::set_permissions(&hook_state, std::fs::Permissions::from_mode(0o600)).unwrap();
        }));

        assert!(store.accept_pending().is_err());
        assert_eq!(std::fs::read(&state_path).unwrap(), b"replacement");
        assert!(displaced.exists());
        assert!(!state_dir.join(format!(".{state_id}.tmp")).exists());
    }

    #[cfg(all(unix, not(target_os = "redox")))]
    #[test]
    fn retained_destination_rejects_disappearance_immediately_before_commit() {
        let dir = crate::test_support::private_tempdir();
        let key = SigningKey::from_bytes(&[11u8; 32]);
        let mutation_tuple = tuple(&key);
        let state_id = mutation_state_id(&mutation_tuple);
        {
            let mut store =
                MutationStore::open(dir.path(), mutation_tuple.clone(), key.clone()).unwrap();
            store
                .stage(PendingMutation::Submit {
                    request_url: "http://127.0.0.1:7719/v1/directory/submit".into(),
                    entry: entry(&key, 1),
                })
                .unwrap();
        }

        let mut store = MutationStore::open(dir.path(), mutation_tuple, key).unwrap();
        let state_dir = dir.path().join(STATE_DIRECTORY);
        let state_path = state_dir.join(format!("{state_id}.json"));
        let hook_state = state_path.clone();
        install_before_state_commit_hook(Box::new(move || {
            std::fs::remove_file(&hook_state).unwrap();
        }));

        assert!(store.accept_pending().is_err());
        assert!(!state_path.exists());
        assert!(!state_dir.join(format!(".{state_id}.tmp")).exists());
    }
}
