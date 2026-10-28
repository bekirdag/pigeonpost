//! End-to-end tests for Phase 3a groups over real sockets, with real MLS devices
//! (`pigeonpost-groupcrypto`) as the clients: the server only ever sees ciphertext.

use crate::*;
use pigeonpost_groupcrypto::{Device, DeviceRef, Incoming};
use reqwest::Client;
use rusqlite::params;
use serde_json::{json, Value};

pub(crate) const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
pub(crate) const CALLS: &str = "svc_calls_0123456789abcdef0123456789abcdef";
const ISSUER: &str = "https://auth.example/realms/x";

pub(crate) struct Server {
    pub public: String,
    pub internal: String,
    pub http: Client,
    pub state: AppState,
}

pub(crate) async fn serve() -> Server {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_CALLS", CALLS),
    ]));
    state.blobs = Some(Arc::new(blobs::Blobs::for_test("groups-e2e")));
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_url = format!("http://{}", public.local_addr().unwrap());
    let internal_url = format!("http://{}", internal.local_addr().unwrap());
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let internal_app = internal_api::router(state.clone())
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, internal_app).await.unwrap() });
    Server {
        public: public_url,
        internal: internal_url,
        http: Client::new(),
        state,
    }
}

fn enc(s: &str) -> String {
    s.replace('/', "%2F")
}

