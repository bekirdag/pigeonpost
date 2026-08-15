//! C2SP `tlog-witness` service for a Pigeonpost registry log.
//!
//! A witness exists to answer one question that the log operator cannot answer about itself: *has
//! this log only ever grown?* It keeps the last checkpoint it accepted, and cosigns a new one only
//! after checking that the new tree is consistent with that stored one. An operator who quietly
//! rewrote history would have to produce a consistency proof that does not exist.
//!
//! What that guarantee is worth depends entirely on **who runs this**. A witness operated by the
//! same person as the log proves the software works; it does not prove the log was not rewritten,
//! because the same hand holds both keys. That is a deployment property, not something this code
//! can establish — so the threshold and roster live in the registry's configuration, and this
//! binary refuses to be pointed at a log whose origin equals its own name.
//!
//! Every verification here delegates to `pigeonpost-registry`: the note signature to
//! `Checkpoint::verify`, and append-only-ness to `log::verify_consistency`. Reimplementing either
//! would mean a witness that disagrees with the log about what it just signed.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use ed25519_dalek::{SigningKey, VerifyingKey};
use pigeonpost_registry::checkpoint::Checkpoint;
use pigeonpost_registry::log::{verify_consistency, Hash};
use sha2::{Digest, Sha256};

/// The protocol caps a submission; anything larger is a client bug or an attack.
const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// A consistency proof over a tree of any plausible size is far shorter than this.
const MAX_PROOF_HASHES: usize = 63;

#[derive(Clone)]
struct Witness {
    name: String,
    signing_key: SigningKey,
    /// The log this witness observes: its origin string and the key its checkpoints are signed with.
    log_origin: String,
    log_key: VerifyingKey,
    state_path: PathBuf,
    /// Serialises the read-verify-write cycle. Two submissions racing could otherwise both verify
    /// against the same stored checkpoint and both be accepted, which is precisely the split view
    /// a witness exists to make impossible.
    state: Arc<Mutex<Option<Stored>>>,
}

/// The last checkpoint this witness accepted. Everything it will ever cosign must be consistent
/// with this.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Stored {
    size: u64,
    #[serde(with = "hex_hash")]
    root: Hash,
    /// The exact note, so monitoring can serve back what was actually signed rather than a
    /// re-rendering of it.
    note: String,
}

mod hex_hash {
    use super::Hash;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Hash, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
        let text = String::deserialize(d)?;
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16))
            .collect::<Result<_, _>>()
            .map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("stored root is not 32 bytes"))
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Standard base64 with padding, matching what the submitter encodes proof hashes with.
fn b64_decode(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let value = ALPHABET.iter().position(|a| *a == byte)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// `old <n>\n`, then one base64 proof hash per line, then a blank line, then the signed note.
struct Submission {
    old_size: u64,
    proof: Vec<Hash>,
    note: String,
}

fn parse_submission(body: &str) -> Option<Submission> {
    let (head, note) = body.split_once("\n\n")?;
    let mut lines = head.split('\n');
    let old_size: u64 = lines.next()?.strip_prefix("old ")?.parse().ok()?;

    let mut proof = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if proof.len() >= MAX_PROOF_HASHES {
            return None;
        }
        let hash: Hash = b64_decode(line)?.try_into().ok()?;
        proof.push(hash);
    }
    // A first submission has nothing to be consistent with, so it carries no proof. Anything else
    // is a malformed request rather than something to interpret generously.
    if old_size == 0 && !proof.is_empty() {
        return None;
    }
    Some(Submission {
        old_size,
        proof,
        note: note.to_string(),
    })
}

fn text(status: StatusCode, body: impl Into<String>) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.into(),
    )
        .into_response()
}

/// Tell a submitter which size this witness actually holds, so it can fetch the right proof and
/// retry. The content type is what the client keys on, so it is exact.
fn conflict(size: u64) -> Response {
    (
        StatusCode::CONFLICT,
        [(header::CONTENT_TYPE, "text/x.tlog.size")],
        format!("{size}\n"),
    )
        .into_response()
}

