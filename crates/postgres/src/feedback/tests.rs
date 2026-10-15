use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};

fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn id(seed: u8) -> EventId {
    EventId::from_bytes([seed; 16])
}
fn feedback(p: i32, seed: u8) -> FeedbackRecord {
    let project_id = project(p);
    let feedback_id = id(seed);
    let attachment_id = BlobObjectId::from_bytes([1; 16]);
    FeedbackRecord {
        project_id,
        feedback_id,
        received_at: at(1000),
        status: FeedbackStatus::Open,
        status_changed_at: at(1000),
        message: "'; DROP TABLE metric.projects; -- 雪".into(),
        name: Some("Ada".into()),
        contact_email: Some("ada@example.test".into()),
        url: Some("https://example.test/checkout".into()),
        associated_event_id: Some(id(7)),
        issue_id: None,
        trace_id: Some(TraceId::from_bytes([8; 16])),
        replay_id: Some(id(9)),
        expires_at: at(1000000),
        attachments: vec![EventAttachment {
            attachment_id,
            blob: BlobObject {
                key: BlobKey::event_owned(project_id, feedback_id, attachment_id),
                kind: BlobKind::EventAttachment,
                size: 123,
                checksum: BlobChecksum::from_bytes([3; 32]),
                created_at: at(900),
            },
            filename: AttachmentFilename::sanitized("../снимок.png").unwrap(),
            content_type: BlobContentType::new("image/png").unwrap(),
            attachment_type: "screen\0shot 雪".into(),
        }],
    }
}
fn query(limit: usize) -> FeedbackQuery {
    FeedbackQuery {
        status: None,
        event_id: None,
        trace_id: None,
        replay_id: None,
        before: None,
        limit,
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresFeedbackStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresFeedbackStore::new(database.clone());
    (fixture, database, store)
}
async fn referenced(database: &PostgresDatabase, record: &FeedbackRecord) -> bool {
    use metric_ports::BlobReferenceStore;
    let store =
        crate::PostgresEventStore::new(database.clone(), crate::EventCodecConfig::default())
            .unwrap();
    store
        .is_referenced(metric_ports::BlobReference {
            project_id: record.project_id,
            event_id: record.feedback_id,
            object_id: record.attachments[0].attachment_id,
        })
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn submissions_preserve_order_identity_and_scoped_blob_references() {
    let (fixture, database, store) = setup().await;
    let mut original = feedback(1, 1);
    for seed in 2..=10 {
        let mut attachment = original.attachments[0].clone();
        attachment.attachment_id = BlobObjectId::from_bytes([seed; 16]);
        attachment.blob.key = BlobKey::event_owned(project(1), id(1), attachment.attachment_id);
        original.attachments.push(attachment);
    }
    original.attachments[9].attachment_type = "雪".repeat(128).into();
    assert_eq!(
        store.persist_feedback(original.clone()).await.unwrap(),
        DurableOutcome::Accepted
    );
    assert_eq!(
        store.load_feedback(project(1), id(1)).await.unwrap(),
        original
    );
    assert!(referenced(&database, &original).await);
    let mut retry = original.clone();
    retry.received_at = at(2000);
    retry.status = FeedbackStatus::Spam;
    retry.status_changed_at = at(3000);
    retry.expires_at = at(4000);
    retry.attachments[0].blob.created_at = at(4000);
    assert_eq!(
        store.persist_feedback(retry).await.unwrap(),
        DurableOutcome::Duplicate
    );
    assert_eq!(
        store.load_feedback(project(1), id(1)).await.unwrap(),
        original
    );
    for change in 0..3 {
        let mut conflict = original.clone();
        match change {
            0 => conflict.message = "different".into(),
            1 => conflict.attachments[0].blob.checksum = BlobChecksum::from_bytes([9; 32]),
            _ => conflict.attachments.reverse(),
        };
        assert_eq!(
            store.persist_feedback(conflict).await,
            Err(FeedbackStoreError::Conflict)
        );
    }
    assert_eq!(
        store.load_feedback(project(2), id(1)).await,
        Err(FeedbackStoreError::NotFound)
    );
    let foreign = feedback(2, 1);
    store.persist_feedback(foreign.clone()).await.unwrap();
    assert_eq!(
        store.load_feedback(project(2), id(1)).await.unwrap(),
        foreign
    );
    assert!(referenced(&database, &foreign).await);
    let mut tasks = vec![];
    for _ in 0..16 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store.persist_feedback(feedback(1, 2)).await.unwrap()
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        accepted += usize::from(task.await.unwrap() == DurableOutcome::Accepted);
    }
    assert_eq!(accepted, 1);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM metric.feedback_attachments WHERE project_id=1 AND feedback_id=$1",
    )
    .bind(id(2).as_bytes().as_slice())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn status_workflow_and_keyset_filters_remain_project_scoped() {
    let (fixture, database, store) = setup().await;
    for seed in 1..=5 {
        store.persist_feedback(feedback(1, seed)).await.unwrap();
    }
    store.persist_feedback(feedback(2, 255)).await.unwrap();
    let first = store.list_feedback(project(1), query(2)).await.unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|r| r.feedback_id)
            .collect::<Vec<_>>(),
        vec![id(5), id(4)]
    );
    let mut next = query(2);
    next.before = first.next;
    let second = store.list_feedback(project(1), next).await.unwrap();
    assert_eq!(
        second
            .items
            .iter()
            .map(|r| r.feedback_id)
            .collect::<Vec<_>>(),
        vec![id(3), id(2)]
    );
    next.before = second.next;
    let third = store.list_feedback(project(1), next).await.unwrap();
    assert_eq!(third.items.len(), 1);
    assert!(third.next.is_none());
    let initial = store.load_feedback(project(1), id(1)).await.unwrap();
    assert_eq!(
        store
            .update_feedback_status(project(1), id(1), FeedbackStatus::Open, at(0))
            .await
            .unwrap(),
        initial
    );
    assert_eq!(
        store
            .update_feedback_status(project(1), id(1), FeedbackStatus::Resolved, at(999))
            .await,
        Err(FeedbackStoreError::Conflict)
    );
    let resolved = store
        .update_feedback_status(project(1), id(1), FeedbackStatus::Resolved, at(2000))
        .await
        .unwrap();
    assert_eq!(
        store
            .update_feedback_status(project(1), id(1), FeedbackStatus::Resolved, at(0))
            .await
            .unwrap(),
        resolved
    );
    assert_eq!(
        store
            .update_feedback_status(project(1), id(1), FeedbackStatus::Spam, at(1999))
            .await,
        Err(FeedbackStoreError::Conflict)
    );
    let spam = store
        .update_feedback_status(project(1), id(1), FeedbackStatus::Spam, at(2000))
        .await
        .unwrap();
    assert_eq!(spam.status, FeedbackStatus::Spam);
    assert_eq!(
        store
            .update_feedback_status(project(2), id(1), FeedbackStatus::Resolved, at(2000))
            .await,
        Err(FeedbackStoreError::NotFound)
    );
    let mut filter = query(100);
    filter.status = Some(FeedbackStatus::Spam);
    filter.event_id = Some(id(7));
    filter.trace_id = Some(TraceId::from_bytes([8; 16]));
    filter.replay_id = Some(id(9));
    assert_eq!(
        store.list_feedback(project(1), filter).await.unwrap().items,
        vec![spam]
    );
    for field in 0..3 {
        let mut miss = filter;
        match field {
            0 => miss.event_id = Some(id(99)),
            1 => miss.trace_id = Some(TraceId::from_bytes([99; 16])),
            _ => miss.replay_id = Some(id(99)),
        };
        assert!(
            store
                .list_feedback(project(1), miss)
                .await
                .unwrap()
                .items
                .is_empty()
        );
    }
    let mut foreign = query(100);
    foreign.before = Some(FeedbackAnchor {
        received_at: at(1000),
        feedback_id: id(255),
    });
    assert_eq!(
        store
            .list_feedback(project(1), foreign)
            .await
            .unwrap()
            .items
            .len(),
        5
    );
    for limit in [0, 101] {
        assert_eq!(
            store.list_feedback(project(1), query(limit)).await,
            Err(FeedbackStoreError::InvalidData)
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn failures_rollback_metadata_attachments_and_references_and_deletion_is_scoped() {
    let (fixture, database, store) = setup().await;
    let original = feedback(1, 1);
    sqlx::query("CREATE FUNCTION metric.reject_feedback_reference() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.source_kind=1 THEN RAISE EXCEPTION 'injected reference failure'; END IF; RETURN NEW; END $$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_feedback_reference BEFORE INSERT ON metric.blob_references FOR EACH ROW EXECUTE FUNCTION metric.reject_feedback_reference()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store.persist_feedback(original.clone()).await,
        Err(FeedbackStoreError::Unavailable)
    );
    assert_eq!(
        store.load_feedback(project(1), id(1)).await,
        Err(FeedbackStoreError::NotFound)
    );
    assert!(!referenced(&database, &original).await);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.feedback_attachments")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER reject_feedback_reference ON metric.blob_references")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_feedback(original.clone()).await.unwrap();
    sqlx::query("CREATE FUNCTION metric.reject_feedback_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected status failure'; END $$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_feedback_update BEFORE UPDATE ON metric.feedback FOR EACH ROW EXECUTE FUNCTION metric.reject_feedback_update()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .update_feedback_status(project(1), id(1), FeedbackStatus::Resolved, at(2000))
            .await,
        Err(FeedbackStoreError::Unavailable)
    );
    assert_eq!(
        store.load_feedback(project(1), id(1)).await.unwrap(),
        original
    );
    sqlx::query("DROP TRIGGER reject_feedback_update ON metric.feedback")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_feedback(feedback(2, 1)).await.unwrap();
    let attachment = &original.attachments[0];
    sqlx::query("INSERT INTO metric.blob_references(project_id,event_id,object_id,source_kind,blob_key) VALUES(1,$1,$2,0,$3)").bind(id(1).as_bytes().as_slice()).bind(attachment.attachment_id.as_bytes().as_slice()).bind(attachment.blob.key.as_str()).execute(&database.pool).await.unwrap();
    sqlx::query("DELETE FROM metric.feedback WHERE project_id=1 AND feedback_id=$1")
        .bind(id(1).as_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(referenced(&database, &original).await);
    assert!(referenced(&database, &feedback(2, 1)).await);
    let kinds: Vec<i16> = sqlx::query_scalar(
        "SELECT source_kind FROM metric.blob_references WHERE project_id=1 AND event_id=$1",
    )
    .bind(id(1).as_bytes().as_slice())
    .fetch_all(&database.pool)
    .await
    .unwrap();
    assert_eq!(kinds, vec![0]);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM metric.feedback_attachments WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DELETE FROM metric.blob_references WHERE project_id=1 AND source_kind=0")
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(!referenced(&database, &original).await);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn actual_lost_commit_reply_is_safe_for_submission_and_status_retry() {
    let (fixture, database, store) = setup().await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxied_database = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied = PostgresFeedbackStore::new(proxied_database.clone());
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.persist_feedback(feedback(1, 1)).await,
        Err(FeedbackStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert_eq!(
        store.load_feedback(project(1), id(1)).await.unwrap(),
        feedback(1, 1)
    );
    assert!(referenced(&database, &feedback(1, 1)).await);
    assert_eq!(
        proxied.persist_feedback(feedback(1, 1)).await.unwrap(),
        DurableOutcome::Duplicate
    );
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied
            .update_feedback_status(project(1), id(1), FeedbackStatus::Resolved, at(2000))
            .await,
        Err(FeedbackStoreError::Unavailable)
    );
    let resolved = store.load_feedback(project(1), id(1)).await.unwrap();
    assert_eq!(resolved.status, FeedbackStatus::Resolved);
    assert_eq!(resolved.status_changed_at, at(2000));
    assert_eq!(
        proxied
            .update_feedback_status(project(1), id(1), FeedbackStatus::Resolved, at(0))
            .await
            .unwrap(),
        resolved
    );
    proxied_database.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn invalid_values_project_fences_and_corrupt_attachments_fail_closed() {
    let (fixture, database, store) = setup().await;
    for change in 0..11 {
        let mut bad = feedback(1, 1);
        match change {
            0 => bad.message = " ".into(),
            1 => bad.message = "nul\0text".into(),
            2 => bad.message = "x".repeat(16385).into(),
            3 => bad.name = Some("".into()),
            4 => bad.expires_at = bad.received_at,
            5 => bad.status_changed_at = at(999),
            6 => bad.attachments = vec![bad.attachments[0].clone(); 11],
            7 => bad.attachments[0].blob.size = u64::MAX,
            8 => bad.attachments[0].blob.kind = BlobKind::ReplayRecording,
            9 => bad.attachments[0].blob.key = feedback(2, 1).attachments[0].blob.key.clone(),
            _ => bad.attachments[0].attachment_type = "雪".repeat(129).into(),
        };
        assert_eq!(
            store.persist_feedback(bad).await,
            Err(FeedbackStoreError::InvalidData)
        );
    }
    assert_eq!(
        store.persist_feedback(feedback(99, 1)).await,
        Err(FeedbackStoreError::NotFound)
    );
    sqlx::query("UPDATE metric.projects SET state=1 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_feedback(feedback(1, 1)).await.unwrap();
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_feedback(feedback(1, 2)).await,
        Err(FeedbackStoreError::NotFound)
    );
    assert_eq!(
        store
            .update_feedback_status(project(1), id(1), FeedbackStatus::Resolved, at(2000))
            .await,
        Err(FeedbackStoreError::NotFound)
    );
    for (column, bytes) in [
        ("filename", b"../unsafe.png".to_vec()),
        ("content_type", vec![255]),
        ("attachment_type", vec![255]),
    ] {
        // Static schema names only; corruption is test-owned, every value stays bound.
        let statement = match column {
            "filename" => "UPDATE metric.feedback_attachments SET filename=$1 WHERE project_id=1",
            "content_type" => {
                "UPDATE metric.feedback_attachments SET content_type=$1 WHERE project_id=1"
            }
            _ => "UPDATE metric.feedback_attachments SET attachment_type=$1 WHERE project_id=1",
        };
        sqlx::query(statement)
            .bind(bytes)
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            store.load_feedback(project(1), id(1)).await,
            Err(FeedbackStoreError::InvalidData)
        );
        let original = feedback(1, 1);
        let a = &original.attachments[0];
        sqlx::query("UPDATE metric.feedback_attachments SET filename=$1,content_type=$2,attachment_type=$3 WHERE project_id=1").bind(a.filename.as_str().as_bytes()).bind(a.content_type.as_str().as_bytes()).bind(a.attachment_type.as_bytes()).execute(&database.pool).await.unwrap();
    }
    sqlx::query("UPDATE metric.feedback_attachments SET blob_key=$1 WHERE project_id=1")
        .bind(feedback(2, 1).attachments[0].blob.key.as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.list_feedback(project(1), query(100)).await,
        Err(FeedbackStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn bounded_feedback_queries_use_native_scoped_indexes() {
    let (fixture, database, store) = setup().await;
    store.persist_feedback(feedback(1, 255)).await.unwrap();
    sqlx::query("INSERT INTO metric.feedback(project_id,feedback_id,received_at,status,status_changed_at,message,associated_event_id,trace_id,replay_id,expire_at) SELECT 2,decode(lpad(to_hex(n),32,'0'),'hex'),1000,0,1000,$1,decode(lpad(to_hex(n),32,'0'),'hex'),decode(lpad(to_hex(n),32,'0'),'hex'),decode(lpad(to_hex(n),32,'0'),'hex'),1000000 FROM generate_series(1,10000) n").bind(b"test".as_slice()).execute(&database.pool).await.unwrap();
    sqlx::query("ANALYZE metric.feedback")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT feedback_id FROM metric.feedback WHERE project_id=1 ORDER BY received_at DESC,feedback_id DESC LIMIT 101").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("feedback_project_timeline"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT feedback_id FROM metric.feedback WHERE project_id=1 AND status=0 ORDER BY received_at DESC,feedback_id DESC LIMIT 101").fetch_one(&database.pool).await.unwrap();
    assert!(
        plan.to_string()
            .contains("feedback_project_status_timeline")
    );
    for (statement, index) in [
        (
            "EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT feedback_id FROM metric.feedback WHERE project_id=2 AND associated_event_id=$1 ORDER BY received_at DESC,feedback_id DESC LIMIT 101",
            "feedback_project_event_timeline",
        ),
        (
            "EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT feedback_id FROM metric.feedback WHERE project_id=2 AND trace_id=$1 ORDER BY received_at DESC,feedback_id DESC LIMIT 101",
            "feedback_project_trace_timeline",
        ),
        (
            "EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT feedback_id FROM metric.feedback WHERE project_id=2 AND replay_id=$1 ORDER BY received_at DESC,feedback_id DESC LIMIT 101",
            "feedback_project_replay_timeline",
        ),
    ] {
        let plan: serde_json::Value = sqlx::query_scalar(statement)
            .bind(id(255).as_bytes().as_slice())
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert!(plan.to_string().contains(index), "{plan}");
        assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    }
    assert_eq!(
        store
            .list_feedback(project(1), query(100))
            .await
            .unwrap()
            .items,
        vec![feedback(1, 255)]
    );
    database.close().await;
    fixture.cleanup().await;
}
