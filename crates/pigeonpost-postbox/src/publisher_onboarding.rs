//! Private, versioned publisher dossiers. Public profiles are a separate approved projection.
use crate::groups::{GErr, GResult};
use crate::marketplace::{into_response, iso, not_found, parse};
use crate::marketplace_m2::{gate, new_id, reason_of, staff_gate, wake, Need};
use crate::policy::Action;
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS publisher_dossiers(
 publisher_id TEXT PRIMARY KEY REFERENCES publisher_profiles(id),
 state TEXT NOT NULL DEFAULT 'draft' CHECK(state IN ('draft','submitted','in_review','needs_information','approved','rejected')),
 revision INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL DEFAULT 'company', country TEXT NOT NULL DEFAULT '',
 sealed BLOB, public_profile TEXT NOT NULL DEFAULT '{}', submitted_at INTEGER, updated_at INTEGER NOT NULL,
 reviewer TEXT, requested_documents TEXT NOT NULL DEFAULT '[]', kyc_reference TEXT, kyc_status TEXT NOT NULL DEFAULT 'not_started', identity_digest TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS publisher_dossiers_queue ON publisher_dossiers(state,submitted_at,publisher_id);
CREATE TABLE IF NOT EXISTS publisher_documents(
 id TEXT PRIMARY KEY, publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id), category TEXT NOT NULL,
 filename TEXT NOT NULL, mime TEXT NOT NULL, size INTEGER NOT NULL, sealed BLOB NOT NULL, created_at INTEGER NOT NULL, created_revision INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS publisher_documents_owner ON publisher_documents(publisher_id);
CREATE TABLE IF NOT EXISTS publisher_dossier_events(
 id INTEGER PRIMARY KEY AUTOINCREMENT, publisher_id TEXT NOT NULL, actor TEXT NOT NULL, event TEXT NOT NULL,
 revision INTEGER NOT NULL, note BLOB NOT NULL, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS publisher_kyc_sessions(
 reference TEXT PRIMARY KEY, publisher_id TEXT NOT NULL, identity_digest TEXT NOT NULL,
 status TEXT NOT NULL, created_at INTEGER NOT NULL, verification_url BLOB
);
"#;
const LABEL: &str = "publisher-onboarding-v1";
const COUNTRIES:&str="AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW";
const DOC_MAX: usize = 5 * 1024 * 1024;
#[derive(Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Details {
    pub kind: String,
    pub country: String,
    pub legal_name: String,
    pub registration_number: String,
    pub tax_id: String,
    pub duns: String,
    pub duns_unavailable_reason: String,
    pub address: String,
    pub contact_name: String,
    pub contact_email: String,
    pub bank_country: String,
    pub bank_name: String,
    pub account_holder: String,
    pub iban: String,
    pub account_number: String,
    pub swift: String,
    pub currency: String,
    pub public_name: String,
    pub public_summary: String,
    pub website: String,
    pub consent: bool,
    pub kyc_consent: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    revision: i64,
    details: Details,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    revision: i64,
}
fn bad(msg: &str) -> GErr {
    GErr::bad("invalid_application", msg)
}
fn conflict() -> GErr {
    GErr::conflict(
        "revision_conflict",
        "The application changed. Reload before continuing.",
    )
}
fn internal() -> GErr {
    GErr::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "private_storage_error",
        "Private storage is unavailable",
    )
}
fn sealed(state: &AppState, id: &str, v: &[u8]) -> GResult<Vec<u8>> {
    state
        .vault
        .seal_labeled(LABEL, id.as_bytes(), v)
        .map_err(|_| internal())
}
fn opened(state: &AppState, id: &str, v: &[u8]) -> GResult<Vec<u8>> {
    state
        .vault
        .open_labeled(LABEL, id.as_bytes(), v)
        .map_err(|_| internal())
}
fn private<T: axum::response::IntoResponse>(result: GResult<T>) -> Response {
    let mut r = into_response(result);
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}
pub(crate) fn init_in(c: &Connection, id: &str, now: i64) -> GResult<()> {
    c.execute(
        "INSERT OR IGNORE INTO publisher_dossiers(publisher_id,updated_at,state,public_profile) SELECT id,?2,CASE WHEN status IN ('approved','rejected') THEN status ELSE 'draft' END,json_object('name',display_name,'summary','','website','') FROM publisher_profiles WHERE id=?1",
        params![id, now],
    )?;
    Ok(())
}
fn editable(c: &Connection, id: &str, revision: i64) -> GResult<()> {
    let row:Option<(String,i64,String)>=c.query_row("SELECT d.state,d.revision,p.status FROM publisher_dossiers d JOIN publisher_profiles p ON p.id=d.publisher_id WHERE d.publisher_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    match row {
        Some((s, r, p))
            if r == revision
                && p == "applied"
                && matches!(s.as_str(), "draft" | "needs_information") =>
        {
            Ok(())
        }
        Some((_, r, _)) if r != revision => Err(conflict()),
        _ => Err(GErr::conflict(
            "application_locked",
            "This application cannot be edited in its current state",
        )),
    }
}
fn details_in(c: &Connection, state: &AppState, id: &str) -> GResult<Details> {
    let b: Option<Vec<u8>> = c.query_row(
        "SELECT sealed FROM publisher_dossiers WHERE publisher_id=?1",
        [id],
        |r| r.get(0),
    )?;
    match b {
        Some(b) => serde_json::from_slice(&opened(state, id, &b)?).map_err(|_| internal()),
        None => Ok(Details::default()),
    }
}
fn identity(d: &Details) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}\0{}", d.kind, d.country, d.legal_name.trim()).as_bytes())
    )
}
fn dossier_event(
    c: &Connection,
    state: &AppState,
    id: &str,
    actor: &str,
    kind: &str,
    revision: i64,
    note: &str,
) -> GResult<()> {
    c.execute("INSERT INTO publisher_dossier_events(publisher_id,actor,event,revision,note,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,actor,kind,revision,sealed(state,id,note.as_bytes())?,now_unix() as i64])?;
    Ok(())
}
fn validate(d: &Details, complete: bool) -> GResult<()> {
    if !matches!(d.kind.as_str(), "company" | "individual") {
        return Err(bad("Choose company or individual"));
    }
    for x in [&d.country, &d.bank_country] {
        if !x.is_empty() && !COUNTRIES.split_whitespace().any(|code| code == x) {
            return Err(bad("Use a two-letter country code"));
        }
    }
    let value = serde_json::to_value(d).map_err(|_| internal())?;
    for (k, v) in value.as_object().unwrap() {
        if let Some(s) = v.as_str() {
            let max = if k == "public_summary" || k == "address" || k == "duns_unavailable_reason" {
                2000
            } else {
                256
            };
            if s.chars().count() > max || s.chars().any(|c| c.is_control() && c != '\n') {
                return Err(bad(
                    "A field is too long or contains unsupported characters",
                ));
            }
        }
    }
    if !d.duns.is_empty() && (d.duns.len() != 9 || !d.duns.bytes().all(|b| b.is_ascii_digit())) {
        return Err(bad("DUNS must contain nine digits"));
    }
    if !d.website.is_empty() {
        let u = reqwest::Url::parse(&d.website).map_err(|_| bad("Website must be an HTTPS URL"))?;
        if u.scheme() != "https"
            || u.host_str().is_none()
            || !u.username().is_empty()
            || u.password().is_some()
        {
            return Err(bad("Website must be an HTTPS URL"));
        }
    }
    if !complete {
        return Ok(());
    }
    for s in [
        &d.country,
        &d.legal_name,
        &d.address,
        &d.contact_name,
        &d.contact_email,
        &d.bank_country,
        &d.bank_name,
        &d.account_holder,
        &d.currency,
        &d.public_name,
    ] {
        if s.trim().is_empty() {
            return Err(bad(
                "Complete legal, contact, bank and public profile details",
            ));
        }
    }
    if !d.contact_email.contains('@') || d.contact_email.contains(char::is_whitespace) {
        return Err(bad("Enter a contact email"));
    }
    if d.currency.len() != 3 || !d.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(bad("Currency must be a three-letter code"));
    }
    if d.iban.trim().is_empty() && (d.account_number.trim().is_empty() || d.swift.trim().is_empty())
    {
        return Err(bad("Provide an IBAN, or account number and SWIFT/BIC"));
    }
    if !d.iban.is_empty() && !valid_iban(&d.iban) {
        return Err(bad("IBAN checksum is invalid"));
    }
    if !d.swift.is_empty()
        && (!matches!(d.swift.len(), 8 | 11) || !d.swift.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return Err(bad("SWIFT/BIC must have 8 or 11 letters/digits"));
    }
    if d.kind == "company"
        && (d.tax_id.trim().is_empty()
            || d.registration_number.trim().is_empty()
            || (d.duns.is_empty() && d.duns_unavailable_reason.trim().is_empty()))
    {
        return Err(bad(
            "Provide company registration, tax ID, and DUNS or an explanation",
        ));
    }
    if !d.consent {
        return Err(bad("Confirm the information and verification consent"));
    }
    Ok(())
}
fn valid_iban(s: &str) -> bool {
    let s = s.replace(' ', "").to_uppercase();
    if !(15..=34).contains(&s.len())
        || !s.bytes().all(|b| b.is_ascii_alphanumeric())
        || !s.as_bytes()[..2].iter().all(u8::is_ascii_alphabetic)
        || !s.as_bytes()[2..4].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    let mut n = 0u32;
    for b in s.bytes().skip(4).chain(s.bytes().take(4)) {
        if b.is_ascii_digit() {
            n = (n * 10 + (b - b'0') as u32) % 97
        } else {
            n = (n * 100 + (b - b'A' + 10) as u32) % 97
        }
    }
    n == 1
}
fn complete_in(c: &Connection, state: &AppState, id: &str) -> GResult<Details> {
    let d = details_in(c, state, id)?;
    validate(&d, true)?;
    let requested: String = c.query_row(
        "SELECT requested_documents FROM publisher_dossiers WHERE publisher_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let requested: Vec<String> = serde_json::from_str(&requested).map_err(|_| internal())?;
    for category in requested {
        let count:i64=c.query_row("SELECT COUNT(*) FROM publisher_documents WHERE publisher_id=?1 AND category=?2 AND created_revision > COALESCE((SELECT MAX(revision) FROM publisher_dossier_events WHERE publisher_id=?1 AND event='needs_information'),0)",params![id,category],|r|r.get(0))?;
        if count == 0 {
            return Err(bad("Upload the additional documents requested by staff"));
        }
    }

    if d.kind == "individual" {
        let ok:bool=c.query_row("SELECT kyc_status='verified' AND identity_digest=?2 FROM publisher_dossiers WHERE publisher_id=?1",params![id,identity(&d)],|r|r.get(0))?;
        if !ok {
            return Err(bad("Complete identity verification before submitting"));
        }
    } else {
        for category in ["registration", "tax"] {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM publisher_documents WHERE publisher_id=?1 AND category=?2",
                params![id, category],
                |r| r.get(0),
            )?;
            if n == 0 {
                return Err(bad("Upload company registration/activity and tax evidence"));
            }
        }
    }
    Ok(d)
}
fn dossier_in(c: &Connection, state: &AppState, id: &str) -> GResult<Value> {
    let mut v:Value=c.query_row("SELECT state,revision,kind,country,submitted_at,updated_at,reviewer,kyc_status FROM publisher_dossiers WHERE publisher_id=?1",[id],|r|Ok(json!({"id":id,"state":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"country":r.get::<_,String>(3)?,"submitted_at":r.get::<_,Option<i64>>(4)?,"updated_at":r.get::<_,i64>(5)?,"reviewer":r.get::<_,Option<String>>(6)?,"kyc_status":r.get::<_,String>(7)?})))?;
    let certification: String = c.query_row(
        "SELECT status FROM publisher_profiles WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if certification == "revoked" {
        v["state"] = json!("revoked");
    }
    v["details"] = serde_json::to_value(details_in(c, state, id)?).map_err(|_| internal())?;
    let requested: String = c.query_row(
        "SELECT requested_documents FROM publisher_dossiers WHERE publisher_id=?1",
        [id],
        |r| r.get(0),
    )?;
    v["requested_documents"] = serde_json::from_str(&requested).map_err(|_| internal())?;

    let mut stmt=c.prepare("SELECT id,category,filename,mime,size,created_at FROM publisher_documents WHERE publisher_id=?1 ORDER BY created_at,id")?;
    v["documents"]=json!(stmt.query_map([id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"category":r.get::<_,String>(1)?,"filename":r.get::<_,String>(2)?,"mime":r.get::<_,String>(3)?,"size":r.get::<_,i64>(4)?,"created_at":r.get::<_,i64>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?);
    let mut stmt=c.prepare("SELECT actor,event,revision,note,created_at FROM publisher_dossier_events WHERE publisher_id=?1 ORDER BY id DESC LIMIT 100")?;
    let rows = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    v["history"]=json!(rows.into_iter().map(|(a,e,r,n,t)|Ok(json!({"actor":a,"event":e,"revision":r,"note":String::from_utf8(opened(state,id,&n)?).map_err(|_|internal())?,"created_at":iso(t)}))).collect::<GResult<Vec<Value>>>()?);
    Ok(v)
}
async fn dossier_owner(state: &AppState, h: &HeaderMap, id: &str) -> GResult<String> {
    let ctx = gate(state, h, id, Action::PublisherManage, Need::Admin).await?;
    if ctx.p.owner_kind != "account" && !ctx.access.grant {
        return Err(GErr::forbidden(
            "publisher_role_required",
            "Only the owner or organization administrators can access legal documents",
        ));
    }
    Ok(ctx.account)
}
async fn reviewer_gate(
    s: &AppState,
    h: &HeaderMap,
    id: &str,
) -> GResult<(crate::Principal, String)> {
    let pid = id.to_string();
    let exists = s
        .store
        .group_read(move |c| -> GResult<bool> {
            Ok(crate::marketplace_m2::pub_in(c, &pid)?.is_some())
        })
        .await?;
    let actor = staff_gate(s, h, Action::NestReview, id, exists.then(|| id.to_string())).await?;
    if !exists {
        return Err(not_found());
    }
    Ok(actor)
}
fn owner_write_in(c: &Connection, id: &str, who: &str) -> GResult<()> {
    let allowed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM publisher_profiles p WHERE p.id=?1 AND ((p.owner_kind='account' AND p.owner_id=?2) OR (p.owner_kind='org' AND EXISTS(SELECT 1 FROM orgs o JOIN org_members m ON m.org_id=o.id JOIN org_roles r ON r.org_id=m.org_id AND r.member_id=m.id WHERE o.id=p.owner_id AND o.status='active' AND m.account_id=?2 AND m.status='active' AND r.role IN ('owner','admin')))))",params![id,who],|r|r.get(0))?;
    if !allowed {
        return Err(not_found());
    }
    Ok(())
}
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publisher-applications/{id}/dossier",
            get(read_dossier).put(save),
        )
        .route("/v1/publisher-applications/{id}/submit", post(submit))
        .route("/v1/publisher-applications/{id}/documents", post(upload))
        .route(
            "/v1/publisher-applications/{id}/documents/{doc}",
            get(download).delete(remove),
        )
        .route("/v1/publisher-applications/{id}/kyc", post(start_kyc))
        .route("/v1/staff/publisher-dossiers", get(queue))
        .route("/v1/staff/publisher-dossiers/{id}", get(staff_read))
        .route(
            "/v1/staff/publisher-dossiers/{id}/documents/{doc}",
            get(staff_download),
        )
        .route("/v1/staff/publisher-dossiers/{id}/review", post(review))
        .route("/v1/marketplace/partners", get(partners))
        .route("/v1/marketplace/partners/{id}", get(partner))
        .route("/v1/verification/shuftipro", post(kyc_callback))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
}
async fn read_dossier(State(s): State<AppState>, h: HeaderMap, Path(id): Path<String>) -> Response {
    private(
        async {
            dossier_owner(&s, &h, &id).await?;
            let st = s.clone();
            let v = s
                .store
                .group_tx(move |c| {
                    init_in(c, &id, now_unix() as i64)?;
                    dossier_in(c, &st, &id)
                })
                .await?;
            Ok(Json(json!({"data":v})))
        }
        .await,
    )
}
async fn staff_read(State(s): State<AppState>, h: HeaderMap, Path(id): Path<String>) -> Response {
    private(
        async {
            reviewer_gate(&s, &h, &id).await?;
            let st = s.clone();
            let v = s
                .store
                .group_tx(move |c| {
                    if crate::marketplace_m2::pub_in(c, &id)?.is_none() {
                        return Err(not_found());
                    }
                    init_in(c, &id, now_unix() as i64)?;
                    dossier_in(c, &st, &id)
                })
                .await?;
            Ok(Json(json!({"data":v})))
        }
        .await,
    )
}
async fn save(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    private(async{
 let who=dossier_owner(&s,&h,&id).await?;let req:Save=parse(body)?;validate(&req.details,false)?;
 let st=s.clone();let v=s.store.group_tx(move|c|{init_in(c,&id,now_unix()as i64)?;owner_write_in(c,&id,&who)?;editable(c,&id,req.revision)?;let d=req.details;let digest=identity(&d);let bytes=serde_json::to_vec(&d).map_err(|_|internal())?;
 c.execute("UPDATE publisher_dossiers SET sealed=?2,kind=?3,country=?4,revision=revision+1,updated_at=?5,kyc_status=CASE WHEN identity_digest=?6 THEN kyc_status ELSE 'not_started' END,kyc_reference=CASE WHEN identity_digest=?6 THEN kyc_reference ELSE NULL END,identity_digest=?6 WHERE publisher_id=?1",params![id,sealed(&st,&id,&bytes)?,d.kind,d.country,now_unix()as i64,digest])?;
 dossier_event(c,&st,&id,&who,"draft_saved",req.revision+1,"")?;dossier_in(c,&st,&id)}).await?;Ok(Json(json!({"data":v})))}.await)
}
async fn submit(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    private(async{
 let who=dossier_owner(&s,&h,&id).await?;let req:Revision=parse(body)?;let st=s.clone();let (v,w)=s.store.group_tx(move|c|{
 owner_write_in(c,&id,&who)?;editable(c,&id,req.revision)?;complete_in(c,&st,&id)?;c.execute("UPDATE publisher_dossiers SET state='submitted',revision=revision+1,submitted_at=?2,updated_at=?2 WHERE publisher_id=?1",params![id,now_unix()as i64])?;
 dossier_event(c,&st,&id,&who,"submitted",req.revision+1,"")?;
 let mut w=crate::store::marketplace::publisher_event_in(c,&id,"nest.publisher.application_submitted",&json!({"publisher_id":id}),now_unix())?;
 let mut stmt=c.prepare("SELECT DISTINCT account_id FROM staff_roles WHERE role='reviewer' AND revoked_at IS NULL")?;
 let reviewers=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
 for reviewer in reviewers {crate::store::marketplace::account_event_in(c,&reviewer,"nest.publisher.application_submitted",&json!({"publisher_id":id}),now_unix())?;w.push(reviewer);}
Ok::<_,GErr>((dossier_in(c,&st,&id)?,w))}).await?;wake(&s,w);Ok(Json(json!({"data":v})))}.await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Upload {
    revision: i64,
    category: String,
    filename: String,
    content_base64: String,
}
fn file_type(b: &[u8]) -> GResult<&'static str> {
    if b.starts_with(b"%PDF-") {
        Ok("application/pdf")
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if b.starts_with(&[255, 216, 255]) {
        Ok("image/jpeg")
    } else {
        Err(bad("Upload a PDF, PNG or JPEG"))
    }
}
async fn upload(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    private(async{
 let who=dossier_owner(&s,&h,&id).await?;let r:Upload=parse(body)?;if !matches!(r.category.as_str(),"registration"|"tax"|"bank"|"authorization"|"additional"){return Err(bad("Unknown document category"))}
 if r.filename.is_empty()||r.filename.len()>120||r.filename.contains(['/', '\\'])||r.filename.chars().any(char::is_control){return Err(bad("Invalid filename"))}
 let bytes=crate::b64_decode(&r.content_base64).map_err(|_|bad("Invalid document encoding"))?;if bytes.is_empty()||bytes.len()>DOC_MAX{return Err(bad("Documents must be 5 MiB or smaller"))}let mime=file_type(&bytes)?;let doc=new_id("pdoc");let encrypted=sealed(&s,&doc,&bytes)?;
 let st=s.clone();let v=s.store.group_tx(move|c|{owner_write_in(c,&id,&who)?;editable(c,&id,r.revision)?;let count:i64=c.query_row("SELECT COUNT(*) FROM publisher_documents WHERE publisher_id=?1",[&id],|r|r.get(0))?;if count>=12{return Err(bad("Maximum 12 documents; remove an old draft document first"))}
 c.execute("INSERT INTO publisher_documents VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![doc,id,r.category,r.filename,mime,bytes.len()as i64,encrypted,now_unix()as i64,r.revision+1])?;
 c.execute("UPDATE publisher_dossiers SET revision=revision+1,updated_at=?2 WHERE publisher_id=?1",params![id,now_unix()as i64])?;dossier_event(c,&st,&id,&who,"document_added",r.revision+1,&r.category)?;dossier_in(c,&st,&id)}).await?;Ok(Json(json!({"data":v})))}.await)
}
async fn remove(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, doc)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    private(
        async {
            let who = dossier_owner(&s, &h, &id).await?;
            let r: Revision = parse(body)?;
            let st = s.clone();
            let v = s
                .store
                .group_tx(move |c| {
                    editable(c, &id, r.revision)?;
                    if c.execute(
                        "DELETE FROM publisher_documents WHERE id=?1 AND publisher_id=?2",
                        params![doc, id],
                    )? != 1
                    {
                        return Err(not_found());
                    }
                    c.execute(
                        "UPDATE publisher_dossiers SET revision=revision+1 WHERE publisher_id=?1",
                        [&id],
                    )?;
                    dossier_event(c, &st, &id, &who, "document_removed", r.revision + 1, "")?;
                    dossier_in(c, &st, &id)
                })
                .await?;
            Ok(Json(json!({"data":v})))
        }
        .await,
    )
}
async fn document(s: &AppState, id: String, doc: String, actor: String) -> GResult<Json<Value>> {
    let st = s.clone();
    let v=s.store.group_tx(move|c|{let row:Option<(String,String,Vec<u8>)>=c.query_row("SELECT filename,mime,sealed FROM publisher_documents WHERE id=?1 AND publisher_id=?2",params![doc,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;let (name,mime,b)=row.ok_or_else(not_found)?;dossier_event(c,&st,&id,&actor,"document_downloaded",0,"")?;Ok::<_,GErr>(json!({"filename":name,"mime":mime,"content_base64":crate::b64_encode(&opened(&st,&doc,&b)?)}))}).await?;
    Ok(Json(json!({"data":v})))
}
async fn download(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, doc)): Path<(String, String)>,
) -> Response {
    private(
        async {
            let who = dossier_owner(&s, &h, &id).await?;
            document(&s, id, doc, who).await
        }
        .await,
    )
}
async fn staff_download(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, doc)): Path<(String, String)>,
) -> Response {
    private(
        async {
            let (_, who) = reviewer_gate(&s, &h, &id).await?;
            document(&s, id, doc, who).await
        }
        .await,
    )
}
async fn queue(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    private(async{
 let (_,who)=staff_gate(&s,&h,Action::NestReview,"publisher-dossiers",None).await?;
 let status=q.get("status").cloned().unwrap_or("submitted".into());if !matches!(status.as_str(),"all"|"draft"|"submitted"|"in_review"|"needs_information"|"approved"|"rejected"|"revoked"){return Err(bad("Unknown application status"))}
 let search=q.get("q").cloned().unwrap_or_default();if search.len()>100{return Err(bad("Search is too long"))}
 let kind=q.get("kind").cloned().unwrap_or_default();let country=q.get("country").cloned().unwrap_or_default();
 let offset=q.get("offset").and_then(|v|v.parse::<i64>().ok()).unwrap_or(0).clamp(0,100000);
 let order=match q.get("sort").map(String::as_str){Some("newest")=>"d.updated_at DESC,p.id",Some("name")=>"p.display_name COLLATE NOCASE,p.id",_=>"d.updated_at,p.id"};
 let v=s.store.group_read(move|c|->GResult<Value>{
 let filter=" FROM publisher_profiles p LEFT JOIN publisher_dossiers d ON d.publisher_id=p.id WHERE p.owner_kind<>'platform' AND (?1='all' OR CASE WHEN p.status='revoked' THEN 'revoked' ELSE COALESCE(d.state,CASE WHEN p.status='applied' THEN 'draft' ELSE p.status END) END=?1) AND (?2='' OR instr(lower(p.display_name),lower(?2))>0 OR instr(p.id,?2)>0) AND (?3='' OR d.kind=?3) AND (?4='' OR d.country=?4)";
 let total:i64=c.query_row(&format!("SELECT COUNT(*){filter}"),params![status,search,kind,country],|r|r.get(0))?;
 let mut stmt=c.prepare(&format!("SELECT p.id,p.display_name,CASE WHEN p.status='revoked' THEN 'revoked' ELSE COALESCE(d.state,p.status) END,COALESCE(d.revision,0),COALESCE(d.kind,''),COALESCE(d.country,''),d.submitted_at,COALESCE(d.updated_at,p.updated_at),d.reviewer,COALESCE(d.kyc_status,'not_started'){filter} ORDER BY {order} LIMIT 25 OFFSET ?5"))?;
 let rows=stmt.query_map(params![status,search,kind,country,offset],|r|Ok(json!({"id":r.get::<_,String>(0)?,"display_name":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"revision":r.get::<_,i64>(3)?,"kind":r.get::<_,String>(4)?,"country":r.get::<_,String>(5)?,"submitted_at":r.get::<_,Option<i64>>(6)?,"updated_at":iso(r.get(7)?),"reviewer":r.get::<_,Option<String>>(8)?,"kyc_status":r.get::<_,String>(9)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
 let mut out=Vec::new();for mut v in rows{v["conflict"]=json!(crate::store::marketplace::publisher_conflict_in(c,&who,v["id"].as_str().unwrap())?);out.push(v)}
 Ok(json!({"data":out,"total":total,"offset":offset,"next_offset":if offset+25<total{Some(offset+25)}else{None}}))}).await?;Ok(Json(v))}.await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    revision: i64,
    decision: String,
    reason: String,
    #[serde(default)]
    requested_documents: Vec<String>,
}
pub(crate) async fn review(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    private(async{
 let (principal,who)=reviewer_gate(&s,&h,&id).await?;let req:Review=parse(body)?;let reason=reason_of(&req.reason)?;
 if req.requested_documents.len()>5||req.requested_documents.iter().any(|c|!matches!(c.as_str(),"registration"|"tax"|"bank"|"authorization"|"additional")){return Err(bad("Unknown requested document category"))}
 if !matches!(req.decision.as_str(),"in_review"|"needs_information"|"approved"|"rejected"){return Err(bad("Unknown review decision"))}
 let audit=crate::marketplace_m2::audit_row(&s,&principal,&who,"marketplace.publisher.dossier_review",format!("publisher:{id}"),json!({"decision":req.decision,"revision":req.revision}),&None,&h);
 let st=s.clone();let (v,w)=s.store.group_tx(move|c|{
 // Recheck live staff membership and conflicts in the same transaction as the decision.
 if !crate::store::marketplace::live_staff_roles_in(c,&who)?.contains(&crate::policy::StaffRole::Reviewer)||crate::store::marketplace::publisher_conflict_in(c,&who,&id)?{return Err(GErr::forbidden("staff_required","Reviewer access changed"))}
 let row:Option<(String,i64)>=c.query_row("SELECT state,revision FROM publisher_dossiers WHERE publisher_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
 let (status,revision)=row.ok_or_else(not_found)?;if revision!=req.revision{return Err(conflict())}if !matches!(status.as_str(),"submitted"|"in_review"){return Err(GErr::conflict("application_decided","Only submitted applications can be reviewed"))}
 if req.decision=="approved" {let d=complete_in(c,&st,&id)?;let public=json!({"name":d.public_name,"summary":d.public_summary,"website":d.website});c.execute("UPDATE publisher_dossiers SET public_profile=?2 WHERE publisher_id=?1",params![id,public.to_string()])?;}
 if matches!(req.decision.as_str(),"approved"|"rejected"){
 let n=c.execute("UPDATE publisher_profiles SET status=?2,decided_by=?3,decided_at=?4,decision_reason=?5,updated_at=?4 WHERE id=?1 AND status='applied'",params![id,req.decision,who,now_unix()as i64,reason])?;if n!=1{return Err(conflict())}
 c.execute("INSERT INTO publisher_certifications(id,publisher_id,decision,reason,decided_by,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![new_id("crt"),id,req.decision,reason,who,now_unix()as i64])?;
 }
 c.execute("UPDATE publisher_dossiers SET state=?2,revision=revision+1,reviewer=?3,updated_at=?4,requested_documents=?5 WHERE publisher_id=?1",params![id,req.decision,who,now_unix()as i64,serde_json::to_string(&req.requested_documents).map_err(|_|internal())?])?;
 dossier_event(c,&st,&id,&who,&req.decision,revision+1,&reason)?;crate::store::audit::append_in(c,&audit)?;
 let w=crate::store::marketplace::publisher_event_in(c,&id,"nest.publisher.decision",&json!({"publisher_id":id,"status":req.decision}),now_unix())?;let mut data=dossier_in(c,&st,&id)?;data["status"]=json!(req.decision);data["decision_reason"]=json!(reason);Ok::<_,GErr>((data,w))}).await?;wake(&s,w);Ok((StatusCode::CREATED,Json(json!({"data":v}))))}.await)
}

// Only an approved projection is public. Legal and payout fields never participate in SELECTs.
async fn public_enabled(s: &AppState) -> GResult<()> {
    if !crate::filtering::feature_on(s, None, crate::capabilities::MARKETPLACE).await {
        return Err(crate::marketplace::feature_off());
    }
    let f = crate::policy::Facts {
        listing_visible: true,
        ..crate::marketplace::facts(s)
    };
    crate::policy::authorize(
        &crate::policy::Actor::Anonymous,
        Action::NestDiscover,
        &crate::policy::Resource::NestListing("partners"),
        &f,
    )
    .map_err(|d| crate::marketplace::deny(d, false))?;
    Ok(())
}
async fn partners(State(s): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Response {
    into_response(async{
 public_enabled(&s).await?;let search=q.get("q").cloned().unwrap_or_default();if search.len()>100{return Err(bad("Search is too long"))}let offset=q.get("offset").and_then(|v|v.parse::<i64>().ok()).unwrap_or(0).clamp(0,100000);
 let v=s.store.group_read(move|c|->GResult<Value>{let mut stmt=c.prepare("SELECT p.id,CASE WHEN d.state='approved' THEN d.public_profile ELSE json_object('name',p.display_name,'summary','','website','') END,(SELECT COUNT(*) FROM nest_listings l WHERE l.publisher_id=p.id AND l.status='published' AND l.visibility='public') FROM publisher_profiles p LEFT JOIN publisher_dossiers d ON d.publisher_id=p.id WHERE p.status='approved' AND p.owner_kind<>'platform' AND (?1='' OR instr(lower(CASE WHEN d.state='approved' THEN json_extract(d.public_profile,'$.name') ELSE p.display_name END),lower(?1))>0) ORDER BY p.display_name COLLATE NOCASE,p.id LIMIT 26 OFFSET ?2")?;
 let mut rows=stmt.query_map(params![search,offset],|r|{let profile:String=r.get(1)?;Ok(json!({"id":r.get::<_,String>(0)?,"profile":serde_json::from_str::<Value>(&profile).unwrap_or(json!({})),"app_count":r.get::<_,i64>(2)?}))})?.collect::<rusqlite::Result<Vec<_>>>()?;let more=rows.len()>25;rows.truncate(25);Ok(json!({"data":rows,"next_offset":if more{Some(offset+25)}else{None}}))}).await?;Ok(Json(v))}.await)
}
async fn partner(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    into_response(async{public_enabled(&s).await?;let v=s.store.group_read(move|c|->GResult<Value>{let p:Option<String>=c.query_row("SELECT CASE WHEN d.state='approved' THEN d.public_profile ELSE json_object('name',p.display_name,'summary','','website','') END FROM publisher_profiles p LEFT JOIN publisher_dossiers d ON d.publisher_id=p.id WHERE p.id=?1 AND p.status='approved' AND p.owner_kind<>'platform'",[&id],|r|r.get(0)).optional()?;let profile:Value=serde_json::from_str(&p.ok_or_else(not_found)?).map_err(|_|internal())?;
 let mut stmt=c.prepare("SELECT id,title,summary FROM nest_listings WHERE publisher_id=?1 AND status='published' AND visibility='public' ORDER BY title,id LIMIT 100")?;
 let apps=stmt.query_map([&id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"summary":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(json!({"id":id,"profile":profile,"apps":apps}))}).await?;Ok(Json(json!({"data":v})))}.await)
}

struct Shufti {
    client: String,
    secret: String,
    callback: String,
    redirect: String,
    legacy: bool,
}
impl Shufti {
    fn shufti_config() -> GResult<Self> {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| {
                    GErr::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "kyc_not_configured",
                        "Identity verification is not configured yet. Your draft is saved.",
                    )
                })
        };
        let c = Self {
            client: get("POSTBOX_SHUFTI_CLIENT_ID")?,
            secret: get("POSTBOX_SHUFTI_SECRET")?,
            callback: get("POSTBOX_SHUFTI_CALLBACK_URL")?,
            redirect: get("POSTBOX_SHUFTI_REDIRECT_URL")?,
            legacy: std::env::var("POSTBOX_SHUFTI_SIGNATURE_MODE").as_deref() == Ok("legacy"),
        };
        for v in [&c.callback, &c.redirect] {
            let u = reqwest::Url::parse(v).map_err(|_| internal())?;
            if u.scheme() != "https" || u.host_str().is_none() || !u.username().is_empty() {
                return Err(internal());
            }
        }
        Ok(c)
    }
    fn verify_shufti_response(&self, body: &[u8], signature: &str) -> bool {
        verify_signature(body, signature, &self.secret, self.legacy)
    }
}
fn verify_signature(body: &[u8], signature: &str, secret: &str, legacy: bool) -> bool {
    use subtle::ConstantTimeEq;
    let suffix = if legacy {
        secret.to_string()
    } else {
        format!("{:x}", Sha256::digest(secret.as_bytes()))
    };
    let mut hash = Sha256::new();
    hash.update(body);
    hash.update(suffix.as_bytes());
    let expected = format!("{:x}", hash.finalize());
    signature.len() == 64 && bool::from(expected.as_bytes().ct_eq(signature.as_bytes()))
}
async fn start_kyc(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    private(async{
 let who=dossier_owner(&s,&h,&id).await?;let req:Revision=parse(body)?;let config=Shufti::shufti_config()?;
 let pid=id.clone();let st=s.clone();
 let resume=s.store.group_read(move|c|->GResult<Option<String>>{editable(c,&pid,req.revision)?;let row:Option<(String,Vec<u8>)>=c.query_row("SELECT k.reference,k.verification_url FROM publisher_kyc_sessions k JOIN publisher_dossiers d ON d.kyc_reference=k.reference WHERE k.publisher_id=?1 AND k.identity_digest=d.identity_digest AND k.status='pending' AND k.created_at>?2 AND k.verification_url IS NOT NULL",params![pid,now_unix()as i64-3600],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;row.map(|(r,u)|String::from_utf8(opened(&st,&r,&u)?).map_err(|_|internal())).transpose()}).await?;
 if let Some(url)=resume{return Ok(Json(json!({"data":{"verification_url":url}})))}
 let reference=new_id("ppkyc");let r=reference.clone();let aid=id.clone();let st=s.clone();
 let d=s.store.group_tx(move|c|{owner_write_in(c,&aid,&who)?;editable(c,&aid,req.revision)?;let d=details_in(c,&st,&aid)?;if !d.kyc_consent||d.kind!="individual"||d.legal_name.trim().is_empty()||d.country.len()!=2{return Err(bad("Save your individual legal name and country first"))}
 let pending:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM publisher_kyc_sessions WHERE publisher_id=?1 AND created_at>?2)",params![aid,now_unix()as i64-3600],|r|r.get(0))?;if pending{return Err(GErr::conflict("kyc_recently_started","A verification was recently started. Wait an hour before starting another."))}
 c.execute("INSERT INTO publisher_kyc_sessions(reference,publisher_id,identity_digest,status,created_at) VALUES(?1,?2,?3,'pending',?4)",params![r,aid,identity(&d),now_unix()as i64])?;c.execute("UPDATE publisher_dossiers SET kyc_reference=?2,kyc_status='pending' WHERE publisher_id=?1",params![aid,r])?;dossier_event(c,&st,&aid,&who,"kyc_started",req.revision,"")?;Ok(d)}).await?;
 let payload=json!({"reference":reference,"callback_url":config.callback,"redirect_url":config.redirect,"country":d.country,"language":"EN","ttl":60,"verification_mode":"any","document":{"supported_types":["id_card","passport","driving_license"],"name":{"full_name":d.legal_name},"proof":"","additional_proof":""},"face":{"proof":""}});
 let response=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(25)).build().map_err(|_|internal())?.post("https://api.shuftipro.com/").basic_auth(&config.client,Some(&config.secret)).json(&payload).send().await.map_err(|_|GErr::new(StatusCode::BAD_GATEWAY,"kyc_unavailable","Verification provider could not be reached"))?;
 let sig=response.headers().get("signature").and_then(|v|v.to_str().ok()).unwrap_or("").to_string();let bytes=response.bytes().await.map_err(|_|internal())?;if bytes.len()>1024*1024||!config.verify_shufti_response(&bytes,&sig){return Err(GErr::new(StatusCode::BAD_GATEWAY,"kyc_invalid_signature","Verification provider response could not be verified"))}
 let v:Value=serde_json::from_slice(&bytes).map_err(|_|internal())?;if v["reference"]!=reference||v["event"]!="request.pending"{return Err(GErr::new(StatusCode::BAD_GATEWAY,"kyc_unavailable","Verification provider could not start the session"))}
 let u=reqwest::Url::parse(v["verification_url"].as_str().unwrap_or("")).map_err(|_|internal())?;let host=u.host_str().unwrap_or("");if u.scheme()!="https"||!(host=="shuftipro.com"||host.ends_with(".shuftipro.com"))||!u.username().is_empty(){return Err(internal())}
 let link=sealed(&s,&reference,u.as_str().as_bytes())?;let rid=reference.clone();s.store.group_tx(move|c|->GResult<()>{c.execute("UPDATE publisher_kyc_sessions SET verification_url=?2 WHERE reference=?1",params![rid,link])?;Ok(())}).await?;
 Ok(Json(json!({"data":{"verification_url":u.as_str(),"reference":reference}})))}.await)
}
async fn kyc_callback(State(s): State<AppState>, h: HeaderMap, body: Bytes) -> Response {
    private(
        async {
            let config = Shufti::shufti_config()?;
            if body.len() > 1024 * 1024
                || !config.verify_shufti_response(
                    &body,
                    h.get_all("signature")
                        .iter()
                        .next()
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or(""),
                )
            {
                return Err(GErr::new(
                    StatusCode::UNAUTHORIZED,
                    "invalid_signature",
                    "Invalid provider signature",
                ));
            }
            let v: Value = serde_json::from_slice(&body).map_err(|_| bad("Invalid callback"))?;
            let reference = v["reference"]
                .as_str()
                .ok_or_else(|| bad("Missing reference"))?
                .to_string();
            let event_name = v["event"].as_str().unwrap_or("");
            let status = match event_name {
                "verification.accepted"
                    if v["verification_result"]["document"] == 1
                        && v["verification_result"]["face"] == 1 =>
                {
                    "verified"
                }
                "verification.declined"
                | "verification.cancelled"
                | "request.timeout"
                | "request.deleted" => "failed",
                "verification.status.changed" => "review_required",
                _ => "pending",
            };
            record_kyc_result(&s, reference, status).await?;
            Ok(Json(json!({"received":true})))
        }
        .await,
    )
}

async fn record_kyc_result(s: &AppState, reference: String, status: &'static str) -> GResult<()> {
    let st = s.clone();
    s.store
        .group_tx(move |c| -> GResult<()> { apply_kyc_in(c, &st, &reference, status) })
        .await
}

fn apply_kyc_in(c: &Connection, st: &AppState, reference: &str, status: &str) -> GResult<()> {
    let row:Option<(String,String,String)>=c.query_row("SELECT publisher_id,identity_digest,status FROM publisher_kyc_sessions WHERE reference=?1",[&reference],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let (id, digest, old) = row.ok_or_else(not_found)?;
    if old != "pending" || status == "pending" {
        return Ok(());
    }
    c.execute(
        "UPDATE publisher_kyc_sessions SET status=?2 WHERE reference=?1",
        params![reference, status],
    )?;
    let changed=c.execute("UPDATE publisher_dossiers SET kyc_status=?2 WHERE publisher_id=?1 AND kyc_reference=?3 AND identity_digest=?4",params![id,status,reference,digest])?;
    if changed == 1 {
        dossier_event(c, st, &id, "shuftipro", "kyc_result", 0, status)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::org_tests::{Person, Srv};
    use reqwest::Method;
    pub(crate) fn company() -> Details {
        Details {
            kind: "company".into(),
            country: "TR".into(),
            legal_name: "Synthetic Company Ltd".into(),
            registration_number: "test-reg".into(),
            tax_id: "test-tax".into(),
            duns: "123456789".into(),
            address: "Synthetic address".into(),
            contact_name: "Synthetic Person".into(),
            contact_email: "test@example.org".into(),
            bank_country: "TR".into(),
            bank_name: "Test Bank".into(),
            account_holder: "Synthetic Company Ltd".into(),
            iban: "TR330006100519786457841326".into(),
            currency: "TRY".into(),
            public_name: "Test Publisher".into(),
            public_summary: "Public description".into(),
            consent: true,
            ..Details::default()
        }
    }
    /// Other marketplace suites exercise releases, not onboarding. Their fixture enters the new
    /// evidence gate with a synthetic complete dossier; production never has this shortcut.
    pub(crate) async fn fixture(srv: &Srv, id: &str) {
        let id = id.to_string();
        let state = srv.state.clone();
        srv.state.store.group_tx(move|c|->GResult<()>{init_in(c,&id,now_unix()as i64)?;let d=company();c.execute("UPDATE publisher_dossiers SET state='submitted',revision=1,sealed=?2,kind='company',country='TR',submitted_at=?3 WHERE publisher_id=?1 AND state='draft'",params![id,sealed(&state,&id,&serde_json::to_vec(&d).unwrap())?,now_unix()as i64])?;for category in ["registration","tax"]{let doc=new_id("pdoc");c.execute("INSERT INTO publisher_documents VALUES(?1,?2,?3,'synthetic.pdf','application/pdf',9,?4,?5,0)",params![doc,id,category,sealed(&state,&doc,b"%PDF-test")?,now_unix()as i64])?;}Ok(())}).await.unwrap();
    }
    async fn create(srv: &Srv, p: &Person) -> String {
        let (s, v) = srv
            .post(
                "/v1/publisher-applications",
                &p.token,
                json!({"display_name":"Test Publisher"}),
            )
            .await;
        assert_eq!(s, 201, "{v}");
        v["data"]["id"].as_str().unwrap().into()
    }
    #[test]
    fn bank_and_country_validation() {
        assert!(valid_iban("TR330006100519786457841326"));
        assert!(!valid_iban("TR330006100519786457841327"));
        assert!(!valid_iban("éééééééééééééééé"));
        assert!(validate(&company(), true).is_ok());
        let mut d = company();
        d.duns = "1".into();
        assert!(validate(&d, true).is_err());
        d = company();
        d.website = "javascript:alert(1)".into();
        assert!(validate(&d, false).is_err());
    }
    #[test]
    fn provider_signatures_bind_exact_bytes_and_account_mode() {
        let body = b"{\"event\":\"verification.accepted\"}";
        let secret = "synthetic-secret";
        let hashed = format!("{:x}", Sha256::digest(secret.as_bytes()));
        let sig = format!(
            "{:x}",
            Sha256::digest([body.as_slice(), hashed.as_bytes()].concat())
        );
        assert!(verify_signature(body, &sig, secret, false));
        assert!(!verify_signature(b"{}", &sig, secret, false));
        assert!(!verify_signature(body, &sig, secret, true));
        assert!(!verify_signature(body, "", secret, false));
    }
    #[tokio::test]
    async fn complete_company_workflow_is_private_versioned_and_reviewed() {
        let (srv, platform) = crate::marketplace_m2_tests::setup().await;
        srv.global_flag("marketplace").await;
        let applicant = srv.person("onboarding-applicant").await;
        let other = srv.person("onboarding-other").await;
        let rev =
            crate::marketplace_m2_tests::reviewer(&srv, &platform, "onboarding-reviewer").await;
        let id = create(&srv, &applicant).await;
        let path = format!("/v1/publisher-applications/{id}");
        let (status, _) = srv.get(&format!("{path}/dossier"), &other.token).await;
        assert_eq!(status, 404);
        let (status, _) = srv
            .post(
                &format!("/v1/staff/publisher-applications/{id}/decisions"),
                &rev.strong,
                json!({"revision":0,"decision":"approved","reason":"must fail"}),
            )
            .await;
        assert_eq!(status, 409);
        let d = company();
        let (status, v) = srv
            .call(
                Method::PUT,
                &format!("{path}/dossier"),
                Some(&applicant.token),
                Some(json!({"revision":0,"details":d})),
            )
            .await;
        assert_eq!(status, 200, "{v}");
        let (status, _) = srv
            .call(
                Method::PUT,
                &format!("{path}/dossier"),
                Some(&applicant.token),
                Some(json!({"revision":0,"details":d})),
            )
            .await;
        assert_eq!(status, 409);
        let (status, _) = srv
            .post(
                &format!("{path}/submit"),
                &applicant.token,
                json!({"revision":1}),
            )
            .await;
        assert_eq!(status, 400);
        let mut revision = 1;
        let mut evidence = b"%PDF-1.7\nsynthetic".to_vec();
        evidence.resize(3 * 1024 * 1024, b' '); // Above Axum default 2 MiB body limit.
        for cat in ["registration", "tax"] {
            let (status,v)=srv.post(&format!("{path}/documents"),&applicant.token,json!({"revision":revision,"category":cat,"filename":"test.pdf","content_base64":crate::b64_encode(&evidence) })).await;
            assert_eq!(status, 200, "{v}");
            revision += 1;
        }
        let (status, v) = srv
            .post(
                &format!("{path}/submit"),
                &applicant.token,
                json!({"revision":revision}),
            )
            .await;
        assert_eq!(status, 200, "{v}");
        revision += 1;
        let review = format!("/v1/staff/publisher-dossiers/{id}/review");
        let (status, _) = srv
            .post(
                &review,
                &rev.token,
                json!({"revision":revision,"decision":"approved","reason":"test"}),
            )
            .await;
        assert_eq!(status, 401);
        let (status,v)=srv.post(&review,&rev.strong,json!({"revision":revision,"decision":"needs_information","reason":"Please add bank evidence","requested_documents":["bank"]})).await;
        assert_eq!(status, 201, "{v}");
        revision += 1;
        assert_eq!(v["data"]["history"][0]["note"], "Please add bank evidence");
        let (status, _) = srv
            .post(
                &format!("{path}/submit"),
                &applicant.token,
                json!({"revision":revision}),
            )
            .await;
        assert_eq!(
            status, 400,
            "Missing specifically requested bank document must block resubmission"
        );
        let (status,v)=srv.post(&format!("{path}/documents"),&applicant.token,json!({"revision":revision,"category":"bank","filename":"bank.pdf","content_base64":crate::b64_encode(b"%PDF-synthetic bank evidence")})).await;
        assert_eq!(status, 200, "{v}");
        revision += 1;

        let (status, v) = srv
            .post(
                &format!("{path}/submit"),
                &applicant.token,
                json!({"revision":revision}),
            )
            .await;
        assert_eq!(status, 200, "{v}");
        revision += 1;
        let (status, v) = srv
            .post(
                &review,
                &rev.strong,
                json!({"revision":revision,"decision":"approved","reason":"Evidence reviewed"}),
            )
            .await;
        assert_eq!(status, 201, "{v}");
        let (status, v) = srv
            .call(
                Method::GET,
                &format!("/v1/marketplace/partners/{id}"),
                None,
                None,
            )
            .await;
        assert_eq!(status, 200, "{v}");
        let text = v.to_string();
        assert!(text.contains("Public description"));
        for field in [
            "test-tax",
            "Synthetic address",
            "TR3300",
            "contact_email",
            "kyc",
            "documents",
        ] {
            assert!(!text.contains(field), "leaked {field}");
        }
        let pid = id.clone();
        let encrypted = srv
            .state
            .store
            .group_read(move |c| -> GResult<Vec<u8>> {
                Ok(c.query_row(
                    "SELECT sealed FROM publisher_dossiers WHERE publisher_id=?1",
                    [pid],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&encrypted).contains("Synthetic"));
        let (status, _) = srv
            .call(
                Method::PUT,
                &format!("{path}/dossier"),
                Some(&applicant.token),
                Some(json!({"revision":revision+1,"details":d})),
            )
            .await;
        assert_eq!(status, 409);
        let (status,v)=srv.get("/v1/staff/publisher-dossiers?status=approved&q=Test&country=TR&kind=company&sort=name",&rev.strong).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["total"], 1);
    }
    #[tokio::test]
    async fn individual_cannot_bypass_kyc_or_self_review() {
        let (srv, platform) = crate::marketplace_m2_tests::setup().await;
        srv.global_flag("marketplace").await;
        let p = crate::marketplace_m2_tests::reviewer(&srv, &platform, "individual").await;
        let id = create(&srv, &p).await;
        let mut d = company();
        d.kind = "individual".into();
        let path = format!("/v1/publisher-applications/{id}");
        let (s, v) = srv
            .call(
                Method::PUT,
                &format!("{path}/dossier"),
                Some(&p.token),
                Some(json!({"revision":0,"details":d})),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        let (s, v) = srv
            .post(&format!("{path}/submit"), &p.token, json!({"revision":1}))
            .await;
        assert_eq!(s, 400, "{v}");
        let (s, v) = srv
            .get(&format!("/v1/staff/publisher-dossiers/{id}"), &p.strong)
            .await;
        assert_eq!(s, 403, "{v}");
    }
    #[tokio::test]
    async fn kyc_callbacks_are_bound_to_current_identity_and_do_not_replay() {
        let (srv, _) = crate::marketplace_m2_tests::setup().await;
        srv.global_flag("marketplace").await;
        let applicant = srv.person("kyc-binding").await;
        let id = create(&srv, &applicant).await;
        let state = srv.state.clone();
        srv.state.store.group_tx(move|c|->GResult<()> {
            c.execute("UPDATE publisher_dossiers SET identity_digest='current',kyc_reference='current-ref',kyc_status='pending' WHERE publisher_id=?1",[&id])?;
            c.execute("INSERT INTO publisher_kyc_sessions(reference,publisher_id,identity_digest,status,created_at) VALUES('old-ref',?1,'old','pending',0),('current-ref',?1,'current','pending',0)",[&id])?;
            apply_kyc_in(c,&state,"old-ref","verified")?;
            let status:String=c.query_row("SELECT kyc_status FROM publisher_dossiers WHERE publisher_id=?1",[&id],|r|r.get(0))?;
            assert_eq!(status,"pending","Old identity must not approve the current applicant");
            apply_kyc_in(c,&state,"current-ref","verified")?;
            apply_kyc_in(c,&state,"current-ref","verified")?;
            let count:i64=c.query_row("SELECT COUNT(*) FROM publisher_dossier_events WHERE publisher_id=?1 AND event='kyc_result'",[&id],|r|r.get(0))?;
            assert_eq!(count,1,"Duplicate callbacks cannot duplicate transitions");
            assert!(apply_kyc_in(c,&state,"unknown-ref","verified").is_err());
            Ok(())
        }).await.unwrap();
    }
}
