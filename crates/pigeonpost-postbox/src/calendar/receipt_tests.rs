// Included in the existing real-socket calendar suite so it uses the same member/worker fixtures.

type RawCreationReceipt = (String, Vec<u8>, String, i64, i64);

#[tokio::test]
async fn creation_receipts_require_persistent_key_for_legacy_migration() {
    let srv = serve().await;
    let who = person(&srv, "receiptdurability").await;
    let scope = who.address.clone();
    srv.state
        .store
        .group_tx(
            move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
                tx.execute(
                    "INSERT INTO calendar_idempotency
                     (scope,action,request_key,body_hash,status,response,created_at)
                     VALUES (?1,'create_event','durable_key','synthetic_hash',201,?2,?3)",
                    rusqlite::params![scope, r#"{"event":{"title":"private"}}"#, now_unix() as i64],
                )?;
                Ok(())
            },
        )
        .await
        .unwrap();
    let before = creation_receipt(&srv, &who, "create_event", "durable_key").await;
    assert_eq!(before.0, "text");
    assert!(super::protect_legacy_receipts(&srv.state, false)
        .await
        .is_err());
    assert_eq!(
        creation_receipt(&srv, &who, "create_event", "durable_key").await,
        before
    );
    assert_eq!(
        super::protect_legacy_receipts(&srv.state, true)
            .await
            .unwrap(),
        1
    );
    let after = creation_receipt(&srv, &who, "create_event", "durable_key").await;
    assert_eq!(after.0, "blob");
    assert_ne!(after.1, before.1);
    assert_eq!((after.2, after.3, after.4), (before.2, before.3, before.4));
    assert_eq!(
        super::protect_legacy_receipts(&srv.state, false)
            .await
            .unwrap(),
        0
    );
}

