//! HR policy Q&A worker: the entrypoint of the reference release image.
//!
//! The dispatcher writes one JSON file per job to `/nest/jobs/in` and reads the answer from
//! `/nest/jobs/out`. The worker's only network peer is the broker at `NEST_BROKER_URL`, with the
//! per-start token nestd writes to `/nest/run/broker-token`. Nothing else is read or written.

mod qa;

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const POLICY: &str = include_str!("../policy/handbook.md");
const INBOX: &str = "/nest/jobs/in";
const OUTBOX: &str = "/nest/jobs/out";
const TOKEN: &str = "/nest/run/broker-token";
/// Largest job file this worker reads; the postbox already caps requests at 256 KiB.
const MAX_JOB_BYTES: u64 = 256 * 1024;
const MAX_REPLY_BYTES: usize = 512 * 1024;

/// The broker over plain HTTP/1.1 on the pool's private bridge.
struct Broker {
    host_port: String,
    token: String,
}

impl Broker {
    fn from_env() -> Option<Broker> {
        let url = std::env::var("NEST_BROKER_URL").ok()?;
        let host_port = url
            .strip_prefix("http://")?
            .trim_end_matches('/')
            .to_string();
        let token = std::fs::read_to_string(TOKEN).ok()?.trim().to_string();
        if host_port.is_empty() || !token.starts_with("nbt_") {
            return None;
        }
        Some(Broker { host_port, token })
    }
}

impl qa::Model for Broker {
    fn complete(&mut self, system: &str, user: &str) -> Result<String, String> {
        let body = json!({
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "max_tokens": 400,
        })
        .to_string();
        let mut stream = TcpStream::connect(&self.host_port).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .map_err(|e| e.to_string())?;
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.host_port,
            self.token,
            body.len(),
            body
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut reply = Vec::new();
        stream
            .take(MAX_REPLY_BYTES as u64)
            .read_to_end(&mut reply)
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&reply);
        let (head, body) = text.split_once("\r\n\r\n").ok_or("malformed reply")?;
        if !head.starts_with("HTTP/1.1 200") {
            return Err("the broker refused".into());
        }
        let value: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        value["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "no answer".into())
    }
}

fn job_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn process(policy: &qa::Policy, path: &Path, broker: &mut Option<Broker>) {
    let too_big = std::fs::metadata(path).map_or(true, |m| m.len() > MAX_JOB_BYTES);
    let job: Value = if too_big {
        Value::Null
    } else {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    };
    let name = path.file_name().map(|n| n.to_owned()).unwrap_or_default();
    let model = broker.as_mut().map(|b| b as &mut dyn qa::Model);
    let answer = qa::handle(policy, &job, model);
    let out = Path::new(OUTBOX).join(&name);
    let tmp = out.with_extension("tmp");
    if std::fs::write(&tmp, answer.to_string()).is_ok() && std::fs::rename(&tmp, &out).is_ok() {
        let _ = std::fs::remove_file(path);
    }
}

fn main() {
    let policy = qa::Policy::parse(POLICY);
    let _ = std::fs::create_dir_all(INBOX);
    let _ = std::fs::create_dir_all(OUTBOX);
    let mut broker = Broker::from_env();
    loop {
        for path in job_files(Path::new(INBOX)) {
            process(&policy, &path, &mut broker);
        }
        std::thread::sleep(Duration::from_millis(500));
        if broker.is_none() {
            broker = Broker::from_env();
        }
    }
}
