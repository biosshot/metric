use super::*;
use crate::{MigrationPolicy, PostgresProjectStore, PostgresSchemaManager, test_support::Fixture};
use metric_domain::{blob::BlobObjectId, processing::ProcessingErrorCode};
use metric_ports::{FinalizationStore, ProjectStore};
use serde_json::json;
use std::time::Duration;

fn at(ms: i64) -> Timestamp {
    Timestamp::from_unix_millis(ms).unwrap()
}
fn project(id: i32) -> ProjectId {
    ProjectId::new(id).unwrap()
}
fn event(project_id: i32, byte: u8, received: i64, message: &str) -> AcceptedEvent {
    let event_id = EventId::from_bytes([byte; 16]);
    AcceptedEvent {
        project_id: project(project_id),
        event_id,
        received_at: at(received),
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            serde_json::to_vec(&json!({
                "event_id": event_id.to_string(), "message": message,
            }))
            .unwrap(),
        ),
    }
}

async fn setup() -> (
    Fixture,
    PostgresDatabase,
    PostgresEventStore,
    PostgresProjectStore,
) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    let control = crate::test_support::seed_projects(database.clone(), 32).await;
    let store = PostgresEventStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    (fixture, database, store, control)
}

fn with_blobs(mut event: AcceptedEvent, keys: &[BlobKey]) -> AcceptedEvent {
    let mut value: Value = serde_json::from_slice(event.payload.as_bytes()).unwrap();
    value["attachments"] = Value::Array(
        keys.iter()
            .map(|key| json!({"blob_key": key.as_str()}))
            .collect(),
    );
    value["native_crash"] = json!({"blob_key": keys[0].as_str()});
    event.payload = ScrubbedEventPayload::new(serde_json::to_vec(&value).unwrap());
    event
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn preparation_bounds_canonical_identity_and_bounded_decompression() {
    let (fixture, database, store, _) = setup().await;
    let original = event(1, 1, 10, "'; DROP TABLE metric.projects; -- Юникод");
    let prepared = store.prepare(original.clone()).unwrap();
    assert_eq!(prepared.body[0], 1);
    assert_eq!(prepared.body[1], 0);
    assert_eq!(
        decode_body(&prepared.body, 4096).unwrap(),
        original.payload.as_bytes()
    );
    assert_eq!(prepared.into_event(), original);
    let compressed = store.prepare(event(1, 2, 10, &"x".repeat(10_000))).unwrap();
    assert_eq!(compressed.body[1], 1);
    assert!(compressed.encoded_len() < 500);
    assert_eq!(
        decode_body(&compressed.body, 100),
        Err(PostgresError::InvalidData)
    );
    for bytes in [
        vec![],
        vec![1],
        vec![2, 0, 123, 125],
        vec![1, 2, 123, 125],
        vec![1, 1, 99],
        b"\x01\x00{ \"event_id\": 1 }".to_vec(),
    ] {
        assert_eq!(decode_body(&bytes, 4096), Err(PostgresError::InvalidData));
    }
    let mut mismatched = event(1, 1, 10, "wrong payload identity");
    mismatched.event_id = EventId::from_bytes([2; 16]);
    assert!(matches!(
        store.prepare(mismatched),
        Err(EventPrepareError::InvalidEvent)
    ));
    for revision in [0, u64::MAX] {
        let mut invalid = event(1, 1, 10, "revision");
        invalid.policy_revision = revision;
        assert!(matches!(
            store.prepare(invalid),
            Err(EventPrepareError::InvalidEvent)
        ));
    }
    let tight = PostgresEventStore::new(
        database.clone(),
        EventCodecConfig {
            max_decoded_body_bytes: 100,
            ..EventCodecConfig::default()
        },
    )
    .unwrap();
    assert!(matches!(
        tight.prepare(event(1, 3, 10, &"x".repeat(200))),
        Err(EventPrepareError::TooLarge)
    ));
    let tight = PostgresEventStore::new(
        database.clone(),
        EventCodecConfig {
            max_encoded_row_bytes: 110,
            ..EventCodecConfig::default()
        },
    )
    .unwrap();
    assert!(matches!(
        tight.prepare(event(1, 3, 10, "bounded")),
        Err(EventPrepareError::TooLarge)
    ));
    let accepted = event(1, 3, 10, "bounded reference count");
    let keys: Vec<_> = (0..=MAX_BLOB_REFERENCES)
        .map(|index| {
            BlobKey::event_owned(
                project(1),
                accepted.event_id,
                BlobObjectId::from_bytes((index as u128).to_be_bytes()),
            )
        })
        .collect();
    assert!(matches!(
        store.prepare(with_blobs(accepted.clone(), &keys)),
        Err(EventPrepareError::InvalidEvent)
    ));
    let with_reference = with_blobs(accepted, &keys[..1]);
    let prepared = store.prepare(with_reference.clone()).unwrap();
    let tight = PostgresEventStore::new(
        database.clone(),
        EventCodecConfig {
            max_encoded_row_bytes: prepared.encoded_len() - 1,
            ..EventCodecConfig::default()
        },
    )
    .unwrap();
    assert!(matches!(
        tight.prepare(with_reference),
        Err(EventPrepareError::TooLarge)
    ));
    let oversized: Vec<_> = (0..=MAX_BATCH)
        .map(|_| store.prepare(event(1, 3, 10, "batch bound")).unwrap())
        .collect();
    assert_eq!(
        store.insert_batch(&oversized).await,
        Err(EventStoreError::Unavailable)
    );
    assert_eq!(store.observe(10).await.unwrap().pending_count, 0);
    assert!(store.insert_batch(&[]).await.unwrap().is_empty());
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn batch_retry_first_body_project_scope_and_persistent_backlog() {
    let (fixture, database, store, control) = setup().await;
    let first = event(1, 1, 100, "first ' UNION SELECT secrets; --");
    let other = event(2, 1, 200, "other organization, same EventId");
    let prepared = [
        store.prepare(first.clone()).unwrap(),
        store
            .prepare(event(1, 1, 300, "conflicting retry"))
            .unwrap(),
        store.prepare(other.clone()).unwrap(),
        store.prepare(event(99, 1, 50, "missing project")).unwrap(),
    ];
    assert_eq!(
        store.insert_batch(&prepared).await.unwrap(),
        [
            EventWriteStatus::Inserted,
            EventWriteStatus::Duplicate,
            EventWriteStatus::Inserted,
            EventWriteStatus::Rejected
        ]
    );
    assert_eq!(
        store.insert_batch(&prepared).await.unwrap(),
        [
            EventWriteStatus::Duplicate,
            EventWriteStatus::Duplicate,
            EventWriteStatus::Duplicate,
            EventWriteStatus::Rejected
        ]
    );
    assert_eq!(store.observe(1).await.unwrap().pending_count, 1);
    assert_eq!(
        store.observe(10).await.unwrap().oldest_pending_at,
        Some(at(100))
    );
    assert_eq!(store.observe(0).await, Err(EventBacklogError::InvalidData));
    assert!(store.load_due(at(99), 10, &[]).await.unwrap().is_empty());
    let first_key = EventKey::new(project(1), first.event_id);
    let loaded = store.load_due(at(1000), 10, &[first_key]).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].event, other);
    // A supported large Dispatcher queue can exceed the refill page bound.
    let excluded = vec![first_key; 40_000];
    assert_eq!(
        store.load_due(at(1000), 10, &excluded).await.unwrap().len(),
        1
    );
    // A fresh adapter/pool recovers the durable work after all old handles close.
    drop(store);
    drop(control);
    database.close().await;
    let restarted = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    let store = PostgresEventStore::new(restarted.clone(), EventCodecConfig::default()).unwrap();
    let loaded = store.load_due(at(1000), 10, &[]).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].event, first);
    assert_eq!(loaded[1].event, other);
    assert!(loaded.iter().all(|event| event.attempts == 0));
    assert_eq!(
        store.load_due(at(1000), MAX_BACKLOG_PAGE + 1, &[]).await,
        Err(EventBacklogError::InvalidData)
    );
    restarted.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn owned_blob_references_and_event_rows_rollback_together() {
    let (fixture, database, store, _) = setup().await;
    let accepted = event(1, 1, 100, "atomic reference registry");
    let object = BlobObjectId::from_bytes([7; 16]);
    let own = BlobKey::event_owned(project(1), accepted.event_id, object);
    let wrong_project = BlobKey::event_owned(project(2), accepted.event_id, object);
    let wrong_event = BlobKey::event_owned(project(1), EventId::from_bytes([2; 16]), object);
    let prepared = store
        .prepare(with_blobs(
            accepted.clone(),
            &[own.clone(), own.clone(), wrong_project, wrong_event],
        ))
        .unwrap();
    assert_eq!(prepared.references.len(), 1);
    let reference = BlobReference {
        project_id: project(1),
        event_id: accepted.event_id,
        object_id: object,
    };
    // The trigger models a database-side failure after the Event insert, before COMMIT.
    sqlx::raw_sql("CREATE FUNCTION metric.test_reject_reference() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic test-only failure'; END $$; CREATE TRIGGER test_reject_reference BEFORE INSERT ON metric.blob_references FOR EACH ROW EXECUTE FUNCTION metric.test_reject_reference()")
        .execute(&database.pool).await.unwrap();
    assert_eq!(
        store.insert_batch(std::slice::from_ref(&prepared)).await,
        Err(EventStoreError::Unavailable)
    );
    assert_eq!(store.observe(10).await.unwrap().pending_count, 0);
    assert!(!store.is_referenced(reference).await.unwrap());
    sqlx::raw_sql("DROP TRIGGER test_reject_reference ON metric.blob_references; DROP FUNCTION metric.test_reject_reference()")
        .execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .insert_batch(std::slice::from_ref(&prepared))
            .await
            .unwrap(),
        [EventWriteStatus::Inserted]
    );
    assert!(store.is_referenced(reference).await.unwrap());
    for (project_id, event_id, object_id) in [
        (project(2), accepted.event_id, object),
        (project(1), EventId::from_bytes([2; 16]), object),
        (
            project(1),
            accepted.event_id,
            BlobObjectId::from_bytes([8; 16]),
        ),
    ] {
        assert!(
            !store
                .is_referenced(BlobReference {
                    project_id,
                    event_id,
                    object_id
                })
                .await
                .unwrap()
        );
    }
    let changed = store
        .prepare(event(1, 1, 100, "retry without references"))
        .unwrap();
    assert_eq!(
        store.insert_batch(&[changed]).await.unwrap(),
        [EventWriteStatus::Duplicate]
    );
    assert!(store.is_referenced(reference).await.unwrap());
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn processing_retry_cas_permanent_failure_and_project_fences() {
    let (fixture, database, store, control) = setup().await;
    let first = store.prepare(event(1, 1, 100, "retry")).unwrap();
    let other = store
        .prepare(event(2, 1, 100, "same id, other project"))
        .unwrap();
    let key = first.key();
    assert_eq!(
        store.insert_batch(&[first, other]).await.unwrap(),
        [EventWriteStatus::Inserted; 2]
    );
    let retry = ProcessingFailure {
        key,
        expected_attempts: 0,
        new_attempts: 1,
        code: ProcessingErrorCode::FinalizerUnavailable,
        disposition: ProcessingFailureDisposition::RetryAt(at(500)),
    };
    let (a, b) = tokio::join!(
        store.record_processing_failure(retry),
        store.record_processing_failure(retry)
    );
    assert!(matches!(
        (a.unwrap(), b.unwrap()),
        (
            ProcessingStateChange::Updated,
            ProcessingStateChange::StaleOrCompleted
        ) | (
            ProcessingStateChange::StaleOrCompleted,
            ProcessingStateChange::Updated
        )
    ));
    assert_eq!(store.load_due(at(499), 10, &[]).await.unwrap().len(), 1);
    let loaded = store.load_due(at(500), 10, &[]).await.unwrap();
    assert_eq!(
        loaded
            .iter()
            .find(|event| event.key() == key)
            .unwrap()
            .attempts,
        1
    );
    assert_eq!(
        store
            .record_processing_failure(ProcessingFailure {
                new_attempts: 3,
                ..retry
            })
            .await,
        Err(ProcessingStateError::InvalidData)
    );
    let permanent = ProcessingFailure {
        expected_attempts: 1,
        new_attempts: 2,
        disposition: ProcessingFailureDisposition::PermanentlyFailed,
        ..retry
    };
    assert_eq!(
        store.record_processing_failure(permanent).await.unwrap(),
        ProcessingStateChange::Updated
    );
    assert_eq!(
        store.record_processing_failure(permanent).await.unwrap(),
        ProcessingStateChange::StaleOrCompleted
    );
    assert_eq!(store.observe(10).await.unwrap().pending_count, 1);
    control
        .set_project_acceptance(project(2), ProjectAcceptanceState::Disabled)
        .await
        .unwrap();
    assert_eq!(store.load_due(at(1000), 10, &[]).await.unwrap().len(), 1);
    let processing = store.load_processing_project(project(2)).await.unwrap();
    assert_eq!(processing.state, ProjectAcceptanceState::Disabled);
    assert!(processing.error_events_enabled);
    assert_eq!(
        (
            processing.grouping_revision,
            processing.debug_file_revision,
            processing.artifact_revision
        ),
        (1, 0, 0)
    );
    control
        .set_project_acceptance(project(2), ProjectAcceptanceState::PendingDelete)
        .await
        .unwrap();
    assert!(store.load_due(at(1000), 10, &[]).await.unwrap().is_empty());
    let late = store.prepare(event(2, 2, 100, "fenced")).unwrap();
    assert_eq!(
        store.insert_batch(&[late]).await.unwrap(),
        [EventWriteStatus::Rejected]
    );
    assert_eq!(
        store.load_processing_project(project(99)).await,
        Err(ProcessingProjectError::NotFound)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn state_change_lock_fences_batch_and_corrupt_payload_is_rejected() {
    let (fixture, database, store, _) = setup().await;
    let mut transaction = database.pool.begin().await.unwrap();
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=$1")
        .bind(1_i32)
        .execute(&mut *transaction)
        .await
        .unwrap();
    let prepared = store
        .prepare(event(1, 1, 100, "cannot cross deletion fence"))
        .unwrap();
    let store_clone = store.clone();
    let mut insert = tokio::spawn(async move { store_clone.insert_batch(&[prepared]).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut insert)
            .await
            .is_err()
    );
    transaction.commit().await.unwrap();
    assert_eq!(insert.await.unwrap().unwrap(), [EventWriteStatus::Rejected]);
    assert_eq!(store.observe(10).await.unwrap().pending_count, 0);
    let prepared = store
        .prepare(event(2, 1, 100, "decode corruption"))
        .unwrap();
    assert_eq!(
        store.insert_batch(&[prepared]).await.unwrap(),
        [EventWriteStatus::Inserted]
    );
    let mismatched =
        serde_json::to_vec(&json!({"event_id": EventId::from_bytes([2; 16]).to_string()})).unwrap();
    let mut body = vec![1, 0];
    body.extend(mismatched);
    sqlx::query("UPDATE metric.error_events SET body=$1 WHERE project_id=$2")
        .bind(body)
        .bind(2_i32)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.load_due(at(1000), 10, &[]).await,
        Err(EventBacklogError::InvalidData)
    );
    // Corruption must fail without fetching its oversized bytes into the adapter.
    let oversized = vec![b'x'; 2 * 1024 * 1024 + 1];
    sqlx::query("UPDATE metric.error_events SET body=$1 WHERE project_id=$2")
        .bind(oversized)
        .bind(2_i32)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.load_due(at(1000), 10, &[]).await,
        Err(EventBacklogError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_batches_preserve_one_body_and_writer_recovers_rejected_handoff() {
    use metric_application::{
        shutdown::ShutdownRoot,
        writer::{EventWriter, EventWriterConfig},
    };
    use metric_ports::{AcceptedEventHandoff, DurableOutcome, EventSink};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct RejectHandoff(AtomicUsize);
    impl AcceptedEventHandoff for RejectHandoff {
        fn offer(&self, event: AcceptedEvent) -> Result<(), AcceptedEvent> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(event)
        }
    }
    let (fixture, database, store, _) = setup().await;
    let left: Vec<_> = (1..=64)
        .map(|byte| {
            store
                .prepare(event(1, byte, i64::from(byte), "left"))
                .unwrap()
        })
        .collect();
    let right: Vec<_> = (1..=64)
        .rev()
        .map(|byte| {
            store
                .prepare(event(1, byte, i64::from(byte), "right"))
                .unwrap()
        })
        .collect();
    let (a, b) = tokio::join!(store.insert_batch(&left), store.insert_batch(&right));
    let a = a.unwrap();
    let mut b = b.unwrap();
    b.reverse();
    for (a, b) in a.into_iter().zip(b) {
        assert!(matches!(
            (a, b),
            (EventWriteStatus::Inserted, EventWriteStatus::Duplicate)
                | (EventWriteStatus::Duplicate, EventWriteStatus::Inserted)
        ));
    }
    assert_eq!(store.observe(1000).await.unwrap().pending_count, 64);
    let handoff = Arc::new(RejectHandoff(AtomicUsize::new(0)));
    let shutdown = ShutdownRoot::new();
    let (writer, task) = EventWriter::start(
        Arc::new(store.clone()),
        handoff.clone(),
        EventWriterConfig::default(),
        shutdown.signal(),
    )
    .unwrap();
    let new_event = event(2, 100, 100, "durable despite full in-process queue");
    assert_eq!(
        writer.persist(new_event.clone()).await.unwrap(),
        DurableOutcome::Accepted
    );
    assert_eq!(
        writer.persist(new_event.clone()).await.unwrap(),
        DurableOutcome::Duplicate
    );
    assert_eq!(handoff.0.load(Ordering::SeqCst), 1);
    shutdown.begin();
    tokio::time::timeout(Duration::from_secs(2), task.wait())
        .await
        .unwrap();
    let recovered = store.load_due(at(1000), 100, &[]).await.unwrap();
    assert_eq!(recovered.len(), 65);
    assert_eq!(
        recovered
            .iter()
            .find(|pending| pending.key().project_id() == project(2))
            .unwrap()
            .event,
        new_event
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn sparse_pending_backlog_uses_partial_index_with_bounded_plan() {
    let (fixture, database, store, _) = setup().await;
    // Disposable representative history: real prepared rows, mostly completed.
    let mut history = Vec::new();
    for batch in 0..20_u128 {
        let events: Vec<_> = (batch * 500..(batch + 1) * 500)
            .map(|index| {
                let id = EventId::from_bytes(index.to_be_bytes());
                let mut accepted = event(1, 1, index as i64 + 1, "synthetic history");
                accepted.event_id = id;
                accepted.payload = ScrubbedEventPayload::new(
                    serde_json::to_vec(&json!({
                        "event_id": id.to_string(), "message": "synthetic history"
                    }))
                    .unwrap(),
                );
                if index < 9900 {
                    history.push(crate::test_support::finalized(
                        1,
                        1,
                        id,
                        index as i64,
                        index as i64 + 1,
                        None,
                        None,
                    ));
                }
                store.prepare(accepted).unwrap()
            })
            .collect();
        assert!(
            store
                .insert_batch(&events)
                .await
                .unwrap()
                .iter()
                .all(|status| *status == EventWriteStatus::Inserted)
        );
    }
    let finalizer = crate::PostgresFinalizationStore::new(
        database.clone(),
        EventCodecConfig::default(),
        crate::IssueCodecConfig::default(),
    )
    .unwrap();
    let result = finalizer
        .finalize(
            metric_domain::finalization::FinalizeBatch { events: history },
            metric_domain::finalization::FinalizationPolicy {
                event_retention: Duration::from_secs(86400),
                hourly_retention: Duration::from_secs(86400),
                archive_events: false,
                max_implicit_releases_per_project_day: 100,
                max_implicit_environments_per_project: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.finalized, 9900);
    sqlx::query("VACUUM ANALYZE metric.error_events")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE metric.projects")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT e.project_id,e.event_id,e.event_key,e.received_at,e.policy_revision,e.attempts,CASE WHEN octet_length(e.body)<=$4 THEN e.body END AS body FROM metric.error_events e JOIN metric.projects p ON p.id=e.project_id WHERE e.processing_state=0 AND e.next_attempt_at<=$1 AND NOT(e.event_key=ANY($2::bytea[])) AND p.state IN (0,1) ORDER BY e.next_attempt_at,e.received_at,e.event_key LIMIT $3")
        .bind(100_000_i64).bind(Vec::<Vec<u8>>::new()).bind(10_i64).bind((store.codec.max_encoded_row_bytes - ROW_METADATA_BYTES) as i64)
        .fetch_one(&database.pool).await.unwrap();
    fn uses_index(node: &Value) -> bool {
        node.get("Index Name").and_then(Value::as_str) == Some("event_pending_due")
            || node
                .get("Plans")
                .and_then(Value::as_array)
                .is_some_and(|children| children.iter().any(uses_index))
    }
    assert!(
        uses_index(&plan[0]["Plan"]),
        "pending scan must use its partial index"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(10.0));
    assert_eq!(store.observe(1000).await.unwrap().pending_count, 100);
    assert_eq!(
        store.load_due(at(100_000), 10, &[]).await.unwrap().len(),
        10
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_reply_is_ambiguous_and_retry_preserves_body_and_references() {
    let (fixture, database, direct, _) = setup().await;
    let proxy = crate::test_support::CommitReplyProxy::start(&fixture.uri).await;
    let policy = crate::ConnectionPolicy {
        max_connections: 1,
        ..fixture.policy.clone()
    };
    let through_proxy = PostgresDatabase::connect(&proxy.uri, &policy)
        .await
        .unwrap();
    let store =
        PostgresEventStore::new(through_proxy.clone(), EventCodecConfig::default()).unwrap();
    let accepted = event(1, 1, 100, "successful COMMIT with missing acknowledgement");
    let reference = BlobReference {
        project_id: project(1),
        event_id: accepted.event_id,
        object_id: BlobObjectId::from_bytes([9; 16]),
    };
    let own = BlobKey::event_owned(
        reference.project_id,
        reference.event_id,
        reference.object_id,
    );
    let prepared = store.prepare(with_blobs(accepted, &[own])).unwrap();
    proxy.drop_next_commit_reply();
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        store.insert_batch(std::slice::from_ref(&prepared)),
    )
    .await
    .unwrap();
    assert_eq!(outcome, Err(EventStoreError::Ambiguous));
    assert!(
        proxy.did_drop_reply(),
        "fault must occur after PostgreSQL confirms COMMIT"
    );
    assert_eq!(direct.observe(10).await.unwrap().pending_count, 1);
    assert!(direct.is_referenced(reference).await.unwrap());
    assert_eq!(
        store
            .insert_batch(std::slice::from_ref(&prepared))
            .await
            .unwrap(),
        [EventWriteStatus::Duplicate]
    );
    let recovered = direct.load_due(at(1000), 10, &[]).await.unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].event, prepared.into_event());
    through_proxy.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}
