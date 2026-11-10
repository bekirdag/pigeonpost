//! Marketplace M2b: source connections (build plan R-62).
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET\|POST /v1/publishers/{publisher}/sources` | `PublisherManage` | read: the team; register: a certified publisher's admins |
//! | `GET\|DELETE /v1/publishers/{publisher}/sources/{source}` | `PublisherManage` | read: the team; delete: the admins |
//! | `GET /internal/v1/marketplace/build-requests/{build}/source-credential` | ops service token | the build plane, for a queued build only |
//!
//! A source is a public `https` repository, or a private one the build host reads with a
//! **deploy key Pigeonpost generates**: an Ed25519 key whose public half the publisher adds to the
//! repository read-only, and whose private half is sealed with the vault (bound to its row) and
//! never returned by a public route. The build plane fetches it for a queued build that names the
//! source, over the internal API only. Deleting a source wipes the sealed key; builds already made
//! keep their pinned commit. Every repository host must be on `POSTBOX_MARKETPLACE_GIT_HOSTS`.

use crate::groups::{GErr, GResult};
use crate::marketplace::{into_response, iso, not_found, parse};
use crate::marketplace_m1::{flag_gate, service_gate};
use crate::marketplace_m2::{certified, gate, new_id, text, Need};
use crate::policy::Action;
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

/// Vault label for deploy-key seeds.
const KEY_LABEL: &str = "marketplace-source-key-v1";
/// Live sources one publisher may hold.
const MAX_SOURCES: i64 = 50;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publishers/{publisher}/sources",
            get(list_sources).post(create_source),
        )
        .route(
            "/v1/publishers/{publisher}/sources/{source}",
            get(read_source).delete(delete_source),
        )
}

pub(crate) fn internal_router() -> Router<AppState> {
    Router::new().route(
        "/internal/v1/marketplace/build-requests/{build}/source-credential",
        get(source_credential),
    )
}

/// The host of a repository URL: `https://host/path`, `ssh://git@host/path` or `git@host:path`.
pub(crate) fn repository_host(url: &str) -> Option<String> {
    let host = if let Some(rest) = url.strip_prefix("https://") {
        rest.split('/').next()?
    } else if let Some(rest) = url.strip_prefix("ssh://git@") {
        rest.split('/').next()?
    } else if let Some(rest) = url.strip_prefix("git@") {
        rest.split(':').next()?
    } else {
        return None;
    };
    let host = host.to_ascii_lowercase();
    crate::marketplace_env::host_ok(&host).then_some(host)
}

/// `403 git_host_not_allowed` unless the repository's host is on the allowlist.
pub(crate) fn host_allowed(state: &AppState, url: &str) -> GResult<String> {
    let host = repository_host(url)
        .ok_or_else(|| GErr::bad("invalid_request", "source.repository: no host"))?;
    if !state.nests.mp.git_host_allowed(&host) {
        return Err(GErr::forbidden(
            "git_host_not_allowed",
            format!("builds do not fetch from {host}"),
        ));
    }
    Ok(host)
}

/// An ssh repository the build host can clone with a deploy key: `ssh://git@host/owner/repo[.git]`
/// or `git@host:owner/repo[.git]`, no port, no options.
pub(crate) fn ssh_repository_ok(url: &str) -> bool {
    let path = if let Some(rest) = url.strip_prefix("ssh://git@") {
        rest.split_once('/').map(|(_, p)| p)
    } else if let Some(rest) = url.strip_prefix("git@") {
        rest.split_once(':').map(|(_, p)| p)
    } else {
        None
    };
    let Some(path) = path else {
        return false;
    };
    url.len() <= 300
        && !path.is_empty()
        && !path.starts_with('-')
        && !path.starts_with('/')
        && !path.contains("..")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
}

/// The repository of a live source of this publisher, for a build request.
pub(crate) fn active_repository_in(
    c: &Connection,
    publisher: &str,
    source: &str,
) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT repository FROM publisher_sources WHERE id = ?1 AND publisher_id = ?2 AND status = 'active'",
        params![source, publisher],
        |r| r.get(0),
    )
    .optional()
}

const SOURCE_COLS: &str =
    "id, publisher_id, kind, repository, host, label, public_key, key_fingerprint, status, \
     created_at, updated_at, deleted_at";

/// A source as the team sees it: never the private key.
fn source_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "publisher_id": r.get::<_, String>(1)?,
        "kind": r.get::<_, String>(2)?,
        "repository": r.get::<_, String>(3)?,
        "host": r.get::<_, String>(4)?,
        "label": r.get::<_, String>(5)?,
        "deploy_key": r.get::<_, Option<String>>(6)?.map(|k| json!({
            "public_key": k,
            "fingerprint": r.get::<_, Option<String>>(7).ok().flatten(),
            "access": "read_only",
        })),
        "status": r.get::<_, String>(8)?,
        "created_at": iso(r.get(9)?),
        "updated_at": iso(r.get(10)?),
        "deleted_at": r.get::<_, Option<i64>>(11)?.map(iso),
    }))
}

