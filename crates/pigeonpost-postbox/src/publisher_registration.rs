//! Verified publisher registration (developer program plan §2–§3, build B): the developer's side.
//!
//! A developer chooses an account type, fills a draft (legal and public names, address, contacts,
//! bank account, tax profile), uploads the documents its type needs, verifies their identity with
//! Shufti Pro (`publisher_kyc`), accepts the agreements and submits with a recent two-factor
//! sign-in. Staff review it (`publisher_registration_staff`) and approve, ask for more
//! information, or reject. Approval certifies the publisher profile and queues the payee and tax
//! profile for saas_be (`publisher_registration_worker`).
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET /v1/publisher-registration/requirements` | `PublisherApply` | a person |
//! | `GET\|POST /v1/publisher-registrations` | `PublisherApply` | a person, for their account or an org they own or administer |
//! | `GET\|PATCH /v1/publisher-registrations/{application}` | `PublisherApply` | the applicant's team; others get 404 |
//! | `POST /v1/publisher-registrations/{application}/documents`, `GET\|DELETE …/documents/{document}` | `PublisherApply` | same |
//! | `POST /v1/publisher-registrations/{application}/kyc`, `POST …/kyc/{check}/simulate` (mock only) | `PublisherApply` | same |
//! | `POST /v1/publisher-registrations/{application}/submit` | `PublisherSubmit` | same, with step-up |
//! | `POST /v1/integrations/shufti/callback` | none | Shufti Pro, authenticated by its signature |
//!
//! States: `draft → submitted → in_review → needs_info → submitted … → approved | rejected`.
//! Only `draft` and `needs_info` are editable. `legacy` is the account type of every M0
//! application (`POST /v1/publisher-applications`); starting a registration converts an open
//! legacy application into a draft.
//!
//! Bank and tax details are sealed under the vault and every read shows the masked copy; only an
//! audited staff reveal opens them. Documents are sealed and kept in the blob store
//! (`<blob dir>/publisherdocs`): the store is built for large immutable bytes with a volume floor
//! and a quota, which the database is not, and sealing them first means the files on disk are
//! ciphertext bound to their document id. Retention: see `publisher_registration_worker`.

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, iso, not_found, parse};
use crate::org::{permits, Perm};
use crate::policy::{authorize, Action, Actor, Resource};
use crate::publisher_kyc as kyc;
use crate::publisher_validate as v;
use crate::{now_unix, AppState, ClientIp, Principal};
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Largest document: 10 MB.
pub(crate) const DOC_MAX: usize = 10 * 1024 * 1024;
/// Vault label for bank and tax details (AAD: `<application>\0bank` / `\0tax`).
pub(crate) const PAYEE_LABEL: &str = "publisher-payee-v1";
/// Vault label for document bytes (AAD: the document id).
pub(crate) const DOC_LABEL: &str = "publisher-document-v1";
/// The agreements a developer accepts. Drafts until legal approves them (plan §4).
pub(crate) const AGREEMENT_VERSION: &str = "dda-2026-11-11-draft";
pub(crate) const CONTENT_POLICY_VERSION: &str = "dcp-2026-11-11-draft";
/// A faaliyet belgesi must be at most six months old.
const MAX_ACTIVITY_CERT_DAYS: i64 = 183;
/// Most documents one application can hold (removed ones included).
const MAX_DOCS: i64 = 60;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publisher-registration/requirements",
            get(registration_requirements),
        )
        .route(
            "/v1/publisher-registrations",
            get(list_registrations).post(create_registration),
        )
        .route(
            "/v1/publisher-registrations/{application}",
            get(read_registration).patch(update_registration),
        )
        .route(
            "/v1/publisher-registrations/{application}/documents",
            post(upload_document).layer(DefaultBodyLimit::max(DOC_MAX + 4096)),
        )
        .route(
            "/v1/publisher-registrations/{application}/documents/{document}",
            get(download_document).delete(remove_document),
        )
        .route(
            "/v1/publisher-registrations/{application}/kyc",
            post(start_kyc),
        )
        .route(
            "/v1/publisher-registrations/{application}/kyc/{check}/simulate",
            post(simulate_kyc),
        )
        .route(
            "/v1/publisher-registrations/{application}/submit",
            post(submit_registration),
        )
}

/// Routes a provider calls, not a Pigeonpost client. Shufti Pro's callback carries no Pigeonpost
/// credential: it is authenticated by its `Signature` header alone
/// (`publisher_kyc::handle_callback`), and only ever changes a check this postbox created.
pub(crate) fn provider_router() -> Router<AppState> {
    Router::new().route("/v1/integrations/shufti/callback", post(shufti_callback))
}

// ---- requirements --------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    Required,
    Optional,
    /// Required when the payee claims a tax treaty rate.
    IfTreaty,
}

pub(crate) struct DocSpec {
    pub kind: &'static str,
    pub en: &'static str,
    pub tr: &'static str,
    pub need: Need,
    /// The document must be issued within this many days (`issued_on`).
    pub max_age_days: Option<i64>,
    /// The document has an expiry date the developer gives (`expires_on`).
    pub expires: bool,
}

const fn doc(
    kind: &'static str,
    en: &'static str,
    tr: &'static str,
    need: Need,
    max_age_days: Option<i64>,
    expires: bool,
) -> DocSpec {
    DocSpec {
        kind,
        en,
        tr,
        need,
        max_age_days,
        expires,
    }
}

const BANK_LETTER: DocSpec = doc(
    "bank_letter",
    "Bank statement or letter showing the account holder name",
    "Hesap sahibinin adını gösteren banka hesap özeti veya yazısı",
    Need::Required,
    None,
    false,
);
const BANK_LETTER_OPTIONAL: DocSpec = DocSpec {
    need: Need::Optional,
    ..BANK_LETTER
};
const RESIDENCE: DocSpec = doc(
    "residence_certificate",
    "Certificate of tax residence (apostilled or notarised translation), for treaty rates",
    "Mukimlik belgesi (apostilli veya noter onaylı tercüme), anlaşma oranları için",
    Need::IfTreaty,
    None,
    true,
);
const VERGI_LEVHASI: DocSpec = doc(
    "vergi_levhasi",
    "Tax plate (vergi levhası)",
    "Vergi levhası",
    Need::Required,
    None,
    false,
);
const FAALIYET: DocSpec = doc(
    "faaliyet_belgesi",
    "Activity certificate (faaliyet belgesi), issued within 6 months",
    "Faaliyet belgesi (son 6 ay içinde alınmış)",
    Need::Required,
    Some(MAX_ACTIVITY_CERT_DAYS),
    false,
);

pub(crate) struct TypeSpec {
    pub id: &'static str,
    pub en: &'static str,
    pub tr: &'static str,
    /// Resident in Turkey (country `TR`) or not.
    pub turkish: bool,
    pub company: bool,
    pub tax_category: &'static str,
    /// Who does KYC: the person themselves, or the company's authorised signatory.
    pub kyc_subject: &'static str,
    /// Allowed for an organization's publisher, not just a person's own account.
    pub org_allowed: bool,
    pub docs: &'static [DocSpec],
}

pub(crate) const TYPES: &[TypeSpec] = &[
    TypeSpec {
        id: "individual_tr",
        en: "Individual (resident in Turkey)",
        tr: "Bireysel (Türkiye'de yerleşik)",
        turkish: true,
        company: false,
        tax_category: "tr_individual",
        kyc_subject: "applicant",
        org_allowed: false,
        docs: &[BANK_LETTER_OPTIONAL],
    },
    TypeSpec {
        id: "individual_foreign",
        en: "Individual (outside Turkey)",
        tr: "Bireysel (yurt dışında yerleşik)",
        turkish: false,
        company: false,
        tax_category: "foreign_individual",
        kyc_subject: "applicant",
        org_allowed: false,
        docs: &[RESIDENCE, BANK_LETTER_OPTIONAL],
    },
    TypeSpec {
        id: "sole_proprietor_tr",
        en: "Sole proprietor (şahıs şirketi / serbest meslek, Turkey)",
        tr: "Şahıs şirketi / serbest meslek (Türkiye)",
        turkish: true,
        company: false,
        tax_category: "tr_business",
        kyc_subject: "applicant",
        org_allowed: false,
        docs: &[VERGI_LEVHASI, FAALIYET, BANK_LETTER_OPTIONAL],
    },
    TypeSpec {
        id: "company_tr",
        en: "Company in Turkey (Ltd. Şti., A.Ş.)",
        tr: "Türkiye'de şirket (Ltd. Şti., A.Ş.)",
        turkish: true,
        company: true,
        tax_category: "tr_business",
        kyc_subject: "signatory",
        org_allowed: true,
        docs: &[
            doc(
                "ticaret_sicil_gazetesi",
                "Trade registry gazette (ticaret sicil gazetesi)",
                "Ticaret sicil gazetesi",
                Need::Required,
                None,
                false,
            ),
            FAALIYET,
            doc(
                "imza_sirkuleri",
                "Signature circular (imza sirküleri)",
                "İmza sirküleri",
                Need::Required,
                None,
                false,
            ),
            VERGI_LEVHASI,
            BANK_LETTER,
        ],
    },
    TypeSpec {
        id: "company_foreign",
        en: "Company outside Turkey",
        tr: "Yurt dışında şirket",
        turkish: false,
        company: true,
        tax_category: "foreign_company",
        kyc_subject: "signatory",
        org_allowed: true,
        docs: &[
            doc(
                "certificate_of_incorporation",
                "Certificate of incorporation or registration extract",
                "Kuruluş belgesi veya sicil kaydı",
                Need::Required,
                None,
                false,
            ),
            doc(
                "tax_id_document",
                "Tax id document (for a US company the EIN letter CP 575 or 147C)",
                "Vergi numarası belgesi (ABD şirketi için EIN mektubu CP 575 veya 147C)",
                Need::Required,
                None,
                false,
            ),
            doc(
                "proof_of_address",
                "Proof of the company's address",
                "Şirket adresini gösteren belge",
                Need::Required,
                None,
                false,
            ),
            doc(
                "signatory_authority",
                "Proof that the signatory may act for the company",
                "Yetkilinin şirketi temsil yetkisini gösteren belge",
                Need::Required,
                None,
                false,
            ),
            BANK_LETTER,
            RESIDENCE,
        ],
    },
];

pub(crate) fn type_spec(id: &str) -> Option<&'static TypeSpec> {
    TYPES.iter().find(|t| t.id == id)
}

fn requirements_json() -> Value {
    let types: Vec<Value> = TYPES
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "label": t.en,
                "label_tr": t.tr,
                "country": if t.turkish { json!("TR") } else { Value::Null },
                "company": t.company,
                "tax_category": t.tax_category,
                "kyc_subject": t.kyc_subject,
                "org_allowed": t.org_allowed,
                "tax_id": match (t.id, t.turkish) {
                    ("individual_tr", _) => "tckn",
                    (_, true) => "vkn",
                    ("company_foreign", _) => "foreign_required",
                    _ => "foreign_optional",
                },
                "payout_currency": if t.turkish { "TRY" } else { "USD" },
                "bank_methods": if t.turkish { json!(["iban"]) } else { json!(["iban", "swift", "ach"]) },
                "documents": t.docs.iter().map(|d| json!({
                    "type": d.kind,
                    "label": d.en,
                    "label_tr": d.tr,
                    "need": match d.need { Need::Required => "required", Need::Optional => "optional", Need::IfTreaty => "if_treaty" },
                    "max_age_days": d.max_age_days,
                    "expires": d.expires,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "account_types": types,
        "document_formats": ["application/pdf", "image/jpeg", "image/png"],
        "document_max_bytes": DOC_MAX,
        "agreement_version": AGREEMENT_VERSION,
        "content_policy_version": CONTENT_POLICY_VERSION,
    })
}

// ---- the application record ------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Address {
    #[serde(default)]
    pub line1: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line2: Option<String>,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub postal_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default)]
    pub country: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Signatory {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
}

/// The non-secret part of an application.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trading_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    /// The Pigeonpost address buyers write to for support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_address: Option<String>,
    /// Where Pigeonpost writes about this application (not shown to buyers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
    /// Foreign companies: the registry number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mersis_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trade_registry_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duns_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatory: Option<Signatory>,
}

/// Bank details as entered; sealed at rest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bank {
    pub holder_name: String,
    /// `iban`, `swift` (account number + BIC) or `ach` (US routing + account).
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iban: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swift_bic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ach_account_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

/// The tax profile as entered; sealed at rest. The category follows from the account type.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tax {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_id: Option<String>,
    pub tax_country: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub treaty_country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residence_certificate_expires_on: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct App {
    pub id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub applicant: String,
    pub account_type: String,
    pub state: String,
    pub publisher_id: Option<String>,
    pub public_name: String,
    pub profile: Profile,
    pub bank_sealed: Option<Vec<u8>>,
    pub bank_masked: Option<Value>,
    pub tax_sealed: Option<Vec<u8>>,
    pub tax_masked: Option<Value>,
    pub tax_category: Option<String>,
    pub agreement_version: Option<String>,
    pub content_policy_version: Option<String>,
    pub agreement_accepted_at: Option<i64>,
    pub revision: i64,
    pub reviewer: Option<String>,
    pub submitted_at: Option<i64>,
    pub decided_at: Option<i64>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub checklist: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
}

const APP_COLS: &str = "id, owner_kind, owner_id, applicant, account_type, state, publisher_id,
    public_name, profile, bank_sealed, bank_masked, tax_sealed, tax_masked, tax_category,
    agreement_version, content_policy_version, agreement_accepted_at, revision, reviewer,
    submitted_at, decided_at, decided_by, decision_reason, checklist, created_at, updated_at";

fn json_col(s: Option<String>) -> Option<Value> {
    s.and_then(|t| serde_json::from_str(&t).ok())
}

fn map_app(r: &rusqlite::Row<'_>) -> rusqlite::Result<App> {
    Ok(App {
        id: r.get(0)?,
        owner_kind: r.get(1)?,
        owner_id: r.get(2)?,
        applicant: r.get(3)?,
        account_type: r.get(4)?,
        state: r.get(5)?,
        publisher_id: r.get(6)?,
        public_name: r.get(7)?,
        profile: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        bank_sealed: r.get(9)?,
        bank_masked: json_col(r.get(10)?),
        tax_sealed: r.get(11)?,
        tax_masked: json_col(r.get(12)?),
        tax_category: r.get(13)?,
        agreement_version: r.get(14)?,
        content_policy_version: r.get(15)?,
        agreement_accepted_at: r.get(16)?,
        revision: r.get(17)?,
        reviewer: r.get(18)?,
        submitted_at: r.get(19)?,
        decided_at: r.get(20)?,
        decided_by: r.get(21)?,
        decision_reason: r.get(22)?,
        checklist: json_col(r.get(23)?),
        created_at: r.get(24)?,
        updated_at: r.get(25)?,
    })
}

pub(crate) fn app_in(c: &Connection, id: &str) -> rusqlite::Result<Option<App>> {
    c.query_row(
        &format!("SELECT {APP_COLS} FROM publisher_applications WHERE id = ?1"),
        [id],
        map_app,
    )
    .optional()
}

pub(crate) fn apps_where(
    c: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<App>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {APP_COLS} FROM publisher_applications WHERE {filter}"
    ))?;
    let rows = stmt.query_map(args, map_app)?.collect();
    rows
}

#[derive(Debug, Clone)]
pub(crate) struct Doc {
    pub id: String,
    pub doc_type: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub sha256: String,
    pub blob: Option<String>,
    pub issued_on: Option<String>,
    pub expires_on: Option<String>,
    pub scan_state: String,
    pub scan_detail: Option<String>,
    pub created_at: i64,
    pub removed_at: Option<i64>,
    pub purged_at: Option<i64>,
}

impl Doc {
    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.id,
            "type": self.doc_type,
            "filename": self.filename,
            "content_type": self.content_type,
            "size": self.size,
            "sha256": self.sha256,
            "issued_on": self.issued_on,
            "expires_on": self.expires_on,
            "scan": self.scan_state,
            "uploaded_at": iso(self.created_at),
            "purged": self.purged_at.is_some(),
        })
    }
}

pub(crate) fn docs_in(
    c: &Connection,
    app: &str,
    include_removed: bool,
) -> rusqlite::Result<Vec<Doc>> {
    let mut stmt =
        c.prepare(&format!(
        "SELECT id, doc_type, filename, content_type, size, sha256, blob, issued_on, expires_on,
                scan_state, scan_detail, created_at, removed_at, purged_at
           FROM publisher_application_documents WHERE application_id = ?1 {}
          ORDER BY created_at, id",
        if include_removed { "" } else { "AND removed_at IS NULL" }
    ))?;
    let rows = stmt
        .query_map([app], |r| {
            Ok(Doc {
                id: r.get(0)?,
                doc_type: r.get(1)?,
                filename: r.get(2)?,
                content_type: r.get(3)?,
                size: r.get(4)?,
                sha256: r.get(5)?,
                blob: r.get(6)?,
                issued_on: r.get(7)?,
                expires_on: r.get(8)?,
                scan_state: r.get(9)?,
                scan_detail: r.get(10)?,
                created_at: r.get(11)?,
                removed_at: r.get(12)?,
                purged_at: r.get(13)?,
            })
        })?
        .collect();
    rows
}

#[derive(Debug, Clone)]
pub(crate) struct Check {
    pub id: String,
    pub subject: String,
    pub provider: String,
    pub reference: String,
    pub event: String,
    pub result: String,
    pub declined_reason: Option<String>,
    pub declined_codes: Option<String>,
    pub aml_result: Option<String>,
    pub verified_name: Option<String>,
    pub verified_dob: Option<String>,
    pub verified_country: Option<String>,
    pub verification_url: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Check {
    /// The developer's view: no verified personal data, the hosted URL while pending.
    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.id,
            "subject": self.subject,
            "provider": self.provider,
            "sandbox": self.provider == "mock",
            "result": self.result,
            "event": self.event,
            "declined_reason": self.declined_reason,
            "verification_url": if self.result == "pending" { json!(self.verification_url) } else { Value::Null },
            "created_at": iso(self.created_at),
            "updated_at": iso(self.updated_at),
        })
    }

    pub(crate) fn staff_json(&self) -> Value {
        let mut v = self.json();
        v["reference"] = json!(self.reference);
        v["declined_codes"] = json!(self
            .declined_codes
            .as_deref()
            .map(|c| c.split(',').collect::<Vec<_>>())
            .unwrap_or_default());
        v["aml"] = json!(self.aml_result);
        v["verified"] = json!({
            "name": self.verified_name,
            "date_of_birth": self.verified_dob,
            "country": self.verified_country,
        });
        v["verification_url"] = Value::Null;
        v
    }
}

pub(crate) fn checks_in(c: &Connection, app: &str) -> rusqlite::Result<Vec<Check>> {
    let mut stmt = c.prepare(
        "SELECT id, subject, provider, reference, event, result, declined_reason, declined_codes,
                aml_result, verified_name, verified_dob, verified_country, verification_url,
                created_at, updated_at
           FROM publisher_kyc_checks WHERE application_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt
        .query_map([app], |r| {
            Ok(Check {
                id: r.get(0)?,
                subject: r.get(1)?,
                provider: r.get(2)?,
                reference: r.get(3)?,
                event: r.get(4)?,
                result: r.get(5)?,
                declined_reason: r.get(6)?,
                declined_codes: r.get(7)?,
                aml_result: r.get(8)?,
                verified_name: r.get(9)?,
                verified_dob: r.get(10)?,
                verified_country: r.get(11)?,
                verification_url: r.get(12)?,
                created_at: r.get(13)?,
                updated_at: r.get(14)?,
            })
        })?
        .collect();
    rows
}

pub(crate) fn events_in(c: &Connection, app: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = c.prepare(
        "SELECT id, actor_role, kind, from_state, to_state, message, created_at
           FROM publisher_application_events WHERE application_id = ?1
          ORDER BY created_at, rowid",
    )?;
    let rows = stmt
        .query_map([app], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "by": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "from": r.get::<_, Option<String>>(3)?,
                "to": r.get::<_, Option<String>>(4)?,
                "message": r.get::<_, Option<String>>(5)?,
                "at": iso(r.get::<_, i64>(6)?),
            }))
        })?
        .collect();
    rows
}

/// The latest KYC check for the application's verification subject.
pub(crate) fn current_check(checks: &[Check]) -> Option<&Check> {
    checks.iter().max_by_key(|c| (c.created_at, c.id.clone()))
}

// ---- validation ------------------------------------------------------------------------------------

fn bad(code: &'static str, msg: impl Into<String>) -> GErr {
    GErr::new(StatusCode::UNPROCESSABLE_ENTITY, code, msg)
}

fn clean(s: &str, field: &str, max: usize) -> GResult<String> {
    let t = s.trim().to_string();
    if t.chars().count() > max || t.chars().any(char::is_control) {
        return Err(bad(
            "invalid_field",
            format!("{field}: at most {max} printable characters"),
        )
        .with("field", field));
    }
    Ok(t)
}

fn opt_clean(s: &Option<String>, field: &str, max: usize) -> GResult<Option<String>> {
    match s {
        None => Ok(None),
        Some(v) => {
            let t = clean(v, field, max)?;
            Ok((!t.is_empty()).then_some(t))
        }
    }
}

pub(crate) fn email_ok(e: &str) -> bool {
    let Some((local, domain)) = e.rsplit_once('@') else {
        return false;
    };
    e.len() <= 254
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !e.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn date_ok(d: &str) -> bool {
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok()
}

fn days_since(date: &str, now: i64) -> Option<i64> {
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let today = chrono::DateTime::from_timestamp(now, 0)?.date_naive();
    Some((today - d).num_days())
}

fn invalid(field: &str, code: &'static str) -> GErr {
    bad(code, format!("{field} is not valid")).with("field", field)
}

/// Normalise and check a profile's formats. Completeness is the submit check's job.
pub(crate) fn clean_profile(p: Profile, spec: &TypeSpec) -> GResult<Profile> {
    let mut out = Profile {
        legal_name: opt_clean(&p.legal_name, "legal_name", 200)?,
        trading_name: opt_clean(&p.trading_name, "trading_name", 200)?,
        country: opt_clean(&p.country, "country", 2)?.map(|c| c.to_ascii_uppercase()),
        address: None,
        website: opt_clean(&p.website, "website", 200)?,
        support_email: opt_clean(&p.support_email, "support_email", 254)?,
        support_address: opt_clean(&p.support_address, "support_address", 120)?,
        contact_email: opt_clean(&p.contact_email, "contact_email", 254)?,
        registration_number: opt_clean(&p.registration_number, "registration_number", 64)?,
        mersis_number: None,
        trade_registry_number: opt_clean(&p.trade_registry_number, "trade_registry_number", 32)?,
        duns_number: None,
        signatory: None,
    };
    if let Some(c) = &out.country {
        if !v::country_ok(c) {
            return Err(invalid("country", "invalid_country"));
        }
        if spec.turkish != (c == "TR") {
            return Err(bad(
                "country_mismatch",
                if spec.turkish {
                    "this account type is for Turkey; choose a foreign type"
                } else {
                    "this account type is for payees outside Turkey; choose a Turkish type"
                },
            )
            .with("field", "country"));
        }
    }
    if let Some(a) = p.address {
        let a = Address {
            line1: clean(&a.line1, "address.line1", 200)?,
            line2: opt_clean(&a.line2, "address.line2", 200)?,
            city: clean(&a.city, "address.city", 100)?,
            postal_code: clean(&a.postal_code, "address.postal_code", 20)?,
            region: opt_clean(&a.region, "address.region", 100)?,
            country: clean(&a.country, "address.country", 2)?.to_ascii_uppercase(),
        };
        if !a.country.is_empty() && !v::country_ok(&a.country) {
            return Err(invalid("address.country", "invalid_country"));
        }
        out.address = Some(a);
    }
    if let Some(w) = &out.website {
        if !w.starts_with("https://") || w.len() < 12 || w.contains(char::is_whitespace) {
            return Err(invalid("website", "invalid_website"));
        }
    }
    for (field, value) in [
        ("support_email", &out.support_email),
        ("contact_email", &out.contact_email),
    ] {
        if let Some(e) = value {
            if !email_ok(e) {
                return Err(invalid(field, "invalid_email"));
            }
        }
    }
    if let Some(a) = &out.support_address {
        if !a.starts_with('/') || a.contains(char::is_whitespace) {
            return Err(invalid("support_address", "invalid_address"));
        }
    }
    if let Some(m) = opt_clean(&p.mersis_number, "mersis_number", 32)? {
        out.mersis_number = Some(v::mersis(&m).map_err(|c| invalid("mersis_number", c))?);
    }
    if let Some(d) = opt_clean(&p.duns_number, "duns_number", 16)? {
        out.duns_number = Some(v::duns(&d).map_err(|c| invalid("duns_number", c))?);
    }
    if let Some(s) = p.signatory {
        let email = opt_clean(&s.contact_email, "signatory.contact_email", 254)?;
        if email.as_deref().is_some_and(|e| !email_ok(e)) {
            return Err(invalid("signatory.contact_email", "invalid_email"));
        }
        out.signatory = Some(Signatory {
            name: clean(&s.name, "signatory.name", 200)?,
            title: clean(&s.title, "signatory.title", 100)?,
            contact_email: email,
        });
    }
    Ok(out)
}

fn invalid_code(field: &str, code: &'static str) -> GErr {
    // The check that failed, so the form can say which part is wrong without echoing the value.
    bad(code, format!("{field} is not valid")).with("field", field)
}

/// Normalise and check bank details for the account type. Returns the cleaned details.
pub(crate) fn clean_bank(b: Bank, spec: &TypeSpec) -> GResult<Bank> {
    let holder = clean(&b.holder_name, "bank.holder_name", 140)?;
    if holder.chars().count() < 2 {
        return Err(invalid_code("bank.holder_name", "invalid_holder_name"));
    }
    let bank_name = opt_clean(&b.bank_name, "bank.bank_name", 128)?;
    let bank_address = opt_clean(&b.bank_address, "bank.bank_address", 255)?;
    let mut out = Bank {
        holder_name: holder,
        method: b.method.trim().to_string(),
        bank_name,
        bank_address,
        ..Bank::default()
    };
    let bic = |raw: &Option<String>| -> GResult<Option<String>> {
        match raw.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => Ok(Some(
                v::swift_bic(s).map_err(|c| invalid_code("bank.swift_bic", c))?,
            )),
        }
    };
    match out.method.as_str() {
        "iban" => {
            let iban = v::iban(b.iban.as_deref().unwrap_or_default())
                .map_err(|c| invalid_code("bank.iban", c))?;
            let country = iban[..2].to_string();
            if spec.turkish && country != "TR" {
                return Err(invalid_code("bank.iban", "iban_must_be_turkish"));
            }
            // USD payouts go to an account outside Turkey; a Turkish account is paid in TRY.
            if !spec.turkish && country == "TR" {
                return Err(invalid_code("bank.iban", "iban_must_be_foreign"));
            }
            out.swift_bic = bic(&b.swift_bic)?;
            if !spec.turkish && out.swift_bic.is_none() {
                return Err(bad(
                    "swift_required",
                    "an international IBAN needs the bank's SWIFT/BIC",
                )
                .with("field", "bank.swift_bic"));
            }
            out.iban = Some(iban);
            out.bank_country = Some(country);
        }
        "swift" if !spec.turkish => {
            out.account_number = Some(
                v::account_number(b.account_number.as_deref().unwrap_or_default(), false)
                    .map_err(|c| invalid_code("bank.account_number", c))?,
            );
            out.swift_bic = bic(&b.swift_bic)?;
            let Some(code) = &out.swift_bic else {
                return Err(bad("swift_required", "a SWIFT/BIC is required")
                    .with("field", "bank.swift_bic"));
            };
            if out.bank_name.is_none() {
                return Err(bad("bank_name_required", "the bank's name is required")
                    .with("field", "bank.bank_name"));
            }
            out.bank_country = Some(code[4..6].to_string());
        }
        "ach" if !spec.turkish => {
            out.routing_number = Some(
                v::aba_routing(b.routing_number.as_deref().unwrap_or_default())
                    .map_err(|c| invalid_code("bank.routing_number", c))?,
            );
            out.account_number = Some(
                v::account_number(b.account_number.as_deref().unwrap_or_default(), true)
                    .map_err(|c| invalid_code("bank.account_number", c))?,
            );
            let kind = b.ach_account_type.as_deref().unwrap_or("checking");
            if !matches!(kind, "checking" | "savings") {
                return Err(invalid_code(
                    "bank.ach_account_type",
                    "invalid_account_type",
                ));
            }
            out.ach_account_type = Some(kind.to_string());
            out.bank_country = Some("US".into());
        }
        _ => {
            return Err(bad(
                "invalid_bank_method",
                if spec.turkish {
                    "Turkish payees are paid to a Turkish IBAN"
                } else {
                    "method: iban, swift or ach"
                },
            )
            .with("field", "bank.method"))
        }
    }
    out.currency = Some(if spec.turkish { "TRY" } else { "USD" }.into());
    Ok(out)
}

pub(crate) fn mask_bank(b: &Bank) -> Value {
    json!({
        "holder_name": b.holder_name,
        "method": b.method,
        "iban": b.iban.as_deref().map(v::mask_iban),
        "account_number": b.account_number.as_deref().map(|a| v::mask_tail(a, 4)),
        "swift_bic": b.swift_bic,
        "routing_number": b.routing_number,
        "ach_account_type": b.ach_account_type,
        "bank_name": b.bank_name,
        "bank_country": b.bank_country,
        "currency": b.currency,
        "last4": b.iban.as_deref().or(b.account_number.as_deref())
            .map(|s| s.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>()),
    })
}

/// Normalise and check the tax profile for the account type. Returns the cleaned profile and the
/// check that passed (`tckn`, `vkn`, `format`, or `none` when no id was given).
pub(crate) fn clean_tax(t: Tax, spec: &TypeSpec, now: i64) -> GResult<(Tax, &'static str)> {
    let country = t.tax_country.trim().to_ascii_uppercase();
    if !v::country_ok(&country) {
        return Err(invalid("tax.tax_country", "invalid_country"));
    }
    if spec.turkish != (country == "TR") {
        return Err(bad(
            "country_mismatch",
            "the tax country does not match the account type",
        )
        .with("field", "tax.tax_country"));
    }
    let raw = t.tax_id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (tax_id, check) = match (spec.id, raw) {
        ("individual_tr", Some(id)) => (
            Some(v::tckn(id).map_err(|c| invalid("tax.tax_id", c))?),
            "tckn",
        ),
        ("sole_proprietor_tr", Some(id)) if v::compact(id).len() == 11 => (
            Some(v::tckn(id).map_err(|c| invalid("tax.tax_id", c))?),
            "tckn",
        ),
        (_, Some(id)) if spec.turkish => (
            Some(v::vkn(id).map_err(|c| invalid("tax.tax_id", c))?),
            "vkn",
        ),
        (_, Some(id)) => (
            Some(v::foreign_tax_id(id).map_err(|c| invalid("tax.tax_id", c))?),
            "format",
        ),
        (_, None) => (None, "none"),
    };
    let treaty = t
        .treaty_country
        .as_deref()
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| !c.is_empty());
    let mut expires = t
        .residence_certificate_expires_on
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(tc) = &treaty {
        if spec.turkish {
            return Err(bad(
                "treaty_not_applicable",
                "treaty rates are for payees outside Turkey",
            )
            .with("field", "tax.treaty_country"));
        }
        if !v::country_ok(tc) || tc == "TR" {
            return Err(invalid("tax.treaty_country", "invalid_country"));
        }
        match &expires {
            Some(d) if date_ok(d) && days_since(d, now).is_some_and(|n| n < 0) => {}
            Some(_) => {
                return Err(bad(
                    "residence_certificate_expired",
                    "the certificate of residence must be valid on a future date",
                )
                .with("field", "tax.residence_certificate_expires_on"))
            }
            None => {
                return Err(bad(
                    "residence_certificate_required",
                    "a treaty rate needs the certificate of residence and its expiry date",
                )
                .with("field", "tax.residence_certificate_expires_on"))
            }
        }
    } else {
        expires = None;
    }
    Ok((
        Tax {
            tax_id,
            tax_country: country,
            treaty_country: treaty,
            residence_certificate_expires_on: expires,
        },
        check,
    ))
}

pub(crate) fn mask_tax(t: &Tax, category: &str, check: &str) -> Value {
    json!({
        "category": category,
        "tax_id": t.tax_id.as_deref().map(|i| v::mask_tail(i, 2)),
        "tax_id_check": check,
        "tax_country": t.tax_country,
        "treaty_country": t.treaty_country,
        "residence_certificate_expires_on": t.residence_certificate_expires_on,
    })
}

pub(crate) fn seal_part(
    state: &AppState,
    app: &str,
    part: &str,
    value: &impl Serialize,
) -> GResult<Vec<u8>> {
    let plain = zeroize::Zeroizing::new(serde_json::to_vec(value).unwrap_or_default());
    state
        .vault
        .seal_labeled(PAYEE_LABEL, format!("{app}\0{part}").as_bytes(), &plain)
        .map_err(|_| store_error("seal failed"))
}

pub(crate) fn open_sealed<T: serde::de::DeserializeOwned>(
    state: &AppState,
    app: &str,
    part: &str,
    sealed: &[u8],
) -> GResult<T> {
    let plain = zeroize::Zeroizing::new(
        state
            .vault
            .open_labeled(PAYEE_LABEL, format!("{app}\0{part}").as_bytes(), sealed)
            .map_err(|_| store_error("a sealed field could not be opened"))?,
    );
    serde_json::from_slice(&plain).map_err(|_| store_error("a sealed field is unreadable"))
}

pub(crate) fn store_error(msg: &'static str) -> GErr {
    GErr::new(StatusCode::INTERNAL_SERVER_ERROR, "store_error", msg)
}

// ---- names -------------------------------------------------------------------------------------------

/// Lower-case ASCII tokens of a name with Turkish letters folded and company suffixes dropped.
pub(crate) fn name_tokens(name: &str) -> Vec<String> {
    let folded: String = name
        .chars()
        .map(|c| match c {
            'ç' | 'Ç' => 'c',
            'ğ' | 'Ğ' => 'g',
            'ı' | 'I' | 'İ' | 'i' | 'î' | 'Î' | 'í' | 'ì' => 'i',
            'ö' | 'Ö' | 'ó' | 'ò' | 'ô' => 'o',
            'ş' | 'Ş' => 's',
            'ü' | 'Ü' | 'ú' | 'ù' | 'û' => 'u',
            'â' | 'Â' | 'á' | 'à' | 'ä' | 'Ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ñ' => 'n',
            c => c.to_ascii_lowercase(),
        })
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect();
    const SUFFIXES: &[&str] = &[
        "ltd", "sti", "as", "a", "s", "limited", "sirketi", "ticaret", "inc", "llc", "gmbh",
        "corp", "co", "bv", "sa", "sas", "srl", "plc",
    ];
    folded
        .split_whitespace()
        .filter(|t| !SUFFIXES.contains(t))
        .map(str::to_string)
        .collect()
}

/// Two names agree when one's tokens are all in the other's (middle names may be missing), with at
/// least one token in common. Advisory: the reviewer decides.
pub(crate) fn names_match(a: &str, b: &str) -> bool {
    let (x, y) = (name_tokens(a), name_tokens(b));
    if x.is_empty() || y.is_empty() {
        return false;
    }
    let (short, long) = if x.len() <= y.len() {
        (&x, &y)
    } else {
        (&y, &x)
    };
    short.iter().all(|t| long.contains(t))
}

// ---- the checklist -------------------------------------------------------------------------------------

/// One checklist line: `ok` is `Some(true|false)` for automated checks and `None` for what only a
/// reviewer can judge. `blocking` items must pass to submit (developer) or approve (staff).
fn item(id: &str, ok: Option<bool>, blocking: bool, detail: impl Into<String>) -> Value {
    json!({ "id": id, "ok": ok, "blocking": blocking, "detail": detail.into() })
}

/// The reviewer's own judgements an approval must confirm (plan §2 "Flow" step 5).
pub(crate) const MANUAL_CHECKS: &[(&str, &str)] = &[
    (
        "names_match_documents",
        "Names match across documents, KYC and the bank account",
    ),
    (
        "documents_authentic",
        "Documents are genuine, legible and current",
    ),
    (
        "registry_consistent",
        "MERSİS / trade registry / company registration details are consistent",
    ),
    ("sanctions_reviewed", "Sanctions and PEP screening reviewed"),
    (
        "bank_holder_matches",
        "The bank account holder is the payee",
    ),
];

pub(crate) struct Facts<'a> {
    pub app: &'a App,
    pub docs: &'a [Doc],
    pub checks: &'a [Check],
    pub bank_ok: bool,
    pub tax_ok: bool,
    pub tax_check: Option<String>,
    pub treaty: bool,
    pub bank_holder: Option<String>,
    pub now: i64,
}

/// The automated checklist, the same for the developer (what is missing) and staff.
pub(crate) fn checklist(f: &Facts<'_>) -> Vec<Value> {
    let a = f.app;
    let mut out = Vec::new();
    let Some(spec) = type_spec(&a.account_type) else {
        out.push(item(
            "account_type",
            Some(false),
            true,
            "A legacy application: choose an account type and complete the registration",
        ));
        return out;
    };
    let p = &a.profile;
    let mut missing = Vec::new();
    if p.legal_name.is_none() {
        missing.push("legal name");
    }
    if p.country.is_none() {
        missing.push("country");
    }
    match &p.address {
        Some(ad) if !ad.line1.is_empty() && !ad.city.is_empty() && !ad.country.is_empty() => {}
        _ => missing.push("address"),
    }
    if p.support_email.is_none() {
        missing.push("support email");
    }
    if p.contact_email.is_none() {
        missing.push("contact email");
    }
    if spec.company && p.website.is_none() {
        missing.push("website");
    }
    if spec.company {
        match &p.signatory {
            Some(s) if !s.name.is_empty() && !s.title.is_empty() => {}
            _ => missing.push("authorised signatory"),
        }
    }
    if spec.id == "company_tr" {
        if p.mersis_number.is_none() {
            missing.push("MERSİS number");
        }
        if p.trade_registry_number.is_none() {
            missing.push("trade registry number");
        }
    }
    if spec.id == "company_foreign" && p.registration_number.is_none() {
        missing.push("company registration number");
    }
    let name_ok = (2..=80).contains(&a.public_name.chars().count());
    if !name_ok {
        missing.push("public publisher name");
    }
    out.push(item(
        "profile_complete",
        Some(missing.is_empty()),
        true,
        if missing.is_empty() {
            "Names, address and contacts are complete".to_string()
        } else {
            format!("Missing: {}", missing.join(", "))
        },
    ));
    out.push(item(
        "bank_account",
        Some(f.bank_ok),
        true,
        if f.bank_ok {
            "Bank account given and well formed (IBAN/SWIFT/routing checks passed)"
        } else {
            "Bank account details are missing"
        },
    ));
    let tax_needs_id = spec.turkish || spec.id == "company_foreign";
    let tax_complete =
        f.tax_ok && (!tax_needs_id || f.tax_check.as_deref().is_some_and(|c| c != "none"));
    out.push(item(
        "tax_profile",
        Some(tax_complete),
        true,
        match (tax_complete, f.tax_check.as_deref()) {
            (true, Some("tckn")) => "Tax profile given; TCKN check digits valid".to_string(),
            (true, Some("vkn")) => "Tax profile given; VKN check digit valid".to_string(),
            (true, _) => "Tax profile given".to_string(),
            (false, _) if f.tax_ok => "The tax id is required for this account type".to_string(),
            (false, _) => "Tax profile is missing".to_string(),
        },
    ));
    // Documents: required ones present, current, and none infected.
    let live: Vec<&Doc> = f.docs.iter().filter(|d| d.removed_at.is_none()).collect();
    let mut need = Vec::new();
    let mut stale = Vec::new();
    for d in spec.docs {
        let required = match d.need {
            Need::Required => true,
            Need::IfTreaty => f.treaty,
            Need::Optional => false,
        };
        let have: Vec<&&Doc> = live.iter().filter(|x| x.doc_type == d.kind).collect();
        if required && have.is_empty() {
            need.push(d.en);
        }
        for x in have {
            if let (Some(max), Some(issued)) = (d.max_age_days, x.issued_on.as_deref()) {
                if days_since(issued, f.now).is_none_or(|n| n > max || n < 0) {
                    stale.push(format!("{} is older than {max} days", d.en));
                }
            } else if d.max_age_days.is_some() {
                stale.push(format!("{}: issue date missing", d.en));
            }
            if let Some(exp) = x.expires_on.as_deref() {
                if days_since(exp, f.now).is_none_or(|n| n >= 0) {
                    stale.push(format!("{} has expired", d.en));
                }
            }
        }
    }
    out.push(item(
        "documents_required",
        Some(need.is_empty()),
        true,
        if need.is_empty() {
            "Every required document is uploaded".to_string()
        } else {
            format!("Missing: {}", need.join("; "))
        },
    ));
    out.push(item(
        "documents_current",
        Some(stale.is_empty()),
        true,
        if stale.is_empty() {
            "Documents are current".to_string()
        } else {
            stale.join("; ")
        },
    ));
    let infected = live.iter().filter(|d| d.scan_state == "infected").count();
    let pending = live.iter().filter(|d| d.scan_state == "pending").count();
    out.push(item(
        "documents_scanned",
        Some(infected == 0),
        true,
        match (infected, pending) {
            (0, 0) => "Every document passed the malware scan".to_string(),
            (0, n) => format!("{n} document(s) waiting for the malware scan"),
            (n, _) => format!("{n} document(s) failed the malware scan; remove them"),
        },
    ));
    // Only a check of the person this account type verifies counts (a changed type needs a new one).
    let relevant: Vec<Check> = f
        .checks
        .iter()
        .filter(|c| c.subject == spec.kyc_subject)
        .cloned()
        .collect();
    let check = current_check(&relevant);
    let (kyc_ok, kyc_detail) = match check.map(|c| (c.result.as_str(), c.provider.as_str())) {
        Some(("accepted", "mock")) => (
            true,
            "Identity verified in the sandbox (not a real verification)",
        ),
        Some(("accepted", _)) => (true, "Identity verified by Shufti Pro"),
        Some(("pending", _)) => (false, "Identity verification is in progress"),
        Some(("declined", _)) => (false, "Identity verification was declined; try again"),
        Some(_) => (
            false,
            "Identity verification did not finish; start it again",
        ),
        None => (false, "Identity verification has not been started"),
    };
    out.push(item(
        "kyc",
        Some(kyc_ok),
        true,
        format!(
            "{kyc_detail} ({})",
            if spec.kyc_subject == "signatory" {
                "the authorised signatory"
            } else {
                "the applicant"
            }
        ),
    ));
    out.push(item(
        "aml",
        check
            .and_then(|c| c.aml_result.as_deref())
            .map(|r| r == "clear"),
        false,
        match check.and_then(|c| c.aml_result.as_deref()) {
            Some("clear") => "No sanctions or PEP match",
            Some("match") => "Possible sanctions or PEP match: review it",
            Some(_) => "AML screening needs review",
            None => "No AML result yet",
        },
    ));
    // Names: KYC (the person) against the legal name (individuals) or the signatory (companies),
    // and the bank holder against the legal name.
    let person = if spec.company {
        p.signatory.as_ref().map(|s| s.name.clone())
    } else {
        p.legal_name.clone()
    };
    let kyc_name = check.and_then(|c| c.verified_name.clone());
    let kyc_match = match (&person, &kyc_name) {
        (Some(a), Some(b)) => Some(names_match(a, b)),
        _ => None,
    };
    let bank_match = match (&p.legal_name, &f.bank_holder) {
        (Some(a), Some(b)) => Some(names_match(a, b)),
        _ => None,
    };
    out.push(item(
        "names_match",
        match (kyc_match, bank_match) {
            (Some(x), Some(y)) => Some(x && y),
            (Some(false), _) | (_, Some(false)) => Some(false),
            _ => None,
        },
        false,
        format!(
            "KYC name {} the {}; bank holder {} the legal name",
            match kyc_match {
                Some(true) => "matches",
                Some(false) => "does not match",
                None => "not compared with",
            },
            if spec.company {
                "signatory"
            } else {
                "legal name"
            },
            match bank_match {
                Some(true) => "matches",
                Some(false) => "does not match",
                None => "not compared with",
            }
        ),
    ));
    let agreed = a.agreement_version.as_deref() == Some(AGREEMENT_VERSION)
        && a.content_policy_version.as_deref() == Some(CONTENT_POLICY_VERSION);
    out.push(item(
        "agreements",
        Some(agreed),
        true,
        if agreed {
            "Developer Distribution Agreement and Content Policy accepted"
        } else {
            "Accept the Developer Distribution Agreement and the Content Policy"
        },
    ));
    out
}

pub(crate) fn blocking_failures(list: &[Value], allow_pending_kyc: bool) -> Vec<String> {
    list.iter()
        .filter(|i| i["blocking"] == true && i["ok"] == false)
        .filter(|i| {
            !(allow_pending_kyc
                && i["id"] == "kyc"
                && i["detail"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("in progress"))
        })
        .map(|i| i["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Everything the checklist needs about one application, read from the store and its seals.
pub(crate) fn facts_for(
    state: &AppState,
    app: &App,
    docs: Vec<Doc>,
    checks: Vec<Check>,
    now: i64,
) -> (Vec<Doc>, Vec<Check>, Vec<Value>) {
    let bank: Option<Bank> = app
        .bank_sealed
        .as_deref()
        .and_then(|s| open_sealed(state, &app.id, "bank", s).ok());
    let tax: Option<Tax> = app
        .tax_sealed
        .as_deref()
        .and_then(|s| open_sealed(state, &app.id, "tax", s).ok());
    let list = checklist(&Facts {
        app,
        docs: &docs,
        checks: &checks,
        bank_ok: bank.is_some(),
        tax_ok: tax.is_some(),
        tax_check: app
            .tax_masked
            .as_ref()
            .and_then(|m| m["tax_id_check"].as_str().map(str::to_string)),
        treaty: tax.as_ref().is_some_and(|t| t.treaty_country.is_some()),
        bank_holder: bank.map(|b| b.holder_name),
        now,
    });
    (docs, checks, list)
}

// ---- views ---------------------------------------------------------------------------------------------

pub(crate) fn base_json(a: &App) -> Value {
    let spec = type_spec(&a.account_type);
    json!({
        "id": a.id,
        "owner_kind": a.owner_kind,
        "owner_id": a.owner_id,
        "account_type": a.account_type,
        "account_type_label": spec.map(|s| s.en).unwrap_or("Legacy application"),
        "state": a.state,
        "public_name": a.public_name,
        "publisher_id": a.publisher_id,
        "tax_category": a.tax_category,
        "kyc_subject": spec.map(|s| s.kyc_subject),
        "revision": a.revision,
        "submitted_at": a.submitted_at.map(iso),
        "decided_at": a.decided_at.map(iso),
        "created_at": iso(a.created_at),
        "updated_at": iso(a.updated_at),
    })
}

/// The latest staff message: what `needs_info` asks for, or why it was rejected.
fn staff_message(events: &[Value]) -> Value {
    events
        .iter()
        .rev()
        .find(|e| e["by"] == "staff" && !e["message"].is_null())
        .map(|e| json!({ "kind": e["to"], "message": e["message"], "at": e["at"] }))
        .unwrap_or(Value::Null)
}

pub(crate) fn developer_json(
    a: &App,
    docs: &[Doc],
    checks: &[Check],
    list: &[Value],
    events: &[Value],
) -> Value {
    let mut v = base_json(a);
    v["profile"] = serde_json::to_value(&a.profile).unwrap_or(json!({}));
    v["bank"] = a.bank_masked.clone().unwrap_or(Value::Null);
    v["tax"] = a.tax_masked.clone().unwrap_or(Value::Null);
    v["agreement"] = json!({
        "agreement_version": a.agreement_version,
        "content_policy_version": a.content_policy_version,
        "accepted_at": a.agreement_accepted_at.map(iso),
        "current_agreement_version": AGREEMENT_VERSION,
        "current_content_policy_version": CONTENT_POLICY_VERSION,
    });
    v["documents"] = json!(docs
        .iter()
        .filter(|d| d.removed_at.is_none())
        .map(Doc::json)
        .collect::<Vec<_>>());
    v["kyc"] = json!(checks.iter().map(Check::json).collect::<Vec<_>>());
    v["kyc_provider"] = Value::Null;
    v["checklist"] = json!(list);
    v["events"] = json!(events
        .iter()
        .map(
            |e| json!({ "by": e["by"], "kind": e["kind"], "from": e["from"], "to": e["to"],
                         "message": e["message"], "at": e["at"] })
        )
        .collect::<Vec<_>>());
    v["staff_message"] = staff_message(events);
    v["editable"] = json!(matches!(a.state.as_str(), "draft" | "needs_info"));
    v
}

// ---- access --------------------------------------------------------------------------------------------

/// The caller's resource for applying as `owner_kind`/`owner_id`, with the org role resolved.
async fn owner_resource(
    state: &AppState,
    headers: &HeaderMap,
    kind: &str,
    owner: &str,
    f: &mut crate::policy::Facts,
) -> GResult<()> {
    if kind == "org" {
        let ctx = crate::org::org_ctx(state, headers, owner).await?;
        ctx.require_writable()?;
        f.org_permitted = permits(&ctx.roles, Perm::Security, ctx.rooms_everyone());
    }
    Ok(())
}

pub(crate) struct DevCtx {
    pub principal: Principal,
    pub account: String,
    pub app: App,
}

/// The caller, authorized for `action` on the application's owner. Anyone without access gets
/// `404`, exactly as for an application that does not exist.
async fn dev_gate(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    action: Action,
) -> GResult<DevCtx> {
    let (principal, account) = caller(state, headers).await?;
    let aid = id.to_string();
    let app = state
        .store
        .group_read(move |c| -> GResult<Option<App>> { Ok(app_in(c, &aid)?) })
        .await?;
    let mut f = facts(state);
    let resource_owner = app
        .as_ref()
        .map(|a| (a.owner_kind.clone(), a.owner_id.clone()));
    let permitted = match &resource_owner {
        Some((kind, owner)) if kind == "org" => {
            owner_resource(state, headers, kind, owner, &mut f)
                .await
                .is_ok()
                && f.org_permitted
        }
        Some((_, owner)) => owner == &account,
        None => false,
    };
    f.org_permitted = permitted;
    let resource = match &resource_owner {
        Some((kind, owner)) if kind == "org" => Resource::Org(owner),
        Some((_, owner)) => Resource::Account(owner),
        None => Resource::Account(""),
    };
    authorize(&Actor::Principal(&principal), action, &resource, &f).map_err(|d| deny(d, true))?;
    if !permitted {
        return Err(not_found());
    }
    let app = app.ok_or_else(not_found)?;
    Ok(DevCtx {
        principal,
        account,
        app,
    })
}

fn editable(a: &App) -> GResult<()> {
    if matches!(a.state.as_str(), "draft" | "needs_info") {
        Ok(())
    } else {
        Err(GErr::conflict(
            "application_locked",
            "this application is with Pigeonpost and cannot be changed now",
        )
        .with("state", a.state.clone()))
    }
}

/// Append to the history and, for a state change, queue the developer's email and message.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_event_in(
    tx: &Transaction<'_>,
    app: &str,
    actor: &str,
    role: &str,
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    message: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO publisher_application_events
            (id, application_id, actor, actor_role, kind, from_state, to_state, message, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            crate::marketplace_m2::new_id("pae"),
            app,
            actor,
            role,
            kind,
            from,
            to,
            message,
            now
        ],
    )?;
    if let (Some(from), Some(to)) = (from, to) {
        if from != to && to != "draft" {
            for channel in ["email", "message"] {
                tx.execute(
                    "INSERT INTO publisher_application_notices
                        (id, application_id, channel, state, next_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![crate::marketplace_m2::new_id("pan"), app, channel, to, now],
                )?;
            }
        }
    }
    Ok(())
}

/// The accounts that hear an application's events: the applicant and the owning account (or the
/// org's owner).
pub(crate) fn listeners_in(c: &Connection, a: &App) -> rusqlite::Result<Vec<String>> {
    let mut out = vec![a.applicant.clone()];
    if a.owner_kind == "account" {
        out.push(a.owner_id.clone());
    } else if let Some(owner) = c
        .query_row(
            "SELECT owner_account FROM orgs WHERE id = ?1",
            [&a.owner_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
    {
        out.push(owner);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Announce a change on `/v1/events` (ids and states only). Returns the accounts to wake.
pub(crate) fn announce_in(
    tx: &Transaction<'_>,
    a: &App,
    state_now: &str,
    now: i64,
) -> GResult<Vec<String>> {
    let who = listeners_in(tx, a)?;
    for account in &who {
        crate::store::publisher_registration::registration_event_in(
            tx,
            account,
            "nest.publisher.registration",
            &json!({ "application_id": a.id, "state": state_now, "revision": a.revision + 1 }),
            now as u64,
        )?;
    }
    Ok(who)
}

/// A KYC check changed (callback, status read or simulation): tell the developer's clients and,
/// while staff hold the application, record it in the history.
pub(crate) async fn kyc_changed(state: &AppState, application: &str) {
    let id = application.to_string();
    let now = now_unix() as i64;
    let woke = state
        .store
        .group_tx(move |tx| -> GResult<Vec<String>> {
            let Some(a) = app_in(tx, &id)? else {
                return Ok(Vec::new());
            };
            let checks = checks_in(tx, &id)?;
            if let Some(c) = current_check(&checks) {
                if matches!(a.state.as_str(), "submitted" | "in_review") {
                    record_event_in(
                        tx,
                        &id,
                        "kyc",
                        "system",
                        "kyc_result",
                        None,
                        None,
                        Some(&format!("Identity verification: {}", c.result)),
                        now,
                    )?;
                }
            }
            announce_in(tx, &a, &a.state, now)
        })
        .await
        .unwrap_or_default();
    crate::marketplace_m2::wake(state, woke);
}

// ---- developer routes ------------------------------------------------------------------------------------

/// `GET /v1/publisher-registration/requirements` — account types, their documents and the
/// agreement versions to accept.
async fn registration_requirements(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        authorize(
            &Actor::Principal(&principal),
            Action::PublisherApply,
            &Resource::Account(&account),
            &facts(&state),
        )
        .map_err(|d| deny(d, false))?;
        let mut v = requirements_json();
        v["kyc_provider"] = json!(kyc::provider(&state).name());
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publisher-registrations` — the caller's applications: their own account's and those of
/// the orgs they may apply for, newest first.
async fn list_registrations(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        authorize(
            &Actor::Principal(&principal),
            Action::PublisherApply,
            &Resource::Account(&account),
            &facts(&state),
        )
        .map_err(|d| deny(d, false))?;
        let who = account.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<App>> {
                Ok(apps_where(
                    c,
                    "(owner_kind = 'account' AND owner_id = ?1) OR applicant = ?1
                     ORDER BY created_at DESC, id LIMIT 50",
                    &[&who],
                )?)
            })
            .await?;
        // An org application filed by someone else is listed when the caller administers the org.
        let mut out = Vec::new();
        for a in rows {
            let mut f = facts(&state);
            if a.owner_kind == "org"
                && a.applicant != account
                && (owner_resource(&state, &headers, "org", &a.owner_id, &mut f)
                    .await
                    .is_err()
                    || !f.org_permitted)
            {
                continue;
            }
            out.push(base_json(&a));
        }
        Ok(Json(json!({ "data": out, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    account_type: String,
    #[serde(default)]
    org_id: Option<String>,
    #[serde(default)]
    public_name: Option<String>,
}

/// `POST /v1/publisher-registrations` — start a draft. An open legacy application for the same
/// owner becomes this draft (same id, same publisher profile).
async fn create_registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let req: CreateReq = parse(body)?;
        let spec = type_spec(&req.account_type).ok_or_else(|| {
            GErr::bad("invalid_account_type", "account_type: individual_tr, individual_foreign, sole_proprietor_tr, company_tr or company_foreign")
        })?;
        let (kind, owner) = match &req.org_id {
            Some(org) if !spec.org_allowed => {
                let _ = org;
                return Err(GErr::bad(
                    "account_type_needs_person",
                    "individuals and sole proprietors register for their own account",
                ));
            }
            Some(org) => ("org", org.clone()),
            None => ("account", account.clone()),
        };
        let mut f = facts(&state);
        owner_resource(&state, &headers, kind, &owner, &mut f).await?;
        let resource = if kind == "org" {
            Resource::Org(&owner)
        } else {
            Resource::Account(&owner)
        };
        authorize(&Actor::Principal(&principal), Action::PublisherApply, &resource, &f)
            .map_err(|d| deny(d, false))?;
        let public_name = match &req.public_name {
            Some(n) => crate::marketplace_m2::text(n, "public_name", 2, 80)?,
            None => String::new(),
        };
        let now = now_unix() as i64;
        let id = crate::marketplace_m2::new_id("pap");
        let (oid, by, t) = (owner.clone(), account.clone(), spec);
        let (app_id, woke) = state
            .store
            .group_tx(move |tx| -> GResult<(String, Vec<String>)> {
                let open: Option<App> = apps_where(
                    tx,
                    "owner_kind = ?1 AND owner_id = ?2
                     AND state IN ('draft','submitted','in_review','needs_info')",
                    &[&kind, &oid],
                )?
                .into_iter()
                .next();
                let app_id = match open {
                    Some(a) if a.account_type == "legacy" && matches!(a.state.as_str(), "submitted" | "needs_info") => {
                        tx.execute(
                            "UPDATE publisher_applications SET account_type = ?2, state = 'draft',
                                tax_category = ?3, public_name = CASE WHEN ?4 = '' THEN public_name ELSE ?4 END,
                                applicant = ?5, revision = revision + 1, updated_at = ?6
                              WHERE id = ?1",
                            params![a.id, t.id, t.tax_category, public_name, by, now],
                        )?;
                        record_event_in(tx, &a.id, &by, "developer", "upgraded", Some(&a.state), Some("draft"), None, now)?;
                        a.id
                    }
                    Some(a) => {
                        return Err(GErr::conflict(
                            "application_exists",
                            "this owner already has an open publisher application",
                        )
                        .with("application_id", a.id))
                    }
                    None => {
                        // A certified publisher re-verifying keeps its profile.
                        let publisher: Option<String> = tx
                            .query_row(
                                "SELECT id FROM publisher_profiles WHERE owner_kind = ?1 AND owner_id = ?2
                                   AND status IN ('applied','approved') LIMIT 1",
                                params![kind, oid],
                                |r| r.get(0),
                            )
                            .optional()?;
                        let display: Option<String> = match &publisher {
                            Some(p) => tx
                                .query_row("SELECT display_name FROM publisher_profiles WHERE id = ?1", [p], |r| r.get(0))
                                .optional()?,
                            None => None,
                        };
                        let name = if public_name.is_empty() { display.unwrap_or_default() } else { public_name.clone() };
                        tx.execute(
                            "INSERT INTO publisher_applications
                                (id, owner_kind, owner_id, applicant, account_type, state, publisher_id,
                                 public_name, tax_category, created_at, updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, 'draft', ?6, ?7, ?8, ?9, ?9)",
                            params![id, kind, oid, by, t.id, publisher, name, t.tax_category, now],
                        )?;
                        record_event_in(tx, &id, &by, "developer", "created", None, Some("draft"), None, now)?;
                        id.clone()
                    }
                };
                let a = app_in(tx, &app_id)?.ok_or_else(not_found)?;
                let woke = announce_in(tx, &a, "draft", now)?;
                Ok((app_id, woke))
            })
            .await?;
        crate::marketplace_m2::wake(&state, woke);
        let v = load_view(&state, &app_id).await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// An application with its documents, checks and history.
type Loaded = (App, Vec<Doc>, Vec<Check>, Vec<Value>);

async fn load_view(state: &AppState, id: &str) -> GResult<Value> {
    let aid = id.to_string();
    let (a, docs, checks, events) = state
        .store
        .group_read(move |c| -> GResult<Loaded> {
            let a = app_in(c, &aid)?.ok_or_else(not_found)?;
            Ok((
                a,
                docs_in(c, &aid, false)?,
                checks_in(c, &aid)?,
                events_in(c, &aid)?,
            ))
        })
        .await?;
    let (docs, checks, list) = facts_for(state, &a, docs, checks, now_unix() as i64);
    let mut v = developer_json(&a, &docs, &checks, &list, &events);
    v["kyc_provider"] = json!(kyc::provider(state).name());
    Ok(v)
}

/// `GET /v1/publisher-registrations/{application}`
async fn read_registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherApply).await?;
        Ok(Json(
            json!({ "data": load_view(&state, &ctx.app.id).await? }),
        ))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgreementReq {
    agreement_version: String,
    content_policy_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateReq {
    #[serde(default)]
    account_type: Option<String>,
    #[serde(default)]
    public_name: Option<String>,
    #[serde(default)]
    profile: Option<Profile>,
    #[serde(default)]
    bank: Option<Bank>,
    #[serde(default)]
    tax: Option<Tax>,
    #[serde(default)]
    accept_agreements: Option<AgreementReq>,
    /// The revision the form was based on; a stale one is refused (`409 revision_mismatch`).
    #[serde(default)]
    revision: Option<i64>,
}

/// `PATCH /v1/publisher-registrations/{application}` — save the draft. Each part given replaces
/// what was saved; bank and tax details are replaced whole (they are never shown back unmasked).
async fn update_registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: UpdateReq = parse(body)?;
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherApply).await?;
        let a = ctx.app;
        editable(&a)?;
        if req.revision.is_some_and(|r| r != a.revision) {
            return Err(GErr::conflict("revision_mismatch", "this application changed; reload it")
                .with("revision", a.revision));
        }
        let account_type = match &req.account_type {
            Some(t) => {
                let spec = type_spec(t).ok_or_else(|| GErr::bad("invalid_account_type", "unknown account type"))?;
                if a.owner_kind == "org" && !spec.org_allowed {
                    return Err(GErr::bad(
                        "account_type_needs_person",
                        "individuals and sole proprietors register for their own account",
                    ));
                }
                spec.id.to_string()
            }
            None => a.account_type.clone(),
        };
        let spec = type_spec(&account_type).ok_or_else(|| {
            GErr::bad("account_type_required", "choose an account type to complete a legacy application")
        })?;
        let now = now_unix() as i64;
        let public_name = match &req.public_name {
            Some(n) => Some(crate::marketplace_m2::text(n, "public_name", 2, 80)?),
            None => None,
        };
        let profile = match req.profile {
            Some(p) => Some(serde_json::to_string(&clean_profile(p, spec)?).unwrap_or_default()),
            None => None,
        };
        let bank = match req.bank {
            Some(b) => {
                let b = clean_bank(b, spec)?;
                Some((seal_part(&state, &a.id, "bank", &b)?, mask_bank(&b).to_string()))
            }
            None => None,
        };
        let tax = match req.tax {
            Some(t) => {
                let (t, check) = clean_tax(t, spec, now)?;
                Some((seal_part(&state, &a.id, "tax", &t)?, mask_tax(&t, spec.tax_category, check).to_string()))
            }
            None => None,
        };
        // A changed account type invalidates the sealed details entered for the old one.
        let type_changed = account_type != a.account_type;
        let agreements = match &req.accept_agreements {
            Some(ag) => {
                if ag.agreement_version != AGREEMENT_VERSION || ag.content_policy_version != CONTENT_POLICY_VERSION {
                    return Err(GErr::conflict(
                        "agreement_version_mismatch",
                        "the agreements changed; read and accept the current versions",
                    )
                    .with("agreement_version", AGREEMENT_VERSION)
                    .with("content_policy_version", CONTENT_POLICY_VERSION));
                }
                true
            }
            None => false,
        };
        let (aid, by, cat, base_rev) = (a.id.clone(), ctx.account.clone(), spec.tax_category, a.revision);
        let at = account_type.clone();
        let clear_tax = type_changed && tax.is_none();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let n = tx.execute(
                    "UPDATE publisher_applications SET
                        account_type = ?3, tax_category = ?4,
                        public_name = COALESCE(?5, public_name),
                        profile = COALESCE(?6, profile),
                        bank_sealed = CASE WHEN ?7 THEN NULL WHEN ?8 IS NOT NULL THEN ?8 ELSE bank_sealed END,
                        bank_masked = CASE WHEN ?7 THEN NULL WHEN ?9 IS NOT NULL THEN ?9 ELSE bank_masked END,
                        tax_sealed = CASE WHEN ?16 THEN NULL WHEN ?10 IS NOT NULL THEN ?10 ELSE tax_sealed END,
                        tax_masked = CASE WHEN ?16 THEN NULL WHEN ?11 IS NOT NULL THEN ?11 ELSE tax_masked END,
                        agreement_version = CASE WHEN ?12 THEN ?13 ELSE agreement_version END,
                        content_policy_version = CASE WHEN ?12 THEN ?14 ELSE content_policy_version END,
                        agreement_accepted_at = CASE WHEN ?12 THEN ?15 ELSE agreement_accepted_at END,
                        revision = revision + 1, updated_at = ?15
                      WHERE id = ?1 AND revision = ?2 AND state IN ('draft','needs_info')",
                    params![
                        aid,
                        base_rev,
                        at,
                        cat,
                        public_name,
                        profile,
                        type_changed && bank.is_none(),
                        bank.as_ref().map(|b| b.0.clone()),
                        bank.as_ref().map(|b| b.1.clone()),
                        tax.as_ref().map(|t| t.0.clone()),
                        tax.as_ref().map(|t| t.1.clone()),
                        agreements,
                        AGREEMENT_VERSION,
                        CONTENT_POLICY_VERSION,
                        now,
                        clear_tax
                    ],
                )?;
                if n == 0 {
                    return Err(GErr::conflict("revision_mismatch", "this application changed; reload it"));
                }
                if agreements {
                    record_event_in(
                        tx,
                        &aid,
                        &by,
                        "developer",
                        "agreements_accepted",
                        None,
                        None,
                        Some(&format!("{AGREEMENT_VERSION}, {CONTENT_POLICY_VERSION}")),
                        now,
                    )?;
                }
                Ok(())
            })
            .await?;
        Ok(Json(json!({ "data": load_view(&state, &a.id).await? })))
    }
    .await;
    into_response(result)
}