impl Server {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        mailbox: Option<&str>,
        device: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let base = if path.starts_with("/internal/") {
            &self.internal
        } else {
            &self.public
        };
        let mut req = self
            .http
            .request(method, format!("{base}{path}"))
            .bearer_auth(token);
        if let Some(m) = mailbox {
            req = req.header("x-pigeonpost-identity", m);
        }
        if let Some(d) = device {
            req = req.header("x-pigeonpost-device", d);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub(crate) async fn flag(&self, name: &str, account: &str) {
        let (status, _) = self
            .call(
                reqwest::Method::PUT,
                &format!("/internal/v1/features/{name}"),
                OPS,
                None,
                None,
                Some(json!({ "scope": "account", "account_id": account, "enabled": true })),
            )
            .await;
        assert_eq!(status, 200, "flag {name}");
    }
}

/// A person (member session) with one hosted mailbox and one MLS device.
pub(crate) struct Member {
    pub token: String,
    pub api_key: String,
    pub address: String,
    device: Device,
    device_id: String,
    cursor: std::collections::HashMap<String, i64>,
    received: Vec<String>,
}

pub(crate) async fn member(srv: &Server, name: &str) -> Member {
    let account = format!("acct_{name}");
    let sub = format!("sub-{name}");
    srv.state
        .store
        .account_for_sub(sub.clone(), account.clone(), now_unix())
        .await
        .unwrap();
    let (api_key, key) = new_api_key();
    srv.state
        .store
        .add_api_key(account.clone(), key, now_unix())
        .await
        .unwrap();
    let v = do_create_identity(&srv.state, Some(account.clone()), None, None)
        .await
        .unwrap();
    let address = v["address"].as_str().unwrap().to_string();
    srv.flag("groups", &account).await;
    srv.flag("disappearing", &account).await;
    srv.flag("account_events", &account).await;
    let token = oidc::test_member_token(ISSUER, &sub);
    let device_id = format!("dev-{name}");
    let device = enroll(srv, &token, &address, &device_id).await;
    Member {
        token,
        api_key,
        address,
        device,
        device_id,
        cursor: Default::default(),
        received: Vec::new(),
    }
}

async fn enroll(srv: &Server, token: &str, address: &str, device_id: &str) -> Device {
    let mut device = Device::new(address, device_id).unwrap();
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            "/v1/mls/devices",
            token,
            Some(address),
            None,
            Some(json!({ "device_id": device_id, "signature_key": b64_encode(&device.signature_public_key()) })),
        )
        .await;
    assert_eq!(s, 201, "register device: {v}");
    device
        .install_attestation(
            &b64_decode(v["mailbox_public_key"].as_str().unwrap()).unwrap(),
            &b64_decode(v["attestation"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
    let kps: Vec<String> = device
        .generate_key_packages(5)
        .unwrap()
        .iter()
        .map(|k| b64_encode(k))
        .collect();
    let lr = b64_encode(&device.generate_last_resort_key_package().unwrap());
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            "/v1/mls/key-packages",
            token,
            Some(address),
            Some(device_id),
            Some(json!({ "request_key": format!("kp-{}", rand_hex(4)), "key_packages": kps, "last_resort": lr })),
        )
        .await;
    assert_eq!(s, 200, "upload key packages: {v}");
    assert_eq!(v["unclaimed"], 5);
    device
}

impl Member {
    pub(crate) async fn req(
        &self,
        srv: &Server,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        srv.call(
            method,
            path,
            &self.token,
            Some(&self.address),
            Some(&self.device_id),
            body,
        )
        .await
    }

    pub(crate) async fn create(&mut self, srv: &Server, name: &str, extra: Value) -> String {
        let mut body = json!({ "request_key": rand_hex(6), "name": name });
        if let Value::Object(m) = extra {
            for (k, v) in m {
                body[k] = v;
            }
        }
        let (s, v) = self
            .req(srv, reqwest::Method::POST, "/v1/groups", Some(body))
            .await;
        assert_eq!(s, 201, "create: {v}");
        let gid = v["group"]["id"].as_str().unwrap().to_string();
        assert_eq!(v["group"]["activation_state"], "provisioning");
        self.device.create_group(&gid).unwrap();
        // Activation: the creator's first commit (a self-update when nobody else is ready).
        self.process_pending(srv, &gid).await;
        let (s, v) = self
            .req(
                srv,
                reqwest::Method::GET,
                &format!("/v1/groups/{gid}"),
                None,
            )
            .await;
        assert_eq!(s, 200);
        assert_eq!(v["activation_state"], "active", "{v}");
        self.cursor
            .insert(gid.clone(), v["last_seq"].as_i64().unwrap());
        gid
    }

    async fn member_revision(&self, srv: &Server, gid: &str, who: &str) -> Value {
        let (_, v) = self
            .req(
                srv,
                reqwest::Method::GET,
                &format!("/v1/groups/{gid}/members"),
                None,
            )
            .await;
        v["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["address"] == who)
            .unwrap()["revision"]
            .clone()
    }

    async fn post_commit(
        &self,
        srv: &Server,
        gid: &str,
        crypto: &Value,
        bundle: &pigeonpost_groupcrypto::CommitBundle,
        rk: &str,
    ) -> (u16, Value) {
        self.req(
            srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/commits"),
            Some(json!({
                "request_key": rk, "epoch": crypto["epoch"], "commit_id": bundle.commit_id,
                "commit": b64_encode(&bundle.commit), "envelope_signature": b64_encode(&bundle.envelope_signature),
                "adds": [], "removes": [], "expected_revision": crypto["revision"],
            })),
        )
        .await
    }

    pub(crate) async fn group(&self, srv: &Server, gid: &str) -> Value {
        self.req(
            srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}"),
            None,
        )
        .await
        .1
    }

    /// Claim key packages for pending adds, commit adds and removes; returns the status.
    pub(crate) async fn process_pending(&mut self, srv: &Server, gid: &str) -> u16 {
        let (s, crypto) = self
            .req(
                srv,
                reqwest::Method::GET,
                &format!("/v1/groups/{gid}/crypto"),
                None,
            )
            .await;
        assert_eq!(s, 200, "crypto: {crypto}");
        let pending = crypto["pending"].as_array().cloned().unwrap_or_default();
        let mailboxes: Vec<String> = pending
            .iter()
            .filter(|p| p["kind"] == "add")
            .map(|p| p["mailbox"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut kps = Vec::new();
        let mut adds = Vec::new();
        if !mailboxes.is_empty() {
            let (s, v) = self
                .req(
                    srv,
                    reqwest::Method::POST,
                    &format!("/v1/groups/{gid}/key-packages/claim"),
                    Some(json!({ "request_key": rand_hex(6), "mailboxes": mailboxes })),
                )
                .await;
            assert_eq!(s, 200, "claim: {v}");
            for c in v["claims"].as_array().unwrap() {
                kps.push(b64_decode(c["key_package"].as_str().unwrap()).unwrap());
                adds.push(json!({ "mailbox": c["mailbox"], "device_id": c["device_id"], "key_package_ref": c["key_package_ref"] }));
            }
        }
        let removes: Vec<DeviceRef> = pending
            .iter()
            .filter(|p| p["kind"] == "remove")
            .map(|p| DeviceRef {
                mailbox: p["mailbox"].as_str().unwrap().into(),
                device_id: p["device_id"].as_str().unwrap().into(),
            })
            .collect();
        let bundle = self.device.commit(gid, &kps, &removes).unwrap();
        let (s, v) = self
            .req(
                srv,
                reqwest::Method::POST,
                &format!("/v1/groups/{gid}/commits"),
                Some(json!({
                    "request_key": rand_hex(6),
                    "epoch": crypto["epoch"],
                    "commit_id": bundle.commit_id,
                    "commit": b64_encode(&bundle.commit),
                    "envelope_signature": b64_encode(&bundle.envelope_signature),
                    "welcome": bundle.welcome.as_ref().map(|w| b64_encode(w)),
                    "adds": adds,
                    "removes": removes.iter().map(|r| json!({ "mailbox": r.mailbox, "device_id": r.device_id })).collect::<Vec<_>>(),
                    "expected_revision": crypto["revision"],
                })),
            )
            .await;
        if s == 201 {
            self.device.confirm_commit(gid).unwrap();
            self.cursor.insert(gid.into(), v["seq"].as_i64().unwrap());
        } else {
            self.device.discard_commit(gid).unwrap();
        }
        s
    }

    pub(crate) async fn accept_welcomes(&mut self, srv: &Server) {
        let (s, v) = self
            .req(srv, reqwest::Method::GET, "/v1/mls/welcomes", None)
            .await;
        assert_eq!(s, 200, "{v}");
        for w in v["welcomes"].as_array().unwrap() {
            let joined = self
                .device
                .join_from_welcome(&b64_decode(w["welcome"].as_str().unwrap()).unwrap())
                .unwrap();
            self.cursor
                .insert(joined.group_id.clone(), w["seq"].as_i64().unwrap());
            let (s, _) = self
                .req(
                    srv,
                    reqwest::Method::POST,
                    &format!("/v1/mls/welcomes/{}/ack", w["welcome_id"].as_str().unwrap()),
                    Some(json!({ "request_key": rand_hex(6), "joined": true })),
                )
                .await;
            assert_eq!(s, 200);
        }
    }

    async fn send(&mut self, srv: &Server, gid: &str, text: &str) -> (u16, Value) {
        let cmid = rand_hex(8);
        let out = self
            .device
            .encrypt_message(gid, &cmid, text.as_bytes(), &[])
            .unwrap();
        self.post_raw(
            srv,
            gid,
            &cmid,
            out.epoch as i64,
            &out.ciphertext,
            &out.envelope_signature,
        )
        .await
    }

    async fn send_edit(
        &mut self,
        srv: &Server,
        gid: &str,
        text: &str,
        edit_of: i64,
    ) -> (u16, Value) {
        let body = self.edit_request(gid, text, edit_of);
        self.req(
            srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/messages"),
            Some(body),
        )
        .await
    }

    fn edit_request(&mut self, gid: &str, text: &str, edit_of: i64) -> Value {
        let cmid = rand_hex(8);
        let plaintext =
            json!({ "v": 1, "type": "text", "text": text, "edit_of": edit_of.to_string() })
                .to_string();
        let out = self
            .device
            .encrypt_message(gid, &cmid, plaintext.as_bytes(), &[])
            .unwrap();
        json!({
            "request_key": format!("rk-{cmid}"), "client_message_id": cmid,
            "epoch": out.epoch as i64, "enc_scheme": pigeonpost_groupcrypto::ENC_SCHEME,
            "ciphertext": b64_encode(&out.ciphertext), "envelope_signature": b64_encode(&out.envelope_signature),
            "edit_of": edit_of,
        })
    }

    async fn set_timer(&self, srv: &Server, gid: &str, seconds: Option<i64>) {
        let revision = self.group(srv, gid).await["revision"].clone();
        let (status, body) = self
            .req(
                srv,
                reqwest::Method::PATCH,
                &format!("/v1/groups/{gid}"),
                Some(json!({ "expected_revision": revision, "ephemeral_secs": seconds })),
            )
            .await;
        assert_eq!(status, 200, "{body}");
    }

    async fn post_raw(
        &self,
        srv: &Server,
        gid: &str,
        cmid: &str,
        epoch: i64,
        ct: &[u8],
        sig: &[u8],
    ) -> (u16, Value) {
        self.req(
            srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/messages"),
            Some(json!({
                "request_key": format!("rk-{cmid}"),
                "client_message_id": cmid,
                "epoch": epoch,
                "enc_scheme": pigeonpost_groupcrypto::ENC_SCHEME,
                "ciphertext": b64_encode(ct),
                "envelope_signature": b64_encode(sig),
            })),
        )
        .await
    }

    /// Fetch and process everything after the cursor; returns the status of the fetch.
    async fn sync(&mut self, srv: &Server, gid: &str) -> u16 {
        let after = *self.cursor.get(gid).unwrap_or(&0);
        let (s, v) = self
            .req(
                srv,
                reqwest::Method::GET,
                &format!("/v1/groups/{gid}/messages?after_seq={after}&limit=100"),
                None,
            )
            .await;
        if s != 200 {
            return s;
        }
        for item in v["items"].as_array().unwrap() {
            let seq = item["seq"].as_i64().unwrap();
            let bytes = match item["type"].as_str().unwrap() {
                "application" => item["ciphertext"].as_str().map(|c| b64_decode(c).unwrap()),
                "commit" => Some(b64_decode(item["commit"].as_str().unwrap()).unwrap()),
                _ => None,
            };
            if let Some(bytes) = bytes {
                match self.device.process_incoming(gid, &bytes).unwrap() {
                    Incoming::Message(m) => self.received.push(String::from_utf8(m.body).unwrap()),
                    Incoming::Commit(_) | Incoming::OwnCommitConfirmed(_) | Incoming::Own => {}
                    Incoming::HistoryOffer(_) => {}
                }
            }
            self.cursor.insert(gid.into(), seq);
        }
        s
    }
}

/// Alice creates and activates; Bob and Carol are invited, Bob accepts and is added by Alice's
/// device, Carol declines.
pub(crate) async fn three(srv: &Server) -> (Member, Member, Member, String) {
    let mut alice = member(srv, &format!("alice{}", rand_hex(3))).await;
    let mut bob = member(srv, &format!("bob{}", rand_hex(3))).await;
    let carol = member(srv, &format!("carol{}", rand_hex(3))).await;
    let gid = alice
        .create(
            srv,
            "crew",
            json!({ "invite": [bob.address, carol.address] }),
        )
        .await;
    // Invitations became visible at activation.
    let (s, v) = bob
        .req(srv, reqwest::Method::GET, "/v1/groups?state=invited", None)
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["groups"][0]["my"]["state"], "invited", "{v}");
    assert!(
        v["groups"][0].get("epoch").is_none(),
        "invitees see a preview only"
    );
    let (s, v) = bob
        .req(
            srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/invitation"),
            Some(json!({ "accept": true })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["group"]["my"]["state"], "keying");
    let (s, _) = carol
        .req(
            srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/invitation"),
            Some(json!({ "accept": false })),
        )
        .await;
    assert_eq!(s, 200);
    // Keying members get no history.
    let (s, v) = bob
        .req(
            srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages"),
            None,
        )
        .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["error"], "key_activation_pending");
    assert_eq!(alice.process_pending(srv, &gid).await, 201);
    bob.accept_welcomes(srv).await;
    assert_eq!(bob.group(srv, &gid).await["my"]["state"], "active");
    (alice, bob, carol, gid)
}

#[tokio::test]
async fn create_invite_activate_chat_round_trip() {
    let srv = serve().await;
    let (mut alice, mut bob, carol, gid) = three(&srv).await;
    let (s, v) = alice.send(&srv, &gid, "hello bob").await;
    assert_eq!(s, 201, "{v}");
    let (s, _) = bob.send(&srv, &gid, "hi alice").await;
    assert_eq!(s, 201);
    assert_eq!(bob.sync(&srv, &gid).await, 200);
    assert_eq!(alice.sync(&srv, &gid).await, 200);
    assert_eq!(bob.received, vec!["hello bob"]);
    assert_eq!(alice.received, vec!["hi alice"]);
    // The member list is right for everyone; Carol has no relationship left.
    let (_, v) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/members"),
            None,
        )
        .await;
    let states: Vec<(String, String)> = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["address"].as_str().unwrap().to_string(),
                m["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(states.len(), 2, "{v}");
    let (s, _) = carol
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}"),
            None,
        )
        .await;
    assert_eq!(s, 404);
    // Unread counts follow the read cursor.
    let g = bob.group(&srv, &gid).await;
    assert_eq!(g["my"]["unread"], 1, "{g}");
    let last = g["last_seq"].as_i64().unwrap();
    bob.req(
        &srv,
        reqwest::Method::POST,
        &format!("/v1/groups/{gid}/read"),
        Some(json!({ "seq": last })),
    )
    .await;
    assert_eq!(bob.group(&srv, &gid).await["my"]["unread"], 0);
    // The server stored ciphertext only.
    let stored: Vec<Vec<u8>> = srv
        .state
        .store
        .group_read(
            move |c: &rusqlite::Connection| -> groups::GResult<Vec<Vec<u8>>> {
                let mut st =
                    c.prepare("SELECT body_blob FROM group_messages WHERE body_blob IS NOT NULL")?;
                let rows = st.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            },
        )
        .await
        .unwrap();
    assert!(stored
        .iter()
        .all(|b| !String::from_utf8_lossy(b).contains("hello bob")));
}

#[tokio::test]
async fn group_metadata_pages_redact_payloads_without_delivery_receipts() {
    let srv = serve().await;
    let (mut alice, bob, carol, gid) = three(&srv).await;
    srv.flag("receipts", "acct_alice").await;
    let (status, posted) = alice.send(&srv, &gid, "metadata is not a delivery").await;
    assert_eq!(status, 201, "{posted}");
    let seq = posted["seq"].as_i64().unwrap();
    assert_eq!(
        alice.send(&srv, &gid, "second metadata page item").await.0,
        201
    );
    let delivered = |rows: Vec<(String, Option<String>, i64, i64, bool)>| {
        rows.into_iter().find(|r| r.0 == bob.address).unwrap().2
    };
    let before = delivered(
        srv.state
            .store
            .group_receipt_rows(gid.clone(), alice.address.clone())
            .await
            .unwrap(),
    );
    let path = format!("/v1/groups/{gid}/messages?metadata_only=true&limit=100");
    let (status, metadata) = bob.req(&srv, reqwest::Method::GET, &path, None).await;
    assert_eq!(status, 200, "{metadata}");
    let items = metadata["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["seq"] == seq));
    assert!(items
        .iter()
        .all(|i| i["ciphertext"].is_null() && i["commit"].is_null()));
    let (status, owner_metadata) = alice.req(&srv, reqwest::Method::GET, &path, None).await;
    assert_eq!(status, 200, "{owner_metadata}");
    assert!(owner_metadata["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["type"] == "commit"));
    assert!(owner_metadata["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["ciphertext"].is_null() && i["commit"].is_null()));
    assert_eq!(
        delivered(
            srv.state
                .store
                .group_receipt_rows(gid.clone(), alice.address.clone())
                .await
                .unwrap()
        ),
        before,
        "metadata reconciliation must not manufacture delivery"
    );
    let (status, page) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("{path}&after_seq={}&before_seq={}", seq - 1, seq + 1),
            None,
        )
        .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["seq"], seq);
    let (status, limited) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages?metadata_only=true&limit=1"),
            None,
        )
        .await;
    assert_eq!(status, 200, "{limited}");
    assert_eq!(limited["items"].as_array().unwrap().len(), 1);
    assert_eq!(limited["more"], true);
    let (status, _) = carol.req(&srv, reqwest::Method::GET, &path, None).await;
    let normal_denial = carol
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages"),
            None,
        )
        .await
        .0;
    assert_eq!(
        status, normal_denial,
        "metadata access requires the same active membership"
    );
    assert_eq!(status, 403);
    let (status, normal) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages?limit=100"),
            None,
        )
        .await;
    assert_eq!(status, 200, "{normal}");
    assert!(normal["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["seq"] == seq && i["ciphertext"].is_string()));
    let (_, owner_normal) = alice
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages?limit=100"),
            None,
        )
        .await;
    assert!(owner_normal["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["type"] == "commit" && i["commit"].is_string()));
    assert!(
        delivered(
            srv.state
                .store
                .group_receipt_rows(gid.clone(), alice.address.clone())
                .await
                .unwrap()
        ) >= seq
    );
    let (status, deleted) = alice
        .req(
            &srv,
            reqwest::Method::DELETE,
            &format!("/v1/groups/{gid}/messages/{seq}"),
            None,
        )
        .await;
    assert_eq!(status, 200, "{deleted}");
    let (_, refreshed) = bob.req(&srv, reqwest::Method::GET, &path, None).await;
    assert!(refreshed["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["seq"] == seq && i["deleted_at"].is_number()));
}

#[tokio::test]
async fn authorization_matrix() {
    let srv = serve().await;
    let (mut alice, mut bob, carol, gid) = three(&srv).await;
    let rev = alice.group(&srv, &gid).await["revision"].clone();
    // Agents and API keys never change settings (human_required).
    let (s, v) = srv
        .call(
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}"),
            &alice.api_key,
            Some(&alice.address),
            None,
            Some(json!({ "expected_revision": rev, "post_policy": "admins" })),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("human_required")),
        "{v}"
    );
    // A member is not an admin.
    let (s, v) = bob
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}"),
            Some(json!({ "expected_revision": rev, "post_policy": "admins" })),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("not_admin")));
    // Stale revision.
    let (s, v) = alice
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}"),
            Some(json!({ "expected_revision": 1, "post_policy": "admins" })),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("revision_conflict")));
    // Announcement mode.
    let (s, v) = alice
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}"),
            Some(json!({ "expected_revision": rev, "post_policy": "admins" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = bob.send(&srv, &gid, "may I?").await;
    assert_eq!((s, v["error"].as_str()), (403, Some("posting_restricted")));
    // Non-members see nothing, even with the id.
    let (s, _) = carol
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages"),
            None,
        )
        .await;
    assert_eq!(s, 403);
    let (s, v) = carol
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/join"),
            Some(json!({})),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("invite_required")));
    // Members cannot invite unless allowed; promote Bob, then he can.
    let (s, v) = bob
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/members"),
            Some(json!({ "request_key": "i1", "invite": [carol.address] })),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("not_admin")));
    let r = alice.member_revision(&srv, &gid, &bob.address).await;
    let (s, v) = alice
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}/members/{}", enc(&bob.address)),
            Some(json!({ "expected_revision": r, "role": "admin" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = bob
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/members"),
            Some(json!({ "request_key": "i2", "invite": [carol.address] })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["results"][0]["status"], "invited");
    // Admins cannot demote the owner or other admins; only the owner demotes (D-07).
    let (s, v) = bob
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}/members/{}", enc(&alice.address)),
            Some(json!({ "expected_revision": 1, "role": "member" })),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("owner_only")));
    // Ownership transfer to the admin, under a revision check.
    let rev = alice.group(&srv, &gid).await["revision"].clone();
    let (s, v) = alice
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/transfer"),
            Some(json!({ "to": bob.address, "expected_revision": rev })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(bob.group(&srv, &gid).await["my"]["role"], "owner");
    // A non-owner cannot delete; the owner can, and everything stops.
    let (s, _) = alice
        .req(
            &srv,
            reqwest::Method::DELETE,
            &format!("/v1/groups/{gid}"),
            None,
        )
        .await;
    assert_eq!(s, 403);
    let (s, _) = bob
        .req(
            &srv,
            reqwest::Method::DELETE,
            &format!("/v1/groups/{gid}"),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = alice.send(&srv, &gid, "anyone?").await;
    assert_eq!((s, v["error"].as_str()), (410, Some("group_deleted")));
    let _ = &mut alice;
    let _ = &mut bob;
}

#[tokio::test]
async fn removal_stops_access_and_requires_a_key_update() {
    let srv = serve().await;
    let (mut alice, mut bob, _carol, gid) = three(&srv).await;
    alice.send(&srv, &gid, "before").await;
    bob.sync(&srv, &gid).await;
    let (_, v) = alice
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/members"),
            None,
        )
        .await;
    let r = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["address"] == bob.address.as_str())
        .unwrap()["revision"]
        .clone();
    let (s, v) = alice
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}/members/{}", enc(&bob.address)),
            Some(json!({ "expected_revision": r, "state": "removed" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // Server access stops at once.
    let (s, _) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages"),
            None,
        )
        .await;
    assert_eq!(s, 403);
    // Application sends pause until the removal is committed.
    let (s, v) = alice.send(&srv, &gid, "too early").await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("key_activation_pending"))
    );
    assert_eq!(alice.process_pending(&srv, &gid).await, 201);
    let (s, v) = alice.send(&srv, &gid, "after").await;
    assert_eq!(s, 201, "{v}");
    // Even holding the ciphertext, Bob's device cannot read the new epoch.
    let ct: Vec<u8> = {
        let gid2 = gid.clone();
        srv.state
            .store
            .group_read(move |c: &rusqlite::Connection| -> groups::GResult<Vec<u8>> {
                Ok(c.query_row("SELECT body_blob FROM group_messages WHERE group_id = ?1 ORDER BY seq DESC LIMIT 1", [gid2], |r| r.get(0))?)
            })
            .await
            .unwrap()
    };
    assert!(bob.device.process_incoming(&gid, &ct).is_err());
    // A removed member's device can no longer post either.
    let (s, _) = bob.send(&srv, &gid, "still here?").await;
    assert_eq!(s, 403);
    assert!(bob.received.iter().all(|m| m != "after"));
}

