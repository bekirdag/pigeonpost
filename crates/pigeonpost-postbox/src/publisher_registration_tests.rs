//! Verified publisher registration over real sockets: drafts, masking, documents and their access
//! control, the KYC sandbox and the Shufti callback (signature and replay), submission with
//! step-up, the staff review (roles, conflict, audit, reveal, decisions), notices, the payee push
//! queue, and the legacy M0 application. Synthetic accounts and data only.

use crate::org_tests::{serve_with, Person, Srv};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

const TCKN: &str = "10000000146";
const IBAN: &str = "TR330006100519786457841326";
const PDF: &[u8] = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\nxref\n0 1\ntrailer << /Root 1 0 R >>\nstartxref\n9\n%%EOF\n";

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

/// A staging postbox with the KYC sandbox on (the only place the sandbox exists).
async fn setup() -> Srv {
    let srv = serve_with(|s| {
        s.blobs = Some(Arc::new(crate::blobs::Blobs::for_test("devreg")));
        let mut rt = nests::Runtime::capture();
        rt.mp.staging = true;
        rt.mp.kyc_sandbox = true;
        s.nests = Arc::new(rt);
    })
    .await;
    srv.global_flag("marketplace").await;
    srv
}

/// A production postbox: no staging, no sandbox, no Shufti credentials.
async fn production() -> Srv {
    let srv = serve_with(|s| {
        s.blobs = Some(Arc::new(crate::blobs::Blobs::for_test("devregprod")));
    })
    .await;
    srv.global_flag("marketplace").await;
    srv
}

