//! Phase 8 (agent SSH, SDS §11) over real sockets: the authorization matrix, the agent's control
//! stream, credential issuance, the broker's leg tokens, approval, host-key pinning and rotation,
//! revocation and timeouts. The SSH protocol itself (agentd's server, the CLI's client, the broker
//! seeing ciphertext only) is exercised end to end by `tests/e2e-ssh/`.

use crate::org_tests::{b64, enc, serve_with, Person, Srv, BILLING, OPS};
use crate::*;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::Method;
use serde_json::{json, Value};

const TUNNEL: &str = "svc_tunnel_0123456789abcdef0123456789abcdef";
const CA_SEED: [u8; 32] = [42; 32];

async fn serve() -> Srv {
    let mut srv = serve_with(|s| {
        // Each test server is its own client address, so these tests do not spend the
        // process-wide per-address limits (invitation acceptance) other test modules rely on.
        s.trusted_proxy_hops = 1;
        s.service_tokens = Arc::new(internal_api::tokens_for_test(&[
            ("POSTBOX_SVC_TOKEN_OPS", OPS),
            ("POSTBOX_SVC_TOKEN_BILLING", BILLING),
            ("POSTBOX_SVC_TOKEN_TUNNEL", TUNNEL),
        ]));
        s.shell = Arc::new(shell::Runtime::for_test(CA_SEED, "ws://127.0.0.1:9"));
    })
    .await;
    let mut ip = [0u8; 3];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut ip);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        format!("10.{}.{}.{}", ip[0], ip[1], ip[2]).parse().unwrap(),
    );
    srv.http = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    for flag in ["orgs", "groups", "org_api", "agent_ssh"] {
        srv.global_flag(flag).await;
    }
    srv
}

fn host_key(seed: u8) -> String {
    ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]))
        .public_key()
        .to_openssh()
        .unwrap()
}

fn client_key(seed: u8) -> String {
    host_key(seed.wrapping_add(100))
}

fn code(v: &(u16, Value)) -> (u16, &str) {
    (v.0, v.1["error"].as_str().unwrap_or(""))
}