#[tokio::test]
async fn commit_races_replays_and_epochs() {
    let srv = serve().await;
    let (mut alice, mut bob, _carol, gid) = three(&srv).await;
    // Both build a self-update on the same epoch; the first wins.
    let crypto = alice
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/crypto"),
            None,
        )
        .await
        .1;
    let a = alice.device.self_update(&gid).unwrap();
    let b = bob.device.self_update(&gid).unwrap();
    let (s1, _) = alice.post_commit(&srv, &gid, &crypto, &a, "ra").await;
    let (s2, v2) = bob.post_commit(&srv, &gid, &crypto, &b, "rb").await;
    assert_eq!(s1, 201);
    assert_eq!((s2, v2["error"].as_str()), (409, Some("revision_conflict")));
    assert!(v2["current_epoch"].is_number());
    alice.device.confirm_commit(&gid).unwrap();
    bob.device.discard_commit(&gid).unwrap();
    assert_eq!(bob.sync(&srv, &gid).await, 200);
    // A message encrypted for the old epoch is refused with the current epoch.
    let cmid = "stale-1";
    let old = bob.device.encrypt_message(&gid, cmid, b"x", &[]).unwrap();
    // Alice moves the epoch again before Bob posts.
    assert_eq!(alice.process_pending(&srv, &gid).await, 201);
    let (s, v) = bob
        .post_raw(
            &srv,
            &gid,
            cmid,
            old.epoch as i64,
            &old.ciphertext,
            &old.envelope_signature,
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("epoch_conflict")),
        "{v}"
    );
    bob.sync(&srv, &gid).await;
    let fresh = bob.device.encrypt_message(&gid, cmid, b"x", &[]).unwrap();
    let (s, v) = bob
        .post_raw(
            &srv,
            &gid,
            cmid,
            fresh.epoch as i64,
            &fresh.ciphertext,
            &fresh.envelope_signature,
        )
        .await;
    assert_eq!(s, 201, "{v}");
    // Replay of the same request returns the original; a different body under the same id is refused.
    let (s2, v2) = bob
        .post_raw(
            &srv,
            &gid,
            cmid,
            fresh.epoch as i64,
            &fresh.ciphertext,
            &fresh.envelope_signature,
        )
        .await;
    assert_eq!((s2, &v2["seq"]), (201, &v["seq"]));
    let other = bob.device.encrypt_message(&gid, cmid, b"y", &[]).unwrap();
    let (s3, v3) = bob
        .req(&srv, reqwest::Method::POST, &format!("/v1/groups/{gid}/messages"), Some(json!({
            "request_key": "another-key", "client_message_id": cmid, "epoch": other.epoch,
            "enc_scheme": pigeonpost_groupcrypto::ENC_SCHEME, "ciphertext": b64_encode(&other.ciphertext),
            "envelope_signature": b64_encode(&other.envelope_signature),
        })))
        .await;
    assert_eq!(
        (s3, v3["error"].as_str()),
        (409, Some("idempotency_conflict"))
    );
    // A ciphertext signed by one device cannot be posted as another mailbox.
    let forged = alice
        .device
        .encrypt_message(&gid, "forge-1", b"z", &[])
        .unwrap();
    let (s4, _) = bob
        .post_raw(
            &srv,
            &gid,
            "forge-1",
            forged.epoch as i64,
            &forged.ciphertext,
            &forged.envelope_signature,
        )
        .await;
    assert_eq!(s4, 400);
}