/// A publisher profile in `status`, for tests of other features. `verified`: behind it is an
/// approved verified registration (the only way to certification); otherwise it is an M0 profile
/// with its legacy application row.
pub(crate) async fn seed_publisher(
    srv: &Srv,
    kind: &str,
    owner: &str,
    applied_by: &str,
    name: &str,
    status: &str,
    verified: bool,
) -> String {
    let id = format!("pub_{}", crate::groups::rand_b32(16));
    let app = format!("pap_{}", crate::groups::rand_b32(16));
    let (k, o, by, n, st, pid) = (
        kind.to_string(),
        owner.to_string(),
        applied_by.to_string(),
        name.to_string(),
        status.to_string(),
        id.clone(),
    );
    srv.state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<()> {
            let now = now_unix() as i64;
            tx.execute(
                "INSERT INTO publisher_profiles (id, owner_kind, owner_id, display_name, status,
                    applied_by, created_at, updated_at, verified_application_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
                rusqlite::params![pid, k, o, n, st, by, now, verified.then(|| app.clone())],
            )?;
            if verified {
                tx.execute(
                    "INSERT INTO publisher_applications (id, owner_kind, owner_id, applicant,
                        account_type, state, publisher_id, public_name, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'approved', ?6, ?7, ?8, ?8)",
                    rusqlite::params![
                        app,
                        k,
                        o,
                        by,
                        if k == "org" {
                            "company_tr"
                        } else {
                            "individual_tr"
                        },
                        pid,
                        n,
                        now
                    ],
                )?;
            } else {
                crate::store::publisher_registration::backfill_legacy(tx)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    id
}

async fn staff(srv: &Srv, who: &Person, role: &str) {
    let (account, role) = (who.account.clone(), role.to_string());
    srv.state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<()> {
            tx.execute(
                "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at)
                 VALUES (?1, ?2, ?3, 'acct_owner', 1)",
                rusqlite::params![format!("stf_{account}_{role}"), account, role],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn upload(srv: &Srv, token: &str, app: &str, query: &str, bytes: &[u8]) -> (u16, Value) {
    let resp = srv
        .http
        .post(format!(
            "{}/v1/publisher-registrations/{app}/documents?{query}",
            srv.public
        ))
        .header("x-forwarded-for", &srv.ip)
        .bearer_auth(token)
        .body(bytes.to_vec())
        .send()
        .await
        .unwrap();
    let s = resp.status().as_u16();
    let text = resp.text().await.unwrap();
    (
        s,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

async fn fetch(srv: &Srv, token: &str, path: &str) -> (u16, Vec<u8>, reqwest::header::HeaderMap) {
    let resp = srv
        .http
        .get(format!("{}{path}", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let s = resp.status().as_u16();
    let h = resp.headers().clone();
    (s, resp.bytes().await.unwrap().to_vec(), h)
}

async fn count(srv: &Srv, sql: &str) -> i64 {
    let sql = sql.to_string();
    srv.state
        .store
        .group_read(move |c| -> crate::groups::GResult<i64> {
            Ok(c.query_row(&sql, [], |r| r.get(0))?)
        })
        .await
        .unwrap()
}

fn individual_profile(name: &str) -> Value {
    json!({
        "legal_name": name,
        "country": "TR",
        "address": { "line1": "Bağdat Cd. 1", "city": "İstanbul", "postal_code": "34000", "country": "TR" },
        "support_email": "support@example.com",
        "contact_email": "dev@example.com",
    })
}

/// A complete individual (TR) draft, KYC accepted in the sandbox. Returns the application id.
async fn ready_individual(srv: &Srv, dev: &Person) -> String {
    let (s, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_tr", "public_name": "Ada Tools" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .patch(
            &format!("/v1/publisher-registrations/{id}"),
            &dev.token,
            json!({
                "profile": individual_profile("Ada Yılmaz"),
                "bank": { "holder_name": "Ada Yilmaz", "method": "iban", "iban": IBAN },
                "tax": { "tax_id": TCKN, "tax_country": "TR" },
                "accept_agreements": {
                    "agreement_version": crate::publisher_registration::AGREEMENT_VERSION,
                    "content_policy_version": crate::publisher_registration::CONTENT_POLICY_VERSION,
                },
            }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/kyc"),
            &dev.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["sandbox"], true);
    let check = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/kyc/{check}/simulate"),
            &dev.token,
            json!({ "outcome": "accepted" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    id
}

fn approve_body(revision: i64) -> Value {
    json!({
        "decision": "approved",
        "revision": revision,
        "acknowledge_sandbox_kyc": true,
        "acknowledge_unscanned_documents": true,
        "checklist": {
            "names_match_documents": true, "documents_authentic": true, "registry_consistent": true,
            "sanctions_reviewed": true, "bank_holder_matches": true,
        },
    })
}

#[tokio::test]
async fn an_individual_registers_is_reviewed_and_becomes_a_certified_publisher() {
    let srv = setup().await;
    let dev = srv.person("regdev").await;
    let rev = srv.person("regrev").await;
    staff(&srv, &rev, "reviewer").await;

    let (s, v) = srv
        .get("/v1/publisher-registration/requirements", &dev.token)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["account_types"].as_array().unwrap().len(), 5);
    assert_eq!(v["data"]["kyc_provider"], "mock");

    let id = ready_individual(&srv, &dev).await;
    let (s, v) = srv
        .get(&format!("/v1/publisher-registrations/{id}"), &dev.token)
        .await;
    assert_eq!(s, 200);
    // Bank and tax details never come back unmasked.
    let text = v.to_string();
    assert!(!text.contains(IBAN) && !text.contains(TCKN), "{text}");
    assert_eq!(
        v["data"]["bank"]["iban"],
        "TR33 •••• •••• •••• •••• ••13 26"
    );
    assert_eq!(v["data"]["tax"]["tax_id"], "•••••••••46");
    assert_eq!(v["data"]["tax"]["tax_id_check"], "tckn");
    assert_eq!(v["data"]["tax_category"], "tr_individual");
    // And not in the database either.
    let raw: String = {
        let id = id.clone();
        srv.state
            .store
            .group_read(move |c| -> crate::groups::GResult<String> {
                Ok(c.query_row(
                    "SELECT profile || COALESCE(bank_masked,'') || COALESCE(tax_masked,'') || hex(bank_sealed)
                       FROM publisher_applications WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    };
    assert!(!raw.contains(IBAN) && !raw.contains(TCKN));

    // Submitting needs a recent two-factor sign-in.
    let submit = format!("/v1/publisher-registrations/{id}/submit");
    let (s, v) = srv.post(&submit, &dev.token, json!({})).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, v) = srv.post(&submit, &dev.strong, json!({})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["state"], "submitted");
    assert_eq!(v["data"]["editable"], false);
    // Locked while staff hold it.
    let (s, v) = srv
        .patch(
            &format!("/v1/publisher-registrations/{id}"),
            &dev.token,
            json!({ "public_name": "Other" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "application_locked"));

    // Staff: role, step-up, and no stake.
    let queue = "/v1/staff/publisher-registrations";
    let (s, v) = srv.get(queue, &dev.strong).await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv.get(queue, &rev.token).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, v) = srv.get(queue, &rev.strong).await;
    assert_eq!(s, 200, "{v}");
    let row = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["kyc"]["result"], "accepted");
    assert_eq!(row["conflict"], false);

    let detail = format!("{queue}/{id}");
    let (s, v) = srv.get(&detail, &rev.strong).await;
    assert_eq!(s, 200, "{v}");
    assert!(!v.to_string().contains(IBAN));
    let items: Vec<(String, Value)> = v["data"]["checklist"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_string(), i["ok"].clone()))
        .collect();
    for wanted in [
        "profile_complete",
        "bank_account",
        "tax_profile",
        "documents_required",
        "kyc",
        "agreements",
    ] {
        assert!(
            items.contains(&(wanted.to_string(), json!(true))),
            "{wanted}: {items:?}"
        );
    }
    assert!(
        items.contains(&("names_match".into(), json!(true))),
        "{items:?}"
    );
    assert_eq!(
        count(&srv, &format!("SELECT COUNT(*) FROM audit_log WHERE action = 'staff.publisher_registration.view' AND actor_account = '{}'", rev.account)).await,
        1
    );

    let (s, v) = srv
        .post(&format!("{detail}/claim"), &rev.strong, json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["state"], "in_review");
    let revision = v["data"]["revision"].as_i64().unwrap();

    let decide = format!("{detail}/decisions");
    // A stale revision, a missing reviewer judgement and an unacknowledged sandbox KYC are refused.
    let (s, v) = srv
        .post(&decide, &rev.strong, approve_body(revision - 1))
        .await;
    assert_eq!((s, code(&v)), (409, "revision_mismatch"), "{v}");
    let mut partial = approve_body(revision);
    partial["checklist"]["documents_authentic"] = json!(false);
    let (s, v) = srv.post(&decide, &rev.strong, partial).await;
    assert_eq!((s, code(&v)), (422, "checklist_incomplete"), "{v}");
    let mut sandbox = approve_body(revision);
    sandbox["acknowledge_sandbox_kyc"] = json!(false);
    let (s, v) = srv.post(&decide, &rev.strong, sandbox).await;
    assert_eq!((s, code(&v)), (422, "sandbox_kyc"), "{v}");

    let (s, v) = srv.post(&decide, &rev.strong, approve_body(revision)).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["state"], "approved");
    let publisher = v["data"]["publisher_id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .get(&format!("/v1/publishers/{publisher}"), &dev.token)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["status"], "approved");
    assert_eq!(v["data"]["display_name"], "Ada Tools");

    // The payee profile is queued for saas_be, and the developer hears about every change.
    assert_eq!(
        count(&srv, &format!("SELECT COUNT(*) FROM publisher_payee_pushes WHERE application_id = '{id}' AND status = 'pending'")).await,
        1
    );
    for st in ["submitted", "in_review", "approved"] {
        assert_eq!(
            count(&srv, &format!("SELECT COUNT(*) FROM publisher_application_notices WHERE application_id = '{id}' AND state = '{st}'")).await,
            2,
            "{st}"
        );
    }
    assert_eq!(
        count(
            &srv,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'staff.publisher_registration.decision'"
        )
        .await,
        1
    );
    // A decided application takes no second decision.
    let (s, v) = srv
        .post(&decide, &rev.strong, approve_body(revision + 1))
        .await;
    assert_eq!((s, code(&v)), (409, "application_not_waiting"), "{v}");
}

#[tokio::test]
async fn submission_needs_a_complete_application_and_needs_info_comes_back_for_a_reply() {
    let srv = setup().await;
    let dev = srv.person("regdev2").await;
    let rev = srv.person("regrev2").await;
    staff(&srv, &rev, "reviewer").await;
    let (s, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_tr" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let submit = format!("/v1/publisher-registrations/{id}/submit");
    let (s, v) = srv.post(&submit, &dev.strong, json!({})).await;
    assert_eq!((s, code(&v)), (422, "application_incomplete"), "{v}");
    let missing: Vec<&str> = v["error"]["details"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for m in [
        "profile_complete",
        "bank_account",
        "tax_profile",
        "kyc",
        "agreements",
    ] {
        assert!(missing.contains(&m), "{m}: {missing:?}");
    }
    // One open application per owner.
    let (s, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_foreign" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "application_exists"), "{v}");

    // Field validation: a bad IBAN, TCKN or country is refused with the field.
    let path = format!("/v1/publisher-registrations/{id}");
    for (body, field) in [
        (
            json!({ "bank": { "holder_name": "Ada", "method": "iban", "iban": "TR330006100519786457841327" } }),
            "bank.iban",
        ),
        (
            json!({ "bank": { "holder_name": "Ada", "method": "ach", "routing_number": "021000021", "account_number": "1234" } }),
            "bank.method",
        ),
        (
            json!({ "tax": { "tax_id": "10000000147", "tax_country": "TR" } }),
            "tax.tax_id",
        ),
        (
            json!({ "tax": { "tax_id": TCKN, "tax_country": "DE" } }),
            "tax.tax_country",
        ),
        (json!({ "profile": { "country": "DE" } }), "country"),
    ] {
        let (s, v) = srv.patch(&path, &dev.token, body).await;
        assert_eq!(s, 422, "{v}");
        assert_eq!(v["error"]["details"]["field"].as_str(), Some(field), "{v}");
    }

    let srv2 = &srv;
    let id = {
        // Fill it through the shared helper on a second developer instead.
        let dev3 = srv2.person("regdev3").await;
        let id = ready_individual(srv2, &dev3).await;
        let (s, v) = srv2
            .post(
                &format!("/v1/publisher-registrations/{id}/submit"),
                &dev3.strong,
                json!({}),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        (id, dev3)
    };
    let (id, dev3) = id;
    let detail = format!("/v1/staff/publisher-registrations/{id}");
    let (_, v) = srv.get(&detail, &rev.strong).await;
    let revision = v["data"]["revision"].as_i64().unwrap();
    let (s, v) = srv
        .post(
            &format!("{detail}/decisions"),
            &rev.strong,
            json!({ "decision": "needs_info", "revision": revision }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "message_required"), "{v}");
    let (s, v) = srv
        .post(
            &format!("{detail}/decisions"),
            &rev.strong,
            json!({ "decision": "needs_info", "revision": revision, "message": "Use the trading name you sell under" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (_, v) = srv
        .get(&format!("/v1/publisher-registrations/{id}"), &dev3.token)
        .await;
    assert_eq!(v["data"]["state"], "needs_info");
    assert_eq!(v["data"]["editable"], true);
    assert_eq!(
        v["data"]["staff_message"]["message"],
        "Use the trading name you sell under"
    );
    let (s, v) = srv
        .patch(
            &format!("/v1/publisher-registrations/{id}"),
            &dev3.token,
            json!({ "public_name": "Ada Studio" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/submit"),
            &dev3.strong,
            json!({ "message": "Renamed to Ada Studio" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["state"], "submitted");
    let kinds: Vec<&str> = v["data"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"resubmitted"), "{kinds:?}");
    // Rejection with a reason starts the retention clock.
    let (_, v) = srv.get(&detail, &rev.strong).await;
    let revision = v["data"]["revision"].as_i64().unwrap();
    let (s, v) = srv
        .post(
            &format!("{detail}/decisions"),
            &rev.strong,
            json!({ "decision": "rejected", "revision": revision, "message": "Not eligible" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["state"], "rejected");
    let _ = dev;
}

#[tokio::test]
async fn company_documents_are_sealed_typed_and_only_reach_the_team_and_staff() {
    let srv = setup().await;
    let dev = srv.person("regco").await;
    let stranger = srv.person("regstranger").await;
    let rev = srv.person("regcorev").await;
    let fin = srv.person("regcofin").await;
    staff(&srv, &rev, "reviewer").await;
    staff(&srv, &fin, "finance").await;
    let (s, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "company_tr", "public_name": "Acme" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_string();

    // Only PDF, JPEG or PNG by their bytes, only the types the account type takes, dates where due.
    let (s, v) = upload(
        &srv,
        &dev.token,
        &id,
        "type=vergi_levhasi&filename=a.pdf",
        b"<html>not a pdf</html>",
    )
    .await;
    assert_eq!((s, code(&v)), (415, "unsupported_document"), "{v}");
    let (s, v) = upload(
        &srv,
        &dev.token,
        &id,
        "type=residence_certificate_x&filename=a.pdf",
        PDF,
    )
    .await;
    assert_eq!((s, code(&v)), (400, "invalid_document_type"), "{v}");
    let (s, v) = upload(
        &srv,
        &dev.token,
        &id,
        "type=faaliyet_belgesi&filename=a.pdf",
        PDF,
    )
    .await;
    assert_eq!((s, code(&v)), (400, "issued_on_required"), "{v}");
    let (s, v) = upload(
        &srv,
        &stranger.token,
        &id,
        "type=vergi_levhasi&filename=a.pdf",
        PDF,
    )
    .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, v) = upload(
        &srv,
        &dev.token,
        &id,
        "type=vergi_levhasi&filename=levha.pdf",
        PDF,
    )
    .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["content_type"], "application/pdf");
    assert_eq!(v["data"]["scan"], "pending");
    let doc = v["data"]["id"].as_str().unwrap().to_string();

    // At rest the bytes are ciphertext.
    let blob: String = {
        let d = doc.clone();
        srv.state
            .store
            .group_read(move |c| -> crate::groups::GResult<String> {
                Ok(c.query_row(
                    "SELECT blob FROM publisher_application_documents WHERE id = ?1",
                    [&d],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    };
    let stored = crate::publisher_registration::doc_blobs(&srv.state)
        .unwrap()
        .get(&blob)
        .unwrap();
    assert!(!stored.windows(5).any(|w| w == b"%PDF-"));

    let mine = format!("/v1/publisher-registrations/{id}/documents/{doc}");
    let (s, bytes, h) = fetch(&srv, &dev.token, &mine).await;
    assert_eq!(s, 200);
    assert_eq!(bytes, PDF);
    assert_eq!(h["content-type"], "application/pdf");
    assert!(h["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));
    let (s, _, _) = fetch(&srv, &stranger.token, &mine).await;
    assert_eq!(s, 404);

    let staff_doc = format!("/v1/staff/publisher-registrations/{id}/documents/{doc}");
    let (s, _, _) = fetch(&srv, &stranger.strong, &staff_doc).await;
    assert_eq!(s, 403);
    let (s, _, _) = fetch(&srv, &rev.token, &staff_doc).await;
    assert_eq!(s, 401);
    let (s, bytes, _) = fetch(&srv, &rev.strong, &staff_doc).await;
    assert_eq!((s, bytes.as_slice()), (200, PDF));
    let (s, bytes, _) = fetch(&srv, &fin.strong, &staff_doc).await;
    assert_eq!((s, bytes.as_slice()), (200, PDF));
    // Finance reads records but does not review.
    let (s, v) = srv
        .get(
            &format!("/v1/staff/publisher-registrations/{id}"),
            &fin.strong,
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    assert_eq!(
        count(&srv, &format!("SELECT COUNT(*) FROM audit_log WHERE action = 'staff.publisher_registration.document' AND target = 'publisher_application:{id}'")).await,
        2
    );

    // A reviewer with a stake (here: the applicant made staff) is refused.
    staff(&srv, &dev, "reviewer").await;
    let (s, _, _) = fetch(&srv, &dev.strong, &staff_doc).await;
    assert_eq!(s, 403);
    let (s, v) = srv
        .get(
            &format!("/v1/staff/publisher-registrations/{id}"),
            &dev.strong,
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");

    // Withdrawn before any submission: the bytes go at once.
    let (s, _) = srv
        .call(Method::DELETE, &mine, Some(&dev.token), None)
        .await;
    assert_eq!(s, 204);
    assert!(crate::publisher_registration::doc_blobs(&srv.state)
        .unwrap()
        .get(&blob)
        .is_none());
    let (s, _, _) = fetch(&srv, &dev.token, &mine).await;
    assert_eq!(s, 404);

    // The company checklist names every required document.
    let (_, v) = srv
        .get(&format!("/v1/publisher-registrations/{id}"), &dev.token)
        .await;
    let docs = v["data"]["checklist"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "documents_required")
        .unwrap()
        .clone();
    let detail = docs["detail"].as_str().unwrap();
    for name in [
        "ticaret sicil gazetesi",
        "imza sirküleri",
        "faaliyet belgesi",
        "vergi levhası",
        "Bank statement",
    ] {
        assert!(
            detail.to_lowercase().contains(&name.to_lowercase()),
            "{name}: {detail}"
        );
    }
}

#[tokio::test]
async fn staff_reveal_is_the_only_unmasked_read_and_is_audited() {
    let srv = setup().await;
    let dev = srv.person("regrevealdev").await;
    let fin = srv.person("regrevealfin").await;
    staff(&srv, &fin, "finance").await;
    let id = ready_individual(&srv, &dev).await;
    let path = format!("/v1/staff/publisher-registrations/{id}/reveal");
    let (s, v) = srv
        .post(
            &path,
            &fin.strong,
            json!({ "fields": ["bank"], "reason": "" }),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = srv
        .post(
            &path,
            &fin.strong,
            json!({ "fields": ["bank", "tax"], "reason": "Verifying payout details" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["bank"]["iban"], IBAN);
    assert_eq!(v["data"]["tax"]["tax_id"], TCKN);
    let detail: String = srv
        .state
        .store
        .group_read(|c| -> crate::groups::GResult<String> {
            Ok(c.query_row(
                "SELECT details FROM audit_log WHERE action = 'staff.publisher_registration.reveal'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert!(detail.contains("Verifying payout details") && !detail.contains(IBAN));
    let (s, _) = srv
        .post(
            &path,
            &dev.strong,
            json!({ "fields": ["bank"], "reason": "curious me" }),
        )
        .await;
    assert_eq!(s, 403);
}

fn signed(srv: &Srv, body: &Value) -> (Vec<u8>, String) {
    let bytes = body.to_string().into_bytes();
    let secret = match crate::publisher_kyc::provider(&srv.state) {
        crate::publisher_kyc::Provider::Mock { secret } => secret,
        _ => panic!("tests run against the mock provider"),
    };
    let sig = crate::publisher_kyc::signature(
        &bytes,
        &secret,
        crate::publisher_kyc::Scheme::HashedSecret,
    );
    (bytes, sig)
}

async fn callback(srv: &Srv, bytes: &[u8], sig: Option<&str>) -> (u16, Value) {
    let mut req = srv
        .http
        .post(format!("{}/v1/integrations/shufti/callback", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .header("content-type", "application/json")
        .body(bytes.to_vec());
    if let Some(s) = sig {
        req = req.header("Signature", s);
    }
    let resp = req.send().await.unwrap();
    let s = resp.status().as_u16();
    (
        s,
        serde_json::from_str(&resp.text().await.unwrap()).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_kyc_callback_needs_its_signature_and_a_replay_changes_nothing() {
    let srv = setup().await;
    let dev = srv.person("regkyc").await;
    let (_, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_foreign" }),
        )
        .await;
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/kyc"),
            &dev.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let check = v["data"]["id"].as_str().unwrap().to_string();
    let reference: String = {
        let c = check.clone();
        srv.state
            .store
            .group_read(move |db| -> crate::groups::GResult<String> {
                Ok(db.query_row(
                    "SELECT reference FROM publisher_kyc_checks WHERE id = ?1",
                    [&c],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    };
    let declined = json!({ "reference": reference, "event": "verification.declined",
                           "declined_reason": "Document expired", "declined_codes": ["SPDR13"] });
    let (bytes, sig) = signed(&srv, &declined);
    // No signature, a wrong one, or one over other bytes: refused.
    assert_eq!(callback(&srv, &bytes, None).await.0, 401);
    assert_eq!(callback(&srv, &bytes, Some(&"0".repeat(64))).await.0, 401);
    let (other, _) = signed(
        &srv,
        &json!({ "reference": reference, "event": "verification.accepted" }),
    );
    assert_eq!(callback(&srv, &other, Some(&sig)).await.0, 401);
    // Signed: applied once.
    assert_eq!(callback(&srv, &bytes, Some(&sig)).await.0, 200);
    let (_, v) = srv
        .get(&format!("/v1/publisher-registrations/{id}"), &dev.token)
        .await;
    assert_eq!(v["data"]["kyc"][0]["result"], "declined");
    assert_eq!(v["data"]["kyc"][0]["declined_reason"], "Document expired");
    // The same body again is a replay: answered 200 (so Shufti stops retrying), recorded once.
    assert_eq!(callback(&srv, &bytes, Some(&sig)).await.0, 200);
    assert_eq!(
        count(&srv, "SELECT COUNT(*) FROM publisher_kyc_events").await,
        1
    );
    // A later, differently signed "accepted" cannot overturn a final result.
    let (late, late_sig) = signed(
        &srv,
        &json!({ "reference": reference, "event": "verification.accepted" }),
    );
    assert_eq!(callback(&srv, &late, Some(&late_sig)).await.0, 200);
    let (_, v) = srv
        .get(&format!("/v1/publisher-registrations/{id}"), &dev.token)
        .await;
    assert_eq!(v["data"]["kyc"][0]["result"], "declined");
    // A reference this postbox never made changes nothing.
    let (unknown, unknown_sig) = signed(
        &srv,
        &json!({ "reference": "ppk_nope", "event": "verification.accepted" }),
    );
    assert_eq!(callback(&srv, &unknown, Some(&unknown_sig)).await.0, 200);
    assert_eq!(
        count(
            &srv,
            "SELECT COUNT(*) FROM publisher_kyc_checks WHERE result = 'accepted'"
        )
        .await,
        0
    );
    // Images are never stored: the check row has no column for them, and the payload's extra
    // fields are dropped.
    let (img, img_sig) = signed(
        &srv,
        &json!({ "reference": "ppk_nope2", "event": "verification.accepted",
                                              "proofs": { "document": { "proof": "data:image/png;base64,AAAA" } } }),
    );
    assert_eq!(callback(&srv, &img, Some(&img_sig)).await.0, 200);
}

#[tokio::test]
async fn legacy_applications_map_to_the_new_model_and_upgrade_into_a_draft() {
    let srv = setup().await;
    let dev = srv.person("reglegacy").await;
    // The M0 route is closed: it points to the registration.
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &dev.token,
            json!({ "display_name": "Old Way" }),
        )
        .await;
    assert_eq!((s, code(&v)), (410, "registration_required"), "{v}");
    // An application made before it closed.
    let publisher = seed_publisher(
        &srv,
        "account",
        &dev.account,
        &dev.account,
        "Old Way",
        "applied",
        false,
    )
    .await;
    let (s, v) = srv.get("/v1/publisher-registrations", &dev.token).await;
    assert_eq!(s, 200, "{v}");
    let rows = v["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["account_type"], "legacy");
    assert_eq!(rows[0]["state"], "submitted");
    assert_eq!(rows[0]["publisher_id"], publisher.as_str());
    let legacy = rows[0]["id"].as_str().unwrap().to_string();
    // The M0 staff route cannot certify a profile with no verified registration.
    let rev = srv.person("reglegacyrev").await;
    staff(&srv, &rev, "reviewer").await;
    let certify = format!("/v1/staff/publisher-applications/{publisher}/decisions");
    let (s, v) = srv
        .post(
            &certify,
            &rev.strong,
            json!({ "decision": "approved", "reason": "ok" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "registration_required"), "{v}");
    // Starting a verified registration turns it into a draft, same id and publisher.
    let (s, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_tr" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["id"], legacy.as_str());
    assert_eq!(v["data"]["state"], "draft");
    assert_eq!(v["data"]["public_name"], "Old Way");
    let (s, v) = srv
        .post(
            &certify,
            &rev.strong,
            json!({ "decision": "approved", "reason": "ok" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "registration_in_progress"), "{v}");
}

async fn hold_of(srv: &Srv, p: &str) -> (String, Option<String>) {
    let p = p.to_string();
    srv.state
        .store
        .group_read(
            move |c| -> crate::groups::GResult<(String, Option<String>)> {
                Ok(c.query_row(
                    "SELECT status, payout_hold FROM publisher_profiles WHERE id = ?1",
                    [&p],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn publishers_certified_before_registration_stay_certified_and_are_held_for_payouts() {
    let srv = setup().await;
    let old = srv.person("regheldold").await;
    let new = srv.person("regheldnew").await;
    let m0 = seed_publisher(
        &srv,
        "account",
        &old.account,
        &old.account,
        "Old",
        "approved",
        false,
    )
    .await;
    let v2 = seed_publisher(
        &srv,
        "account",
        &new.account,
        &new.account,
        "New",
        "approved",
        true,
    )
    .await;
    srv.state
        .store
        .group_tx(|tx| -> crate::groups::GResult<()> {
            crate::store::publisher_registration::hold_unverified(tx)?;
            crate::store::publisher_registration::hold_unverified(tx)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        hold_of(&srv, &m0).await,
        ("approved".into(), Some("verification_required".into()))
    );
    assert_eq!(hold_of(&srv, &v2).await, ("approved".into(), None));
}

#[tokio::test]
async fn the_payee_payload_carries_the_verified_details_for_saas_be() {
    use crate::publisher_registration::{type_spec, Bank, Tax};
    let spec = type_spec("company_foreign").unwrap();
    let bank = crate::publisher_registration::clean_bank(
        Bank {
            holder_name: "Acme Inc".into(),
            method: "ach".into(),
            routing_number: Some("021000021".into()),
            account_number: Some("000123456789".into()),
            ..Bank::default()
        },
        spec,
        Some("US"),
    )
    .unwrap();
    let (tax, check) = crate::publisher_registration::clean_tax(
        Tax {
            tax_id: Some("12-3456789".into()),
            tax_country: "US".into(),
            treaty_country: Some("US".into()),
            residence_certificate_expires_on: Some("2099-01-01".into()),
        },
        spec,
        now_unix() as i64,
    )
    .unwrap();
    assert_eq!(check, "format");
    let app = crate::publisher_registration::App {
        id: "pap_x".into(),
        owner_kind: "account".into(),
        owner_id: "acct_x".into(),
        applicant: "acct_x".into(),
        account_type: "company_foreign".into(),
        state: "approved".into(),
        publisher_id: Some("pub_x".into()),
        public_name: "Acme".into(),
        profile: crate::publisher_registration::Profile {
            country: Some("US".into()),
            ..Default::default()
        },
        bank_sealed: None,
        bank_masked: None,
        tax_sealed: None,
        tax_masked: None,
        tax_category: Some("foreign_company".into()),
        agreement_version: None,
        content_policy_version: None,
        agreement_accepted_at: None,
        revision: 1,
        reviewer: None,
        submitted_at: None,
        decided_at: None,
        decided_by: None,
        decision_reason: None,
        checklist: None,
        created_at: 0,
        updated_at: 0,
    };
    let p = crate::publisher_registration_worker::payee_payload(
        &app,
        &bank,
        &tax,
        "acct_staff",
        0,
        Some("ppk_1"),
    );
    assert_eq!(p["method"], "ach");
    assert_eq!(p["country"], "US");
    assert_eq!(p["currency"], "USD");
    assert_eq!(p["payee_type"], "business");
    assert_eq!(p["tax_profile"]["category"], "foreign_company");
    assert_eq!(p["tax_profile"]["treaty_country"], "US");
    assert_eq!(p["actor_id"], "staff:acct_staff");
    assert_eq!(p["source"]["application_id"], "pap_x");
    // Turkish payees are paid in TRY to a Turkish IBAN only.
    let tr = type_spec("company_tr").unwrap();
    assert!(crate::publisher_registration::clean_bank(
        Bank {
            holder_name: "Acme Ltd".into(),
            method: "iban".into(),
            iban: Some("DE89370400440532013000".into()),
            ..Bank::default()
        },
        tr,
        None,
    )
    .is_err());
    let b = crate::publisher_registration::clean_bank(
        Bank {
            holder_name: "Acme Ltd".into(),
            method: "iban".into(),
            iban: Some(IBAN.into()),
            ..Bank::default()
        },
        tr,
        None,
    )
    .unwrap();
    assert_eq!(b.currency.as_deref(), Some("TRY"));
    // A foreign payee: the declared country is the payout country, never the bank code's; no
    // Turkish bank; ACH only for US payees; the country must be known first.
    let fi = type_spec("individual_foreign").unwrap();
    let swift = |bic: &str| Bank {
        holder_name: "Ada L".into(),
        method: "swift".into(),
        account_number: Some("12345678".into()),
        swift_bic: Some(bic.into()),
        bank_name: Some("Bank".into()),
        ..Bank::default()
    };
    let e =
        crate::publisher_registration::clean_bank(swift("TGBATRIS"), fi, Some("DE")).unwrap_err();
    assert_eq!(e.code, "swift_must_be_foreign");
    let e = crate::publisher_registration::clean_bank(swift("DEUTDEFF"), fi, None).unwrap_err();
    assert_eq!(e.code, "country_required");
    let ach = Bank {
        holder_name: "Ada L".into(),
        method: "ach".into(),
        routing_number: Some("021000021".into()),
        account_number: Some("000123456789".into()),
        ..Bank::default()
    };
    let e = crate::publisher_registration::clean_bank(ach, fi, Some("DE")).unwrap_err();
    assert_eq!(e.code, "ach_requires_us");
    let b = crate::publisher_registration::clean_bank(swift("CHASUS33"), fi, Some("GB")).unwrap();
    let mut gb = app.clone();
    gb.account_type = "individual_foreign".into();
    gb.profile.country = Some("GB".into());
    let p =
        crate::publisher_registration_worker::payee_payload(&gb, &b, &tax, "acct_staff", 0, None);
    assert_eq!(p["country"], "GB", "the declared country, not the BIC's US");
}

#[test]
fn names_are_compared_with_turkish_letters_folded() {
    use crate::publisher_registration::names_match;
    assert!(names_match("Ada Yılmaz", "ADA YILMAZ"));
    assert!(names_match("Şükrü Öztürk", "Sukru Ozturk"));
    assert!(names_match("Ada Lovelace", "Ada Augusta Lovelace"));
    assert!(names_match(
        "Acme Yazılım Ltd. Şti.",
        "ACME YAZILIM LIMITED SIRKETI"
    ));
    assert!(!names_match("Ada Lovelace", "Grace Hopper"));
    assert!(!names_match("", "Ada"));
}

// ---- security review fixes ------------------------------------------------------------------------

/// Production, no Shufti: KYC is unavailable. Nothing falls back to the sandbox, nobody who needs
/// KYC can submit, and a sandbox acknowledgement is refused outright.
#[tokio::test]
async fn without_shufti_production_kyc_is_unavailable_never_the_sandbox() {
    let srv = production().await;
    let dev = srv.person("regprod").await;
    let (s, v) = srv
        .get("/v1/publisher-registration/requirements", &dev.token)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["kyc_provider"], "unavailable");
    assert_eq!(v["data"]["kyc_unavailable_reason"], "not_configured");
    let (_, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_tr", "public_name": "Prod Tools" }),
        )
        .await;
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/kyc"),
            &dev.token,
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (503, "kyc_unavailable"), "{v}");
    assert_eq!(
        count(&srv, "SELECT COUNT(*) FROM publisher_kyc_checks").await,
        0
    );
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/submit"),
            &dev.strong,
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (422, "application_incomplete"), "{v}");
    let kyc = v["error"]["details"]["checklist"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "kyc")
        .unwrap()
        .clone();
    assert!(
        kyc["detail"]
            .as_str()
            .unwrap()
            .contains("temporarily unavailable"),
        "{kyc}"
    );
    // A sandbox check left over (or forged into the store) neither counts nor can be simulated.
    let aid = id.clone();
    srv.state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<()> {
            tx.execute(
                "INSERT INTO publisher_kyc_checks (id, application_id, subject, provider, reference,
                    event, result, created_by, created_at, updated_at)
                 VALUES ('kyc_forgedforgedforged', ?1, 'applicant', 'mock', 'ppk_forged',
                    'request.pending', 'pending', 'x', 1, 1)",
                [&aid],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (s, _) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/kyc/kyc_forgedforgedforged/simulate"),
            &dev.token,
            json!({ "outcome": "accepted" }),
        )
        .await;
    assert_eq!(s, 404);
    // Staff can never acknowledge a sandbox KYC on production.
    let rev = srv.person("regprodrev").await;
    staff(&srv, &rev, "reviewer").await;
    let aid = id.clone();
    srv.state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<()> {
            tx.execute(
                "UPDATE publisher_applications SET state = 'submitted' WHERE id = ?1",
                [&aid],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let detail = format!("/v1/staff/publisher-registrations/{id}");
    let (_, v) = srv.get(&detail, &rev.strong).await;
    let revision = v["data"]["revision"].as_i64().unwrap();
    let (s, v) = srv
        .post(
            &format!("{detail}/decisions"),
            &rev.strong,
            approve_body(revision),
        )
        .await;
    assert_eq!((s, code(&v)), (422, "sandbox_kyc_not_allowed"), "{v}");
}

/// A named credential file that cannot be read is an error, never "not configured".
#[test]
fn an_unreadable_shufti_secret_file_is_a_configuration_error() {
    std::env::set_var("PP_TEST_KYC_SECRET_FILE", "/nonexistent/pp-test-secret");
    let e = crate::publisher_kyc::secret_from_env("PP_TEST_KYC_SECRET").unwrap_err();
    assert!(e.contains("could not be read"), "{e}");
    let empty = std::env::temp_dir().join(format!("pp-kyc-empty-{}", std::process::id()));
    std::fs::write(&empty, "  \n").unwrap();
    std::env::set_var("PP_TEST_KYC_EMPTY_FILE", &empty);
    assert!(crate::publisher_kyc::secret_from_env("PP_TEST_KYC_EMPTY").is_err());
    std::env::remove_var("PP_TEST_KYC_SECRET_FILE");
    std::env::remove_var("PP_TEST_KYC_EMPTY_FILE");
    assert_eq!(
        crate::publisher_kyc::secret_from_env("PP_TEST_KYC_UNSET").unwrap(),
        None
    );
}

/// The sandbox only finishes checks while the application is editable.
#[tokio::test]
async fn a_sandbox_check_cannot_be_simulated_once_submitted() {
    let srv = setup().await;
    let dev = srv.person("reglocked").await;
    let id = ready_individual(&srv, &dev).await;
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/kyc"),
            &dev.token,
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "kyc_done"), "{v}");
    let (s, _) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/submit"),
            &dev.strong,
            json!({}),
        )
        .await;
    assert_eq!(s, 200);
    let (_, v) = srv
        .get(&format!("/v1/publisher-registrations/{id}"), &dev.token)
        .await;
    let check = v["data"]["kyc"][0]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/kyc/{check}/simulate"),
            &dev.token,
            json!({ "outcome": "declined" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "application_locked"), "{v}");
}

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([200, 10, 10]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Structure, not just the signature: polyglots and appended payloads are refused.
#[test]
fn documents_must_be_well_formed_not_just_correctly_signed() {
    use crate::publisher_registration::structure_ok;
    assert_eq!(structure_ok("application/pdf", PDF), Ok(()));
    assert_eq!(
        structure_ok(
            "application/pdf",
            b"%PDF-1.4\n<html><script>alert(1)</script>\nstartxref\n9\n%%EOF\n"
        ),
        Err("pdf_polyglot")
    );
    let mut trailing = PDF.to_vec();
    trailing.extend_from_slice(b"<html>payload</html>");
    assert_eq!(
        structure_ok("application/pdf", &trailing),
        Err("pdf_trailing_data")
    );
    assert_eq!(
        structure_ok("application/pdf", b"%PDF-1.4\nno trailer"),
        Err("pdf_trailer")
    );
    assert_eq!(
        structure_ok("application/pdf", b"%PDF-9.9\nstartxref\n%%EOF"),
        Err("pdf_header")
    );
    let good = png();
    assert_eq!(structure_ok("image/png", &good), Ok(()));
    let mut appended = good.clone();
    appended.extend_from_slice(b"PK\x03\x04zip");
    assert_eq!(
        structure_ok("image/png", &appended),
        Err("image_trailing_data")
    );
    let mut broken = good.clone();
    let mid = broken.len() / 2;
    broken[mid..mid + 8].copy_from_slice(&[0u8; 8]);
    assert_eq!(structure_ok("image/png", &broken), Err("image_decode"));
    assert_eq!(
        structure_ok("image/jpeg", &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0xFF, 0xD9]),
        Err("image_decode")
    );
}

#[tokio::test]
async fn a_polyglot_upload_is_refused() {
    let srv = setup().await;
    let dev = srv.person("regpoly").await;
    let (_, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_tr" }),
        )
        .await;
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let mut poly = PDF.to_vec();
    poly.extend_from_slice(b"\n<script>x</script>");
    let (s, v) = upload(
        &srv,
        &dev.token,
        &id,
        "type=bank_letter&filename=a.pdf",
        &poly,
    )
    .await;
    assert_eq!((s, code(&v)), (415, "malformed_document"), "{v}");
    let (s, v) = upload(
        &srv,
        &dev.token,
        &id,
        "type=bank_letter&filename=a.png",
        &png(),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["content_type"], "image/png");
}

/// Approval with unscanned documents needs an explicit acknowledgement, which is audited; reviewer
/// checks a name mismatch or AML hit fails need a written, audited override.
#[tokio::test]
async fn unscanned_documents_and_name_mismatches_need_explicit_audited_staff_acts() {
    let srv = setup().await;
    let dev = srv.person("regmismatch").await;
    let rev = srv.person("regmismatchrev").await;
    staff(&srv, &rev, "reviewer").await;
    let id = ready_individual(&srv, &dev).await;
    // The bank account is in someone else's name.
    let (s, v) = srv
        .patch(
            &format!("/v1/publisher-registrations/{id}"),
            &dev.token,
            json!({ "bank": { "holder_name": "Grace Hopper", "method": "iban", "iban": IBAN } }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/publisher-registrations/{id}/submit"),
            &dev.strong,
            json!({}),
        )
        .await;
    assert_eq!(
        s, 200,
        "a reviewer judges names, so the developer can submit: {v}"
    );
    let detail = format!("/v1/staff/publisher-registrations/{id}");
    let (_, v) = srv.get(&detail, &rev.strong).await;
    let revision = v["data"]["revision"].as_i64().unwrap();
    let decide = format!("{detail}/decisions");
    let (s, v) = srv.post(&decide, &rev.strong, approve_body(revision)).await;
    assert_eq!((s, code(&v)), (422, "checks_failing"), "{v}");
    assert_eq!(v["error"]["details"]["failing"], json!(["names_match"]));
    assert_eq!(v["error"]["details"]["overridable"], json!(["names_match"]));
    let mut short = approve_body(revision);
    short["overrides"] = json!({ "names_match": "ok" });
    let (s, _) = srv.post(&decide, &rev.strong, short).await;
    assert_eq!(s, 400, "an override needs a real reason");
    let mut body = approve_body(revision);
    body["overrides"] =
        json!({ "names_match": "Spouse's joint account; marriage certificate seen" });
    body["acknowledge_unscanned_documents"] = json!(false);
    let (s, v) = srv.post(&decide, &rev.strong, body.clone()).await;
    assert_eq!((s, code(&v)), (422, "documents_not_scanned"), "{v}");
    body["acknowledge_unscanned_documents"] = json!(true);
    let (s, v) = srv.post(&decide, &rev.strong, body).await;
    assert_eq!(s, 201, "{v}");
    let details: String = srv
        .state
        .store
        .group_read(|c| -> crate::groups::GResult<String> {
            Ok(c.query_row(
                "SELECT details FROM audit_log WHERE action = 'staff.publisher_registration.decision'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert!(details.contains("marriage certificate seen"), "{details}");
    assert!(
        details.contains("\"documents_not_malware_scanned\":true"),
        "{details}"
    );
}

/// A KYC result reversed after approval is not ignored: alert, payout hold, owner told; a staff
/// member resolves it with a reason.
#[tokio::test]
async fn a_kyc_reversal_after_approval_holds_payouts_until_staff_resolve_it() {
    let srv = setup().await;
    let dev = srv.person("regrevdev").await;
    let rev = srv.person("regrevrev").await;
    staff(&srv, &rev, "reviewer").await;
    let id = ready_individual(&srv, &dev).await;
    srv.post(
        &format!("/v1/publisher-registrations/{id}/submit"),
        &dev.strong,
        json!({}),
    )
    .await;
    let detail = format!("/v1/staff/publisher-registrations/{id}");
    let (_, v) = srv.get(&detail, &rev.strong).await;
    let (s, v) = srv
        .post(
            &format!("{detail}/decisions"),
            &rev.strong,
            approve_body(v["data"]["revision"].as_i64().unwrap()),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let publisher = v["data"]["publisher_id"].as_str().unwrap().to_string();
    // Shufti changes its outcome; the signed status read applies it.
    let aid = id.clone();
    let reference: String = srv
        .state
        .store
        .group_read(move |c| -> crate::groups::GResult<String> {
            Ok(c.query_row(
                "SELECT reference FROM publisher_kyc_checks WHERE application_id = ?1",
                [&aid],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    let body = json!({ "reference": reference, "event": "verification.declined",
                       "declined_reason": "Document reported stolen" });
    let bytes = body.to_string().into_bytes();
    let o = crate::publisher_kyc::outcome(&body);
    let changed = srv
        .state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<Option<String>> {
            crate::publisher_kyc::apply_kyc_message_in(tx, &bytes, &o, "status", now_unix() as i64)
        })
        .await
        .unwrap();
    assert_eq!(changed.as_deref(), Some(id.as_str()));
    crate::publisher_registration::kyc_changed(&srv.state, &id).await;
    crate::publisher_registration::kyc_changed(&srv.state, &id).await;
    assert_eq!(
        hold_of(&srv, &publisher).await.1.as_deref(),
        Some("kyc_reversed")
    );
    assert_eq!(
        count(&srv, &format!("SELECT COUNT(*) FROM publisher_staff_alerts WHERE application_id = '{id}' AND kind = 'kyc_reversed' AND resolved_at IS NULL")).await,
        1,
        "one alert, however often it is reported"
    );
    assert_eq!(
        count(&srv, &format!("SELECT COUNT(*) FROM publisher_application_notices WHERE application_id = '{id}' AND channel = 'staff'")).await,
        1
    );
    let (s, v) = srv
        .get("/v1/staff/publisher-registration-alerts", &rev.strong)
        .await;
    assert_eq!(s, 200, "{v}");
    let alert = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["application_id"] == id.as_str())
        .unwrap()
        .clone();
    let (_, v) = srv.get(&detail, &rev.strong).await;
    assert_eq!(v["data"]["payout_hold"], "kyc_reversed");
    let resolve = format!("{detail}/alerts/{}/resolve", alert["id"].as_str().unwrap());
    let (s, _) = srv
        .post(&resolve, &rev.strong, json!({ "reason": "short" }))
        .await;
    assert_eq!(s, 400);
    let (s, v) = srv
        .post(
            &resolve,
            &rev.strong,
            json!({ "reason": "New passport verified by phone; Shufti re-run accepted" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(hold_of(&srv, &publisher).await.1, None);
    assert_eq!(
        count(&srv, "SELECT COUNT(*) FROM audit_log WHERE action = 'staff.publisher_registration.alert.resolve'").await,
        1
    );
}

/// The retention sweep marks purged exactly what it deletes in a pass.
#[tokio::test]
async fn retention_marks_purged_only_the_batch_it_deletes() {
    let srv = setup().await;
    let dev = srv.person("regretain").await;
    let (_, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_tr" }),
        )
        .await;
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let total = crate::publisher_registration_worker::PURGE_BATCH + 5;
    let aid = id.clone();
    srv.state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<()> {
            for i in 0..total {
                tx.execute(
                    "INSERT INTO publisher_application_documents (id, application_id, doc_type, filename,
                        content_type, size, sha256, blob, uploaded_by, created_at, retain_until)
                     VALUES (?1, ?2, 'bank_letter', 'a.pdf', 'application/pdf', 1, 'x', ?3, 'u', 1, 2)",
                    rusqlite::params![format!("pad_r{i:04}"), aid, format!("{:064x}", i)],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    crate::publisher_registration_worker::sweep_retention(&srv.state).await;
    let purged = format!("SELECT COUNT(*) FROM publisher_application_documents WHERE application_id = '{id}' AND purged_at IS NOT NULL");
    let unpurged_without_blob = format!("SELECT COUNT(*) FROM publisher_application_documents WHERE application_id = '{id}' AND purged_at IS NULL AND blob IS NULL");
    assert_eq!(
        count(&srv, &purged).await,
        crate::publisher_registration_worker::PURGE_BATCH
    );
    assert_eq!(
        count(&srv, &unpurged_without_blob).await,
        0,
        "no row lost its blob without being purged"
    );
    crate::publisher_registration_worker::sweep_retention(&srv.state).await;
    assert_eq!(count(&srv, &purged).await, total);
}

/// A payee push that fails for good raises a dashboard alert and tells the owner, once.
#[tokio::test]
async fn a_final_payee_push_failure_alerts_staff() {
    let srv = setup().await;
    let dev = srv.person("regpushfail").await;
    let (_, v) = srv
        .post(
            "/v1/publisher-registrations",
            &dev.token,
            json!({ "account_type": "individual_tr" }),
        )
        .await;
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let aid = id.clone();
    srv.state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<()> {
            for _ in 0..2 {
                crate::publisher_registration_worker::payee_push_failed_in(
                    tx, &aid, "pub_x", "http 400", 5,
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        count(&srv, &format!("SELECT COUNT(*) FROM publisher_staff_alerts WHERE application_id = '{id}' AND kind = 'payee_push_failed'")).await,
        1
    );
    assert_eq!(
        count(&srv, &format!("SELECT COUNT(*) FROM publisher_application_notices WHERE application_id = '{id}' AND channel = 'staff' AND state = 'payee_push_failed'")).await,
        1
    );
}