async fn add_checkpoint(State(w): State<Witness>, body: Bytes) -> Response {
    if body.len() > MAX_REQUEST_BYTES {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "submission too large\n");
    }
    let Ok(body) = std::str::from_utf8(&body) else {
        return text(StatusCode::BAD_REQUEST, "submission is not UTF-8\n");
    };
    let Some(submission) = parse_submission(body) else {
        return text(StatusCode::BAD_REQUEST, "malformed submission\n");
    };

    // The signature is checked before anything else is believed about the note. Origin, size and
    // root are only meaningful once they are known to have come from the log's own key.
    let checkpoint = match Checkpoint::verify(&submission.note, &w.log_key) {
        Ok(c) => c,
        Err(_) => {
            return text(
                StatusCode::FORBIDDEN,
                "checkpoint signature did not verify\n",
            )
        }
    };
    if checkpoint.origin != w.log_origin {
        return text(StatusCode::FORBIDDEN, "checkpoint is for another log\n");
    }

    let mut guard = w.state.lock().expect("witness state");
    let line = {
        match guard.as_ref() {
            // Trust on first use: a fresh witness has no history to check against, so the first
            // checkpoint it sees becomes its baseline. Every later one is checked against it. This
            // is the one moment a witness cannot detect a rewrite, which is why a witness added
            // long after a log started is worth less than one that watched from the beginning.
            None => {
                if submission.old_size != 0 {
                    return conflict(0);
                }
                tracing::info!(size = checkpoint.size, "first checkpoint observed");
                checkpoint.cosignature_line(&w.name, &w.signing_key, now_unix())
            }
            Some(stored) => {
                // A submitter that disagrees about where this witness is cannot be answered with a
                // cosignature; it has to come back with a proof from the size actually held.
                if submission.old_size != stored.size {
                    return conflict(stored.size);
                }
                // Re-signing the identical checkpoint is legitimate and common: the operator polls,
                // and nothing has been appended since.
                if checkpoint.size == stored.size {
                    if checkpoint.root != stored.root {
                        return text(
                            StatusCode::FORBIDDEN,
                            "tree size unchanged but root differs — the log was rewritten\n",
                        );
                    }
                } else if !verify_consistency(
                    stored.size,
                    &stored.root,
                    checkpoint.size,
                    &checkpoint.root,
                    &submission.proof,
                ) {
                    // The whole point of the service. Refuse loudly: a witness that cosigns here
                    // is worse than no witness, because it certifies a rewrite.
                    tracing::error!(
                        from = stored.size,
                        to = checkpoint.size,
                        "consistency proof failed — refusing to cosign"
                    );
                    return text(StatusCode::FORBIDDEN, "consistency proof did not verify\n");
                }
                checkpoint.cosignature_line(&w.name, &w.signing_key, now_unix())
            }
        }
    };

    let Ok(line) = line else {
        return text(StatusCode::INTERNAL_SERVER_ERROR, "could not cosign\n");
    };

    // Persist before answering. A cosignature handed out for a checkpoint this witness would not
    // remember on restart is a promise it cannot keep.
    let next = Stored {
        size: checkpoint.size,
        root: checkpoint.root,
        note: submission.note.clone(),
    };
    if let Err(e) = save_state(&w.state_path, &next) {
        tracing::error!(error = %e, "could not persist witness state");
        return text(StatusCode::INTERNAL_SERVER_ERROR, "could not persist\n");
    }
    *guard = Some(next);
    drop(guard);

    text(StatusCode::OK, line)
}

async fn monitoring(
    State(w): State<Witness>,
    AxumPath(origin_hash): AxumPath<String>,
    _headers: HeaderMap,
) -> Response {
    let expected: String = Sha256::digest(w.log_origin.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if origin_hash != expected {
        return text(StatusCode::NOT_FOUND, "unknown log\n");
    }
    match w.state.lock().expect("witness state").as_ref() {
        Some(stored) => text(StatusCode::OK, stored.note.clone()),
        None => text(StatusCode::NOT_FOUND, "nothing observed yet\n"),
    }
}

fn save_state(path: &Path, stored: &Stored) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(stored)?)?;
    std::fs::rename(&tmp, path)
}

