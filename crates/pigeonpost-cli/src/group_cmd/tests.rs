//! The group client's protocol loop against an in-process fake of the postbox's group routes
//! (`docs/groups.md`). The fake applies the server-side rules the loop depends on — commit
//! serialization by epoch and revision, envelope signatures and headers, pending adds and
//! removals, Welcome routing by KeyPackageRef — with the real `pigeonpost-groupcrypto` checks.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Kp {
    bytes: Vec<u8>,
    reference: String,
    last_resort: bool,
    claimed: bool,
}

#[derive(Default)]
struct G {
    epoch: u64,
    revision: u64,
    last_seq: u64,
    creator: String,
    active: bool,
    members: BTreeMap<String, String>,
    leaves: BTreeSet<(String, String)>,
    items: Vec<Value>,
}

#[derive(Default)]
struct Hub {
    seeds: BTreeMap<String, [u8; 32]>,
    devices: BTreeMap<(String, String), Vec<u8>>,
    kps: BTreeMap<(String, String), Vec<Kp>>,
    welcomes: Vec<(String, String, String, Value)>,
    groups: BTreeMap<String, G>,
    blobs: BTreeMap<String, Vec<u8>>,
    next: u64,
    second_device_needs_human: bool,
}

fn err(status: u16, code: &str) -> ApiError {
    ApiError {
        status,
        code: code.into(),
        message: code.into(),
    }
}

impl Hub {
    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn pending(&self, g: &G) -> Vec<Value> {
        let mut out = Vec::new();
        for (m, state) in &g.members {
            if state != "active" && state != "keying" {
                continue;
            }
            for (mb, dev) in self.devices.keys() {
                if mb == m && !g.leaves.contains(&(mb.clone(), dev.clone())) {
                    out.push(json!({"kind":"add","mailbox":mb,"device_id":dev,"since":0}));
                }
            }
        }
        for (mb, dev) in &g.leaves {
            let state = g.members.get(mb).map(String::as_str).unwrap_or("");
            if state != "active" && state != "keying" {
                out.push(json!({"kind":"remove","mailbox":mb,"device_id":dev,"since":0}));
            }
        }
        out
    }