fn source_in(c: &Connection, id: &str, publisher: &str) -> rusqlite::Result<Option<Value>> {
    c.query_row(
        &format!("SELECT {SOURCE_COLS} FROM publisher_sources WHERE id = ?1 AND publisher_id = ?2"),
        params![id, publisher],
        source_json,
    )
    .optional()
}

/// `GET /v1/publishers/{publisher}/sources`
async fn list_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Read,
        )
        .await?;
        let pid = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {SOURCE_COLS} FROM publisher_sources WHERE publisher_id = ?1
                      ORDER BY status = 'active' DESC, created_at, id LIMIT 200"
                ))?;
                let rows = stmt
                    .query_map([&pid], source_json)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publishers/{publisher}/sources/{source}`
async fn read_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, source)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Read,
        )
        .await?;
        let pid = ctx.p.id.clone();
        let v = state
            .store
            .group_read(move |c| -> GResult<Option<Value>> { Ok(source_in(c, &source, &pid)?) })
            .await?
            .ok_or_else(not_found)?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceReq {
    kind: String,
    repository: String,
    label: String,
}

/// A fresh Ed25519 deploy key: `(seed, OpenSSH public key line, SHA-256 fingerprint)`.
fn new_deploy_key(comment: &str) -> GResult<(zeroize::Zeroizing<[u8; 32]>, String, String)> {
    let mut seed = zeroize::Zeroizing::new([0u8; 32]);
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, seed.as_mut());
    let mut key = ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&seed));
    key.set_comment(comment);
    let public = key.public_key();
    let line = public.to_openssh().map_err(|_| store_error())?;
    let fp = public.fingerprint(ssh_key::HashAlg::Sha256).to_string();
    Ok((seed, line, fp))
}

fn store_error() -> GErr {
    GErr::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_error",
        "the key could not be prepared",
    )
}

/// `POST /v1/publishers/{publisher}/sources` — register a repository. A `deploy_key` source
/// answers with the public key to add to the repository (read-only); the private key stays here.
async fn create_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Admin,
        )
        .await?;
        certified(&ctx.p)?;
        let req: SourceReq = parse(body)?;
        let label = text(&req.label, "label", 1, 80)?;
        let repository = req.repository.trim().to_string();
        match req.kind.as_str() {
            "https" => {
                if !crate::marketplace_m2::repository_ok(&repository) {
                    return Err(GErr::bad(
                        "invalid_request",
                        "repository: an https URL with no credentials",
                    ));
                }
            }
            "deploy_key" => {
                if !ssh_repository_ok(&repository) {
                    return Err(GErr::bad(
                        "invalid_request",
                        "repository: ssh://git@host/owner/repo or git@host:owner/repo",
                    ));
                }
            }
            _ => {
                return Err(GErr::bad(
                    "invalid_request",
                    "kind: https or deploy_key",
                ))
            }
        }
        let host = host_allowed(&state, &repository)?;
        let id = new_id("src");
        let (public, fingerprint, sealed) = if req.kind == "deploy_key" {
            let (seed, line, fp) = new_deploy_key(&format!("pigeonpost-build-{id}"))?;
            let sealed = state
                .vault
                .seal_labeled(KEY_LABEL, id.as_bytes(), seed.as_ref())
                .map_err(|_| store_error())?;
            (Some(line), Some(fp), Some(sealed))
        } else {
            (None, None, None)
        };
        let now = now_unix() as i64;
        let (sid, pid, by) = (id.clone(), ctx.p.id.clone(), ctx.account.clone());
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let live: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM publisher_sources WHERE publisher_id = ?1 AND status = 'active'",
                    [&pid],
                    |r| r.get(0),
                )?;
                if live >= MAX_SOURCES {
                    return Err(GErr::conflict("too_many_sources", "this publisher holds the most sources it may"));
                }
                tx.execute(
                    "INSERT INTO publisher_sources (id, publisher_id, kind, repository, host, label,
                        public_key, key_fingerprint, private_key_sealed, status, created_by, created_at,
                        updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', ?10, ?11, ?11)",
                    params![sid, pid, req.kind, repository, host, label, public, fingerprint, sealed, by, now],
                )?;
                Ok(source_in(tx, &sid, &pid)?.unwrap_or(Value::Null))
            })
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/publishers/{publisher}/sources/{source}` — the sealed key is wiped; queued builds
/// that named the source fail with `source_deleted`.
async fn delete_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, source)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Admin,
        )
        .await?;
        let now = now_unix() as i64;
        let pid = ctx.p.id.clone();
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let n = tx.execute(
                    "UPDATE publisher_sources SET status = 'deleted', private_key_sealed = NULL,
                        deleted_at = ?3, updated_at = ?3
                      WHERE id = ?1 AND publisher_id = ?2 AND status = 'active'",
                    params![source, pid, now],
                )?;
                if n == 0 {
                    return Err(not_found());
                }
                tx.execute(
                    "UPDATE nest_builds SET status = 'failed', failure_code = 'source_deleted',
                        failure_reason = 'the source connection was deleted', updated_at = ?2
                      WHERE source_id = ?1 AND status = 'queued'",
                    params![source, now],
                )?;
                Ok(source_in(tx, &source, &pid)?.unwrap_or(Value::Null))
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `GET /internal/v1/marketplace/build-requests/{build}/source-credential` — the deploy key of a
/// queued build's source, for the build plane only (`no-store`). Anything else is `404`.
async fn source_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(build): Path<String>,
) -> Response {
    // The ops token, or the build plane's token scoped to this route alone (hardening:
    // `pigeonpost-nest-buildd` holds nothing else that acts on the postbox).
    let scoped = crate::marketplace_hardening::credential_token_ok(&state, &headers);
    if !scoped {
        if let Err(r) = service_gate(&state, &headers) {
            return *r;
        }
    }
    let result = async {
        flag_gate(&state).await?;
        type Row = (String, String, String, Option<Vec<u8>>);
        let row: Option<Row> = state
            .store
            .group_read(move |c| -> GResult<Option<Row>> {
                Ok(c.query_row(
                    "SELECT s.id, s.repository, s.kind, s.private_key_sealed
                       FROM nest_builds b JOIN publisher_sources s ON s.id = b.source_id
                       JOIN publisher_profiles p ON p.id = b.publisher_id
                      WHERE b.id = ?1 AND b.status = 'queued' AND s.status = 'active'
                        AND s.publisher_id = b.publisher_id AND p.status = 'approved'",
                    [&build],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?)
            })
            .await?;
        let Some((sid, repository, kind, sealed)) = row else {
            return Err(not_found());
        };
        if kind != "deploy_key" {
            return Err(not_found());
        }
        let sealed = sealed.ok_or_else(not_found)?;
        let seed = zeroize::Zeroizing::new(
            state
                .vault
                .open_labeled(KEY_LABEL, sid.as_bytes(), &sealed)
                .map_err(|_| store_error())?,
        );
        let seed: [u8; 32] = seed.as_slice().try_into().map_err(|_| store_error())?;
        let key = ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&seed));
        let pem = key
            .to_openssh(ssh_key::LineEnding::LF)
            .map_err(|_| store_error())?;
        let mut resp = into_response::<Json<Value>>(Ok(Json(json!({ "data": {
            "source_id": sid,
            "repository": repository,
            "private_key": pem.as_str(),
        }}))));
        resp.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        Ok(resp)
    }
    .await;
    into_response(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_hosts_and_ssh_forms() {
        assert_eq!(
            repository_host("https://GitHub.com/acme/x.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            repository_host("ssh://git@gitlab.com/acme/x.git").as_deref(),
            Some("gitlab.com")
        );
        assert_eq!(
            repository_host("git@codeberg.org:acme/x.git").as_deref(),
            Some("codeberg.org")
        );
        assert_eq!(repository_host("ssh://git@host:2222/x"), None);
        assert_eq!(repository_host("file:///etc"), None);
        assert!(ssh_repository_ok("git@github.com:acme/team-digest.git"));
        assert!(ssh_repository_ok(
            "ssh://git@github.com/acme/team-digest.git"
        ));
        for bad in [
            "git@github.com:-oProxyCommand=x",
            "git@github.com:/etc/passwd",
            "git@github.com:a/../b",
            "ssh://git@github.com/",
            "https://github.com/a/b",
            "git@github.com:a b",
        ] {
            assert!(!ssh_repository_ok(bad), "{bad}");
        }
    }

    #[test]
    fn deploy_keys_are_ed25519_and_reproducible_from_the_seed() {
        let (seed, line, fp) = new_deploy_key("pigeonpost-build-src_x").unwrap();
        assert!(line.starts_with("ssh-ed25519 "), "{line}");
        assert!(fp.starts_with("SHA256:"), "{fp}");
        let again = ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&seed));
        assert_eq!(
            again
                .public_key()
                .fingerprint(ssh_key::HashAlg::Sha256)
                .to_string(),
            fp
        );
    }
}