async fn creation_receipt(srv: &Srv, owner: &P, action: &str, key: &str) -> RawCreationReceipt {
    let scope = owner.address.clone();
    let action = action.to_owned();
    let key = key.to_owned();
    srv.state
        .store
        .group_read(
            move |c: &rusqlite::Connection| -> crate::groups::GResult<_> {
                Ok(c.query_row(
                    "SELECT typeof(response), CAST(response AS BLOB), body_hash, status, created_at
               FROM calendar_idempotency WHERE scope = ?1 AND action = ?2 AND request_key = ?3",
                    rusqlite::params![scope, action, key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )?)
            },
        )
        .await
        .unwrap()
}

async fn creation_counts(srv: &Srv, owner: &P) -> (i64, i64, i64) {
    let mailbox = owner.address.clone();
    let account = owner.account.clone();
    srv.state
        .store
        .group_read(
            move |c: &rusqlite::Connection| -> crate::groups::GResult<_> {
                Ok((
                    c.query_row(
                        "SELECT COUNT(*) FROM events WHERE organizer = ?1",
                        [&mailbox],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT COUNT(*) FROM event_reminders WHERE owner_identity = ?1",
                        [&mailbox],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT COUNT(*) FROM account_events WHERE account_id = ?1",
                        [&account],
                        |r| r.get(0),
                    )?,
                ))
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn creation_receipts_are_sealed_and_replay_without_effects() {
    let srv = serve().await;
    let who = person(&srv, "sealedreplies").await;
    let folder =
        json!({"request_key":"sealed_folder", "name":"PRIVATE_FOLDER_RECEIPT", "color":"blue"});
    let (status, first_folder) = srv
        .call(Method::POST, "/v1/calendars", &who, Some(folder.clone()))
        .await;
    assert_eq!(status, 201, "{first_folder}");
    let mut body = meeting_body(json!([]));
    body["request_key"] = json!("sealed_event");
    body["title"] = json!("PRIVATE_EVENT_RECEIPT");
    body["description"] = json!("PRIVATE_DESCRIPTION_RECEIPT");
    body["reminders"] = json!([{"minutes_before":5,"channel":"push"}]);
    let (status, first_event) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &who,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, 201, "{first_event}");
    let before = creation_counts(&srv, &who).await;
    assert_eq!((before.0, before.1), (1, 1));
    for _ in 0..2 {
        assert_eq!(
            srv.call(Method::POST, "/v1/calendars", &who, Some(folder.clone()))
                .await,
            (201, first_folder.clone())
        );
        assert_eq!(
            srv.call(
                Method::POST,
                "/v1/calendar/events",
                &who,
                Some(body.clone())
            )
            .await,
            (201, first_event.clone())
        );
    }
    assert_eq!(creation_counts(&srv, &who).await, before);
    for (action, key, marker) in [
        (
            "create_calendar",
            "sealed_folder",
            b"PRIVATE_FOLDER_RECEIPT".as_slice(),
        ),
        (
            "create_event",
            "sealed_event",
            b"PRIVATE_EVENT_RECEIPT".as_slice(),
        ),
    ] {
        let saved = creation_receipt(&srv, &who, action, key).await;
        assert_eq!(saved.0, "blob");
        assert!(!saved.1.windows(marker.len()).any(|bytes| bytes == marker));
        assert!(serde_json::from_slice::<Value>(&saved.1).is_err());
        assert_eq!(saved.3, 201);
    }
}

#[tokio::test]
async fn creation_receipts_reseal_legacy_data_and_migrate_in_bounded_batches() {
    let srv = serve().await;
    let who = person(&srv, "legacyreplies").await;
    let folder =
        json!({"request_key":"legacy_folder", "name":"Private old folder", "color":"blue"});
    let (_, original_folder) = srv
        .call(Method::POST, "/v1/calendars", &who, Some(folder.clone()))
        .await;
    let mut body = meeting_body(json!([]));
    body["request_key"] = json!("legacy_event");
    let (_, original_event) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &who,
            Some(body.clone()),
        )
        .await;
    let event_meta = creation_receipt(&srv, &who, "create_event", "legacy_event").await;
    let folder_meta = creation_receipt(&srv, &who, "create_calendar", "legacy_folder").await;
    let before = creation_counts(&srv, &who).await;
    let scope = who.address.clone();
    let event_json = original_event.to_string();
    let folder_json = original_folder.to_string();
    srv.state.store.group_tx(move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
        for (action, key, response) in [("create_event", "legacy_event", event_json), ("create_calendar", "legacy_folder", folder_json)] {
            tx.execute("UPDATE calendar_idempotency SET response = ?1 WHERE scope = ?2 AND action = ?3 AND request_key = ?4",
                       rusqlite::params![response, scope, action, key])?;
        }
        Ok(())
    }).await.unwrap();
    assert_eq!(
        srv.call(Method::POST, "/v1/calendar/events", &who, Some(body))
            .await,
        (201, original_event)
    );
    let resealed = creation_receipt(&srv, &who, "create_event", "legacy_event").await;
    assert_eq!(resealed.0, "blob");
    assert_eq!(
        (&resealed.2, resealed.3, resealed.4),
        (&event_meta.2, event_meta.3, event_meta.4)
    );
    let scope = who.address.clone();
    srv.state.store.group_tx(move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
        for index in 0..70 {
            let response = if index == 0 { "{malformed private legacy response".to_string() }
                           else { json!({"event":{"id":format!("evt_legacy_{index}")}}).to_string() };
            tx.execute(
                "INSERT INTO calendar_idempotency VALUES (?1,'create_event',?2,'legacy_hash',201,?3,?4)",
                rusqlite::params![scope, format!("legacy_{index}"), response, 1_i64],
            )?;
        }
        tx.execute("INSERT INTO calendar_idempotency VALUES (?1,'create_calendar','legacy_invalid_utf8','legacy_hash',201,CAST(x'fffe' AS TEXT),1)", [&scope])?;
        tx.execute("INSERT INTO calendar_idempotency VALUES ('evt_note','cancel_note','1','',0,'kept-sealed-note-format',1)", [])?;
        tx.execute("INSERT INTO calendar_idempotency VALUES ('import_scope','import_confirm','draft','hash',200,x'010203',1)", [])?;
        Ok(())
    }).await.unwrap();
    assert_eq!(
        super::protect_legacy_receipts(&srv.state, true)
            .await
            .unwrap(),
        72
    );
    assert_eq!(
        super::protect_legacy_receipts(&srv.state.clone(), true)
            .await
            .unwrap(),
        0
    );
    let remaining = srv.state.store.group_read(|c: &rusqlite::Connection| -> crate::groups::GResult<(i64, String, Vec<u8>)> {
        Ok((
            c.query_row("SELECT COUNT(*) FROM calendar_idempotency WHERE action IN ('create_calendar','create_event') AND typeof(response)='text'", [], |r| r.get(0))?,
            c.query_row("SELECT response FROM calendar_idempotency WHERE action='cancel_note'", [], |r| r.get(0))?,
            c.query_row("SELECT response FROM calendar_idempotency WHERE action='import_confirm'", [], |r| r.get(0))?,
        ))
    }).await.unwrap();
    assert_eq!(
        remaining,
        (0, "kept-sealed-note-format".into(), vec![1, 2, 3])
    );
    let migrated_folder = creation_receipt(&srv, &who, "create_calendar", "legacy_folder").await;
    assert_eq!(migrated_folder.0, "blob");
    assert_eq!(
        (&migrated_folder.2, migrated_folder.3, migrated_folder.4),
        (&folder_meta.2, folder_meta.3, folder_meta.4)
    );
    assert_eq!(
        srv.call(Method::POST, "/v1/calendars", &who, Some(folder))
            .await,
        (201, original_folder)
    );
    assert_eq!(creation_counts(&srv, &who).await, before);
}

