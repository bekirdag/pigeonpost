//! Test-only adapter for the exact-binary M6 acceptance ceremony.
//!
//! The release workflow builds this example to exercise the packaged `ppcompliance` asset. It is
//! never copied into a release artifact and is not a production approval or destruction backend.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

const APPROVAL_REQUEST_MAGIC: &[u8; 8] = b"PPAPREQ\0";
const APPROVAL_RESPONSE_MAGIC: &[u8; 8] = b"PPAPRES\0";
const DESTRUCTION_REQUEST_MAGIC: &[u8; 8] = b"PPSHRED\0";
const DESTRUCTION_RESPONSE_MAGIC: &[u8; 8] = b"PPSHRES\0";
const ADAPTER_PROTOCOL_VERSION: u8 = 1;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::FAILURE,
    }
}

fn run() -> Result<(), ()> {
    let executable = env::current_exe().map_err(|_| ())?;
    let name = executable
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(())?;
    let mut request = Vec::new();
    io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .map_err(|_| ())?;
    if request.is_empty() || request.len() as u64 > MAX_REQUEST_BYTES {
        return Err(());
    }

    let response = if name == "approval-adapter" {
        approval_response(&request)?
    } else if name == "destruction-adapter" {
        destruction_response(&executable, &request)?
    } else {
        return Err(());
    };
    io::stdout().write_all(&response).map_err(|_| ())?;
    io::stdout().flush().map_err(|_| ())
}

fn approval_response(request: &[u8]) -> Result<Vec<u8>, ()> {
    if request.len() < 60
        || &request[..8] != APPROVAL_REQUEST_MAGIC
        || request[8] != ADAPTER_PROTOCOL_VERSION
    {
        return Err(());
    }
    let request_id: [u8; 32] = request[9..41].try_into().map_err(|_| ())?;
    let approved_at_ms = u64::from_be_bytes(request[43..51].try_into().map_err(|_| ())?);
    if approved_at_ms == 0 {
        return Err(());
    }

    let mut response = Vec::with_capacity(217);
    response.extend_from_slice(APPROVAL_RESPONSE_MAGIC);
    response.push(ADAPTER_PROTOCOL_VERSION);
    for seed in [21u8, 22u8] {
        let signer = SigningKey::from_bytes(&[seed; 32]);
        let mut preimage = Vec::with_capacity(75);
        preimage.extend_from_slice(b"pigeonpost/disclosure-approval/v1");
        preimage.extend_from_slice(&request_id);
        preimage.extend_from_slice(&approved_at_ms.to_be_bytes());
        preimage.push(ADAPTER_PROTOCOL_VERSION);
        response.extend_from_slice(&signer.verifying_key().to_bytes());
        response.extend_from_slice(&approved_at_ms.to_be_bytes());
        response.extend_from_slice(&signer.sign(&preimage).to_bytes());
    }
    Ok(response)
}

fn destruction_response(executable: &Path, request: &[u8]) -> Result<Vec<u8>, ()> {
    if request.len() < 42
        || &request[..8] != DESTRUCTION_REQUEST_MAGIC
        || request[8] != ADAPTER_PROTOCOL_VERSION
    {
        return Err(());
    }

    let secret = executable.parent().ok_or(())?.join("trace-custody.secret");
    match fs::remove_file(secret) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(()),
    }

    let mut digest = Sha256::new();
    digest.update(b"pigeonpost/m6-test-destruction/v1");
    digest.update(request);
    let commitment: [u8; 32] = digest.finalize().into();
    let mut response = Vec::with_capacity(42);
    response.extend_from_slice(DESTRUCTION_RESPONSE_MAGIC);
    response.push(ADAPTER_PROTOCOL_VERSION);
    response.push(1);
    response.extend_from_slice(&commitment);
    Ok(response)
}
