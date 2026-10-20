//! Native Rust interop agent. Speaks the JSON-lines protocol documented in
//! `interop/run_interop.py`; also serves the orchestrator's server-side checks (`server_*` ops),
//! which are exactly what the postbox will run.

use std::io::{BufRead, Write};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use pigeonpost_groupcrypto::*;
use serde_json::{json, Map, Value};
use std::result::Result;

fn b64(v: &[u8]) -> Value {
    Value::String(B64.encode(v))
}

fn bytes(req: &Value, key: &str) -> Result<Vec<u8>, String> {
    let s = req
        .get(key)
        .and_then(Value::as_str)
        .ok_or(format!("missing {key}"))?;
    B64.decode(s).map_err(|e| e.to_string())
}

fn string(req: &Value, key: &str) -> Result<String, String> {
    req.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(format!("missing {key}"))
}

fn byte_list(req: &Value, key: &str) -> Result<Vec<Vec<u8>>, String> {
    req.get(key)
        .and_then(Value::as_array)
        .ok_or(format!("missing {key}"))?
        .iter()
        .map(|v| {
            B64.decode(v.as_str().unwrap_or_default())
                .map_err(|e| e.to_string())
        })
        .collect()
}

fn commit_json(b: CommitBundle) -> Value {
    json!({"epoch": b.epoch, "commit_id": b.commit_id, "commit": b64(&b.commit),
           "welcome": b.welcome.as_deref().map(b64), "signature": b64(&b.envelope_signature)})
}

fn att_json(a: &AttachmentRef) -> Value {
    json!({"key": b64(&a.key), "sha": b64(&a.sealed_sha256), "len": a.plaintext_len})
}

fn att_in(v: &Value) -> Result<AttachmentRef, String> {
    let arr = |k: &str| -> Result<[u8; 32], String> {
        bytes(v, k)?
            .try_into()
            .map_err(|_| format!("{k} must be 32 bytes"))
    };
    Ok(AttachmentRef {
        key: arr("key")?,
        sealed_sha256: arr("sha")?,
        plaintext_len: v["len"].as_u64().unwrap_or(0),
    })
}

fn incoming_json(i: Incoming) -> Value {
    match i {
        Incoming::Message(m) => {
            json!({"type": "message", "body": b64(&m.body), "sender_mailbox": m.sender.mailbox,
            "sender_device": m.sender.device_id, "client_message_id": m.client_message_id, "epoch": m.epoch,
            "attachments": m.attachments.iter().map(att_json).collect::<Vec<_>>()})
        }
        Incoming::HistoryOffer(_) => json!({"type": "history_offer"}),
        Incoming::Commit(o) => {
            json!({"type": "commit", "epoch": o.epoch, "self_removed": o.self_removed,
            "committer_mailbox": o.committer.mailbox, "added": o.added.len(), "removed": o.removed.len()})
        }
        Incoming::OwnCommitConfirmed(o) => {
            json!({"type": "own_commit_confirmed", "epoch": o.epoch})
        }
        Incoming::Own => json!({"type": "own"}),
    }
}

struct Agent {
    device: Option<Device>,
}

impl Agent {
    fn dev(&mut self) -> Result<&mut Device, String> {
        self.device.as_mut().ok_or("init first".into())
    }