#[tokio::test]
async fn creation_receipts_reject_cross_mailbox_copies_and_tampering() {
    let srv = serve().await;
    let first = person(&srv, "receiptfirst").await;
    let other = person(&srv, "receiptother").await;
    let mut body = meeting_body(json!([]));
    body["request_key"] = json!("shared_key");
    for who in [&first, &other] {
        let (status, response) = srv
            .call(Method::POST, "/v1/calendar/events", who, Some(body.clone()))
            .await;
        assert_eq!(status, 201, "{response}");
    }
    let source = creation_receipt(&srv, &first, "create_event", "shared_key").await;
    let target = creation_receipt(&srv, &other, "create_event", "shared_key").await;
    assert_eq!(
        source.2, target.2,
        "identical input has the same hash across actors"
    );
    let before_first = creation_counts(&srv, &first).await;
    let before_other = creation_counts(&srv, &other).await;
    let scope = other.address.clone();
    let blob = source.1.clone();
    let created_at = source.4;
    srv.state.store.group_tx(move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
        tx.execute("UPDATE calendar_idempotency SET response=?1, created_at=?2 WHERE scope=?3 AND action='create_event' AND request_key='shared_key'",
                   rusqlite::params![blob, created_at, scope])?;
        Ok(())
    }).await.unwrap();
    let (status, denied) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &other,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, 503, "{denied}");
    assert_eq!(denied["error"], "calendar_receipt_unavailable");
    assert!(denied.get("event").is_none());
    for kind in ["cipher", "status", "time"] {
        let mut blob = source.1.clone();
        if kind == "cipher" {
            blob[0] ^= 0xff;
        }
        let status = if kind == "status" { 202 } else { source.3 };
        let created = source.4 - i64::from(kind == "time");
        let scope = first.address.clone();
        srv.state.store.group_tx(move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
            tx.execute("UPDATE calendar_idempotency SET response=?1, status=?2, created_at=?3 WHERE scope=?4 AND action='create_event' AND request_key='shared_key'",
                       rusqlite::params![blob, status, created, scope])?;
            Ok(())
        }).await.unwrap();
        let (status, refused) = srv
            .call(
                Method::POST,
                "/v1/calendar/events",
                &first,
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, 503, "{kind}: {refused}");
        assert!(refused.get("event").is_none());
    }
    assert_eq!(creation_counts(&srv, &first).await, before_first);
    assert_eq!(creation_counts(&srv, &other).await, before_other);
}

#[tokio::test]
async fn creation_receipts_seal_malformed_legacy_without_recreating_events() {
    let srv = serve().await;
    let who = person(&srv, "brokenreceipt").await;
    let mut body = meeting_body(json!([]));
    body["request_key"] = json!("broken_key");
    let (status, _) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &who,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, 201);
    let counts = creation_counts(&srv, &who).await;
    let scope = who.address.clone();
    srv.state.store.group_tx(move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
        tx.execute("UPDATE calendar_idempotency SET response='{unparseable private reply' WHERE scope=?1 AND action='create_event'", [scope])?;
        Ok(())
    }).await.unwrap();
    assert_eq!(
        super::protect_legacy_receipts(&srv.state, true)
            .await
            .unwrap(),
        1
    );
    let saved = creation_receipt(&srv, &who, "create_event", "broken_key").await;
    assert_eq!(saved.0, "blob");
    for _ in 0..2 {
        let (status, refused) = srv
            .call(
                Method::POST,
                "/v1/calendar/events",
                &who,
                Some(body.clone()),
            )
            .await;
        assert_eq!(status, 503, "{refused}");
        assert!(refused.get("event").is_none());
    }
    assert_eq!(creation_counts(&srv, &who).await, counts);
}

#[tokio::test]
async fn creation_receipts_preserve_conflicts_and_expiry() {
    let srv = serve().await;
    let who = person(&srv, "receiptexpiry").await;
    let mut body = meeting_body(json!([]));
    body["request_key"] = json!("bounded_key");
    let (status, original) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &who,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, 201);
    let mut different = body.clone();
    different["title"] = json!("Different intent");
    let (status, conflict) = srv
        .call(Method::POST, "/v1/calendar/events", &who, Some(different))
        .await;
    assert_eq!(status, 409, "{conflict}");
    let scope = who.address.clone();
    srv.state.store.group_tx(move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
        tx.execute("UPDATE calendar_idempotency SET created_at=?1 WHERE scope=?2 AND action='create_event'",
                   rusqlite::params![(now_unix()-super::IDEMPOTENCY_SECS-1) as i64, scope])?;
        Ok(())
    }).await.unwrap();
    let (status, fresh) = srv
        .call(Method::POST, "/v1/calendar/events", &who, Some(body))
        .await;
    assert_eq!(status, 201, "{fresh}");
    assert_ne!(fresh["event"]["id"], original["event"]["id"]);
    assert_eq!(creation_counts(&srv, &who).await.0, 2);
}
