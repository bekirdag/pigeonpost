//! Publisher-team support conversations. HTTP persists first; WebSocket events invalidate views.
//! Staff reads and writes use explicit support actions and audited, fresh human sessions.
use crate::backoffice::{audit_row, require_staff};
use crate::groups::{rand_b32, GErr, GResult};
use crate::marketplace::{into_response, iso, not_found};
use crate::marketplace_m2::{gate, Need};
use crate::policy::Action;
use crate::{now_unix, AppState, ClientIp};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tokio::sync::{broadcast, Semaphore};

pub(crate) fn migrate(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("
CREATE TABLE IF NOT EXISTS partner_support_tickets (
 id TEXT PRIMARY KEY,publisher_id TEXT NOT NULL REFERENCES publisher_profiles(id),subject TEXT NOT NULL,
 category TEXT NOT NULL,priority TEXT NOT NULL DEFAULT 'normal',status TEXT NOT NULL DEFAULT 'open',
 assigned_to TEXT,created_by TEXT NOT NULL,client_id TEXT NOT NULL,created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,last_seq INTEGER NOT NULL DEFAULT 0, UNIQUE(created_by,client_id));
CREATE INDEX IF NOT EXISTS partner_support_queue ON partner_support_tickets(publisher_id,updated_at);
CREATE TABLE IF NOT EXISTS partner_support_messages (
 seq INTEGER PRIMARY KEY AUTOINCREMENT,ticket_id TEXT NOT NULL REFERENCES partner_support_tickets(id),
 author_id TEXT NOT NULL,author_kind TEXT NOT NULL,body TEXT NOT NULL,client_id TEXT NOT NULL,created_at INTEGER NOT NULL,
 UNIQUE(ticket_id,author_id,client_id));
CREATE INDEX IF NOT EXISTS partner_support_history ON partner_support_messages(ticket_id,seq);
CREATE TABLE IF NOT EXISTS partner_support_reads (
 ticket_id TEXT NOT NULL REFERENCES partner_support_tickets(id),account_id TEXT NOT NULL,last_seq INTEGER NOT NULL,
 PRIMARY KEY(ticket_id,account_id));")
}

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/support/tickets",
            get(support_list).post(support_create),
        )
        .route(
            "/v1/support/tickets/{ticket}",
            get(support_detail).patch(support_update),
        )
        .route(
            "/v1/support/tickets/{ticket}/messages",
            axum::routing::post(support_reply),
        )
        .route(
            "/v1/support/tickets/{ticket}/read",
            axum::routing::post(support_mark_read),
        )
        .route("/v1/support/live", get(support_upgrade))
}
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    publisher: Option<String>,
    #[serde(default)]
    staff: bool,
    q: Option<String>,
    status: Option<String>,
    assigned: Option<String>,
    offset: Option<u32>,
    before: Option<i64>,
}
impl Scope {
    fn validate(&self) -> GResult<()> {
        if self.publisher.as_ref().is_some_and(|s| s.len() > 80)
            || self.q.as_ref().is_some_and(|s| s.len() > 120)
            || self.offset.unwrap_or(0) > 100000
            || self.before.is_some_and(|s| s < 0)
        {
            return Err(bad("Invalid search"));
        }
        if self
            .status
            .as_deref()
            .is_some_and(|s| !matches!(s, "all" | "open" | "waiting_partner" | "resolved"))
            || self
                .assigned
                .as_deref()
                .is_some_and(|s| !matches!(s, "all" | "mine" | "unassigned"))
        {
            return Err(bad("Invalid filter"));
        }
        Ok(())
    }
}
fn bad(s: &str) -> GErr {
    GErr::bad("invalid_request", s)
}
fn private(r: GResult<Json<Value>>) -> Response {
    let mut r = into_response(r);
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}
async fn support_access(s: &AppState, h: &HeaderMap, q: &Scope, write: bool) -> GResult<String> {
    q.validate()?;
    if q.staff {
        Ok(require_staff(
            s,
            h,
            if write {
                Action::StaffSupportWrite
            } else {
                Action::StaffSupportRead
            },
            "partner_support",
        )
        .await?
        .account)
    } else {
        let p = q
            .publisher
            .as_deref()
            .ok_or_else(|| bad("Choose a business"))?;
        Ok(gate(s, h, p, Action::PublisherManage, Need::Read)
            .await?
            .account)
    }
}
async fn support_audit(
    s: &AppState,
    h: &HeaderMap,
    q: &Scope,
    ip: &Option<Extension<ClientIp>>,
    verb: &str,
    id: Option<String>,
) -> GResult<()> {
    if q.staff {
        let who = require_staff(s, h, Action::StaffSupportRead, "partner_support").await?;
        s.store
            .append_audit(audit_row(s, &who, verb, id, json!({}), ip, h))
            .await?;
    }
    Ok(())
}
fn ticket_in(c: &Connection, id: &str, q: &Scope, account: &str) -> GResult<Value> {
    let v=c.query_row("SELECT t.id,t.publisher_id,t.subject,t.category,t.priority,t.status,t.assigned_to,t.created_at,t.updated_at,t.last_seq,p.display_name,p.status,COALESCE(r.last_seq,0) FROM partner_support_tickets t JOIN publisher_profiles p ON p.id=t.publisher_id LEFT JOIN partner_support_reads r ON r.ticket_id=t.id AND r.account_id=?2 WHERE t.id=?1 AND (?3 IS NULL OR t.publisher_id=?3)",params![id,account,q.publisher],|r|Ok(json!({"id":r.get::<_,String>(0)?,"publisher_id":r.get::<_,String>(1)?,"subject":r.get::<_,String>(2)?,"category":r.get::<_,String>(3)?,"priority":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,"assigned_to":r.get::<_,Option<String>>(6)?,"created_at":iso(r.get(7)?),"updated_at":iso(r.get(8)?),"last_seq":r.get::<_,i64>(9)?,"publisher_name":r.get::<_,String>(10)?,"publisher_status":r.get::<_,String>(11)?,"unread":r.get::<_,i64>(9)?>r.get::<_,i64>(12)?}))).optional()?.ok_or_else(not_found)?;
    Ok(v)
}
fn messages(c: &Connection, id: &str, before: Option<i64>) -> GResult<Vec<Value>> {
    let mut st=c.prepare("SELECT seq,author_kind,body,created_at,client_id FROM partner_support_messages WHERE ticket_id=?1 AND (?2 IS NULL OR seq<?2) ORDER BY seq DESC LIMIT 101")?;
    let rows=st.query_map(params![id,before],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"author_kind":r.get::<_,String>(1)?,"body":r.get::<_,String>(2)?,"created_at":iso(r.get(3)?),"client_id":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
async fn support_list(
    State(s): State<AppState>,
    h: HeaderMap,
    ip: Option<Extension<ClientIp>>,
    Query(q): Query<Scope>,
) -> Response {
    private(async {
 let a=support_access(&s,&h,&q,false).await?;support_audit(&s,&h,&q,&ip,"staff.support.list",None).await?;
 let v=s.store.group_read(move|c|->GResult<Value>{
 let mut st=c.prepare("SELECT id FROM partner_support_tickets WHERE (?1 IS NULL OR publisher_id=?1) AND (?2='all' OR status=?2) AND (instr(lower(subject),lower(?3))>0 OR publisher_id IN(SELECT id FROM publisher_profiles WHERE instr(lower(display_name),lower(?3))>0)) AND (?4='all' OR (?4='mine' AND assigned_to=?5) OR (?4='unassigned' AND assigned_to IS NULL)) ORDER BY updated_at DESC,id LIMIT 51 OFFSET ?6")?;
 let ids=st.query_map(params![q.publisher,q.status.as_deref().unwrap_or("all"),q.q.as_deref().unwrap_or(""),q.assigned.as_deref().unwrap_or("all"),a,q.offset.unwrap_or(0)],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
 let rows=ids.iter().take(50).map(|id|ticket_in(c,id,&q,&a)).collect::<GResult<Vec<_>>>()?;
 Ok(json!({"data":rows,"next_offset":(ids.len()>50).then_some(q.offset.unwrap_or(0)+50)}))}).await?;Ok(Json(v))}.await)
}
async fn support_detail(
    State(s): State<AppState>,
    h: HeaderMap,
    ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Query(q): Query<Scope>,
) -> Response {
    private(
        async {
            let a = support_access(&s, &h, &q, false).await?;
            support_audit(&s, &h, &q, &ip, "staff.support.read", Some(id.clone())).await?;
            let v = s
                .store
                .group_read(move |c| -> GResult<Value> {
                    let tx = c.unchecked_transaction()?;
                    let mut t = ticket_in(&tx, &id, &q, &a)?;
                    let mut m = messages(&tx, &id, q.before)?;
                    let more = m.len() > 100;
                    m.truncate(100);
                    m.reverse();
                    t["older_before"] = if more {
                        m[0]["seq"].clone()
                    } else {
                        Value::Null
                    };
                    t["messages"] = json!(m);
                    Ok(t)
                })
                .await?;
            Ok(Json(json!({"data":v})))
        }
        .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTicket {
    subject: String,
    body: String,
    category: String,
    client_id: String,
}
fn valid_text(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max && !s.contains('\0')
}
fn valid_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn insert_message(
    c: &Connection,
    id: &str,
    account: &str,
    staff: bool,
    body: &str,
    key: &str,
) -> GResult<()> {
    if !valid_text(body, 8000) || !valid_key(key) {
        return Err(bad("Message must contain 1–8000 characters"));
    }
    let count: i64 = c.query_row(
        "SELECT COUNT(*) FROM partner_support_messages WHERE author_id=?1 AND created_at>?2",
        params![account, now_unix() as i64 - 60],
        |r| r.get(0),
    )?;
    if count >= 30 {
        return Err(GErr::bad(
            "rate_limited",
            "Please wait a minute before sending more messages",
        ));
    }
    let prior:Option<String>=c.query_row("SELECT body FROM partner_support_messages WHERE ticket_id=?1 AND author_id=?2 AND client_id=?3",params![id,account,key],|r|r.get(0)).optional()?;
    if let Some(p) = prior {
        return if p == body.trim() {
            Ok(())
        } else {
            Err(bad("Retry identifier already used for another message"))
        };
    }
    c.execute("INSERT INTO partner_support_messages(ticket_id,author_id,author_kind,body,client_id,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,account,if staff{"staff"}else{"partner"},body.trim(),key,now_unix() as i64])?;
    let seq = c.last_insert_rowid();
    c.execute(
        "UPDATE partner_support_tickets SET last_seq=?2,updated_at=?3,status=?4 WHERE id=?1",
        params![
            id,
            seq,
            now_unix() as i64,
            if staff { "waiting_partner" } else { "open" }
        ],
    )?;
    c.execute("INSERT INTO partner_support_reads VALUES(?1,?2,?3) ON CONFLICT(ticket_id,account_id) DO UPDATE SET last_seq=MAX(last_seq,excluded.last_seq)",params![id,account,seq])?;
    Ok(())
}
async fn support_create(
    State(s): State<AppState>,
    h: HeaderMap,
    ip: Option<Extension<ClientIp>>,
    Query(q): Query<Scope>,
    Json(b): Json<NewTicket>,
) -> Response {
    private(async{
 let a=support_access(&s,&h,&q,true).await?;let p=q.publisher.clone().ok_or_else(||bad("Choose a business"))?;
 if !valid_text(&b.subject,140)||!valid_text(&b.body,8000)||!valid_key(&b.client_id)||!matches!(b.category.as_str(),"application"|"apps"|"billing"|"technical"|"other"){return Err(bad("Check the subject, category and message"));}
 support_audit(&s,&h,&q,&ip,"staff.support.create",None).await?;
 let id=s.store.group_tx(move|c|->GResult<String>{
 let prior:Option<(String,String)>=c.query_row("SELECT id,publisher_id FROM partner_support_tickets WHERE created_by=?1 AND client_id=?2",params![a,b.client_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
 if let Some((id,pub_id))=prior {if pub_id!=p{return Err(bad("Retry identifier belongs to another business"));}return Ok(id);}
 let n:i64=c.query_row("SELECT COUNT(*) FROM partner_support_tickets WHERE created_by=?1 AND created_at>?2",params![a,now_unix() as i64-3600],|r|r.get(0))?;if n>=10{return Err(bad("Please wait before opening another conversation"));}
 let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM publisher_profiles WHERE id=?1)",[&p],|r|r.get(0))?;if !exists{return Err(not_found());}
 let id=format!("sup_{}",rand_b32(16));c.execute("INSERT INTO partner_support_tickets(id,publisher_id,subject,category,created_by,client_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",params![id,p,b.subject.trim(),b.category,a,b.client_id,now_unix() as i64])?;insert_message(c,&id,&a,q.staff,&b.body,&b.client_id)?;Ok(id)}).await?;
 support_notify(&s,&id).await;Ok(Json(json!({"data":{"id":id}})))}.await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    body: String,
    client_id: String,
}
async fn support_reply(
    State(s): State<AppState>,
    h: HeaderMap,
    ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Query(q): Query<Scope>,
    Json(b): Json<Reply>,
) -> Response {
    private(
        async {
            let a = support_access(&s, &h, &q, true).await?;
            support_audit(&s, &h, &q, &ip, "staff.support.reply", Some(id.clone())).await?;
            let key = id.clone();
            s.store
                .group_tx(move |c| -> GResult<()> {
                    ticket_in(c, &key, &q, &a)?;
                    insert_message(c, &key, &a, q.staff, &b.body, &b.client_id)
                })
                .await?;
            support_notify(&s, &id).await;
            Ok(Json(json!({"data":{"id":id}})))
        }
        .await,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    status: Option<String>,
    priority: Option<String>,
    assignment: Option<String>,
}
async fn support_update(
    State(s): State<AppState>,
    h: HeaderMap,
    ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Query(q): Query<Scope>,
    Json(b): Json<Update>,
) -> Response {
    private(async{
 let a=support_access(&s,&h,&q,true).await?;
 if b.status.as_deref().is_some_and(|v|!matches!(v,"open"|"waiting_partner"|"resolved"))||b.priority.as_deref().is_some_and(|v|!matches!(v,"normal"|"high"|"urgent"))||b.assignment.as_deref().is_some_and(|v|!matches!(v,"mine"|"unassigned")){return Err(bad("Invalid ticket update"));}
 if !q.staff&&(b.priority.is_some()||b.assignment.is_some()||b.status.as_deref()==Some("waiting_partner")){return Err(GErr::forbidden("staff_required","Only support staff can assign and prioritize conversations"));}
 support_audit(&s,&h,&q,&ip,"staff.support.update",Some(id.clone())).await?;let key=id.clone();
 s.store.group_tx(move|c|->GResult<()>{ticket_in(c,&key,&q,&a)?;c.execute("UPDATE partner_support_tickets SET status=COALESCE(?2,status),priority=COALESCE(?3,priority),assigned_to=CASE WHEN ?4='mine' THEN ?5 WHEN ?4='unassigned' THEN NULL ELSE assigned_to END,updated_at=?6 WHERE id=?1",params![key,b.status,b.priority,b.assignment,a,now_unix() as i64])?;Ok(())}).await?;support_notify(&s,&id).await;Ok(Json(json!({"data":{"id":id}})))}.await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    last_seq: i64,
}
async fn support_mark_read(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<Scope>,
    Json(b): Json<Read>,
) -> Response {
    private(async{
 let a=support_access(&s,&h,&q,false).await?;s.store.group_tx(move|c|->GResult<()>{let t=ticket_in(c,&id,&q,&a)?;let seq=b.last_seq.clamp(0,t["last_seq"].as_i64().unwrap_or(0));c.execute("INSERT INTO partner_support_reads VALUES(?1,?2,?3) ON CONFLICT(ticket_id,account_id) DO UPDATE SET last_seq=MAX(last_seq,excluded.last_seq)",params![id,a,seq])?;Ok(())}).await?;Ok(Json(json!({"data":{"read":true}})))}.await)
}
#[derive(Clone)]
struct Event {
    publisher: String,
    ticket: String,
}
fn events() -> &'static broadcast::Sender<Event> {
    static B: OnceLock<broadcast::Sender<Event>> = OnceLock::new();
    B.get_or_init(|| broadcast::channel(256).0)
}
async fn support_notify(s: &AppState, id: &str) {
    let key = id.to_string();
    let p = s
        .store
        .group_read(move |c| -> GResult<String> {
            Ok(c.query_row(
                "SELECT publisher_id FROM partner_support_tickets WHERE id=?1",
                [key],
                |r| r.get(0),
            )?)
        })
        .await;
    if let Ok(p) = p {
        let _ = events().send(Event {
            publisher: p,
            ticket: id.to_string(),
        });
    }
}
async fn support_upgrade(
    State(s): State<AppState>,
    h: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !h
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| crate::CORS_ORIGINS.contains(&v))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    static CAP: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let Ok(permit) = CAP
        .get_or_init(|| Arc::new(Semaphore::new(512)))
        .clone()
        .try_acquire_owned()
    else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    ws.max_message_size(16384)
        .max_frame_size(16384)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            support_live(s, socket).await
        })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    token: String,
    publisher: Option<String>,
    #[serde(default)]
    staff: bool,
}
async fn support_live(s: AppState, mut socket: WebSocket) {
    let Ok(Some(Ok(Message::Text(raw)))) =
        tokio::time::timeout(std::time::Duration::from_secs(5), socket.recv()).await
    else {
        return;
    };
    let Ok(hello) = serde_json::from_str::<Hello>(&raw) else {
        return;
    };
    let mut h = HeaderMap::new();
    let Ok(token) = format!("Bearer {}", hello.token).parse() else {
        return;
    };
    h.insert(header::AUTHORIZATION, token);
    let q = Scope {
        publisher: hello.publisher,
        staff: hello.staff,
        ..Scope::default()
    };
    let mut rx = events().subscribe();
    if support_access(&s, &h, &q, false).await.is_err() {
        let _ = socket
            .send(Message::Text(
                json!({"type":"auth_required"}).to_string().into(),
            ))
            .await;
        return;
    }
    if socket
        .send(Message::Text(json!({"type":"ready"}).to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
    loop {
        let payload = tokio::select! {
         message=socket.recv()=>{match message{Some(Ok(Message::Ping(p)))=>{if socket.send(Message::Pong(p)).await.is_err(){break;}continue;},Some(Ok(Message::Pong(_)))=>continue,_=>break}},
         _=heartbeat.tick()=>json!({"type":"heartbeat"}),
         event=rx.recv()=>match event {Ok(e)=>{if q.publisher.as_ref().is_some_and(|p|p!=&e.publisher){continue;}json!({"type":"support.changed","ticket_id":e.ticket})},Err(broadcast::error::RecvError::Lagged(_))=>json!({"type":"resync"}),Err(_)=>break}
        };
        if support_access(&s, &h, &q, false).await.is_err() {
            let _ = socket
                .send(Message::Text(
                    json!({"type":"auth_required"}).to_string().into(),
                ))
                .await;
            break;
        }
        if !matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                socket.send(Message::Text(payload.to_string().into()))
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace_m2_tests::{apply, reviewer, setup};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message as WsMessage},
    };
    #[tokio::test]
    async fn support_isolates_teams_retries_and_staff_controls() {
        let (srv, owner) = setup().await;
        srv.global_flag("marketplace").await;
        let p = srv.person("support_partner").await;
        let other = srv.person("support_other").await;
        let pid = apply(&srv, &p, json!({"display_name":"Support Partner"})).await;
        let otherpid = apply(&srv, &other, json!({"display_name":"Other Partner"})).await;
        let base = format!("/v1/support/tickets?publisher={pid}");
        let body = json!({"subject":"Application question","body":"Please explain the document request.","category":"application","client_id":"create-one"});
        let (code, result) = srv.post(&base, &p.token, body.clone()).await;
        assert_eq!(code, 200, "{result}");
        let id = result["data"]["id"].as_str().unwrap();
        assert_eq!(srv.post(&base, &p.token, body).await.1, result);
        assert_eq!(srv.get(&base, &other.token).await.0, 404);
        assert_eq!(
            srv.get(
                &format!("/v1/support/tickets/{id}?publisher={otherpid}"),
                &other.token
            )
            .await
            .0,
            404
        );
        assert_eq!(
            srv.get("/v1/support/tickets?staff=true", &p.strong).await.0,
            403
        );
        let rev = reviewer(&srv, &owner, "support_reviewer").await;
        assert_eq!(
            srv.get("/v1/support/tickets?staff=true", &rev.strong)
                .await
                .0,
            403
        );
        assert_eq!(
            srv.get("/v1/support/tickets?staff=true", &owner.token)
                .await
                .0,
            401
        );
        let (code, list) = srv
            .get("/v1/support/tickets?staff=true", &owner.strong)
            .await;
        assert_eq!(code, 200, "{list}");
        assert_eq!(list["data"][0]["unread"], true);
        let reply = json!({"body":"Please upload your current activity certificate.","client_id":"reply-one"});
        let route = format!("/v1/support/tickets/{id}/messages?staff=true");
        assert_eq!(srv.post(&route, &owner.strong, reply.clone()).await.0, 200);
        assert_eq!(srv.post(&route, &owner.strong, reply).await.0, 200);
        let route = format!("/v1/support/tickets/{id}?publisher={pid}");
        let (_, v) = srv.get(&route, &p.token).await;
        assert_eq!(v["data"]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(v["data"]["status"], "waiting_partner");
        assert_eq!(v["data"]["unread"], true);
        let code = srv
            .http
            .patch(format!("{}{route}", srv.public))
            .bearer_auth(&p.token)
            .json(&json!({"priority":"urgent"}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(code, 403);
        assert_eq!(
            srv.post(
                &format!("/v1/support/tickets/{id}/read?publisher={pid}"),
                &p.token,
                json!({"last_seq":v["data"]["last_seq"]})
            )
            .await
            .0,
            200
        );
        assert_eq!(srv.get(&route, &p.token).await.1["data"]["unread"], false);
        let (code, v) = srv
            .post(
                &format!("/v1/support/tickets/{id}/messages?publisher={pid}"),
                &p.token,
                json!({"body":"Thanks, uploaded.","client_id":"reply-two"}),
            )
            .await;
        assert_eq!(code, 200, "{v}");
        assert_eq!(srv.get(&route, &p.token).await.1["data"]["status"], "open");
    }
    #[tokio::test]
    async fn websocket_delivers_committed_updates_only_to_authorized_scope() {
        let (srv, owner) = setup().await;
        srv.global_flag("marketplace").await;
        let p = srv.person("socket_partner").await;
        let other = srv.person("socket_other").await;
        let pid = apply(&srv, &p, json!({"display_name":"Socket Partner"})).await;
        let op = apply(&srv, &other, json!({"display_name":"Socket Other"})).await;
        let url = srv.public.replace("http:", "ws:") + "/v1/support/live";
        let mut bad = url.clone().into_client_request().unwrap();
        bad.headers_mut()
            .insert("origin", "https://evil.example".parse().unwrap());
        assert!(connect_async(bad).await.is_err());
        let mut req = url.into_client_request().unwrap();
        req.headers_mut()
            .insert("origin", "https://pigeonpost.dev".parse().unwrap());
        let (mut socket, _) = connect_async(req).await.unwrap();
        socket
            .send(WsMessage::Text(
                json!({"token":p.token,"publisher":pid}).to_string().into(),
            ))
            .await
            .unwrap();
        let first = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(first.contains("ready"));
        let(_,v)=srv.post(&format!("/v1/support/tickets?publisher={pid}"),&p.token,json!({"subject":"Live help","body":"A question","category":"technical","client_id":"live-one"})).await;
        let id = v["data"]["id"].as_str().unwrap();
        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let x = socket.next().await.unwrap().unwrap().into_text().unwrap();
                if x.contains("support.changed") {
                    break x;
                }
            }
        })
        .await
        .unwrap();
        assert!(msg.contains(id));
        let (_,v)=srv.post(&format!("/v1/support/tickets?publisher={op}"),&other.token,json!({"subject":"Private question","body":"Other business","category":"apps","client_id":"other-one"})).await;
        let foreign = v["data"]["id"].as_str().unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                loop {
                    let x = socket.next().await.unwrap().unwrap().into_text().unwrap();
                    assert!(!x.contains(foreign));
                    if x.contains("support.changed") {
                        break;
                    }
                }
            })
            .await
            .is_err()
        );
        assert_eq!(
            srv.post(
                &format!("/v1/support/tickets/{id}/messages?staff=true"),
                &owner.strong,
                json!({"body":"Live answer","client_id":"staff-live"})
            )
            .await
            .0,
            200
        );
        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let x = socket.next().await.unwrap().unwrap().into_text().unwrap();
                if x.contains("support.changed") {
                    break x;
                }
            }
        })
        .await
        .unwrap();
        assert!(msg.contains(id));
        assert_eq!(
            srv.get(
                &format!("/v1/support/tickets/{id}?publisher={pid}"),
                &p.token
            )
            .await
            .1["data"]["messages"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn pagination_and_validation_are_bounded() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE publisher_profiles(id TEXT PRIMARY KEY,display_name TEXT,status TEXT);",
        )
        .unwrap();
        migrate(&c).unwrap();
        c.execute(
            "INSERT INTO publisher_profiles VALUES('p','Example','applied')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO partner_support_tickets(id,publisher_id,subject,category,created_by,client_id,created_at,updated_at) VALUES('t','p','Question','apps','a','x',0,0)",[]).unwrap();
        for n in 0..205 {
            c.execute("INSERT INTO partner_support_messages(ticket_id,author_id,author_kind,body,client_id,created_at)VALUES('t','a','partner','body',?1,0)",[n.to_string()]).unwrap();
        }
        let page = messages(&c, "t", None).unwrap();
        assert_eq!(page.len(), 101);
        assert_eq!(page[0]["seq"], 205);
        let next = messages(&c, "t", Some(106)).unwrap();
        assert_eq!(next[0]["seq"], 105);
        assert!(!valid_text("  ", 8000));
        assert!(!valid_key("bad/key"));
        assert!(Scope {
            status: Some("invented".into()),
            ..Scope::default()
        }
        .validate()
        .is_err());
    }
}