/// The blob store for registration documents, when the postbox has one.
pub(crate) fn doc_blobs(state: &AppState) -> Option<crate::blobs::Blobs> {
    let base = state.blobs.as_ref()?;
    base.sub_store(
        "publisherdocs",
        DOC_MAX + 64,
        crate::env_num::<u64>("PIGEONPOST_PUBLISHER_DOC_TOTAL_MB", 20_480) * 1024 * 1024,
    )
}

/// The media type the bytes actually are: PDF, JPEG or PNG, by signature. Anything else is refused.
pub(crate) fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else {
        None
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, b| {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
            out
        })
}

fn documents_unavailable() -> GErr {
    GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "documents_unavailable",
        "document storage is not configured on this postbox",
    )
}

/// `POST /v1/publisher-registrations/{application}/documents?type=&filename=&issued_on=&expires_on=`
/// — the raw file as the body (PDF, JPEG or PNG, at most 10 MB).
async fn upload_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let result = async {
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherApply).await?;
        let a = ctx.app;
        editable(&a)?;
        let spec = type_spec(&a.account_type)
            .ok_or_else(|| GErr::bad("account_type_required", "choose an account type first"))?;
        let doc_type = q.get("type").map(String::as_str).unwrap_or_default();
        let dspec = spec
            .docs
            .iter()
            .find(|d| d.kind == doc_type)
            .ok_or_else(|| GErr::bad("invalid_document_type", "this account type does not take that document"))?;
        if body.is_empty() || body.len() > DOC_MAX {
            return Err(GErr::new(StatusCode::PAYLOAD_TOO_LARGE, "document_too_large", "documents are 1 byte to 10 MB"));
        }
        let content_type = sniff(&body)
            .ok_or_else(|| GErr::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_document", "upload a PDF, JPEG or PNG file"))?;
        let filename = crate::blobs::safe_filename(q.get("filename").map(String::as_str).unwrap_or("document"));
        let date = |key: &str| -> GResult<Option<String>> {
            match q.get(key).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                None => Ok(None),
                Some(d) if date_ok(d) => Ok(Some(d.to_string())),
                Some(_) => Err(GErr::bad("invalid_date", format!("{key}: YYYY-MM-DD"))),
            }
        };
        let issued_on = date("issued_on")?;
        let expires_on = date("expires_on")?;
        if dspec.max_age_days.is_some() && issued_on.is_none() {
            return Err(GErr::bad("issued_on_required", "give the date this document was issued"));
        }
        if dspec.expires && expires_on.is_none() {
            return Err(GErr::bad("expires_on_required", "give the date this document expires"));
        }
        let blobs = doc_blobs(&state).ok_or_else(documents_unavailable)?;
        let doc_id = crate::marketplace_m2::new_id("pad");
        let plain_digest = hex_sha256(&body);
        let sealed = state
            .vault
            .seal_labeled(DOC_LABEL, doc_id.as_bytes(), &body)
            .map_err(|_| store_error("seal failed"))?;
        let stored: i64 = state
            .store
            .group_read(|c| -> GResult<i64> {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(size), 0) FROM publisher_application_documents
                      WHERE purged_at IS NULL",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await?;
        if blobs.headroom(stored.max(0) as u64, sealed.len() as u64).is_err() {
            return Err(documents_unavailable());
        }
        let blob = blobs.put(&sealed).map_err(|_| documents_unavailable())?;
        let now = now_unix() as i64;
        let (aid, did, by, size) = (a.id.clone(), doc_id.clone(), ctx.account.clone(), body.len() as i64);
        let (dt, fname, ct, b2) = (doc_type.to_string(), filename, content_type, blob.clone());
        let saved = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM publisher_application_documents WHERE application_id = ?1",
                    [&aid],
                    |r| r.get(0),
                )?;
                if count >= MAX_DOCS {
                    return Err(GErr::conflict("too_many_documents", "remove documents you no longer need"));
                }
                let st: String = tx.query_row(
                    "SELECT state FROM publisher_applications WHERE id = ?1",
                    [&aid],
                    |r| r.get(0),
                )?;
                if !matches!(st.as_str(), "draft" | "needs_info") {
                    return Err(GErr::conflict("application_locked", "this application cannot be changed now"));
                }
                tx.execute(
                    "INSERT INTO publisher_application_documents
                        (id, application_id, doc_type, filename, content_type, size, sha256, blob,
                         issued_on, expires_on, uploaded_by, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![did, aid, dt, fname, ct, size, plain_digest, b2, issued_on, expires_on, by, now],
                )?;
                tx.execute(
                    "UPDATE publisher_applications SET revision = revision + 1, updated_at = ?2 WHERE id = ?1",
                    params![aid, now],
                )?;
                Ok(())
            })
            .await;
        if let Err(e) = saved {
            // Nothing refers to the sealed bytes: the content address is unique to this upload.
            blobs.remove(&blob);
            return Err(e);
        }
        let did = doc_id.clone();
        let doc = state
            .store
            .group_read(move |c| -> GResult<Value> {
                Ok(docs_in(c, &a.id, false)?
                    .into_iter()
                    .find(|d| d.id == did)
                    .map(|d| d.json())
                    .unwrap_or(Value::Null))
            })
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": doc }))))
    }
    .await;
    into_response(result)
}

