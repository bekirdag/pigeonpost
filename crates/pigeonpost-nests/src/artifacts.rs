//! Release images between the build plane and the worker host (hardening, W1.3).
//!
//! The build plane (`pigeonpost-nest-buildd`, on another host) saves every release image as an
//! OCI archive and serves it, read-only and by digest, behind its artifact token. The worker host
//! pulls an image it does not have the first time an attested release needs it:
//!
//! 1. `GET /v1/artifacts/{image_digest}?offset=N` in bounded chunks (no streaming body needed on
//!    either side, memory stays at one chunk), checked against the archive's SHA-256 the build
//!    plane reports;
//! 2. `podman load` of the archive;
//! 3. `podman image inspect` must then report exactly the attested manifest digest, or the image
//!    is removed again and the start fails. The attestation covers the manifest digest, and
//!    podman verifies every layer against the manifest while loading, so nothing the build host
//!    (or the link) changes can run under an attested name.

use crate::config::ArtifactsConfig;
use crate::dispatch::ImageSource;
use crate::sandbox::Podman;
use crate::util::sha256_hex;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Largest chunk one artifact request returns.
pub const CHUNK_BYTES: u64 = 8 * 1024 * 1024;
/// The response header carrying the whole archive's size and SHA-256.
pub const SIZE_HEADER: &str = "x-artifact-size";
pub const SHA256_HEADER: &str = "x-artifact-sha256";

/// `sha256:<64 hex>`.
pub fn digest_ok(d: &str) -> bool {
    d.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// The manifest digest an OCI archive (`podman save --format oci-archive`) holds: the first entry
/// of its `index.json`. A minimal ustar reader, enough for what podman writes.
pub fn oci_archive_manifest_digest(archive: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut header = [0u8; 512];
    loop {
        if f.read_exact(&mut header).is_err() {
            return Err("no index.json in the archive".into());
        }
        if header.iter().all(|b| *b == 0) {
            return Err("no index.json in the archive".into());
        }
        let field = |r: std::ops::Range<usize>| {
            let raw = &header[r];
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).trim().to_string()
        };
        let name = field(0..100);
        let prefix = field(345..500);
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let size = u64::from_str_radix(field(124..136).trim_matches(' '), 8)
            .map_err(|_| "a malformed tar header".to_string())?;
        let padded = size.div_ceil(512) * 512;
        if full.trim_start_matches("./") == "index.json" {
            if size > 1024 * 1024 {
                return Err("index.json is too large".into());
            }
            let mut body = vec![0u8; size as usize];
            f.read_exact(&mut body).map_err(|e| e.to_string())?;
            let v: serde_json::Value =
                serde_json::from_slice(&body).map_err(|_| "index.json is not JSON".to_string())?;
            let digest = v["manifests"][0]["digest"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !digest_ok(&digest) {
                return Err("index.json names no manifest digest".into());
            }
            return Ok(digest);
        }
        std::io::copy(&mut (&mut f).take(padded), &mut std::io::sink())
            .map_err(|e| e.to_string())?;
    }
}

/// The local digest of an image podman has, if it has it.
pub fn local_digest<P: Podman>(podman: &P, image: &str) -> Option<String> {
    podman
        .run(&["image", "inspect", "--format", "{{.Digest}}", image])
        .ok()
        .map(|d| d.trim().to_string())
}

/// Pulls release images from the build plane's artifact API.
pub struct BuilddImages<P: Podman> {
    pub cfg: ArtifactsConfig,
    pub token: String,
    pub podman: P,
    client: reqwest::Client,
}

impl<P: Podman> BuilddImages<P> {
    pub fn new(cfg: ArtifactsConfig, token: String, podman: P) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(BuilddImages {
            cfg,
            token,
            podman,
            client,
        })
    }

    /// Download the archive of `digest` into `dest`, chunk by chunk, checking size and SHA-256.
    async fn download(&self, digest: &str, dest: &Path) -> Result<(), String> {
        use std::io::Write;
        let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let (mut offset, mut total) = (0u64, None::<u64>);
        let mut want: String;
        loop {
            let url = format!(
                "{}/v1/artifacts/{digest}?offset={offset}",
                self.cfg.url.trim_end_matches('/')
            );
            let resp = self
                .client
                .get(url)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| e.without_url().to_string())?;
            if !resp.status().is_success() {
                return Err(format!("the build plane answered {}", resp.status()));
            }
            let header = |name: &str| {
                resp.headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let size: u64 = header(SIZE_HEADER)
                .and_then(|s| s.parse().ok())
                .ok_or("no artifact size")?;
            if size > self.cfg.max_bytes {
                return Err("the image archive is larger than artifacts.max_bytes".into());
            }
            if *total.get_or_insert(size) != size {
                return Err("the archive changed during the download".into());
            }
            want = header(SHA256_HEADER).unwrap_or_default();
            let body = resp.bytes().await.map_err(|e| e.to_string())?;
            if body.len() as u64 > CHUNK_BYTES {
                return Err("an oversized chunk".into());
            }
            hasher.update(&body);
            out.write_all(&body).map_err(|e| e.to_string())?;
            offset += body.len() as u64;
            if offset >= size || body.is_empty() {
                break;
            }
        }
        if Some(offset) != total {
            return Err("the archive is short".into());
        }
        let got = crate::util::hex(&hasher.finalize());
        if !crate::util::ct_eq(&got, want.trim_start_matches("sha256:")) {
            return Err("the archive does not match its SHA-256".into());
        }
        Ok(())
    }
}

