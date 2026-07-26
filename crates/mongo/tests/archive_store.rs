use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use metric_domain::{
    AcceptedEvent, EventId, EventKey, ProjectId, ScrubbedEventPayload, SecretBytes, Timestamp,
    archive::{ArchiveBatchState, ArchiveKind, ArchiveRecords},
    blob::{BlobChecksum, BlobKind, BlobObject},
    signals::{
        LogId, LogRecord, LogSeverity, SignalBody, SpanId, SpanOperationClass, SpanRecord,
        SpanRecordId, TraceId,
    },
};
use metric_mongo::{
    EventCodecConfig, MongoArchiveStore, MongoEventStore, MongoProjectStore, MongoSignalStore,
    SignalRetention,
};
use metric_ports::{
    ArchiveClaimRequest, ArchiveCompleteRequest, ArchiveSourceCommitRequest, ArchiveStore,
    ArchiveStoreError, EventStore, EventWriteStatus, SignalStore,
};
use mongodb::{
    Client, Database,
    bson::{Binary, Document, doc, spec::BinarySubtype},
};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn infrastructure_archive_manifest_crash_points_and_hot_delete_ordering() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let codec = EventCodecConfig::default();
    let events = MongoEventStore::from_database(database.clone(), codec);
    let now = Timestamp::from_unix_millis(1_800_000_000_000)?;
    let event = accepted(1, now);
    assert_eq!(
        events
            .insert_batch(&[events.prepare(event.clone())?])
            .await?,
        vec![EventWriteStatus::Inserted]
    );
    database
        .collection::<Document>("error_events")
        .update_one(
            doc! { "_id": binary(EventKey::new(event.project_id, event.event_id).as_bytes()) },
            doc! {
                "$set": {
                    "u": binary([3_u8; 16]),
                    "h": mongodb::bson::DateTime::from_millis(now.unix_millis()),
                },
                "$unset": { "q": "" },
            },
        )
        .await?;

    let archive = MongoArchiveStore::from_database(database.clone(), codec);
    let claim = archive
        .claim(ArchiveClaimRequest {
            kind: ArchiveKind::Event,
            now,
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert_eq!(claim.state, ArchiveBatchState::Writing);
    assert!(matches!(claim.records, ArchiveRecords::Events(ref values) if values.len() == 1));
    assert_hot_event_waiting(database, event_key(&event)).await?;

    let checksum = BlobChecksum::from_bytes([9; 32]);
    archive
        .complete(ArchiveCompleteRequest {
            segment_id: claim.segment_id,
            object: BlobObject {
                key: claim.object_key.clone(),
                kind: BlobKind::EventArchive,
                size: 123,
                checksum,
                created_at: now,
            },
            completed_at: now,
        })
        .await?;
    // A crash here has a complete manifest but the source Event still has no TTL.
    assert_hot_event_waiting(database, event_key(&event)).await?;
    let resumed = archive
        .claim(ArchiveClaimRequest {
            kind: ArchiveKind::Event,
            now,
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert_eq!(resumed.state, ArchiveBatchState::Complete);
    assert!(resumed.records.is_empty());

    assert_eq!(
        archive
            .commit_sources(ArchiveSourceCommitRequest {
                kind: ArchiveKind::Event,
                segment_id: resumed.segment_id,
                source_ids: resumed.source_ids.clone(),
                expire_at: now,
            })
            .await?,
        1
    );
    let stored = database
        .collection::<Document>("error_events")
        .find_one(doc! { "_id": event_key(&event) })
        .await?
        .unwrap();
    assert!(!stored.contains_key("h"));
    assert!(stored.contains_key("x"));
    assert_eq!(
        stored.get_binary_generic("z")?.as_slice(),
        resumed.segment_id.as_bytes()
    );
    assert_eq!(
        archive
            .commit_sources(ArchiveSourceCommitRequest {
                kind: ArchiveKind::Event,
                segment_id: resumed.segment_id,
                source_ids: resumed.source_ids,
                expire_at: now,
            })
            .await?,
        0
    );
    assert_eq!(
        archive
            .complete(ArchiveCompleteRequest {
                segment_id: claim.segment_id,
                object: BlobObject {
                    key: claim.object_key,
                    kind: BlobKind::EventArchive,
                    size: 124,
                    checksum,
                    created_at: now,
                },
                completed_at: now,
            })
            .await,
        Err(ArchiveStoreError::Conflict)
    );

    let failed = accepted(2, now);
    events
        .insert_batch(&[events.prepare(failed.clone())?])
        .await?;
    database
        .collection::<Document>("error_events")
        .update_one(
            doc! { "_id": binary(EventKey::new(failed.project_id, failed.event_id).as_bytes()) },
            doc! {
                "$set": {
                    "q.s": 1_i32,
                    "q.a": 1_i32,
                    "q.c": 1_i32,
                    "h": mongodb::bson::DateTime::from_millis(now.unix_millis()),
                },
                "$unset": { "q.n": "" },
            },
        )
        .await?;
    let failed_claim = archive
        .claim(ArchiveClaimRequest {
            kind: ArchiveKind::Event,
            now,
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert!(
        matches!(failed_claim.records, ArchiveRecords::Events(ref values) if values.len() == 1)
    );
    // Simulated BlobStore/permission failure: no complete call, therefore no x.
    assert_hot_event_waiting(database, event_key(&failed)).await?;
    exercise_signal_archives(database, &archive, now).await?;
    Ok(())
}

async fn exercise_signal_archives(
    database: &Database,
    archive: &MongoArchiveStore,
    now: Timestamp,
) -> Result<(), Box<dyn Error>> {
    let project_id = ProjectId::new(7)?;
    let trace_id = TraceId::from_bytes([4; 16]);
    let span_id = SpanId::from_bytes([5; 8]);
    let signals = MongoSignalStore::with_retention(
        database.clone(),
        SignalRetention {
            logs_days: 0,
            spans_days: 0,
            span_stats_hourly_days: 90,
            archive: true,
        },
    );
    let log = LogRecord {
        id: LogId::deterministic(project_id, now, now.unix_millis() * 1_000_000, b"log"),
        project_id,
        received_at: now,
        occurred_at_ns: now.unix_millis() * 1_000_000,
        severity: LogSeverity::Info,
        message: "archive log".into(),
        trace_id: Some(trace_id),
        span_id: Some(span_id),
        environment: None,
        release: None,
        service: Some("api".into()),
        body: SignalBody::new(br#"{"body":"archive log"}"#.as_slice()),
    };
    signals.persist_logs(vec![log.clone()]).await?;
    archive_signal(
        database,
        archive,
        ArchiveKind::Log,
        BlobKind::LogArchive,
        log.id.as_bytes(),
        now,
    )
    .await?;

    let span = SpanRecord {
        id: SpanRecordId::deterministic(project_id, trace_id, span_id),
        project_id,
        received_at: now,
        started_at_ns: now.unix_millis() * 1_000_000,
        duration_ns: 1_000_000,
        trace_id,
        span_id,
        parent_span_id: None,
        is_segment: true,
        operation_class: SpanOperationClass::HttpServer,
        operation: "http.server".into(),
        status: "ok".into(),
        name: "GET /archive".into(),
        environment: None,
        release: None,
        service: Some("api".into()),
        insight_flags: 0,
        body: SignalBody::new(br#"{"transaction":"GET /archive"}"#.as_slice()),
    };
    signals.persist_spans(vec![span.clone()]).await?;
    archive_signal(
        database,
        archive,
        ArchiveKind::Span,
        BlobKind::SpanArchive,
        span.id.as_bytes(),
        now,
    )
    .await
}

async fn archive_signal(
    database: &Database,
    archive: &MongoArchiveStore,
    kind: ArchiveKind,
    blob_kind: BlobKind,
    source_id: [u8; 16],
    now: Timestamp,
) -> Result<(), Box<dyn Error>> {
    let claim = archive
        .claim(ArchiveClaimRequest {
            kind,
            now,
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert_eq!(claim.kind, kind);
    assert_eq!(claim.records.len(), 1);
    archive
        .complete(ArchiveCompleteRequest {
            segment_id: claim.segment_id,
            object: BlobObject {
                key: claim.object_key,
                kind: blob_kind,
                size: 123,
                checksum: BlobChecksum::from_bytes([kind as u8 + 1; 32]),
                created_at: now,
            },
            completed_at: now,
        })
        .await?;
    assert_eq!(
        archive
            .commit_sources(ArchiveSourceCommitRequest {
                kind,
                segment_id: claim.segment_id,
                source_ids: claim.source_ids,
                expire_at: now,
            })
            .await?,
        1
    );
    let collection = if kind == ArchiveKind::Log {
        "logs"
    } else {
        "spans"
    };
    let stored = database
        .collection::<Document>(collection)
        .find_one(doc! { "_id": binary(source_id) })
        .await?
        .unwrap();
    assert!(!stored.contains_key("h"));
    assert!(stored.contains_key("z"));
    assert!(stored.contains_key("x"));
    Ok(())
}

async fn assert_hot_event_waiting(database: &Database, id: Binary) -> Result<(), Box<dyn Error>> {
    let event = database
        .collection::<Document>("error_events")
        .find_one(doc! { "_id": id })
        .await?
        .unwrap();
    assert!(event.contains_key("h"));
    assert!(!event.contains_key("x"));
    assert!(!event.contains_key("z"));
    Ok(())
}

fn accepted(seed: u8, received_at: Timestamp) -> AcceptedEvent {
    AcceptedEvent {
        project_id: ProjectId::new(7).unwrap(),
        event_id: EventId::from_bytes([seed; 16]),
        received_at,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(
                "{{\"event_id\":\"{}\",\"message\":\"archive {seed}\",\"platform\":\"rust\"}}",
                EventId::from_bytes([seed; 16])
            )
            .into_bytes(),
        ),
    }
}

fn event_key(event: &AcceptedEvent) -> Binary {
    binary(EventKey::new(event.project_id, event.event_id).as_bytes())
}

fn binary<const N: usize>(bytes: [u8; N]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

async fn test_database() -> Result<Database, Box<dyn Error>> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://127.0.0.1:27017/?directConnection=true&serverSelectionTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(client.database(&format!("metric_phase21_archive_{nonce}")))
}