#[tokio::test]
async fn links_public_join_and_second_device() {
    let srv = serve().await;
    let mut alice = member(&srv, &format!("al{}", rand_hex(3))).await;
    let mut dave = member(&srv, &format!("da{}", rand_hex(3))).await;
    let gid = alice
        .create(&srv, "open", json!({ "join_policy": "public" }))
        .await;
    let (s, v) = alice
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/link"),
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let token = v["link_token"].as_str().unwrap().to_string();
    // Unlisted: no link, no join.
    let (s, v) = dave
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/join"),
            Some(json!({})),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("invalid_link")));
    let (s, v) = dave
        .req(
            &srv,
            reqwest::Method::POST,
            "/v1/groups/resolve",
            Some(json!({ "group_id": gid, "link_token": token })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["can_join"], true);
    // Reset: the old token stops working.
    let (_, v) = alice
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/link"),
            Some(json!({ "enabled": true })),
        )
        .await;
    let fresh = v["link_token"].as_str().unwrap().to_string();
    let (s, v) = dave
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/join"),
            Some(json!({ "link_token": token })),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("invalid_link")));
    let (s, v) = dave
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/join"),
            Some(json!({ "link_token": fresh })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(alice.process_pending(&srv, &gid).await, 201);
    dave.accept_welcomes(&srv).await;
    alice.send(&srv, &gid, "welcome dave").await;
    dave.sync(&srv, &gid).await;
    assert_eq!(dave.received, vec!["welcome dave"]);
    // A second device for Dave: an API key may not add it, a person may; it gets no history.
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            "/v1/mls/devices",
            &dave.api_key,
            Some(&dave.address),
            None,
            Some(json!({ "device_id": "dev-2", "signature_key": b64_encode(&[7u8; 32]) })),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("human_required")),
        "{v}"
    );
    let second = enroll(&srv, &dave.token, &dave.address, "dave-phone").await;
    let mut dave2 = Member {
        token: dave.token.clone(),
        api_key: dave.api_key.clone(),
        address: dave.address.clone(),
        device: second,
        device_id: "dave-phone".into(),
        cursor: Default::default(),
        received: Vec::new(),
    };
    // Co-members see the device change as a notice.
    let (_, page) = alice
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages?after_seq=0"),
            None,
        )
        .await;
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "device_added"),
        "{page}"
    );
    assert_eq!(alice.process_pending(&srv, &gid).await, 201);
    dave2.accept_welcomes(&srv).await;
    dave2.sync(&srv, &gid).await;
    assert!(dave2.received.is_empty(), "a new device gets no history");
    alice.send(&srv, &gid, "both devices").await;
    dave.sync(&srv, &gid).await;
    dave2.sync(&srv, &gid).await;
    assert_eq!(dave2.received, vec!["both devices"]);
}

