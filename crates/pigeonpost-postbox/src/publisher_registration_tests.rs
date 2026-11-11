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
const PDF: &[u8] =
    b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n";

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

async fn setup() -> Srv {
    let srv = serve_with(|s| {
        s.blobs = Some(Arc::new(crate::blobs::Blobs::for_test("devreg")));
    })
    .await;
    srv.global_flag("marketplace").await;
    srv
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
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &dev.token,
            json!({ "display_name": "Old Way" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let publisher = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv.get("/v1/publisher-registrations", &dev.token).await;
    assert_eq!(s, 200, "{v}");
    let rows = v["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["account_type"], "legacy");
    assert_eq!(rows[0]["state"], "submitted");
    assert_eq!(rows[0]["publisher_id"], publisher.as_str());
    let legacy = rows[0]["id"].as_str().unwrap().to_string();
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
    // The M0 staff route no longer decides it.
    let rev = srv.person("reglegacyrev").await;
    staff(&srv, &rev, "reviewer").await;
    let (s, v) = srv
        .post(
            &format!("/v1/staff/publisher-applications/{publisher}/decisions"),
            &rev.strong,
            json!({ "decision": "approved", "reason": "ok" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "registration_in_progress"), "{v}");
    // Nor may the old route open a second application beside it.
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &dev.token,
            json!({ "display_name": "Again" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "application_exists"), "{v}");
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
        profile: Default::default(),
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
    )
    .unwrap();
    assert_eq!(b.currency.as_deref(), Some("TRY"));
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