fn load_state(path: &Path) -> Option<Stored> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn env(key: &str) -> Result<String, String> {
    std::env::var(key).map_err(|_| format!("{key} is required"))
}

/// Read a 32-byte Ed25519 seed from a file holding 64 hex characters.
fn read_signing_key(path: &str) -> Result<SigningKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let text = text.trim();
    if text.len() != 64 {
        return Err(format!(
            "{path}: expected 64 hex characters (a 32-byte seed)"
        ));
    }
    let mut seed = [0u8; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("{path}: not hexadecimal"))?;
    }
    Ok(SigningKey::from_bytes(&seed))
}

fn read_verifying_key(hex: &str) -> Result<VerifyingKey, String> {
    if hex.len() != 64 {
        return Err("log public key must be 64 hex characters".into());
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "log public key is not hexadecimal".to_string())?;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| "log public key is not a valid Ed25519 key".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let name = env("WITNESS_NAME")?;
    let log_origin = env("WITNESS_LOG_ORIGIN")?;
    // A witness whose name is the log's origin is the log signing its own homework. The registry
    // refuses that roster too; refusing it here as well means a misconfiguration cannot be run at
    // all rather than discovered later.
    if name == log_origin {
        return Err("WITNESS_NAME must differ from WITNESS_LOG_ORIGIN".into());
    }
    let log_key = read_verifying_key(&env("WITNESS_LOG_PUBLIC_KEY")?)?;
    let signing_key = read_signing_key(&env("WITNESS_KEY_FILE")?)?;
    let state_path = PathBuf::from(env("WITNESS_STATE_PATH")?);
    let bind: SocketAddr = env("WITNESS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:7720".into())
        .parse()?;

    let restored = load_state(&state_path);
    if let Some(stored) = &restored {
        tracing::info!(size = stored.size, "resumed from stored checkpoint");
    }

    let witness = Witness {
        name: name.clone(),
        signing_key,
        log_origin: log_origin.clone(),
        log_key,
        state_path,
        state: Arc::new(Mutex::new(restored)),
    };

    let app = Router::new()
        .route("/add-checkpoint", post(add_checkpoint))
        .route("/{origin_hash}/checkpoint", get(monitoring))
        .with_state(witness);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%name, %log_origin, %bind, "pigeonpost-witness listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_submission_round_trips_the_shape_the_registry_sends() {
        let note = "origin\n1\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n\n— origin sig\n";
        // A 32-byte hash encodes to 43 characters plus one pad, not 44 raw ones — getting that
        // wrong here would have the test pass against a parser that accepted 33-byte "hashes".
        let hash = format!("{}=", "B".repeat(43));
        let body = format!("old 1\n{hash}\n\n{note}");
        let parsed = parse_submission(&body).expect("parses");
        assert_eq!(parsed.old_size, 1);
        assert_eq!(parsed.proof.len(), 1);
        assert_eq!(parsed.note, note);
    }

    /// A first submission has nothing to prove consistency against, so carrying a proof is a
    /// malformed request rather than something to interpret generously.
    #[test]
    fn a_first_submission_may_not_carry_a_proof() {
        let note = "origin\n1\nAAAA\n\n— origin sig\n";
        let body = format!("old 0\n{}=\n\n{note}", "B".repeat(43));
        assert!(parse_submission(&body).is_none());
    }

    #[test]
    fn an_overlong_proof_is_refused_rather_than_truncated() {
        let note = "origin\n1\nAAAA\n\n— origin sig\n";
        let hashes = vec![format!("{}=", "B".repeat(43)); MAX_PROOF_HASHES + 1].join("\n");
        let body = format!("old 1\n{hashes}\n\n{note}");
        assert!(parse_submission(&body).is_none());
    }

    #[test]
    fn state_survives_a_restart_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let stored = Stored {
            size: 42,
            root: [9u8; 32],
            note: "origin\n42\nroot\n\n— origin sig\n".into(),
        };
        save_state(&path, &stored).unwrap();
        let back = load_state(&path).expect("reloads");
        assert_eq!(back.size, 42);
        assert_eq!(back.root, [9u8; 32]);
        assert_eq!(back.note, stored.note);
    }
}