    fn route(
        &mut self,
        me: &str,
        method: &str,
        path: &str,
        device: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, ApiError> {
        let body = body.unwrap_or(Value::Null);
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let dev = || -> Result<(String, String), ApiError> {
            let d = device.ok_or(err(400, "device_required"))?;
            Ok((me.to_string(), d.to_string()))
        };
        match (method, parts.as_slice()) {
            ("POST", ["v1", "mls", "devices"]) => {
                let id = body["device_id"].as_str().unwrap().to_string();
                let key = unb64(body["signature_key"].as_str().unwrap()).unwrap();
                if self.devices.contains_key(&(me.into(), id.clone())) {
                    return Err(err(409, "device_exists"));
                }
                if self.second_device_needs_human && self.devices.keys().any(|(m, _)| m == me) {
                    return Err(err(403, "human_required"));
                }
                let seed = self.seeds[me];
                let payload = gc::attestation_payload(me, &id, &key).unwrap();
                let sig = gc::sign_attestation_with_seed(&seed, &payload).unwrap();
                let pk = ed25519_dalek::SigningKey::from_bytes(&seed)
                    .verifying_key()
                    .to_bytes();
                self.devices.insert((me.into(), id.clone()), key);
                Ok(
                    json!({"device_id": id, "attestation": b64(&sig), "mailbox_public_key": b64(&pk)}),
                )
            }
            ("GET", ["v1", "mls", "key-packages", "count"]) => {
                let list = self.kps.get(&dev()?);
                let unclaimed = list
                    .map(|l| l.iter().filter(|k| !k.claimed && !k.last_resort).count())
                    .unwrap_or(0);
                let lr = list.is_some_and(|l| l.iter().any(|k| k.last_resort));
                Ok(json!({"unclaimed": unclaimed, "last_resort": lr}))
            }
            ("POST", ["v1", "mls", "key-packages"]) => {
                let d = dev()?;
                let registered = self.devices[&d].clone();
                let add = |b64s: &str, last: bool, list: &mut Vec<Kp>| -> Result<(), ApiError> {
                    let bytes = unb64(b64s).unwrap();
                    let info = gc::inspect_key_package(&bytes).map_err(|_| err(400, "bad"))?;
                    if info.mailbox != d.0
                        || info.device_id != d.1
                        || info.signature_key != registered
                    {
                        return Err(err(400, "key_package_mismatch"));
                    }
                    if last {
                        list.retain(|k| !k.last_resort);
                    }
                    list.push(Kp {
                        bytes,
                        reference: hex(&info.key_package_ref),
                        last_resort: last,
                        claimed: false,
                    });
                    Ok(())
                };
                let mut list = self.kps.remove(&d).unwrap_or_default();
                for k in body["key_packages"].as_array().unwrap() {
                    add(k.as_str().unwrap(), false, &mut list)?;
                }
                if let Some(lr) = body["last_resort"].as_str() {
                    add(lr, true, &mut list)?;
                }
                self.kps.insert(d, list);
                Ok(json!({"stored": true}))
            }
            ("GET", ["v1", "mls", "welcomes"]) => {
                let d = dev()?;
                let list: Vec<Value> = self
                    .welcomes
                    .iter()
                    .filter(|w| w.1 == d.0 && w.2 == d.1)
                    .map(|w| w.3.clone())
                    .collect();
                Ok(json!({ "welcomes": list }))
            }
            ("POST", ["v1", "mls", "welcomes", id, "ack"]) => {
                self.welcomes.retain(|w| w.0 != *id);
                Ok(json!({}))
            }
            ("POST", ["v1", "groups"]) => {
                let id = self.id("grp_test");
                let mut g = G {
                    revision: 1,
                    creator: me.into(),
                    ..G::default()
                };
                g.members.insert(me.into(), "keying".into());
                let mut inv = Vec::new();
                for a in body["invite"].as_array().into_iter().flatten() {
                    g.members
                        .insert(a.as_str().unwrap().into(), "invited".into());
                    inv.push(json!({"address": a, "status": "invited"}));
                }
                self.groups.insert(id.clone(), g);
                Ok(json!({"group": {"id": id}, "invitations": inv}))
            }
            ("POST", ["v1", "groups", gid, "invitation"]) => {
                let g = self.groups.get_mut(*gid).ok_or(err(404, "not_found"))?;
                let state = if body["accept"].as_bool().unwrap() {
                    "keying"
                } else {
                    "removed"
                };
                g.members.insert(me.into(), state.into());
                Ok(json!({}))
            }
            ("GET", ["v1", "groups", gid, "crypto"]) => {
                let d = dev()?;
                let g = &self.groups[*gid];
                Ok(json!({
                    "epoch": g.epoch, "revision": g.revision,
                    "activation_state": if g.active {"active"} else {"provisioning"},
                    "in_group": g.leaves.contains(&d),
                    "pending": self.pending(g), "lagging_devices": [],
                }))
            }
            ("POST", ["v1", "groups", gid, "key-packages", "claim"]) => {
                let pending = self.pending(&self.groups[*gid]);
                let mut claims = Vec::new();
                for m in body["mailboxes"].as_array().unwrap() {
                    for p in pending
                        .iter()
                        .filter(|p| p["kind"] == "add" && p["mailbox"] == *m)
                    {
                        let key = (
                            p["mailbox"].as_str().unwrap().to_string(),
                            p["device_id"].as_str().unwrap().to_string(),
                        );
                        let list = self.kps.entry(key.clone()).or_default();
                        let index = match list.iter().position(|k| !k.claimed && !k.last_resort) {
                            Some(i) => {
                                list[i].claimed = true;
                                i
                            }
                            None => match list.iter().position(|k| k.last_resort) {
                                Some(i) => i,
                                None => continue,
                            },
                        };
                        let kp = &list[index];
                        claims.push(json!({"mailbox": key.0, "device_id": key.1,
                            "key_package": b64(&kp.bytes), "key_package_ref": kp.reference}));
                    }
                }
                Ok(json!({"claims": claims, "exhausted": []}))
            }
            ("POST", ["v1", "groups", gid, "commits"]) => {
                let d = dev()?;
                let key = self.devices[&d].clone();
                let g = self.groups.get_mut(*gid).ok_or(err(404, "not_found"))?;
                let initial = !g.active && g.creator == d.0;
                if !initial && !g.leaves.contains(&d) {
                    return Err(err(403, "not_member"));
                }
                if body["epoch"].as_u64() != Some(g.epoch)
                    || body["expected_revision"].as_u64() != Some(g.revision)
                {
                    return Err(err(409, "revision_conflict"));
                }
                let commit = unb64(body["commit"].as_str().unwrap()).unwrap();
                let sig = unb64(body["envelope_signature"].as_str().unwrap()).unwrap();
                gc::verify_envelope_signature(&key, &commit, &sig).map_err(|_| err(400, "sig"))?;
                let env = gc::inspect_envelope(&commit).map_err(|_| err(400, "env"))?;
                assert_eq!(env.kind, gc::MessageKind::Commit);
                assert_eq!((env.sender_mailbox.clone(), env.sender_device.clone()), d);
                assert_eq!(env.group_id, *gid);
                let adds = body["adds"].as_array().cloned().unwrap_or_default();
                let refs: BTreeSet<String> = adds
                    .iter()
                    .map(|a| a["key_package_ref"].as_str().unwrap().to_string())
                    .collect();
                if !adds.is_empty() {
                    let w = unb64(body["welcome"].as_str().ok_or(err(400, "welcome"))?).unwrap();
                    let got: BTreeSet<String> = gc::welcome_recipients(&w)
                        .unwrap()
                        .iter()
                        .map(|r| hex(r))
                        .collect();
                    assert_eq!(got, refs, "welcome addressed to exactly the adds");
                }
                g.last_seq += 1;
                let seq = g.last_seq;
                g.items.push(json!({"seq": seq, "type": "commit", "epoch": g.epoch,
                    "sender": d.0, "sender_device": d.1, "commit": body["commit"], "created_at": 1}));
                if initial {
                    g.active = true;
                    g.leaves.insert(d.clone());
                    g.members.insert(d.0.clone(), "active".into());
                }
                g.epoch += 1;
                g.revision += 1;
                for r in body["removes"].as_array().into_iter().flatten() {
                    g.leaves.remove(&(
                        r["mailbox"].as_str().unwrap().into(),
                        r["device_id"].as_str().unwrap().into(),
                    ));
                }
                let mut new_welcomes = Vec::new();
                for a in &adds {
                    let m = a["mailbox"].as_str().unwrap().to_string();
                    let dv = a["device_id"].as_str().unwrap().to_string();
                    g.leaves.insert((m.clone(), dv.clone()));
                    g.members.insert(m.clone(), "active".into());
                    new_welcomes.push((m, dv));
                }
                let (epoch, revision) = (g.epoch, g.revision);
                for (m, dv) in new_welcomes {
                    let wid = self.id("wel_");
                    let w = json!({"welcome_id": wid, "group_id": gid, "epoch": epoch, "seq": seq,
                        "welcome": body["welcome"]});
                    self.welcomes.push((wid, m, dv, w));
                }
                Ok(json!({"seq": seq, "epoch": epoch, "revision": revision}))
            }
            ("GET", ["v1", "groups", gid, "messages"]) => {
                let g = &self.groups[*gid];
                if g.members.get(me).map(String::as_str) != Some("active") {
                    return Err(err(403, "not_member"));
                }
                let after: u64 = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("after_seq="))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let items: Vec<Value> = g
                    .items
                    .iter()
                    .filter(|i| i["seq"].as_u64().unwrap() > after)
                    .cloned()
                    .collect();
                Ok(json!({"items": items, "more": false, "epoch": g.epoch, "last_seq": g.last_seq}))
            }
            ("POST", ["v1", "groups", gid, "messages"]) => {
                let d = dev()?;
                let key = self.devices[&d].clone();
                let pending_removal = self
                    .pending(&self.groups[*gid])
                    .iter()
                    .any(|p| p["kind"] == "remove");
                let g = self.groups.get_mut(*gid).unwrap();
                if g.members.get(me).map(String::as_str) != Some("active") || !g.leaves.contains(&d)
                {
                    return Err(err(403, "not_member"));
                }
                if body["epoch"].as_u64() != Some(g.epoch) {
                    return Err(err(409, "epoch_conflict"));
                }
                if pending_removal {
                    return Err(err(409, "key_activation_pending"));
                }
                assert_eq!(body["enc_scheme"], gc::ENC_SCHEME);
                let ct = unb64(body["ciphertext"].as_str().unwrap()).unwrap();
                let sig = unb64(body["envelope_signature"].as_str().unwrap()).unwrap();
                gc::verify_envelope_signature(&key, &ct, &sig).map_err(|_| err(400, "sig"))?;
                let env = gc::inspect_envelope(&ct).unwrap();
                assert_eq!(env.kind, gc::MessageKind::Application);
                assert_eq!(
                    env.client_message_id,
                    body["client_message_id"].as_str().unwrap()
                );
                g.last_seq += 1;
                let seq = g.last_seq;
                let atts: Vec<Value> = body["attachments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| json!({"id": a, "bytes": 0}))
                    .collect();
                g.items.push(json!({"seq": seq, "type": "application", "id": format!("gm_{seq}"),
                    "epoch": g.epoch, "sender": d.0, "sender_device": d.1,
                    "client_message_id": body["client_message_id"], "ciphertext": body["ciphertext"],
                    "attachments": atts, "created_at": 2, "expires_at": null}));
                Ok(json!({"seq": seq, "id": format!("gm_{seq}"), "expires_at": null}))
            }
            ("POST", ["v1", "groups", _, "epoch-ack" | "read" | "resync"]) => Ok(json!({})),
            _ => panic!("fake postbox has no route {method} {path}"),
        }
    }
}

#[derive(Clone)]
struct Fake {
    hub: Arc<Mutex<Hub>>,
    mailbox: String,
}

impl Transport for Fake {
    fn mailbox(&self) -> &str {
        &self.mailbox
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        device: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, ApiError> {
        self.hub
            .lock()
            .unwrap()
            .route(&self.mailbox, method, path, device, body)
    }
    async fn upload(&self, _path: &str, _device: &str, bytes: Vec<u8>) -> Result<Value, ApiError> {
        let mut hub = self.hub.lock().unwrap();
        let id = hub.id("gat_");
        hub.blobs.insert(id.clone(), bytes);
        Ok(json!({ "id": id }))
    }
    async fn download(&self, path: &str, _device: &str) -> Result<Vec<u8>, ApiError> {
        let id = path.rsplit('/').next().unwrap();
        self.hub
            .lock()
            .unwrap()
            .blobs
            .get(id)
            .cloned()
            .ok_or(err(404, "not_found"))
    }
}

fn fake(hub: &Arc<Mutex<Hub>>, n: u8) -> Fake {
    let seed = [n; 32];
    let mailbox = gc::mailbox_for_seed(&seed);
    hub.lock().unwrap().seeds.insert(mailbox.clone(), seed);
    Fake {
        hub: hub.clone(),
        mailbox,
    }
}

async fn client(hub: &Arc<Mutex<Hub>>, n: u8, dir: &Path) -> GroupClient<Fake> {
    let mut c = GroupClient::open(fake(hub, n), dir).unwrap();
    c.ensure_device().await.unwrap();
    c
}

fn texts(c: &GroupClient<Fake>, gid: &str) -> Vec<String> {
    c.entries(gid)
        .into_iter()
        .filter(|e| e.kind == "message")
        .filter_map(|e| e.text)
        .collect()
}

/// A creates a group inviting B; B accepts; A's device adds B's; B joins from the Welcome.
async fn two_member_group(
    hub: &Arc<Mutex<Hub>>,
    a_dir: &Path,
    b_dir: &Path,
) -> (GroupClient<Fake>, GroupClient<Fake>, String) {
    let mut a = client(hub, 1, a_dir).await;
    let mut b = client(hub, 2, b_dir).await;
    let created =
        a.t.call(
            "POST",
            "/v1/groups",
            None,
            Some(json!({"name": "crew", "invite": [b.t.mailbox]})),
        )
        .await
        .unwrap();
    let gid = created["group"]["id"].as_str().unwrap().to_string();
    a.activate(&gid).await.unwrap();
    assert!(
        hub.lock().unwrap().groups[&gid].active,
        "first commit activates"
    );
    b.t.call(
        "POST",
        &format!("/v1/groups/{gid}/invitation"),
        None,
        Some(json!({"accept": true})),
    )
    .await
    .unwrap();
    a.sync_all().await.unwrap();
    let report = b.sync_all().await.unwrap();
    assert_eq!(report.joined, vec![gid.clone()]);
    (a, b, gid)
}

#[tokio::test]
async fn create_invite_join_chat_and_attach() {
    let hub = Arc::new(Mutex::new(Hub::default()));
    let (ad, bd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, mut b, gid) = two_member_group(&hub, ad.path(), bd.path()).await;

    let file = b"quarterly numbers".to_vec();
    let seq = a
        .send(
            &gid,
            "hello \u{1b}[31mcrew",
            vec![Upload {
                name: "q.txt".into(),
                media_type: "text/plain".into(),
                bytes: file.clone(),
            }],
            true,
        )
        .await
        .unwrap();
    // The server stored ciphertext only.
    let stored = hub.lock().unwrap().groups[&gid]
        .items
        .last()
        .unwrap()
        .clone();
    assert!(!stored.to_string().contains("hello"));
    assert!(!hub.lock().unwrap().blobs.values().any(|v| v == &file));

    b.sync_all().await.unwrap();
    assert_eq!(texts(&b, &gid), vec!["hello \u{1b}[31mcrew".to_string()]);
    assert_eq!(clean("hello \u{1b}[31mcrew"), "hello \u{fffd}[31mcrew");
    let (meta, plain) = b.download(&gid, seq, 0).await.unwrap();
    assert_eq!(meta.name, "q.txt");
    assert_eq!(plain, file);

    b.send(&gid, "hi back", vec![], true).await.unwrap();
    a.sync_all().await.unwrap();
    assert_eq!(
        texts(&a, &gid),
        vec!["hello \u{1b}[31mcrew".to_string(), "hi back".to_string()]
    );
    // B's join was shown to A as the verified commit, with B's device fingerprint.
    assert!(b.entries(&gid).iter().all(|e| e.kind != "undecryptable"));
}

#[tokio::test]
async fn state_survives_restart_and_epoch_conflict_reencrypts() {
    let hub = Arc::new(Mutex::new(Hub::default()));
    let (ad, bd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, b, gid) = two_member_group(&hub, ad.path(), bd.path()).await;
    a.send(&gid, "before restart", vec![], true).await.unwrap();
    drop(b);

    // Reopened from disk: the plaintext it decrypted is still here, the ratchet continues.
    let mut b = GroupClient::open(fake(&hub, 2), bd.path()).unwrap();
    b.sync_all().await.unwrap();
    assert_eq!(texts(&b, &gid), vec!["before restart".to_string()]);
    let mut b = GroupClient::open(fake(&hub, 2), bd.path()).unwrap();
    assert_eq!(texts(&b, &gid), vec!["before restart".to_string()]);

    // B rotates its keys; A, not caught up, encrypts at the old epoch, is refused and retries.
    b.process_pending(&gid, true).await.unwrap();
    let epoch = hub.lock().unwrap().groups[&gid].epoch;
    a.send(&gid, "after rotation", vec![], false).await.unwrap();
    assert_eq!(a.device.epoch(&gid).unwrap(), epoch);
    b.sync_all().await.unwrap();
    assert_eq!(
        texts(&b, &gid),
        vec!["before restart".to_string(), "after rotation".to_string()]
    );
}

#[tokio::test]
async fn concurrent_commit_loses_then_discards_and_retries() {
    let hub = Arc::new(Mutex::new(Hub::default()));
    let (ad, bd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, mut b, gid) = two_member_group(&hub, ad.path(), bd.path()).await;
    let start = hub.lock().unwrap().groups[&gid].epoch;
    b.process_pending(&gid, true).await.unwrap(); // wins epoch `start`
    a.process_pending(&gid, true).await.unwrap(); // built on `start`: refused, catches up, retries
    assert_eq!(hub.lock().unwrap().groups[&gid].epoch, start + 2);
    assert_eq!(a.device.epoch(&gid).unwrap(), start + 2);
    b.sync_all().await.unwrap();
    assert_eq!(b.device.epoch(&gid).unwrap(), start + 2);
}

#[tokio::test]
async fn removal_is_committed_before_posting_and_the_removed_device_reads_nothing_after() {
    let hub = Arc::new(Mutex::new(Hub::default()));
    let (ad, bd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (mut a, mut b, gid) = two_member_group(&hub, ad.path(), bd.path()).await;
    let b_mailbox = b.t.mailbox.clone();
    hub.lock()
        .unwrap()
        .groups
        .get_mut(&gid)
        .unwrap()
        .members
        .insert(b_mailbox, "removed".into());

    // The post first meets key_activation_pending, commits the removal, then goes through.
    a.send(&gid, "secret", vec![], true).await.unwrap();
    assert!(
        b.sync_group(&gid).await.is_err(),
        "no server access after removal"
    );
    // Even with the ciphertext in hand, B's keys are from before the removal commit.
    let ct = {
        let hub = hub.lock().unwrap();
        let item = hub.groups[&gid].items.last().unwrap().clone();
        unb64(item["ciphertext"].as_str().unwrap()).unwrap()
    };
    assert!(b.device.process_incoming(&gid, &ct).is_err());
    assert!(!texts(&b, &gid).contains(&"secret".to_string()));
}

#[tokio::test]
async fn a_second_device_needs_a_person_and_says_so() {
    let hub = Arc::new(Mutex::new(Hub {
        second_device_needs_human: true,
        ..Hub::default()
    }));
    let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let _first = client(&hub, 1, d1.path()).await;
    let mut second = GroupClient::open(fake(&hub, 1), d2.path()).unwrap();
    let e = second.ensure_device().await.unwrap_err().to_string();
    assert!(e.contains("human_required") && e.contains("person"), "{e}");
}

#[test]
fn log_is_encrypted_and_needs_its_key() {
    let mut log = Log::default();
    log.groups
        .entry("grp_x".into())
        .or_default()
        .items
        .push(notice_entry(1, 0, "plaintext marker".into()));
    let key = [7u8; 32];
    let sealed = seal_log(&key, &log).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("plaintext marker"));
    let back = open_log(&key, &sealed).unwrap();
    assert_eq!(
        back.groups["grp_x"].items[0].text.as_deref(),
        Some("plaintext marker")
    );
    assert!(open_log(&[8u8; 32], &sealed).is_err());
}

#[cfg(unix)]
#[test]
fn state_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.key");
    write_private(&path, b"k").unwrap();
    write_private(&path, b"k2").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"k2");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn join_targets_and_identifiers_are_checked() {
    assert_eq!(
        parse_join_target("https://inbox.pigeonpost.dev/g/grp_abc#tok123").unwrap(),
        ("grp_abc".to_string(), Some("tok123".to_string()))
    );
    assert_eq!(
        parse_join_target("grp_abc").unwrap(),
        ("grp_abc".to_string(), None)
    );
    assert!(parse_join_target("grp_../../v1/x").is_err());
    assert!(check_group_id("not-a-group").is_err());
    assert!(check_token("a/b").is_err());
    assert_eq!(unb64(&b64(b"\x00\x01\xffabc")).unwrap(), b"\x00\x01\xffabc");
    assert!(new_device_id().starts_with("cli-") && new_device_id().len() == 16);
}
