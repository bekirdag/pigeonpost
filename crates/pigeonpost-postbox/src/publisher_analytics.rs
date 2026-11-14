//! Aggregate app usage and authoritative finance, shared by publishers and staff.
//! No customer identifiers, job payloads or finance credentials leave this module.
use crate::backoffice::{audit_row, require_staff};
use crate::groups::{GErr, GResult};
use crate::marketplace::{into_response, iso, not_found};
use crate::marketplace_m2::{gate, Need};
use crate::policy::Action;
use crate::{now_unix, AppState, ClientIp};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap},
    response::Response,
    routing::get,
    Extension, Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publishers/{publisher}/analytics",
            get(publisher_report),
        )
        .route(
            "/v1/staff/publishers/{publisher}/analytics",
            get(staff_report),
        )
        .route("/v1/staff/publisher-analytics", get(staff_publishers))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportQuery {
    from: Option<String>,
    to: Option<String>,
    listing_id: Option<String>,
}
#[derive(Clone)]
struct Range {
    from: i64,
    to: i64,
    listing: Option<String>,
}
fn invalid() -> GErr {
    GErr::bad(
        "invalid_range",
        "Use RFC3339 UTC dates in an increasing range of at most 366 days",
    )
}
fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}
impl ReportQuery {
    fn range(self) -> GResult<Range> {
        let parse = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|d| d.timestamp())
                .map_err(|_| invalid())
        };
        let to = self
            .to
            .as_deref()
            .map(parse)
            .transpose()?
            .unwrap_or(now_unix() as i64 + 1);
        let from = self
            .from
            .as_deref()
            .map(parse)
            .transpose()?
            .unwrap_or(to - 30 * 86400);
        if from < 0 || to <= from || to - from > 366 * 86400 {
            return Err(invalid());
        }
        if self.listing_id.as_deref().is_some_and(|id| !id_ok(id)) {
            return Err(GErr::bad("invalid_listing", "Invalid app id"));
        }
        Ok(Range {
            from,
            to,
            listing: self.listing_id,
        })
    }
}
fn private(result: GResult<Json<Value>>) -> Response {
    let mut response = into_response(result);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

/// `GET /v1/publishers/{publisher}/analytics` — team members, including finance readers.
async fn publisher_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    Query(query): Query<ReportQuery>,
) -> Response {
    let result = async {
        gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Read,
        )
        .await?;
        Ok(Json(
            json!({"data": report(&state, publisher, query.range()?).await?}),
        ))
    }
    .await;
    private(result)
}
/// `GET /v1/staff/publishers/{publisher}/analytics` — finance staff, strong sign-in, audited.
async fn staff_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    client_ip: Option<Extension<ClientIp>>,
    Path(publisher): Path<String>,
    Query(query): Query<ReportQuery>,
) -> Response {
    let result = async {
        let who = require_staff(
            &state,
            &headers,
            Action::StaffFinanceRead,
            "publisher_analytics",
        )
        .await?;
        let range = query.range()?;
        if !id_ok(&publisher) {
            return Err(not_found());
        }
        let audit = audit_row(
            &state,
            &who,
            "staff.publishers.analytics",
            Some(publisher.clone()),
            json!({"from":range.from,"to":range.to,"listing_id":range.listing}),
            &client_ip,
            &headers,
        );
        state.store.append_audit(audit).await?;
        Ok(Json(
            json!({"data": report(&state, publisher, range).await?}),
        ))
    }
    .await;
    private(result)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Picker {
    q: Option<String>,
    offset: Option<usize>,
}
/// `GET /v1/staff/publisher-analytics` — finance-only publisher picker, including revoked publishers.
async fn staff_publishers(
    State(state): State<AppState>,
    headers: HeaderMap,
    client_ip: Option<Extension<ClientIp>>,
    Query(query): Query<Picker>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffFinanceRead, "publisher_analytics").await?;
        let q = query.q.unwrap_or_default();
        let offset = query.offset.unwrap_or(0);
        if q.len() > 120 || offset > 1_000_000 { return Err(GErr::bad("invalid_request","Search or offset is too large")); }
        state.store.append_audit(audit_row(&state, &who, "staff.publishers.analytics_list", None,
            json!({"offset":offset}), &client_ip, &headers)).await?;
        let rows = state.store.group_read(move |c| -> GResult<Vec<Value>> {
            let mut stmt = c.prepare("SELECT id, display_name, status FROM publisher_profiles WHERE instr(lower(display_name),lower(?1))>0 OR instr(id,?1)>0 ORDER BY display_name,id LIMIT 51 OFFSET ?2")?;
            let rows = stmt.query_map(params![q, offset as i64], |r|Ok(json!({"id":r.get::<_,String>(0)?,"display_name":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await?;
        Ok(Json(json!({"data":rows.iter().take(50).collect::<Vec<_>>(),"next_offset":(rows.len()>50).then_some(offset+50)})))
    }.await;
    private(result)
}

#[derive(Default, Clone, Serialize)]
struct Usage {
    jobs: i64,
    succeeded: i64,
    failed: i64,
    cancelled: i64,
    in_progress: i64,
    active_users: i64,
    new_installations: i64,
    active_installations: i64,
}
fn jobs_in(c: &Connection, publisher: &str, r: &Range, listing: Option<&str>) -> GResult<Usage> {
    // Execution paths are disjoint. Review/conformance jobs live in nest_review_jobs, not here.
    let sql = "WITH jobs AS (
      SELECT j.account_id,CASE WHEN j.admission='rejected' THEN 'failed' ELSE j.execution END state FROM nest_jobs j JOIN nest_listings l ON l.id=j.listing_id
       WHERE l.publisher_id=?1 AND j.created_at>=?2 AND j.created_at<?3 AND (?4 IS NULL OR j.listing_id=?4)
      UNION ALL
      SELECT j.account_id,j.state FROM nest_hosted_jobs j JOIN nest_listings l ON l.id=j.listing_id
       WHERE l.publisher_id=?1 AND j.created_at>=?2 AND j.created_at<?3 AND (?4 IS NULL OR j.listing_id=?4))
      SELECT COUNT(*),COALESCE(SUM(state='succeeded'),0),COALESCE(SUM(state IN ('failed','expired','stopped_security','rejected')),0),
       COALESCE(SUM(state='cancelled'),0),COUNT(DISTINCT account_id) FROM jobs";
    let mut usage = c.query_row(sql, params![publisher, r.from, r.to, listing], |row| {
        Ok(Usage {
            jobs: row.get(0)?,
            succeeded: row.get(1)?,
            failed: row.get(2)?,
            cancelled: row.get(3)?,
            active_users: row.get(4)?,
            ..Usage::default()
        })
    })?;
    usage.in_progress = usage.jobs - usage.succeeded - usage.failed - usage.cancelled;
    let (new,active)=c.query_row("SELECT COALESCE(SUM(i.created_at>=?2 AND i.created_at<?3),0),COALESCE(SUM(i.status='active'),0)
      FROM nest_installations i JOIN nest_listings l ON l.id=i.listing_id WHERE l.publisher_id=?1 AND (?4 IS NULL OR i.listing_id=?4)",
      params![publisher,r.from,r.to,listing],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?)))?;
    usage.new_installations = new;
    usage.active_installations = active;
    Ok(usage)
}
fn usage_in(c: &Connection, publisher: &str, r: &Range) -> GResult<Value> {
    let name: Option<String> = c
        .query_row(
            "SELECT display_name FROM publisher_profiles WHERE id=?1",
            [publisher],
            |row| row.get(0),
        )
        .optional()?;
    let name = name.ok_or_else(not_found)?;
    let mut stmt=c.prepare("SELECT id,title,status FROM nest_listings WHERE publisher_id=?1 ORDER BY title,id LIMIT 1001")?;
    let apps = stmt
        .query_map([publisher], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if apps.len() > 1000 {
        return Err(GErr::bad(
            "report_too_large",
            "Publisher has too many apps for this report",
        ));
    }
    if r.listing
        .as_ref()
        .is_some_and(|id| !apps.iter().any(|a| &a.0 == id))
    {
        return Err(not_found());
    }
    let app_views=apps.iter().filter(|a|r.listing.as_ref().is_none_or(|id|&a.0==id)).map(|a|Ok(json!({"id":a.0,"title":a.1,"status":a.2,"usage":jobs_in(c,publisher,r,Some(&a.0))?}))).collect::<GResult<Vec<_>>>()?;
    let mut daily = BTreeMap::<String, Value>::new();
    let mut q=c.prepare("WITH jobs AS (
      SELECT j.created_at,CASE WHEN j.admission='rejected' THEN 'failed' ELSE j.execution END state FROM nest_jobs j JOIN nest_listings l ON l.id=j.listing_id WHERE l.publisher_id=?1 AND j.created_at>=?2 AND j.created_at<?3 AND (?4 IS NULL OR j.listing_id=?4)
      UNION ALL SELECT j.created_at,j.state FROM nest_hosted_jobs j JOIN nest_listings l ON l.id=j.listing_id WHERE l.publisher_id=?1 AND j.created_at>=?2 AND j.created_at<?3 AND (?4 IS NULL OR j.listing_id=?4))
      SELECT strftime('%Y-%m-%d',created_at,'unixepoch'),COUNT(*),SUM(state='succeeded'),SUM(state IN ('failed','expired','stopped_security')),SUM(state='cancelled') FROM jobs GROUP BY 1 ORDER BY 1")?;
    let rows=q.query_map(params![publisher,r.from,r.to,r.listing],|row|Ok((row.get::<_,String>(0)?,json!({"jobs":row.get::<_,i64>(1)?,"succeeded":row.get::<_,i64>(2)?,"failed":row.get::<_,i64>(3)?,"cancelled":row.get::<_,i64>(4)?}))))?;
    for row in rows {
        let (day, v) = row?;
        daily.insert(day, v);
    }
    let daily = daily
        .into_iter()
        .map(|(day, mut v)| {
            v["day"] = json!(day);
            v
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"publisher":{"id":publisher,"display_name":name},"from":iso(r.from),"to":iso(r.to),
      "listing_id":r.listing,"apps":app_views,"app_choices":apps.iter().map(|a|json!({"id":a.0,"title":a.1})).collect::<Vec<_>>(),
      "usage":jobs_in(c,publisher,r,r.listing.as_deref())?,"daily":daily}),
    )
}
async fn report(state: &AppState, publisher: String, range: Range) -> GResult<Value> {
    let (p, r) = (publisher.clone(), range.clone());
    let mut data = state
        .store
        .group_read(move |c| -> GResult<Value> {
            let tx = c.unchecked_transaction()?;
            let v = usage_in(&tx, &p, &r)?;
            tx.commit()?;
            Ok(v)
        })
        .await?;
    let query = vec![("from", iso(range.from)), ("to", iso(range.to))];
    let mut query = query;
    if let Some(id) = &range.listing {
        query.push(("listing_id", id.clone()));
    }
    data["finance"] = match state
        .staff_finance
        .publisher_analytics(&publisher, &query)
        .await
    {
        Ok(mut v) => {
            v["status"] = json!("available");
            v
        }
        Err(_) => {
            json!({"status":"unavailable","message":"Earnings are temporarily unavailable. Usage is still shown; missing earnings are not zero."})
        }
    };
    data["generated_at"] = json!(iso(now_unix() as i64));
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_deduplicates_people_across_apps_and_execution_paths() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE publisher_profiles(id TEXT,display_name TEXT);
          CREATE TABLE nest_listings(id TEXT,publisher_id TEXT,title TEXT,status TEXT);
          CREATE TABLE nest_jobs(account_id TEXT,listing_id TEXT,created_at INTEGER,execution TEXT,admission TEXT DEFAULT 'admitted');
          CREATE TABLE nest_hosted_jobs(account_id TEXT,listing_id TEXT,created_at INTEGER,state TEXT);
          CREATE TABLE nest_installations(listing_id TEXT,created_at INTEGER,status TEXT);
          INSERT INTO publisher_profiles VALUES('pub_a','A'),('pub_b','B');
          INSERT INTO nest_listings VALUES('a','pub_a','First','published'),('b','pub_a','Second','withdrawn'),('c','pub_b','Other','published');
          INSERT INTO nest_jobs (account_id,listing_id,created_at,execution) VALUES('one','a',100,'succeeded'),('one','b',101,'failed'),('two','b',102,'running'),('other','c',100,'succeeded'),('one','a',200,'succeeded');
          INSERT INTO nest_hosted_jobs VALUES('one','a',100,'succeeded'),('two','b',103,'cancelled'),('three','a',99,'succeeded');
          INSERT INTO nest_installations VALUES('a',50,'active'),('a',100,'uninstalled'),('b',101,'active'),('c',100,'active');").unwrap();
        let r = Range {
            from: 100,
            to: 200,
            listing: None,
        };
        let v = usage_in(&c, "pub_a", &r).unwrap();
        assert_eq!(v["usage"]["jobs"], 5);
        assert_eq!(v["usage"]["active_users"], 2);
        assert_eq!(v["usage"]["succeeded"], 2);
        assert_eq!(v["usage"]["failed"], 1);
        assert_eq!(v["usage"]["in_progress"], 1);
        assert_eq!(v["usage"]["cancelled"], 1);
        assert_eq!(v["usage"]["new_installations"], 2);
        assert_eq!(v["usage"]["active_installations"], 2);
        assert_eq!(v["apps"].as_array().unwrap().len(), 2);
        assert_eq!(v["daily"][0]["jobs"], 5);
        let one = usage_in(
            &c,
            "pub_a",
            &Range {
                listing: Some("a".into()),
                ..r.clone()
            },
        )
        .unwrap();
        assert_eq!(one["usage"]["jobs"], 2);
        assert_eq!(one["apps"].as_array().unwrap().len(), 1);
        assert!(usage_in(
            &c,
            "pub_a",
            &Range {
                listing: Some("c".into()),
                ..r
            }
        )
        .is_err());
        assert!(!v.to_string().contains("account_id"));
    }
    #[test]
    fn ranges_are_bounded_and_half_open() {
        assert!(ReportQuery {
            from: Some("2026-01-01T00:00:00Z".into()),
            to: Some("2026-01-01T00:00:00Z".into()),
            listing_id: None
        }
        .range()
        .is_err());
        assert!(ReportQuery {
            from: Some("2020-01-01T00:00:00Z".into()),
            to: Some("2026-01-01T00:00:00Z".into()),
            listing_id: None
        }
        .range()
        .is_err());
        assert!(ReportQuery {
            listing_id: Some("../other".into()),
            ..Default::default()
        }
        .range()
        .is_err());
        assert!(ReportQuery::default().range().is_ok());
    }
    #[tokio::test]
    async fn finance_projection_rejects_wrong_publisher_and_strips_private_fields() {
        use crate::backoffice_finance::Finance;
        use axum::extract::Path;
        let app=Router::new().route("/finance/publishers/{id}/analytics",get(|Path(id):Path<String>|async move {
            Json(json!({"publisher_id":if id=="wrong" {"someone_else"} else {&id},
              "basis":"accrued_sales_current_reversals", "from":"2026-01-01T00:00:00Z", "to":"2026-02-01T00:00:00Z",
              "currencies":[{"currency":"USD","sales":1,"gross_minor":100,"tax_minor":0,"commission_minor":10,
                "publisher_minor":90,"reversed_publisher_minor":0,"net_publisher_minor":90,"buyer_email":"private@example.test"}],
              "apps":[],"daily":[],"publisher_balances":[],"bank_account":"private"}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let f = Finance::http(&url, "synthetic_012345678901234567890123456789");
        let q = vec![
            ("from", "2026-01-01T00:00:00Z".into()),
            ("to", "2026-02-01T00:00:00Z".into()),
        ];
        let v = f.publisher_analytics("pub_a", &q).await.unwrap();
        assert_eq!(v["currencies"][0]["net_publisher_minor"], 90);
        assert!(!v.to_string().contains("private"));
        assert!(f.publisher_analytics("wrong", &q).await.is_err());
        assert!(f.publisher_analytics("../other", &q).await.is_err());
    }

    #[tokio::test]
    async fn routes_isolate_publishers_and_audit_staff_even_when_finance_is_off() {
        use crate::marketplace_m2_tests::{apply, reviewer, setup};
        let (srv, owner) = setup().await;
        srv.global_flag("marketplace").await;
        let publisher = srv.person("analytics_publisher").await;
        let other = srv.person("analytics_other").await;
        let id = apply(
            &srv,
            &publisher,
            json!({"display_name":"Synthetic analytics"}),
        )
        .await;
        let path = format!("/v1/publishers/{id}/analytics");
        let (status, body) = srv.get(&path, &publisher.token).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["data"]["usage"]["jobs"], 0);
        assert_eq!(body["data"]["finance"]["status"], "unavailable");
        assert_eq!(srv.get(&path, &other.token).await.0, 404);
        let sp = format!("/v1/staff/publishers/{id}/analytics");
        assert_eq!(srv.get(&sp, &publisher.strong).await.0, 403);
        let rev = reviewer(&srv, &owner, "analytics_review").await;
        assert_eq!(srv.get(&sp, &rev.strong).await.0, 403);
        assert_eq!(srv.get(&sp, &owner.token).await.0, 403);
        assert_eq!(srv.get(&sp, &owner.strong).await.0, 403);
        let (status, _) = srv
            .post(
                "/v1/staff/roles",
                &owner.strong,
                json!({"account_id":owner.account,"role":"finance"}),
            )
            .await;
        assert_eq!(status, 201);
        let (s, v) = srv.get(&sp, &owner.strong).await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["data"]["usage"], body["data"]["usage"]);
        assert_eq!(
            srv.get(&format!("{path}?listing_id=foreign_app"), &publisher.token)
                .await
                .0,
            404
        );
        assert_eq!(
            srv.get(&format!("{path}?from=invalid"), &publisher.token)
                .await
                .0,
            400
        );
        let (_, list) = srv
            .get("/v1/staff/publisher-analytics?q=Synthetic", &owner.strong)
            .await;
        assert_eq!(list["data"][0]["id"], id);
        let audit = srv
            .state
            .store
            .audit_for_actor(Some(owner.account.clone()), 10)
            .await
            .unwrap();
        assert!(audit
            .iter()
            .any(|r| r.action == "staff.publishers.analytics"));
    }
}