/// The document's bytes, opened, as an attachment that never renders in the postbox's origin.
pub(crate) fn serve_document(state: &AppState, d: &Doc, inline: bool) -> GResult<Response> {
    if d.purged_at.is_some() {
        return Err(GErr::new(
            StatusCode::GONE,
            "document_purged",
            "this document was deleted under the retention policy",
        ));
    }
    if d.scan_state == "infected" {
        return Err(GErr::conflict(
            "document_infected",
            "this file failed the malware scan and is not served",
        ));
    }
    let blobs = doc_blobs(state).ok_or_else(documents_unavailable)?;
    let sealed = d
        .blob
        .as_deref()
        .and_then(|b| blobs.get(b))
        .ok_or_else(|| {
            GErr::new(
                StatusCode::GONE,
                "document_missing",
                "the document's bytes are missing",
            )
        })?;
    let plain = state
        .vault
        .open_labeled(DOC_LABEL, d.id.as_bytes(), &sealed)
        .map_err(|_| store_error("a document could not be opened"))?;
    let mut resp = plain.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(match d.content_type.as_str() {
            "application/pdf" => "application/pdf",
            "image/png" => "image/png",
            _ => "image/jpeg",
        }),
    );
    let disposition = format!(
        "{}; filename=\"{}\"",
        if inline { "inline" } else { "attachment" },
        d.filename.replace(['"', '\\'], "_")
    );
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "sandbox; default-src 'none'; img-src 'self'; style-src 'unsafe-inline'",
        ),
    );
    if let Ok(v) = HeaderValue::from_str(&d.scan_state) {
        h.insert("x-pigeonpost-scan", v);
    }
    Ok(resp)
}