/// The agent machine's side: its control stream, read in the background.
struct AgentSide {
    rx: tokio::sync::mpsc::Receiver<(String, Value)>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AgentSide {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl AgentSide {
    async fn connect(srv: &Srv, token: &str) -> AgentSide {
        let resp = srv
            .http
            .get(format!("{}/v1/shell/agent/events", srv.public))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "control stream");
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let task = tokio::spawn(async move {
            let mut resp = resp;
            let mut buf = String::new();
            while let Ok(Some(chunk)) = resp.chunk().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(i) = buf.find("\n\n") {
                    let frame = buf[..i].to_string();
                    buf.drain(..i + 2);
                    let kind = frame
                        .lines()
                        .find_map(|l| l.strip_prefix("event:"))
                        .map(|s| s.trim().to_string());
                    let data = frame
                        .lines()
                        .find_map(|l| l.strip_prefix("data:"))
                        .and_then(|d| serde_json::from_str::<Value>(d.trim()).ok());
                    if let (Some(k), Some(d)) = (kind, data) {
                        if tx.send((k, d)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut side = AgentSide { rx, task };
        let hello = side.next("shell.hello").await;
        assert!(hello["ca_public_key"]
            .as_str()
            .unwrap()
            .starts_with("ssh-ed25519 "));
        side
    }

    async fn next(&mut self, kind: &str) -> Value {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let got = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("no {kind} event"))
                .expect("stream ended");
            if got.0 == kind {
                return got.1;
            }
        }
    }
}

async fn register(srv: &Srv, token: &str, seed: u8, mode: &str) -> (u16, Value) {
    srv.call(
        Method::PUT,
        "/v1/shell/agent",
        Some(token),
        Some(json!({ "enabled": true, "mode": mode, "max_sessions": 2,
                      "host_key": host_key(seed), "version": "test" })),
    )
    .await
}

async fn open(srv: &Srv, token: &str, agent: &str, seed: u8) -> (u16, Value) {
    srv.post(
        "/v1/shell/sessions",
        token,
        json!({ "agent": agent, "client_public_key": client_key(seed), "cols": 100, "rows": 30 }),
    )
    .await
}

async fn leg(srv: &Srv, sid: &str, token: &str) -> (u16, Value) {
    srv.call(
        Method::POST,
        "/internal/v1/shell/legs",
        Some(TUNNEL),
        Some(json!({ "session_id": sid, "leg_token": token })),
    )
    .await
}

async fn check(srv: &Srv, sid: &str, spliced: bool) -> Value {
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/shell/check",
            Some(TUNNEL),
            Some(
                json!({ "sessions": [{ "session_id": sid, "bytes_client": 10,
                                        "bytes_agent": 20, "spliced": spliced }] }),
            ),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    v
}

async fn session_state(srv: &Srv, sid: &str) -> (String, Option<String>) {
    let sid = sid.to_string();
    srv.state
        .store
        .group_read(
            move |c| -> crate::groups::GResult<(String, Option<String>)> {
                Ok(c.query_row(
                    "SELECT state, close_reason FROM ssh_sessions WHERE id = ?1",
                    [&sid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            },
        )
        .await
        .unwrap()
}

/// An org with agent `/acme/agent1` enrolled on a "machine" (a device credential), the machine's
/// control stream open and its host key registered.
struct OrgFixture {
    srv: Srv,
    owner: Person,
    org: String,
    agent: String,
    cred: String,
}

async fn org_fixture() -> OrgFixture {
    let srv = serve().await;
    let owner = srv.person("owner").await;
    let org = srv.org(&owner, "acme", 10).await;
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/me/mailbox"),
            &owner.token,
            json!({ "handle": "boss" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (_, a) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": "agent1" }),
        )
        .await;
    let agent = a["agent"]["address"].as_str().unwrap().to_string();
    let (s, e) = srv
        .post(
            &format!("/v1/orgs/{org}/agents/{}/enrollments", enc(&agent)),
            &owner.strong,
            json!({ "ttl_hours": 1 }),
        )
        .await;
    assert_eq!(s, 201, "{e}");
    let code = e["code"].as_str().unwrap().to_string();
    let k = SigningKey::from_bytes(&[5u8; 32]);
    let (s, r) = srv
        .call(
            Method::POST,
            "/v1/enrollments/redeem",
            None,
            Some(json!({ "code": code, "device_public_key": b64(k.verifying_key().as_bytes()),
                "signature": b64(&k.sign(format!("pigeonpost-enroll-v1\0{code}").as_bytes()).to_bytes()),
                "label": "box" })),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    let cred = r["credential"].as_str().unwrap().to_string();
    OrgFixture {
        srv,
        owner,
        org,
        agent,
        cred,
    }
}

impl OrgFixture {
    async fn settings(&self, v: Value) -> (u16, Value) {
        self.srv
            .patch(
                &format!("/v1/orgs/{}", self.org),
                &self.owner.strong,
                json!({ "settings": v }),
            )
            .await
    }
    async fn switch(&self, on: bool) -> (u16, Value) {
        self.srv
            .call(
                Method::PUT,
                &format!("/v1/orgs/{}/agents/{}/shell", self.org, enc(&self.agent)),
                Some(&self.owner.strong),
                Some(json!({ "enabled": on })),
            )
            .await
    }
    async fn delegate(&self, member: &str, caps: &[&str]) -> String {
        let (s, d) = self
            .srv
            .post(
                &format!("/v1/orgs/{}/delegations", self.org),
                &self.owner.token,
                json!({ "member_id": member, "target": "/acme/agent1", "capabilities": caps }),
            )
            .await;
        assert_eq!(s, 201, "{d}");
        d["delegations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["member_id"] == member || x["grantee_member_id"] == member)
            .unwrap_or(&d["delegations"][0])["id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

/// SDS §11.5 / §6.4: who may open a shell on an org agent, and who may not.
#[tokio::test]
async fn authorization_matrix() {
    let f = org_fixture().await;
    let srv = &f.srv;
    let mut machine = AgentSide::connect(srv, &f.cred).await;
    assert_eq!(register(srv, &f.cred, 1, "container").await.0, 200);

    let admin = srv.person("admin").await;
    let agent_admin = srv.person("agentadmin").await;
    let delegate = srv.person("delegate").await;
    let reader = srv.person("reader").await;
    let revoked = srv.person("revoked").await;
    let outsider = srv.person("outsider").await;
    srv.join(&f.org, &f.owner, &admin, "admin", &["admin"])
        .await;
    srv.join(&f.org, &f.owner, &agent_admin, "aa", &["agent_admin"])
        .await;
    let (dm, _) = srv.join(&f.org, &f.owner, &delegate, "dele", &[]).await;
    let (rm, _) = srv.join(&f.org, &f.owner, &reader, "reader", &[]).await;
    let (vm, _) = srv.join(&f.org, &f.owner, &revoked, "revoked", &[]).await;
    f.delegate(&dm, &["ssh"]).await;
    f.delegate(&rm, &["read_inbox", "enroll"]).await;
    let gone = f.delegate(&vm, &["ssh"]).await;
    assert_eq!(
        srv.call(
            Method::DELETE,
            &format!("/v1/orgs/{}/delegations/{gone}", f.org),
            Some(&f.owner.token),
            None
        )
        .await
        .0,
        200
    );

    // Off by default at the org level, then at the agent level.
    assert_eq!(
        code(&open(srv, &f.owner.strong, "/acme/agent1", 1).await),
        (403, "ssh_not_allowed")
    );
    // Turning it on needs a strong sign-in.
    let (s, _) = srv
        .patch(
            &format!("/v1/orgs/{}", f.org),
            &f.owner.token,
            json!({ "settings": { "ssh_allowed": true } }),
        )
        .await;
    assert_eq!(s, 401);
    assert_eq!(f.settings(json!({ "ssh_allowed": true })).await.0, 200);
    assert_eq!(
        code(&open(srv, &f.owner.strong, "/acme/agent1", 1).await),
        (403, "ssh_not_enabled")
    );
    // Only an agent-managing role switches it on, with step-up.
    let (s, v) = srv
        .call(
            Method::PUT,
            &format!("/v1/orgs/{}/agents/{}/shell", f.org, enc(&f.agent)),
            Some(&delegate.strong),
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(s, 403, "{v}");
    let (s, _) = srv
        .call(
            Method::PUT,
            &format!("/v1/orgs/{}/agents/{}/shell", f.org, enc(&f.agent)),
            Some(&agent_admin.token),
            Some(json!({ "enabled": true })),
        )
        .await;
    assert_eq!(s, 401, "enabling needs step-up");
    assert_eq!(f.switch(true).await.0, 200);

    // Allowed: the org owner and a live `ssh` delegate.
    let owner_session = open(srv, &f.owner.strong, "/acme/agent1", 1).await;
    assert_eq!(owner_session.0, 201, "{}", owner_session.1);
    assert_eq!(owner_session.1["session"]["via"], "org_owner");
    let ev = machine.next("shell.request").await;
    assert_eq!(ev["via"], "org_owner");
    let dele = open(srv, &delegate.strong, "/acme/agent1", 2).await;
    assert_eq!(dele.0, 201, "{}", dele.1);
    assert_eq!(dele.1["session"]["via"], "delegation");
    // Two is this machine's limit.
    assert_eq!(
        code(&open(srv, &delegate.strong, "/acme/agent1", 3).await),
        (409, "agent_busy")
    );
    for sid in [
        owner_session.1["session_id"].as_str().unwrap(),
        dele.1["session_id"].as_str().unwrap(),
    ] {
        assert_eq!(
            srv.call(
                Method::DELETE,
                &format!("/v1/shell/sessions/{sid}"),
                Some(&f.owner.token),
                None
            )
            .await
            .0,
            200
        );
    }

    // Refused: admins and agent admins get no shell implicitly; other capabilities are not `ssh`;
    // a revoked delegation is nothing; a non-member does not even learn the agent exists.
    for (who, want) in [
        (&admin, "delegation_required"),
        (&agent_admin, "delegation_required"),
        (&reader, "delegation_required"),
        (&revoked, "delegation_required"),
    ] {
        assert_eq!(
            code(&open(srv, &who.strong, "/acme/agent1", 4).await),
            (403, want),
            "{}",
            who.account
        );
    }
    assert_eq!(
        code(&open(srv, &outsider.strong, "/acme/agent1", 4).await),
        (404, "no_agent")
    );
    // Missing step-up: the RFC 9470 challenge, before anything else is decided.
    let (s, v) = open(srv, &delegate.token, "/acme/agent1", 4).await;
    assert_eq!((s, v["error"].as_str()), (401, Some("step_up_required")));
    let (s, _) = open(srv, &outsider.token, "/no/such/agent", 4).await;
    assert_eq!(s, 401, "the challenge comes first, whatever the agent");

    // Automation: an account API key, the owner's own; an org API key; another agent's mailbox
    // credential; the agent's own credential. None is a person.
    let (s, k) = srv.post("/v1/api-keys", &f.owner.token, json!({})).await;
    assert_eq!(s, 201, "{k}");
    let account_key = k["api_key"].as_str().unwrap().to_string();
    let (s, ok) = srv
        .post(
            &format!("/v1/orgs/{}/api-keys", f.org),
            &f.owner.strong,
            json!({ "name": "auto", "scopes": ["org:read", "agents:write"] }),
        )
        .await;
    assert_eq!(s, 201, "{ok}");
    let org_key = ok["secret"].as_str().unwrap().to_string();
    let (_, other) = srv
        .post("/v1/identities", &outsider.token, json!({ "label": "bot" }))
        .await;
    let other_agent = other["capability_token"].as_str().unwrap().to_string();
    for (cred, what) in [
        (&account_key, "account API key"),
        (&other_agent, "another agent"),
        (&f.cred, "the agent itself"),
    ] {
        assert_eq!(
            code(&open(srv, cred, "/acme/agent1", 5).await),
            (403, "human_required"),
            "{what}"
        );
    }
    let (s, _) = open(srv, &org_key, "/acme/agent1", 5).await;
    assert!(
        s == 401 || s == 403,
        "an org key is no /v1 credential at all ({s})"
    );

    // Nothing was created by any refusal.
    let n: i64 = srv
        .state
        .store
        .group_read(|c| -> crate::groups::GResult<i64> {
            Ok(c.query_row("SELECT COUNT(*) FROM ssh_sessions", [], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(n, 2);

    // The flag, per account.
    srv.call(
        Method::PUT,
        "/internal/v1/features/agent_ssh",
        Some(OPS),
        Some(json!({ "scope": "global", "enabled": false })),
    )
    .await;
    assert_eq!(
        code(&open(srv, &f.owner.strong, "/acme/agent1", 6).await),
        (404, "feature_disabled")
    );
}

/// The whole life of a personal agent's session: issuance, the certificate's bindings, one-time
/// legs, splice, close, and the metadata-only audit trail.
#[tokio::test]
async fn session_lifecycle_and_broker_legs() {
    let srv = serve().await;
    let me = srv.person("me").await;
    let (_, mb) = srv
        .post("/v1/identities", &me.token, json!({ "label": "box" }))
        .await;
    let agent = mb["address"].as_str().unwrap().to_string();
    let cap = mb["capability_token"].as_str().unwrap().to_string();

    // The agent's side takes only the agent's own credential.
    for cred in [&me.token, &me.strong] {
        assert_eq!(
            code(&register(&srv, cred, 1, "container").await),
            (403, "mailbox_credential_required")
        );
    }
    assert_eq!(
        code(&register(&srv, &cap, 1, "user").await),
        (400, "bad_mode")
    );
    // Not connected yet.
    let mut machine = AgentSide::connect(&srv, &cap).await;
    assert_eq!(
        code(&open(&srv, &me.strong, &agent, 1).await),
        (409, "shell_not_offered")
    );
    let (s, r) = register(&srv, &cap, 1, "allowlist").await;
    assert_eq!((s, r["status"].as_str()), (200, Some("ok")), "{r}");

    let (s, st) = srv
        .get(&format!("/v1/agents/{}/shell", enc(&agent)), &me.token)
        .await;
    assert_eq!(s, 200, "{st}");
    assert_eq!(
        (st["connected"].as_bool(), st["can_open"].as_bool()),
        (Some(true), Some(true))
    );
    assert_eq!(st["mode"], "allowlist");

    let (s, v) = open(&srv, &me.strong, &agent, 1).await;
    assert_eq!(s, 201, "{v}");
    let sid = v["session_id"].as_str().unwrap().to_string();
    assert_eq!(v["broker_url"], format!("ws://127.0.0.1:9/t/{sid}"));
    assert_eq!(v["host_key_fp"], st["host_key_fp"]);
    let ev = machine.next("shell.request").await;
    assert_eq!(ev["session_id"], sid);
    assert_eq!(ev["mode"], "allowlist");
    assert_ne!(ev["leg_token"], v["leg_token"], "one token per side");

    // The certificate: this CA, this key, this session only, five minutes, no forwarding.
    let (_, ca) = srv.call(Method::GET, "/v1/shell/ca", None, None).await;
    let ca_fp = ssh_key::PublicKey::from_openssh(ca["public_key"].as_str().unwrap())
        .unwrap()
        .fingerprint(ssh_key::HashAlg::Sha256);
    assert_eq!(ca["fingerprint"], ca_fp.to_string());
    let cert = ssh_key::Certificate::from_openssh(v["user_cert"].as_str().unwrap()).unwrap();
    let now = now_unix();
    cert.validate_at(now, [&ca_fp]).unwrap();
    assert!(cert
        .validate_at(now + shell::CERT_SECS + 5, [&ca_fp])
        .is_err());
    assert_eq!(cert.valid_principals(), [format!("pp-ssh-{sid}")]);
    let grant: Value = serde_json::from_str(cert.key_id()).unwrap();
    assert_eq!(
        (
            grant["sid"].as_str(),
            grant["agent"].as_str(),
            grant["via"].as_str()
        ),
        (Some(sid.as_str()), Some(agent.as_str()), Some("owner"))
    );
    assert_eq!(grant["host_fp"], v["host_key_fp"]);
    assert_eq!(
        cert.public_key(),
        ssh_key::PublicKey::from_openssh(&client_key(1))
            .unwrap()
            .key_data()
    );
    assert_eq!(
        format!("{:016x}", cert.serial()),
        ev["cert_serial"].as_str().unwrap()
    );

    // Legs: the broker's token only, each once, the right side.
    let client_leg = v["leg_token"].as_str().unwrap();
    let agent_leg = ev["leg_token"].as_str().unwrap();
    assert_eq!(
        srv.call(
            Method::POST,
            "/internal/v1/shell/legs",
            Some(OPS),
            Some(json!({ "session_id": sid, "leg_token": client_leg }))
        )
        .await
        .0,
        403,
        "another service may not spend legs"
    );
    assert_eq!(
        code(&leg(&srv, &sid, "lt_wrong").await),
        (403, "leg_invalid")
    );
    let (s, l) = leg(&srv, &sid, client_leg).await;
    assert_eq!((s, l["side"].as_str()), (200, Some("client")));
    assert_eq!(
        code(&leg(&srv, &sid, client_leg).await),
        (403, "leg_invalid")
    );
    assert_eq!(leg(&srv, &sid, agent_leg).await.1["side"], "agent");
    assert_eq!(
        session_state(&srv, &sid).await.0,
        "connected",
        "connected as soon as both legs are at the broker"
    );
    assert_eq!(check(&srv, &sid, true).await["end"], json!([]));
    assert_eq!(session_state(&srv, &sid).await.0, "connected");
    // The postbox never accepts a leg for another session.
    let (s, v2) = open(&srv, &me.strong, &agent, 2).await;
    assert_eq!(s, 201);
    let sid2 = v2["session_id"].as_str().unwrap().to_string();
    assert_eq!(
        code(&leg(&srv, &sid2, client_leg).await),
        (403, "leg_invalid")
    );

    // Close: the agent hears it, the broker is told to end it, the audit says only metadata.
    let (s, c) = srv
        .call(
            Method::DELETE,
            &format!("/v1/shell/sessions/{sid}"),
            Some(&me.token),
            None,
        )
        .await;
    assert_eq!((s, c["session"]["state"].as_str()), (200, Some("closed")));
    let rv = machine.next("shell.revoke").await;
    assert_eq!(rv["session_id"], sid);
    let end = check(&srv, &sid, true).await;
    assert_eq!(end["end"][0]["session_id"], sid);
    let rows = srv
        .state
        .store
        .group_read(|c| -> crate::groups::GResult<Vec<(String, String)>> {
            let mut st = c.prepare(
                "SELECT action, details FROM audit_log WHERE action LIKE 'ssh.%' ORDER BY id",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(actions.starts_with(&[
        "ssh.host_key.register",
        "ssh.session.request",
        "ssh.session.open"
    ]));
    assert!(actions.contains(&"ssh.session.close"));
    let close = &rows.iter().find(|r| r.0 == "ssh.session.close").unwrap().1;
    let close: Value = serde_json::from_str(close).unwrap();
    assert_eq!(close["bytes_client"], 10);
    assert_eq!(close["bytes_agent"], 20);
    assert!(close["duration_secs"].is_number());
    for (_, details) in &rows {
        for secret in ["lt_", "-cert-v01", "BEGIN"] {
            assert!(
                !details.contains(secret),
                "audit carries no credentials: {details}"
            );
        }
    }

    // Disabling locally ends what is open.
    assert_eq!(check(&srv, &sid2, false).await["end"], json!([]));
    let (s, _) = srv
        .call(
            Method::PUT,
            "/v1/shell/agent",
            Some(&cap),
            Some(
                json!({ "enabled": false, "mode": "allowlist", "max_sessions": 2,
                          "host_key": host_key(1) }),
            ),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(machine.next("shell.revoke").await["session_id"], sid2);
    assert_eq!(
        session_state(&srv, &sid2).await,
        ("closed".into(), Some("agent_disabled".into()))
    );
}

/// Revoking a delegation, removing the member or turning the org setting off ends live sessions.
#[tokio::test]
async fn revocation_ends_live_sessions() {
    let f = org_fixture().await;
    let srv = &f.srv;
    let mut machine = AgentSide::connect(srv, &f.cred).await;
    register(srv, &f.cred, 1, "container").await;
    assert_eq!(f.settings(json!({ "ssh_allowed": true })).await.0, 200);
    f.switch(true).await;
    let dele = srv.person("dele").await;
    let (dm, _) = srv.join(&f.org, &f.owner, &dele, "dele", &[]).await;
    let did = f.delegate(&dm, &["ssh"]).await;

    let start = |seed: u8| {
        let token = dele.strong.clone();
        async move {
            let (s, v) = open(srv, &token, "/acme/agent1", seed).await;
            assert_eq!(s, 201, "{v}");
            v["session_id"].as_str().unwrap().to_string()
        }
    };
    let sid = start(1).await;
    machine.next("shell.request").await;
    let _ = check(srv, &sid, false).await;

    // The delegation goes: the machine is told at once, and the broker on its next check.
    srv.call(
        Method::DELETE,
        &format!("/v1/orgs/{}/delegations/{did}", f.org),
        Some(&f.owner.token),
        None,
    )
    .await;
    let rv = machine.next("shell.revoke").await;
    assert_eq!(rv["session_id"], sid);
    assert_eq!(rv["reason"], "delegation_required");
    assert_eq!(check(srv, &sid, true).await["end"][0]["session_id"], sid);

    // A new delegation: a new grant; the org setting off ends it too.
    f.delegate(&dm, &["ssh"]).await;
    let sid = start(2).await;
    machine.next("shell.request").await;
    assert_eq!(f.settings(json!({ "ssh_allowed": false })).await.0, 200);
    let rv = machine.next("shell.revoke").await;
    assert_eq!(
        (rv["session_id"].as_str(), rv["reason"].as_str()),
        (Some(sid.as_str()), Some("ssh_not_allowed"))
    );

    // Removing the member.
    f.settings(json!({ "ssh_allowed": true })).await;
    let sid = start(3).await;
    machine.next("shell.request").await;
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{}/members/{dm}/remove", f.org),
            &f.owner.strong,
            json!({ "mode": "reserve" }),
        )
        .await;
    assert!(s == 200, "{v}");
    let rv = machine.next("shell.revoke").await;
    assert_eq!(rv["session_id"], sid);
}

/// `ssh_requires_approval`: a second person approves; never the requester; the requester then
/// collects credentials with a fresh step-up.
#[tokio::test]
async fn approval_flow() {
    let f = org_fixture().await;
    let srv = &f.srv;
    let mut machine = AgentSide::connect(srv, &f.cred).await;
    register(srv, &f.cred, 1, "container").await;
    f.settings(json!({ "ssh_allowed": true, "ssh_requires_approval": true }))
        .await;
    f.switch(true).await;
    let dele = srv.person("dele").await;
    let aa = srv.person("aa").await;
    let (dm, _) = srv.join(&f.org, &f.owner, &dele, "dele", &[]).await;
    srv.join(&f.org, &f.owner, &aa, "aa", &["agent_admin"])
        .await;
    f.delegate(&dm, &["ssh"]).await;

    let (s, v) = open(srv, &dele.strong, "/acme/agent1", 1).await;
    assert_eq!((s, v["state"].as_str()), (202, Some("awaiting_approval")));
    assert!(v.get("user_cert").is_none() && v.get("leg_token").is_none());
    let sid = v["session_id"].as_str().unwrap().to_string();
    // Not yet: no credentials.
    assert_eq!(
        code(
            &srv.post(
                &format!("/v1/shell/sessions/{sid}/connect"),
                &dele.strong,
                json!({})
            )
            .await
        ),
        (409, "not_approved")
    );
    // The requester cannot approve themselves; approvers need step-up.
    assert_eq!(
        srv.post(
            &format!("/v1/shell/sessions/{sid}/approve"),
            &dele.strong,
            json!({})
        )
        .await
        .0,
        403
    );
    assert_eq!(
        srv.post(
            &format!("/v1/shell/sessions/{sid}/approve"),
            &aa.token,
            json!({})
        )
        .await
        .0,
        401
    );
    let (s, l) = srv
        .get("/v1/shell/sessions?state=awaiting_approval", &aa.token)
        .await;
    assert_eq!(
        (s, l["sessions"][0]["id"].as_str()),
        (200, Some(sid.as_str()))
    );
    let (s, a) = srv
        .post(
            &format!("/v1/shell/sessions/{sid}/approve"),
            &aa.strong,
            json!({}),
        )
        .await;
    assert_eq!((s, a["session"]["state"].as_str()), (200, Some("approved")));
    // Only the requester collects, with step-up, once.
    assert_eq!(
        srv.post(
            &format!("/v1/shell/sessions/{sid}/connect"),
            &aa.strong,
            json!({})
        )
        .await
        .0,
        404
    );
    assert_eq!(
        srv.post(
            &format!("/v1/shell/sessions/{sid}/connect"),
            &dele.token,
            json!({})
        )
        .await
        .0,
        401
    );
    let (s, c) = srv
        .post(
            &format!("/v1/shell/sessions/{sid}/connect"),
            &dele.strong,
            json!({}),
        )
        .await;
    assert_eq!(s, 201, "{c}");
    assert!(c["user_cert"].as_str().unwrap().contains("cert-v01"));
    assert_eq!(machine.next("shell.request").await["session_id"], sid);
    assert_eq!(
        code(
            &srv.post(
                &format!("/v1/shell/sessions/{sid}/connect"),
                &dele.strong,
                json!({})
            )
            .await
        ),
        (409, "not_approved")
    );
    // A denial closes it.
    let (_, v) = open(srv, &dele.strong, "/acme/agent1", 2).await;
    let sid2 = v["session_id"].as_str().unwrap();
    let (s, d) = srv
        .post(
            &format!("/v1/shell/sessions/{sid2}/deny"),
            &f.owner.strong,
            json!({}),
        )
        .await;
    assert_eq!(
        (s, d["session"]["close_reason"].as_str()),
        (200, Some("denied"))
    );
}

/// The first host key is pinned; a different one waits for a person's approval, and no session
/// opens meanwhile.
#[tokio::test]
async fn host_key_pinning_and_rotation() {
    let srv = serve().await;
    let me = srv.person("me").await;
    let other = srv.person("other").await;
    let (_, mb) = srv
        .post("/v1/identities", &me.token, json!({ "label": "box" }))
        .await;
    let agent = mb["address"].as_str().unwrap().to_string();
    let cap = mb["capability_token"].as_str().unwrap().to_string();
    let mut machine = AgentSide::connect(&srv, &cap).await;
    let (_, first) = register(&srv, &cap, 1, "unconfined").await;
    let pinned = first["host_key_fp"].as_str().unwrap().to_string();
    let (s, v) = open(&srv, &me.strong, &agent, 1).await;
    assert_eq!(s, 201);
    let old = v["session_id"].as_str().unwrap().to_string();
    machine.next("shell.request").await;

    // A different key (a reinstalled machine, or someone holding a stolen credential).
    let (s, r) = register(&srv, &cap, 2, "unconfined").await;
    assert_eq!((s, r["status"].as_str()), (200, Some("host_key_pending")));
    assert_eq!(r["host_key_fp"], pinned, "the pin is unchanged");
    let offered = r["pending_host_key_fp"].as_str().unwrap().to_string();
    let (s, v) = open(&srv, &me.strong, &agent, 2).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("host_key_pending")));
    assert_eq!(v["pending_host_key_fp"], offered);
    // The sweep ends the session opened under the old key.
    shell::sweep(&srv.state).await;
    assert_eq!(machine.next("shell.revoke").await["session_id"], old);

    let path = format!("/v1/agents/{}/shell/host-key", enc(&agent));
    assert_eq!(
        srv.post(&path, &other.strong, json!({ "fingerprint": offered }))
            .await
            .0,
        404,
        "strangers cannot approve"
    );
    assert_eq!(
        srv.post(&path, &me.token, json!({ "fingerprint": offered }))
            .await
            .0,
        401,
        "approving needs step-up"
    );
    assert_eq!(
        code(
            &srv.post(&path, &me.strong, json!({ "fingerprint": pinned }))
                .await
        ),
        (409, "fingerprint_mismatch")
    );
    let (s, a) = srv
        .post(&path, &me.strong, json!({ "fingerprint": offered }))
        .await;
    assert_eq!(
        (s, a["host_key_fp"].as_str()),
        (200, Some(offered.as_str()))
    );
    let (s, v) = open(&srv, &me.strong, &agent, 3).await;
    assert_eq!(
        (s, v["host_key_fp"].as_str()),
        (201, Some(offered.as_str()))
    );
}

/// Pending approvals expire, unconnected credentials expire, and nothing outlives eight hours.
#[tokio::test]
async fn timeouts() {
    let srv = serve().await;
    let me = srv.person("me").await;
    let (_, mb) = srv
        .post("/v1/identities", &me.token, json!({ "label": "box" }))
        .await;
    let agent = mb["address"].as_str().unwrap().to_string();
    let cap = mb["capability_token"].as_str().unwrap().to_string();
    let mut machine = AgentSide::connect(&srv, &cap).await;
    register(&srv, &cap, 1, "container").await;

    let backdate = |sid: String, column: &'static str| {
        let state = srv.state.clone();
        async move {
            state
                .store
                .group_tx(move |tx| -> crate::groups::GResult<()> {
                    tx.execute(
                        &format!("UPDATE ssh_sessions SET {column} = ?2 WHERE id = ?1"),
                        rusqlite::params![sid, now_unix() as i64 - 1],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
    };
    // Credentials nobody used.
    let (_, v) = open(&srv, &me.strong, &agent, 1).await;
    let sid = v["session_id"].as_str().unwrap().to_string();
    machine.next("shell.request").await;
    backdate(sid.clone(), "connect_by").await;
    shell::sweep(&srv.state).await;
    assert_eq!(
        session_state(&srv, &sid).await,
        ("closed".into(), Some("connect_timeout".into()))
    );
    assert_eq!(machine.next("shell.revoke").await["session_id"], sid);
    // Its legs are dead too.
    assert_eq!(
        code(&leg(&srv, &sid, v["leg_token"].as_str().unwrap()).await),
        (403, "leg_invalid")
    );
    // The absolute limit.
    let (_, v) = open(&srv, &me.strong, &agent, 2).await;
    let sid = v["session_id"].as_str().unwrap().to_string();
    leg(&srv, &sid, v["leg_token"].as_str().unwrap()).await;
    backdate(sid.clone(), "ends_by").await;
    let end = check(&srv, &sid, true).await;
    assert_eq!(end["end"][0]["session_id"], sid);
    assert_eq!(
        session_state(&srv, &sid).await,
        ("closed".into(), Some("max_duration".into()))
    );
}

/// Without a CA the feature says so; it never issues anything weaker.
#[tokio::test]
async fn unconfigured_server_refuses() {
    let srv = serve_with(|_| {}).await;
    srv.global_flag("agent_ssh").await;
    let me = srv.person("me").await;
    let (_, mb) = srv
        .post("/v1/identities", &me.token, json!({ "label": "box" }))
        .await;
    let agent = mb["address"].as_str().unwrap();
    assert_eq!(
        code(&open(&srv, &me.strong, agent, 1).await),
        (503, "shell_unavailable")
    );
    assert_eq!(
        srv.call(Method::GET, "/v1/shell/ca", None, None).await.0,
        503
    );
}

/// Deleting the agent's mailbox removes its pinned host key with the next sweep.
#[tokio::test]
async fn a_deleted_mailbox_takes_its_host_key_with_it() {
    let srv = serve().await;
    let me = srv.person("me").await;
    let (_, mb) = srv
        .post("/v1/identities", &me.token, json!({ "label": "box" }))
        .await;
    let agent = mb["address"].as_str().unwrap().to_string();
    let cap = mb["capability_token"].as_str().unwrap().to_string();
    assert_eq!(register(&srv, &cap, 1, "container").await.0, 200);
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("/v1/identities?identity={}", enc(&agent)),
            Some(&cap),
            None,
        )
        .await;
    assert_eq!(s, 200);
    shell::sweep(&srv.state).await;
    let left: i64 = srv
        .state
        .store
        .group_read(move |c| -> crate::groups::GResult<i64> {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM agent_shell_hosts WHERE identity = ?1",
                [&agent],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}