#[tokio::test]
async fn disappearing_messages_expire_for_groups_and_threads() {
    let srv = serve().await;
    let (mut alice, mut bob, _carol, gid) = three(&srv).await;
    let rev = alice.group(&srv, &gid).await["revision"].clone();
    let (s, v) = alice
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}"),
            Some(json!({ "expected_revision": rev, "ephemeral_secs": 300 })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = alice.send(&srv, &gid, "gone soon").await;
    assert_eq!(s, 201);
    assert!(v["expires_at"].as_i64().unwrap() > now_unix() as i64);
    // Run the worker as if six minutes passed.
    let later = now_unix() + 360;
    let swept: groups::GResult<groups::Sweep> = srv
        .state
        .store
        .group_tx(move |tx| groups::sweep_in(tx, later, 1000))
        .await;
    assert_eq!(swept.unwrap().expired_messages, 1);
    bob.sync(&srv, &gid).await;
    assert!(bob.received.is_empty(), "expired content is never served");
    // Direct thread timer: set by one side, applies to both, and expires.
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            "/v1/send",
            &alice.api_key,
            Some(&alice.address),
            None,
            Some(json!({ "to": bob.address, "body": "dm one", "from": alice.address })),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (_, threads) = srv
        .call(
            reqwest::Method::GET,
            &format!("/v1/threads?identity={}", enc(&bob.address)),
            &bob.api_key,
            None,
            None,
            None,
        )
        .await;
    let tid = threads["threads"][0]["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, v) = srv
        .call(
            reqwest::Method::PATCH,
            &format!("/v1/threads/{tid}"),
            &bob.api_key,
            None,
            None,
            Some(json!({ "ephemeral_secs": 3600, "identity": bob.address })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, threads) = srv
        .call(
            reqwest::Method::GET,
            &format!("/v1/threads?identity={}", enc(&alice.address)),
            &alice.api_key,
            None,
            None,
            None,
        )
        .await;
    assert_eq!(threads["threads"][0]["ephemeral_secs"], 3600, "{threads}");
    srv.call(
        reqwest::Method::POST,
        "/v1/send",
        &alice.api_key,
        Some(&alice.address),
        None,
        Some(json!({ "to": bob.address, "body": "dm two", "from": alice.address })),
    )
    .await;
    let (_, inbox) = srv
        .call(
            reqwest::Method::GET,
            &format!("/v1/inbox?identity={}", enc(&bob.address)),
            &bob.api_key,
            None,
            None,
            None,
        )
        .await;
    let msgs = inbox["messages"].as_array().unwrap();
    assert!(
        msgs.iter()
            .any(|m| m["body"] == "dm two" && m["expires_at"].is_number()),
        "{inbox}"
    );
    assert!(
        msgs.iter()
            .any(|m| m["body"] == "dm one" && m["expires_at"].is_null()),
        "only new messages get the timer"
    );
    let done = srv
        .state
        .store
        .expire_direct_messages(now_unix() + 7200, 1000)
        .await
        .unwrap();
    assert_eq!(
        done.expired.iter().map(|e| e.3.len()).sum::<usize>(),
        2,
        "both copies of dm two"
    );
    let (_, inbox) = srv
        .call(
            reqwest::Method::GET,
            &format!("/v1/inbox?identity={}", enc(&bob.address)),
            &bob.api_key,
            None,
            None,
            None,
        )
        .await;
    assert!(!inbox["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["body"] == "dm two"));
    let _ = &mut alice;
}

#[tokio::test]
async fn group_edit_keeps_expiry_after_disabling_the_timer() {
    let srv = serve().await;
    let (mut alice, _bob, _carol, gid) = three(&srv).await;
    alice.set_timer(&srv, &gid, Some(300)).await;
    let (status, original) = alice.send(&srv, &gid, "original").await;
    assert_eq!(status, 201);
    assert!(original["expires_at"].is_number());
    alice.set_timer(&srv, &gid, None).await;
    let (status, edit) = alice
        .send_edit(&srv, &gid, "changed", original["seq"].as_i64().unwrap())
        .await;
    assert_eq!(status, 201, "{edit}");
    assert_eq!(
        edit["expires_at"], original["expires_at"],
        "editing must not retain disappearing content"
    );
    alice.set_timer(&srv, &gid, Some(3600)).await;
    let (status, later) = alice
        .send_edit(&srv, &gid, "changed again", edit["seq"].as_i64().unwrap())
        .await;
    assert_eq!(status, 201, "{later}");
    assert_eq!(
        later["expires_at"], original["expires_at"],
        "a longer timer cannot extend a chain"
    );
}

#[tokio::test]
async fn group_edit_keeps_no_expiry_after_enabling_the_timer() {
    let srv = serve().await;
    let (mut alice, _bob, _carol, gid) = three(&srv).await;
    let (_, original) = alice.send(&srv, &gid, "original").await;
    assert!(original["expires_at"].is_null());
    alice.set_timer(&srv, &gid, Some(300)).await;
    let (status, edit) = alice
        .send_edit(&srv, &gid, "changed", original["seq"].as_i64().unwrap())
        .await;
    assert_eq!(status, 201, "{edit}");
    assert!(
        edit["expires_at"].is_null(),
        "a timer change affects new messages, not versions"
    );
}

#[tokio::test]
async fn group_edit_chain_cannot_restart_the_original_window() {
    let srv = serve().await;
    let (mut alice, _bob, _carol, gid) = three(&srv).await;
    let (_, original) = alice.send(&srv, &gid, "original").await;
    let root = original["seq"].as_i64().unwrap();
    let (status, edit) = alice.send_edit(&srv, &gid, "changed once", root).await;
    assert_eq!(status, 201);
    let group = gid.clone();
    let at = now_unix().saturating_sub(86_400) as i64;
    srv.state
        .store
        .group_tx(move |tx| -> groups::GResult<()> {
            tx.execute(
                "UPDATE group_messages SET created_at = ?3 WHERE group_id = ?1 AND seq = ?2",
                params![group, root, at],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (status, body) = alice
        .send_edit(&srv, &gid, "too late", edit["seq"].as_i64().unwrap())
        .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"], "not_editable");
}

#[tokio::test]
async fn group_edit_refuses_an_expired_or_missing_original() {
    let srv = serve().await;
    let (mut alice, _bob, _carol, gid) = three(&srv).await;
    let (_, original) = alice.send(&srv, &gid, "original").await;
    let root = original["seq"].as_i64().unwrap();
    let (status, edit) = alice.send_edit(&srv, &gid, "changed", root).await;
    assert_eq!(status, 201);
    let group = gid.clone();
    let expired = now_unix() as i64 - 1;
    srv.state
        .store
        .group_tx(move |tx| -> groups::GResult<()> {
            tx.execute(
                "UPDATE group_messages SET expires_at = ?3 WHERE group_id = ?1 AND seq = ?2",
                params![group, root, expired],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let target = edit["seq"].as_i64().unwrap();
    let (status, body) = alice.send_edit(&srv, &gid, "after expiry", target).await;
    assert_eq!(status, 403, "{body}");
    let group = gid.clone();
    srv.state
        .store
        .group_tx(move |tx| -> groups::GResult<()> {
            tx.execute(
                "DELETE FROM group_messages WHERE group_id = ?1 AND seq = ?2",
                params![group, root],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (status, body) = alice.send_edit(&srv, &gid, "after pruning", target).await;
    assert_eq!(status, 403, "{body}");
}

#[tokio::test]
async fn group_edit_deleting_any_version_erases_the_whole_chain_only() {
    let srv = serve().await;
    let (mut alice, bob, _carol, gid) = three(&srv).await;
    let (_, original) = alice.send(&srv, &gid, "original").await;
    let root = original["seq"].as_i64().unwrap();
    let (status, edit) = alice.send_edit(&srv, &gid, "changed once", root).await;
    assert_eq!(status, 201);
    let middle = edit["seq"].as_i64().unwrap();
    let (status, final_edit) = alice.send_edit(&srv, &gid, "changed twice", middle).await;
    assert_eq!(status, 201);
    let leaf = final_edit["seq"].as_i64().unwrap();
    let (_, unrelated) = alice.send(&srv, &gid, "unrelated").await;
    let other_group = alice.create(&srv, "Other group", json!({})).await;
    // Use the same sequence number in another group to verify the recursive delete's scope.
    loop {
        let (_, other) = alice.send(&srv, &other_group, "different group").await;
        if other["seq"].as_i64().unwrap() >= root {
            break;
        }
    }
    let path = format!("/v1/groups/{gid}/messages/{middle}");
    assert_eq!(
        bob.req(&srv, reqwest::Method::DELETE, &path, None).await.0,
        403
    );
    let (status, body) = alice.req(&srv, reqwest::Method::DELETE, &path, None).await;
    assert_eq!(status, 200, "{body}");
    let group = gid.clone();
    let rows = srv.state.store.group_tx(move |tx| -> groups::GResult<Vec<(i64, bool, bool)>> {
        let mut stmt = tx.prepare("SELECT seq, body_blob IS NULL, deleted_at IS NOT NULL FROM group_messages WHERE group_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![group], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }).await.unwrap();
    for seq in [root, middle, leaf] {
        assert!(
            rows.contains(&(seq, true, true)),
            "version {seq} was not erased: {rows:?}"
        );
    }
    assert!(rows.contains(&(unrelated["seq"].as_i64().unwrap(), false, false)));
    assert_eq!(body["seqs"], json!([root, middle, leaf]));
    let other = other_group.clone();
    let untouched: bool = srv.state.store.group_tx(move |tx| -> groups::GResult<bool> {
        Ok(tx.query_row("SELECT body_blob IS NOT NULL AND deleted_at IS NULL FROM group_messages WHERE group_id = ?1 AND seq = ?2", params![other, root], |r| r.get(0))?)
    }).await.unwrap();
    assert!(untouched, "another group's matching sequence stays intact");
    assert_eq!(
        alice
            .req(&srv, reqwest::Method::DELETE, &path, None)
            .await
            .0,
        200,
        "deletion remains idempotent"
    );
}

#[tokio::test]
async fn group_edit_racing_deletion_cannot_resurrect_a_version() {
    let srv = serve().await;
    let (mut alice, _bob, _carol, gid) = three(&srv).await;
    let (_, original) = alice.send(&srv, &gid, "original").await;
    let root = original["seq"].as_i64().unwrap();
    let request = alice.edit_request(&gid, "concurrent edit", root);
    let post_path = format!("/v1/groups/{gid}/messages");
    let delete_path = format!("/v1/groups/{gid}/messages/{root}");
    let (edit, deleted) = tokio::join!(
        alice.req(&srv, reqwest::Method::POST, &post_path, Some(request)),
        alice.req(&srv, reqwest::Method::DELETE, &delete_path, None),
    );
    assert_eq!(deleted.0, 200, "{deleted:?}");
    assert!([201, 403].contains(&edit.0), "{edit:?}");
    if edit.0 == 403 {
        assert_eq!(edit.1["error"], "not_editable");
    }
    let live: i64 = srv.state.store.group_tx(move |tx| -> groups::GResult<i64> {
        Ok(tx.query_row("SELECT COUNT(*) FROM group_messages WHERE group_id = ?1 AND (body_blob IS NOT NULL OR deleted_at IS NULL)", params![gid], |r| r.get(0))?)
    }).await.unwrap();
    assert_eq!(live, 0, "either transaction order must erase every version");
}

#[tokio::test]
async fn group_edit_deletion_releases_all_versions_attachments_without_other_groups() {
    let srv = serve().await;
    let (mut alice, mut bob, _carol, first) = three(&srv).await;
    let second = alice.create(&srv, "Other files", json!({})).await;
    let sealed = pigeonpost_groupcrypto::seal_attachment(b"versioned secret file").unwrap();
    let mut posted = Vec::new();
    for gid in [&first, &second] {
        // Deliberately share the same sealed bytes to exercise global blob reference counts.
        let response = srv
            .http
            .post(format!("{}/v1/groups/{gid}/attachments", srv.public))
            .bearer_auth(&alice.token)
            .header("x-pigeonpost-identity", &alice.address)
            .header("x-pigeonpost-device", &alice.device_id)
            .body(sealed.sealed.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 201);
        let upload: Value = response.json().await.unwrap();
        let id = upload["id"].as_str().unwrap().to_owned();
        let cmid = rand_hex(8);
        let encrypted = alice
            .device
            .encrypt_message(
                gid,
                &cmid,
                b"see file",
                std::slice::from_ref(&sealed.reference),
            )
            .unwrap();
        let (status, body) = alice.req(&srv, reqwest::Method::POST, &format!("/v1/groups/{gid}/messages"), Some(json!({
            "request_key": format!("rk-{cmid}"), "client_message_id": cmid, "epoch": encrypted.epoch,
            "enc_scheme": pigeonpost_groupcrypto::ENC_SCHEME, "ciphertext": b64_encode(&encrypted.ciphertext),
            "envelope_signature": b64_encode(&encrypted.envelope_signature), "attachments": [id],
        }))).await;
        assert_eq!(status, 201, "{body}");
        posted.push((id, body["seq"].as_i64().unwrap()));
    }
    // Claiming an existing file is allowed only as a text edit of its current version.
    let (_, unrelated) = alice.send(&srv, &first, "unrelated message").await;
    let (_, bobs) = bob.send(&srv, &first, "Bob's message").await;
    for (who, gid, target) in [
        (&mut bob, &first, bobs["seq"].as_i64().unwrap()),
        (&mut alice, &second, posted[1].1),
    ] {
        let mut request = who.edit_request(gid, "cannot take another file", target);
        request["attachments"] = json!([posted[0].0]);
        let (status, body) = who
            .req(
                &srv,
                reqwest::Method::POST,
                &format!("/v1/groups/{gid}/messages"),
                Some(request),
            )
            .await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"], "bad_attachment");
    }
    let mut wrong_target = alice.edit_request(
        &first,
        "cannot take another message's file",
        unrelated["seq"].as_i64().unwrap(),
    );
    wrong_target["attachments"] = json!([posted[0].0]);
    let (status, body) = alice
        .req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{first}/messages"),
            Some(wrong_target),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "bad_attachment");
    let mut latest = posted[0].1;
    for caption in ["new caption", "newest caption"] {
        let mut request = alice.edit_request(&first, caption, latest);
        request["attachments"] = json!([posted[0].0]);
        let (status, body) = alice
            .req(
                &srv,
                reqwest::Method::POST,
                &format!("/v1/groups/{first}/messages"),
                Some(request),
            )
            .await;
        assert_eq!(status, 201, "{body}");
        latest = body["seq"].as_i64().unwrap();
    }
    let group = first.clone();
    let attachment = posted[0].0.clone();
    let (count, at): (i64, i64) = srv.state.store.group_read(move |c: &rusqlite::Connection| -> groups::GResult<_> {
        Ok(c.query_row("SELECT COUNT(*), MAX(message_seq) FROM group_attachments WHERE group_id=?1 AND id=?2",
                       params![group, attachment], |r| Ok((r.get(0)?, r.get(1)?)))?)
    }).await.unwrap();
    assert_eq!(
        (count, at),
        (1, latest),
        "caption edits move one existing attachment rather than duplicating uploads"
    );
    let (status, body) = alice
        .req(
            &srv,
            reqwest::Method::DELETE,
            &format!("/v1/groups/{first}/messages/{}", latest),
            None,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        alice
            .req(
                &srv,
                reqwest::Method::GET,
                &format!("/v1/groups/{first}/attachments/{}", posted[0].0),
                None
            )
            .await
            .0,
        404
    );
    let response = srv
        .http
        .get(format!(
            "{}/v1/groups/{second}/attachments/{}",
            srv.public, posted[1].0
        ))
        .bearer_auth(&alice.token)
        .header("x-pigeonpost-identity", &alice.address)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        200,
        "another group's blob must survive"
    );
    let bytes = response.bytes().await.unwrap();
    assert_eq!(
        pigeonpost_groupcrypto::open_attachment(&bytes, &sealed.reference).unwrap(),
        b"versioned secret file"
    );
    let first_id = first.clone();
    let refs: i64 = srv
        .state
        .store
        .group_tx(move |tx| -> groups::GResult<i64> {
            Ok(tx.query_row(
                "SELECT COUNT(*) FROM group_attachments WHERE group_id = ?1",
                params![first_id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        refs, 0,
        "no attachment metadata from an earlier version remains"
    );
}

#[tokio::test]
async fn quota_and_attachments_are_bound_to_members() {
    let srv = serve().await;
    let (mut alice, mut bob, carol, gid) = three(&srv).await;
    let sealed = pigeonpost_groupcrypto::seal_attachment(b"secret file").unwrap();
    let resp = srv
        .http
        .post(format!("{}/v1/groups/{gid}/attachments", srv.public))
        .bearer_auth(&alice.token)
        .header("x-pigeonpost-identity", &alice.address)
        .header("x-pigeonpost-device", &alice.device_id)
        .body(sealed.sealed.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let att: Value = resp.json().await.unwrap();
    let att_id = att["id"].as_str().unwrap().to_string();
    // Unclaimed: invisible to Bob.
    let (s, _) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/attachments/{att_id}"),
            None,
        )
        .await;
    assert_eq!(s, 404);
    let cmid = "with-file";
    let out = alice
        .device
        .encrypt_message(
            &gid,
            cmid,
            b"see file",
            std::slice::from_ref(&sealed.reference),
        )
        .unwrap();
    let (s, v) = alice
        .req(&srv, reqwest::Method::POST, &format!("/v1/groups/{gid}/messages"), Some(json!({
            "request_key": "rk-file", "client_message_id": cmid, "epoch": out.epoch,
            "enc_scheme": pigeonpost_groupcrypto::ENC_SCHEME, "ciphertext": b64_encode(&out.ciphertext),
            "envelope_signature": b64_encode(&out.envelope_signature), "attachments": [att_id],
        })))
        .await;
    assert_eq!(s, 201, "{v}");
    let bytes = srv
        .http
        .get(format!(
            "{}/v1/groups/{gid}/attachments/{att_id}",
            srv.public
        ))
        .bearer_auth(&bob.token)
        .header("x-pigeonpost-identity", &bob.address)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        pigeonpost_groupcrypto::open_attachment(&bytes, &sealed.reference).unwrap(),
        b"secret file"
    );
    // A non-member cannot fetch it even with the id.
    let (s, _) = carol
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/attachments/{att_id}"),
            None,
        )
        .await;
    assert_eq!(s, 403);
    // Quota: fill the group past 100 MB and the next post is refused.
    let g2 = gid.clone();
    srv.state
        .store
        .group_tx(move |tx| -> groups::GResult<()> {
            tx.execute(
                "INSERT INTO group_messages (group_id, seq, id, client_message_id, sender, sender_device, kind, bytes, body_hash, created_at, epoch, enc_scheme)
                 VALUES (?1, 100000, 'gm_fill', 'fill', 'x', 'x', 'text', 104857600, 'x', 0, 0, 'x')",
                [g2],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (s, v) = bob.send(&srv, &gid, "over quota").await;
    assert_eq!((s, v["error"].as_str()), (409, Some("quota_exceeded")));
    let _ = &mut alice;
}

#[tokio::test]
async fn events_recheck_membership_and_preflight_allows_the_device_header() {
    let srv = serve().await;
    let (mut alice, bob, _carol, gid) = three(&srv).await;
    // CORS: a browser on the inbox may send x-pigeonpost-device to device routes.
    let resp = srv
        .http
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/v1/groups/{gid}/messages", srv.public),
        )
        .header("origin", "https://inbox.pigeonpost.dev")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "authorization, content-type, x-pigeonpost-identity, x-pigeonpost-device",
        )
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let allowed = resp.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        allowed.split(", ").any(|h| h == "x-pigeonpost-device"),
        "{allowed}"
    );
    // Remove Bob, then post: Bob's queued group.message must not be delivered.
    let (_, v) = alice
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/members"),
            None,
        )
        .await;
    let r = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["address"] == bob.address.as_str())
        .unwrap()["revision"]
        .clone();
    alice
        .req(
            &srv,
            reqwest::Method::PATCH,
            &format!("/v1/groups/{gid}/members/{}", enc(&bob.address)),
            Some(json!({ "expected_revision": r, "state": "removed" })),
        )
        .await;
    alice.process_pending(&srv, &gid).await;
    alice.send(&srv, &gid, "secret plans").await;
    let mut stream = srv
        .http
        .get(format!(
            "{}/v1/events?typed=1&last_event_id=0.0",
            srv.public
        ))
        .bearer_auth(&bob.token)
        .send()
        .await
        .unwrap();
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(800);
    while let Ok(Ok(Some(chunk))) = tokio::time::timeout_at(deadline, stream.chunk()).await {
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(
        seen.contains("group.member"),
        "lifecycle notices about himself still arrive: {seen}"
    );
    let after_removal = seen.split("member_removed").nth(1).unwrap_or("");
    assert!(!after_removal.contains("event: group.message"), "{seen}");
}

#[tokio::test]
async fn group_call_admission_is_for_active_members_only() {
    let srv = serve().await;
    let (alice, bob, carol, gid) = three(&srv).await;
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            "/internal/v1/group-call-admission",
            CALLS,
            None,
            None,
            Some(json!({ "group_id": gid, "from": { "mailbox": alice.address }, "kind": "video" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["allowed"], true);
    let members: Vec<&str> = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["address"].as_str().unwrap())
        .collect();
    assert_eq!(members, vec![bob.address.as_str()]);
    let (_, v) = srv
        .call(reqwest::Method::POST, "/internal/v1/group-call-admission", CALLS, None, None, Some(json!({ "group_id": gid, "from": { "mailbox": carol.address }, "kind": "audio", "join": true })))
        .await;
    assert_eq!(v["allowed"], false);
    // Only the calls service may ask.
    let (s, _) = srv
        .call(
            reqwest::Method::POST,
            "/internal/v1/group-call-admission",
            OPS,
            None,
            None,
            Some(json!({ "group_id": gid, "from": { "mailbox": alice.address }, "kind": "audio" })),
        )
        .await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn groups_are_invisible_without_the_flag() {
    let srv = serve().await;
    let (api_key, key) = new_api_key();
    srv.state
        .store
        .create_account("acct_noflag".into(), key, now_unix())
        .await
        .unwrap();
    let v = do_create_identity(&srv.state, Some("acct_noflag".into()), None, None)
        .await
        .unwrap();
    let address = v["address"].as_str().unwrap();
    let (s, v) = srv
        .call(
            reqwest::Method::GET,
            "/v1/groups",
            &api_key,
            Some(address),
            None,
            None,
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("feature_disabled")));
}

#[tokio::test]
async fn hosted_mcp_tools_carry_metadata_and_ciphertext_only() {
    let srv = serve().await;
    let (mut alice, bob, _carol, gid) = three(&srv).await;
    alice.send(&srv, &gid, "for agents").await;
    let principal = principal_for_token(&srv.state, Some(&bob.api_key))
        .await
        .unwrap();
    let listed = mcp::call_tool_as(
        &srv.state,
        principal,
        json!({ "name": "list_pigeonpost_groups", "arguments": { "identity": bob.address } }),
    )
    .await
    .unwrap();
    let text = listed["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(text.contains(&gid), "{listed}");
    assert!(!text.contains("for agents"));
    let principal = principal_for_token(&srv.state, Some(&bob.api_key))
        .await
        .unwrap();
    let read = mcp::call_tool_as(
        &srv.state,
        principal,
        json!({ "name": "read_pigeonpost_group", "arguments": { "identity": bob.address, "group_id": gid } }),
    )
    .await
    .unwrap();
    let text = read["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        text.contains("ciphertext") && !text.contains("for agents"),
        "{read}"
    );
    let principal = principal_for_token(&srv.state, Some(&bob.api_key))
        .await
        .unwrap();
    let sent = mcp::call_tool_as(
        &srv.state,
        principal,
        json!({ "name": "send_pigeonpost_group_message", "arguments": { "identity": bob.address, "group_id": gid } }),
    )
    .await
    .unwrap();
    assert_eq!(sent["isError"], true, "{sent}");
    assert!(sent.to_string().contains("encryption_required"));
}

#[tokio::test]
async fn pruning_only_desyncs_devices_that_missed_a_pruned_commit() {
    let srv = serve().await;
    let (mut alice, mut bob, _carol, gid) = three(&srv).await;
    // Two more epochs, both devices acknowledge them, then prune.
    for _ in 0..2 {
        assert_eq!(alice.process_pending(&srv, &gid).await, 201);
        bob.sync(&srv, &gid).await;
    }
    for m in [&alice, &bob] {
        let epoch = m.group(&srv, &gid).await["epoch"].clone();
        m.req(
            &srv,
            reqwest::Method::POST,
            &format!("/v1/groups/{gid}/epoch-ack"),
            Some(json!({ "epoch": epoch })),
        )
        .await;
    }
    let now = now_unix();
    srv.state
        .store
        .group_tx(move |tx| -> groups::GResult<()> { mls::sweep_mls(tx, now) })
        .await
        .unwrap();
    assert!(alice.group(&srv, &gid).await["last_seq"].as_i64().unwrap() > 0);
    // Reading from the start is fine: nothing this device needs is gone.
    let (s, _) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages?after_seq=0"),
            None,
        )
        .await;
    assert_eq!(s, 200);
    // A device whose acknowledged epoch predates the oldest kept commit must resync.
    let g2 = gid.clone();
    let bob_addr = bob.address.clone();
    srv.state
        .store
        .group_tx(move |tx| -> groups::GResult<()> {
            tx.execute("UPDATE group_leaves SET acked_epoch = 0, added_epoch = 0 WHERE group_id = ?1 AND mailbox = ?2", rusqlite::params![g2, bob_addr])?;
            tx.execute("UPDATE groups SET pruned_through_seq = MAX(pruned_through_seq, 1) WHERE id = ?1", [g2.clone()])?;
            tx.execute("DELETE FROM group_handshakes WHERE group_id = ?1 AND epoch < 2", [g2])?;
            Ok(())
        })
        .await
        .unwrap();
    let (s, v) = bob
        .req(
            &srv,
            reqwest::Method::GET,
            &format!("/v1/groups/{gid}/messages?after_seq=0"),
            None,
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (410, Some("resync_required")),
        "{v}"
    );
}

/// Phase 9 load measurement, not a regression test: group fan-out at the largest enabled sizes.
/// Real MLS devices; the server stores one ciphertext per post and every member reads it.
///
///     GROUP_LOAD_SIZES=50,200,1000 cargo test --release -p pigeonpost-postbox \
///         group_fanout_load -- --ignored --nocapture
///
/// Prints one JSON line per size (`group_load {...}`); tests/load/README.md explains the numbers.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "load measurement; run explicitly"]
async fn group_fanout_load() {
    let sizes: Vec<usize> = std::env::var("GROUP_LOAD_SIZES")
        .unwrap_or_else(|_| "50,200".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let posts: usize = std::env::var("GROUP_LOAD_POSTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let pct = |v: &mut Vec<f64>, p: f64| -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[((p * v.len() as f64) as usize).min(v.len() - 1)]
    };
    for n in sizes {
        let srv = serve().await;
        let t_setup = std::time::Instant::now();
        let mut owner = member(&srv, &format!("own{n}{}", rand_hex(3))).await;
        let mut others = Vec::with_capacity(n - 1);
        for i in 0..n - 1 {
            others.push(member(&srv, &format!("m{n}x{i}{}", rand_hex(2))).await);
        }
        let gid = owner.create(&srv, "load", json!({})).await;
        // Invitations in batches of the per-request cap, then every invitee accepts.
        for chunk in others.chunks(crate::groups::INVITES_PER_REQUEST) {
            let addrs: Vec<&str> = chunk.iter().map(|m| m.address.as_str()).collect();
            let (s, v) = owner
                .req(
                    &srv,
                    reqwest::Method::POST,
                    &format!("/v1/groups/{gid}/members"),
                    Some(json!({ "request_key": rand_hex(6), "invite": addrs })),
                )
                .await;
            assert!(s == 200 || s == 201, "invite: {s} {v}");
        }
        for m in &others {
            let (s, v) = m
                .req(
                    &srv,
                    reqwest::Method::POST,
                    &format!("/v1/groups/{gid}/invitation"),
                    Some(json!({ "accept": true })),
                )
                .await;
            assert_eq!(s, 200, "accept: {v}");
        }
        // The owner's device adds pending members, at most 200 key packages per claim.
        let mut commits = 0;
        let mut last_added: Vec<String> = Vec::new();
        loop {
            let (s, crypto) = owner
                .req(
                    &srv,
                    reqwest::Method::GET,
                    &format!("/v1/groups/{gid}/crypto"),
                    None,
                )
                .await;
            assert_eq!(s, 200);
            let adds: Vec<String> = crypto["pending"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter(|p| p["kind"] == "add")
                .map(|p| p["mailbox"].as_str().unwrap().to_string())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .take(200)
                .collect();
            if adds.is_empty() {
                break;
            }
            last_added = adds.clone();
            let (s, v) = owner
                .req(
                    &srv,
                    reqwest::Method::POST,
                    &format!("/v1/groups/{gid}/key-packages/claim"),
                    Some(json!({ "request_key": rand_hex(6), "mailboxes": adds })),
                )
                .await;
            assert_eq!(s, 200, "claim: {v}");
            let mut kps = Vec::new();
            let mut add_refs = Vec::new();
            for c in v["claims"].as_array().unwrap() {
                kps.push(b64_decode(c["key_package"].as_str().unwrap()).unwrap());
                add_refs.push(json!({ "mailbox": c["mailbox"], "device_id": c["device_id"], "key_package_ref": c["key_package_ref"] }));
            }
            let bundle = owner.device.commit(&gid, &kps, &[]).unwrap();
            let (s, v) = owner
                .req(
                    &srv,
                    reqwest::Method::POST,
                    &format!("/v1/groups/{gid}/commits"),
                    Some(json!({
                        "request_key": rand_hex(6),
                        "epoch": crypto["epoch"],
                        "commit_id": bundle.commit_id,
                        "commit": b64_encode(&bundle.commit),
                        "envelope_signature": b64_encode(&bundle.envelope_signature),
                        "welcome": bundle.welcome.as_ref().map(|w| b64_encode(w)),
                        "adds": add_refs,
                        "removes": [],
                        "expected_revision": crypto["revision"],
                    })),
                )
                .await;
            assert_eq!(s, 201, "commit: {v}");
            owner.device.confirm_commit(&gid).unwrap();
            owner.cursor.insert(gid.clone(), v["seq"].as_i64().unwrap());
            commits += 1;
        }
        for m in others.iter_mut() {
            m.accept_welcomes(&srv).await;
        }
        let setup_s = t_setup.elapsed().as_secs_f64();

        // Posts: the owner and up to nine others take turns (the per-sender limit is 120/min).
        // The others are members from the owner's final commit: they hold the current epoch.
        let sender_idx: Vec<usize> = others
            .iter()
            .enumerate()
            .filter(|(_, m)| last_added.contains(&m.address))
            .map(|(k, _)| k)
            .take(9)
            .collect();
        let senders = sender_idx.len();
        let mut post_ms = Vec::new();
        // The owner's posts alone: request + server work + one encryption, no client sync.
        let mut owner_ms = Vec::new();
        let t_posts = std::time::Instant::now();
        for i in 0..posts {
            let t = std::time::Instant::now();
            let (s, v) = if i % (senders + 1) == 0 || senders == 0 {
                owner.send(&srv, &gid, &format!("load {i}")).await
            } else {
                let k = sender_idx[i % (senders + 1) - 1];
                others[k].sync(&srv, &gid).await;
                others[k].send(&srv, &gid, &format!("load {i}")).await
            };
            assert_eq!(s, 201, "post: {v}");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if i % (senders + 1) == 0 || senders == 0 {
                owner_ms.push(ms);
            }
            post_ms.push(ms);
        }
        let posts_s = t_posts.elapsed().as_secs_f64();

        // Fan-out read: every member fetches the page with the new posts, 64 at a time.
        let after = *others[0].cursor.get(&gid).unwrap_or(&0);
        let reqs: Vec<(String, String, String)> = others
            .iter()
            .map(|m| (m.token.clone(), m.address.clone(), m.device_id.clone()))
            .collect();
        let read_ms = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let t_read = std::time::Instant::now();
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(64));
        let mut tasks = Vec::new();
        for (token, address, device) in reqs {
            let http = srv.http.clone();
            let url = format!(
                "{}/v1/groups/{gid}/messages?after_seq={after}&limit=100",
                srv.public
            );
            let sem = sem.clone();
            let read_ms = read_ms.clone();
            tasks.push(tokio::spawn(async move {
                let _p = sem.acquire().await.unwrap();
                let t = std::time::Instant::now();
                let r = http
                    .get(url)
                    .bearer_auth(token)
                    .header("x-pigeonpost-identity", address)
                    .header("x-pigeonpost-device", device)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(r.status().as_u16(), 200);
                let _ = r.bytes().await.unwrap();
                read_ms
                    .lock()
                    .unwrap()
                    .push(t.elapsed().as_secs_f64() * 1000.0);
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let read_s = t_read.elapsed().as_secs_f64();
        // Correctness under load: a member from the final commit decrypts every post.
        let newest = sender_idx.first().copied().unwrap_or(others.len() - 1);
        let last = &mut others[newest];
        last.sync(&srv, &gid).await;
        let decrypted = last
            .received
            .iter()
            .filter(|b| b.starts_with("load "))
            .count();
        let mut reads = read_ms.lock().unwrap().clone();
        println!(
            "group_load {}",
            json!({
                "members": n, "commits_to_add": commits, "setup_s": (setup_s * 10.0).round() / 10.0,
                "posts": posts, "post_rps": (posts as f64 / posts_s * 10.0).round() / 10.0,
                "post_p50_ms": (pct(&mut post_ms, 0.5) * 10.0).round() / 10.0,
                "post_p99_ms": (pct(&mut post_ms, 0.99) * 10.0).round() / 10.0,
                "owner_post_p50_ms": (pct(&mut owner_ms, 0.5) * 10.0).round() / 10.0,
                "owner_post_max_ms": (pct(&mut owner_ms, 1.0) * 10.0).round() / 10.0,
                "fanout_reads": reads.len(), "fanout_read_s": (read_s * 100.0).round() / 100.0,
                "read_p50_ms": (pct(&mut reads, 0.5) * 10.0).round() / 10.0,
                "read_p99_ms": (pct(&mut reads, 0.99) * 10.0).round() / 10.0,
                "newest_member_decrypted": decrypted,
            })
        );
        assert!(
            decrypted >= posts - posts / (senders + 1) - 1,
            "newest member decrypts the posts"
        );
    }
}