    fn handle(&mut self, req: &Value) -> Result<Value, Error> {
        let e = |m: String| Error::InvalidInput(m);
        let op = req["op"].as_str().unwrap_or_default();
        let g = || string(req, "group").map_err(e);
        Ok(match op {
            "init" => {
                let seed: [u8; 32] = bytes(req, "seed")
                    .map_err(e)?
                    .try_into()
                    .map_err(|_| e("seed".into()))?;
                let d = Device::new_with_identity_seed(&seed, &string(req, "device").map_err(e)?)?;
                let out = json!({"mailbox": d.mailbox(), "signature_key": b64(&d.signature_public_key())});
                self.device = Some(d);
                out
            }
            "key_packages" => {
                let n = req["count"].as_u64().unwrap_or(1) as u32;
                let d = self.dev().map_err(e)?;
                let mut kps = d.generate_key_packages(n)?;
                kps.push(d.generate_last_resort_key_package()?);
                json!({"key_packages": kps.iter().map(|k| b64(k)).collect::<Vec<_>>()})
            }
            "create" => json!({"epoch": self.dev().map_err(e)?.create_group(&g()?)?.epoch}),
            "add" => {
                let kps = byte_list(req, "key_packages").map_err(e)?;
                commit_json(self.dev().map_err(e)?.add_members(&g()?, &kps)?)
            }
            "remove_mailbox" => commit_json(
                self.dev()
                    .map_err(e)?
                    .remove_mailbox(&g()?, &string(req, "mailbox").map_err(e)?)?,
            ),
            "self_update" => commit_json(self.dev().map_err(e)?.self_update(&g()?)?),
            "confirm" => json!({"epoch": self.dev().map_err(e)?.confirm_commit(&g()?)?.epoch}),
            "discard" => {
                self.dev().map_err(e)?.discard_commit(&g()?)?;
                json!({})
            }
            "join" => {
                let j = self
                    .dev()
                    .map_err(e)?
                    .join_from_welcome(&bytes(req, "welcome").map_err(e)?)?;
                json!({"group": j.group_id, "epoch": j.epoch, "members": j.members.len()})
            }
            "encrypt" => {
                let mut atts = Vec::new();
                let mut sealed = Value::Null;
                if req.get("attachment").is_some_and(|v| !v.is_null()) {
                    let s = seal_attachment(&bytes(req, "attachment").map_err(e)?)?;
                    sealed = b64(&s.sealed);
                    atts.push(s.reference.clone());
                }
                let m = self.dev().map_err(e)?.encrypt_message(
                    &g()?,
                    &string(req, "id").map_err(e)?,
                    &bytes(req, "body").map_err(e)?,
                    &atts,
                )?;
                json!({"ciphertext": b64(&m.ciphertext), "signature": b64(&m.envelope_signature), "epoch": m.epoch, "sealed": sealed})
            }
            "process" => incoming_json(
                self.dev()
                    .map_err(e)?
                    .process_incoming(&g()?, &bytes(req, "message").map_err(e)?)?,
            ),
            "open_attachment" => {
                let r = att_in(&req["ref"]).map_err(e)?;
                json!({"plaintext": b64(&open_attachment(&bytes(req, "sealed").map_err(e)?, &r)?)})
            }
            "status" => {
                let d = self.dev().map_err(e)?;
                let g = g()?;
                json!({"epoch": d.epoch(&g)?, "epoch_authenticator": b64(&d.epoch_authenticator(&g)?),
                       "safety_number": d.safety_number(&g)?, "members": d.members(&g)?.len()})
            }
            "persist" => {
                let key = generate_state_key()?;
                let blob = self.dev().map_err(e)?.export_state(&key)?;
                self.device = Some(Device::import_state(&blob, &key)?);
                json!({"bytes": blob.len()})
            }
            "snapshot" => {
                let key = generate_state_key()?;
                json!({"state": b64(&self.dev().map_err(e)?.export_state(&key)?), "key": b64(&key)})
            }
            "probe_snapshot" => {
                let mut d = Device::import_state(
                    &bytes(req, "state").map_err(e)?,
                    &bytes(req, "key").map_err(e)?,
                )?;
                match d.process_incoming(&g()?, &bytes(req, "message").map_err(e)?) {
                    Ok(i) => json!({"code": "ok", "result": incoming_json(i)}),
                    Err(err) => json!({"code": err.code()}),
                }
            }
            // Server-side validation (what the postbox runs on upload).
            "server_key_package" => {
                let i = inspect_key_package(&bytes(req, "key_package").map_err(e)?)?;
                json!({"ref": b64(&i.key_package_ref), "mailbox": i.mailbox, "device": i.device_id,
                       "signature_key": b64(&i.signature_key), "last_resort": i.last_resort})
            }
            "server_envelope" => {
                let m = bytes(req, "message").map_err(e)?;
                verify_envelope_signature(
                    &bytes(req, "signature_key").map_err(e)?,
                    &m,
                    &bytes(req, "signature").map_err(e)?,
                )?;
                let i = inspect_envelope(&m)?;
                json!({"group": i.group_id, "epoch": i.epoch, "kind": if i.kind == MessageKind::Commit {"commit"} else {"application"},
                       "sender_mailbox": i.sender_mailbox, "sender_device": i.sender_device, "client_message_id": i.client_message_id})
            }
            "server_welcome" => {
                let r = welcome_recipients(&bytes(req, "welcome").map_err(e)?)?;
                json!({"recipients": r.iter().map(|x| b64(x)).collect::<Vec<_>>()})
            }
            other => return Err(Error::InvalidInput(format!("unknown op {other}"))),
        })
    }
}

fn main() {
    let mut agent = Agent { device: None };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => break,
        };
        if req["op"] == "quit" {
            break;
        }
        let resp = match agent.handle(&req) {
            Ok(Value::Object(mut m)) => {
                m.insert("ok".into(), Value::Bool(true));
                Value::Object(m)
            }
            Ok(other) => other,
            Err(err) => {
                let mut m = Map::new();
                m.insert("ok".into(), Value::Bool(false));
                m.insert("code".into(), err.code().into());
                m.insert("message".into(), err.to_string().into());
                Value::Object(m)
            }
        };
        writeln!(out, "{resp}").unwrap();
        out.flush().unwrap();
    }
}