impl<P: Podman> ImageSource for BuilddImages<P> {
    fn ensure(&self, image: &str) -> Result<(), String> {
        let Some((_, digest)) = image.split_once('@') else {
            return Err("an image must be pinned by digest".into());
        };
        if !digest_ok(digest) {
            return Err("an image must be pinned by digest".into());
        }
        if local_digest(&self.podman, image).as_deref() == Some(digest) {
            return Ok(());
        }
        std::fs::create_dir_all(&self.cfg.cache_dir).map_err(|e| e.to_string())?;
        let part = self
            .cfg
            .cache_dir
            .join(format!("{}.part", &sha256_hex(image.as_bytes())[..32]));
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| "image pulls need the service runtime".to_string())?;
        let fetched = handle.block_on(self.download(digest, &part));
        let loaded = fetched.and_then(|_| {
            self.podman
                .run(&["load", "--quiet", "--input", &part.to_string_lossy()])
                .map(|_| ())
        });
        let _ = std::fs::remove_file(&part);
        loaded?;
        match local_digest(&self.podman, image) {
            Some(d) if d == digest => Ok(()),
            _ => {
                let _ = self.podman.run(&["rmi", "--force", "--ignore", image]);
                Err("the loaded image does not have the attested digest".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_entry(name: &str, body: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:011o}\0", body.len());
        h[124..136].copy_from_slice(size.as_bytes());
        h[156] = b'0';
        let mut out = h.to_vec();
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    #[test]
    fn the_manifest_digest_is_read_from_the_archive_index() {
        let dir = tempfile::tempdir().unwrap();
        let digest = format!("sha256:{}", "ab".repeat(32));
        let mut tar = tar_entry("oci-layout", b"{\"imageLayoutVersion\":\"1.0.0\"}");
        tar.extend(tar_entry("blobs/sha256/00", &[1u8; 700]));
        tar.extend(tar_entry(
            "index.json",
            format!("{{\"schemaVersion\":2,\"manifests\":[{{\"digest\":\"{digest}\"}}]}}")
                .as_bytes(),
        ));
        tar.extend([0u8; 1024]);
        let p = dir.path().join("a.tar");
        std::fs::write(&p, &tar).unwrap();
        assert_eq!(oci_archive_manifest_digest(&p).unwrap(), digest);
        let empty = dir.path().join("b.tar");
        std::fs::write(&empty, tar_entry("oci-layout", b"{}")).unwrap();
        assert!(oci_archive_manifest_digest(&empty).is_err());
        assert!(digest_ok(&digest) && !digest_ok("sha256:AB") && !digest_ok("md5:00"));
    }
}