/// `GET /v1/publisher-registrations/{application}/documents/{document}`
async fn download_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, doc)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherApply).await?;
        let aid = ctx.app.id.clone();
        let d = state
            .store
            .group_read(move |c| -> GResult<Option<Doc>> {
                Ok(docs_in(c, &aid, false)?.into_iter().find(|d| d.id == doc))
            })
            .await?
            .ok_or_else(not_found)?;
        serve_document(&state, &d, false)
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/publisher-registrations/{application}/documents/{document}` — withdraw a document
/// while the application is editable. Never submitted: the bytes go at once. Already seen by
/// staff: it stays (removed) as part of the record until the application's retention ends.
async fn remove_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, doc)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherApply).await?;
        editable(&ctx.app)?;
        let now = now_unix() as i64;
        let (aid, did) = (ctx.app.id.clone(), doc.clone());
        let purge = ctx.app.submitted_at.is_none();
        let blob = state
            .store
            .group_tx(move |tx| -> GResult<Option<String>> {
                let blob: Option<Option<String>> = tx
                    .query_row(
                        "SELECT blob FROM publisher_application_documents
                          WHERE id = ?1 AND application_id = ?2 AND removed_at IS NULL",
                        params![did, aid],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(blob) = blob else {
                    return Err(not_found());
                };
                tx.execute(
                    "UPDATE publisher_application_documents SET removed_at = ?2,
                        purged_at = CASE WHEN ?3 THEN ?2 ELSE purged_at END,
                        blob = CASE WHEN ?3 THEN NULL ELSE blob END
                      WHERE id = ?1",
                    params![did, now, purge],
                )?;
                tx.execute(
                    "UPDATE publisher_applications SET revision = revision + 1, updated_at = ?2 WHERE id = ?1",
                    params![aid, now],
                )?;
                Ok(if purge { blob } else { None })
            })
            .await?;
        if let (Some(b), Some(blobs)) = (blob, doc_blobs(&state)) {
            blobs.remove(&b);
        }
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    into_response(result)
}

/// `POST /v1/publisher-registrations/{application}/kyc` — start (or restart) identity
/// verification. Answers the check with the hosted flow's `verification_url` (live provider).
async fn start_kyc(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherApply).await?;
        let a = ctx.app;
        editable(&a)?;
        let spec = type_spec(&a.account_type)
            .ok_or_else(|| GErr::bad("account_type_required", "choose an account type first"))?;
        let aid = a.id.clone();
        let checks = state
            .store
            .group_read(move |c| -> GResult<Vec<Check>> { Ok(checks_in(c, &aid)?) })
            .await?;
        if checks.len() >= 10 {
            return Err(GErr::conflict(
                "kyc_attempts_exceeded",
                "too many verification attempts; contact Pigeonpost",
            ));
        }
        let mine: Vec<Check> = checks
            .iter()
            .filter(|c| c.subject == spec.kyc_subject)
            .cloned()
            .collect();
        if let Some(c) = current_check(&mine) {
            if c.result == "accepted" {
                return Err(GErr::conflict("kyc_done", "identity is already verified"));
            }
            // A pending live check is resumed rather than duplicated.
            if c.result == "pending"
                && c.verification_url.is_some()
                && now_unix() as i64 - c.created_at < 3000
            {
                return Ok((StatusCode::OK, Json(json!({ "data": c.json() }))));
            }
        }
        let p = kyc::provider(&state);
        let reference = crate::marketplace_m2::new_id("ppk");
        let email = a
            .profile
            .contact_email
            .clone()
            .or_else(|| a.profile.support_email.clone());
        let country = a.profile.country.clone();
        let language = if spec.turkish { "TR" } else { "EN" };
        let outcome = kyc::create(
            &p,
            &kyc::Start {
                reference: &reference,
                email: email.as_deref(),
                country: country.as_deref(),
                language,
                callback_url: format!("{}/v1/integrations/shufti/callback", state.public_url),
            },
        )
        .await?;
        let now = now_unix() as i64;
        let check_id = crate::marketplace_m2::new_id("kyc");
        let (cid, aid, by, provider) = (
            check_id.clone(),
            a.id.clone(),
            ctx.account.clone(),
            p.name(),
        );
        let subject = spec.kyc_subject;
        let checks = state
            .store
            .group_tx(move |tx| -> GResult<Vec<Check>> {
                tx.execute(
                    "INSERT INTO publisher_kyc_checks
                        (id, application_id, subject, provider, reference, event, result,
                         verification_url, created_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, ?9)",
                    params![
                        cid,
                        aid,
                        subject,
                        provider,
                        outcome.reference,
                        outcome.event,
                        outcome.verification_url,
                        by,
                        now
                    ],
                )?;
                record_event_in(
                    tx,
                    &aid,
                    &by,
                    "developer",
                    "kyc_started",
                    None,
                    None,
                    None,
                    now,
                )?;
                Ok(checks_in(tx, &aid)?)
            })
            .await?;
        let c = checks
            .iter()
            .find(|c| c.id == check_id)
            .map(Check::json)
            .unwrap_or(Value::Null);
        Ok((StatusCode::CREATED, Json(json!({ "data": c }))))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SimulateReq {
    outcome: String,
}

/// `POST /v1/publisher-registrations/{application}/kyc/{check}/simulate` — sandbox only: finish a
/// mock check as `accepted` or `declined` through the same signed-message path a callback takes.
async fn simulate_kyc(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, check)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: SimulateReq = parse(body)?;
        if !matches!(req.outcome.as_str(), "accepted" | "declined") {
            return Err(GErr::bad(
                "invalid_request",
                "outcome: accepted or declined",
            ));
        }
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherApply).await?;
        let p = kyc::provider(&state);
        if !matches!(p, kyc::Provider::Mock { .. }) {
            return Err(not_found());
        }
        let aid = ctx.app.id.clone();
        let c = state
            .store
            .group_read(move |c| -> GResult<Option<Check>> {
                Ok(checks_in(c, &aid)?.into_iter().find(|x| x.id == check))
            })
            .await?
            .filter(|c| c.provider == "mock")
            .ok_or_else(not_found)?;
        let spec = type_spec(&ctx.app.account_type);
        let name = if spec.is_some_and(|s| s.company) {
            ctx.app.profile.signatory.as_ref().map(|s| s.name.clone())
        } else {
            ctx.app.profile.legal_name.clone()
        }
        .unwrap_or_else(|| "Sandbox Person".into());
        let country = ctx
            .app
            .profile
            .country
            .clone()
            .unwrap_or_else(|| "TR".into());
        let (bytes, sig) = kyc::mock_message(&p, &c.reference, &req.outcome, &name, &country);
        if !kyc::shufti_signature_ok(
            &bytes,
            Some(&sig),
            match &p {
                kyc::Provider::Mock { secret } => secret,
                _ => "",
            },
            kyc::Scheme::HashedSecret,
        ) {
            return Err(store_error("sandbox signature failed"));
        }
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let o = kyc::outcome(&v);
        let now = now_unix() as i64;
        let changed = state
            .store
            .group_tx(move |tx| -> GResult<Option<String>> {
                kyc::apply_kyc_message_in(tx, &bytes, &o, "mock", now)
            })
            .await?;
        if let Some(app) = changed {
            kyc_changed(&state, &app).await;
        }
        Ok(Json(
            json!({ "data": load_view(&state, &ctx.app.id).await? }),
        ))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SubmitReq {
    /// A reply to a request for information.
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    revision: Option<i64>,
}

/// `POST /v1/publisher-registrations/{application}/submit` — submit a draft, or resubmit after
/// `needs_info` (with a reply). Needs a recent two-factor sign-in.
async fn submit_registration(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let result = async {
        let req: SubmitReq = match body {
            Some(Json(v)) => serde_json::from_value(v)
                .map_err(|e| GErr::bad("invalid_request", e.to_string()))?,
            None => SubmitReq::default(),
        };
        let ctx = dev_gate(&state, &headers, &id, Action::PublisherSubmit).await?;
        let a = ctx.app;
        editable(&a)?;
        if req.revision.is_some_and(|r| r != a.revision) {
            return Err(GErr::conflict(
                "revision_mismatch",
                "this application changed; reload it",
            ));
        }
        let message = match &req.message {
            Some(m) => Some(crate::marketplace_m2::text(m, "message", 1, 4000)?),
            None => None,
        };
        let now = now_unix() as i64;
        let aid = a.id.clone();
        let (docs, checks) = state
            .store
            .group_read(move |c| -> GResult<(Vec<Doc>, Vec<Check>)> {
                Ok((docs_in(c, &aid, false)?, checks_in(c, &aid)?))
            })
            .await?;
        let (_, _, list) = facts_for(&state, &a, docs, checks, now);
        let failing = blocking_failures(&list, true);
        if !failing.is_empty() {
            return Err(GErr::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "application_incomplete",
                "finish the items in the checklist before submitting",
            )
            .with("missing", failing)
            .with("checklist", list));
        }
        let audit = crate::marketplace_m2::audit_row(
            &state,
            &ctx.principal,
            &ctx.account,
            "marketplace.publisher_registration.submit",
            format!("publisher_application:{}", a.id),
            json!({ "revision": a.revision, "resubmission": a.state == "needs_info" }),
            &client_ip,
            &headers,
        );
        let (aid, by, from) = (a.id.clone(), ctx.account.clone(), a.state.clone());
        let woke = state
            .store
            .group_tx(move |tx| -> GResult<Vec<String>> {
                let n = tx.execute(
                    "UPDATE publisher_applications SET state = 'submitted', submitted_at = ?3,
                        reviewer = NULL, revision = revision + 1, updated_at = ?3
                      WHERE id = ?1 AND revision = ?2 AND state IN ('draft','needs_info')",
                    params![aid, a.revision, now],
                )?;
                if n == 0 {
                    return Err(GErr::conflict(
                        "revision_mismatch",
                        "this application changed; reload it",
                    ));
                }
                record_event_in(
                    tx,
                    &aid,
                    &by,
                    "developer",
                    if from == "needs_info" {
                        "resubmitted"
                    } else {
                        "submitted"
                    },
                    Some(&from),
                    Some("submitted"),
                    message.as_deref(),
                    now,
                )?;
                crate::store::audit::append_in(tx, &audit)?;
                announce_in(tx, &a, "submitted", now)
            })
            .await?;
        crate::marketplace_m2::wake(&state, woke);
        Ok(Json(json!({ "data": load_view(&state, &id).await? })))
    }
    .await;
    into_response(result)
}

/// `POST /v1/integrations/shufti/callback` (see `publisher_kyc::handle_callback`).
async fn shufti_callback(state: State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    kyc::handle_callback(state, headers, body).await
}
