use std::{
    error::Error,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::TryStreamExt;
use metric_application::archive::{ArchiveConfig, ArchiveService};
use metric_blob::{LocalBlobConfig, LocalBlobStore};
use metric_domain::{
    AcceptedEvent, EventId, EventKey, ProjectId, ScrubbedEventPayload, SecretBytes, Timestamp,
    blob::BlobKey,
    signals::{
        LogId, LogRecord, LogSeverity, SignalBody, SpanId, SpanOperationClass, SpanRecord,
        SpanRecordId, TraceId,
    },
};
use metric_mongo::{
    EventCodecConfig, MongoEventStore, MongoProjectStore, MongoSignalStore, SignalRetention,
};
use metric_ports::{BlobStore, Clock, EventStore, EventWriteStatus, SignalStore};
use mongodb::{
    Client, Database,
    bson::{Binary, Document, doc, spec::BinarySubtype},
};

struct FixedClock(Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn cumulative_event_to_archive_object_manifest_then_hot_retention() {
    let database = test_database().await.unwrap();
    let root = std::env::temp_dir().join(format!("metric-archive-e2e-{}", uuid::Uuid::new_v4()));
    let result = exercise(&database, &root).await;
    let cleanup_database = database.drop().await;
    let cleanup_blobs = std::fs::remove_dir_all(&root);
    result.unwrap();
    cleanup_database.unwrap();
    cleanup_blobs.unwrap();
}

async fn exercise(database: &Database, root: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let now = Timestamp::from_unix_millis(1_800_000_000_000)?;
    let codec = EventCodecConfig::default();
    let events = MongoEventStore::from_database(database.clone(), codec);
    let event = AcceptedEvent {
        project_id: ProjectId::new(7)?,
        event_id: EventId::from_bytes([1; 16]),
        received_at: now,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(
                "{{\"event_id\":\"{}\",\"message\":\"archive e2e\",\"platform\":\"rust\"}}",
                EventId::from_bytes([1; 16])
            )
            .into_bytes(),
        ),
    };
    assert_eq!(
        events
            .insert_batch(&[events.prepare(event.clone())?])
            .await?,
        vec![EventWriteStatus::Inserted]
    );
    let event_key = EventKey::new(event.project_id, event.event_id);
    database
        .collection::<Document>("error_events")
        .update_one(
            doc! { "_id": binary(event_key.as_bytes()) },
            doc! {
                "$set": {
                    "u": binary([2_u8; 16]),
                    "h": mongodb::bson::DateTime::from_millis(now.unix_millis()),
                },
                "$unset": { "q": "" },
            },
        )
        .await?;
    let trace_id = TraceId::from_bytes([3; 16]);
    let span_id = SpanId::from_bytes([4; 8]);
    let signals = MongoSignalStore::with_retention(
        database.clone(),
        SignalRetention {
            logs_days: 0,
            spans_days: 0,
            span_stats_hourly_days: 90,
            archive: true,
        },
    );
    let log_id = LogId::deterministic(
        event.project_id,
        now,
        now.unix_millis() * 1_000_000,
        b"archive log",
    );
    signals
        .persist_logs(vec![LogRecord {
            id: log_id,
            project_id: event.project_id,
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
        }])
        .await?;
    let span_record_id = SpanRecordId::deterministic(event.project_id, trace_id, span_id);
    signals
        .persist_spans(vec![SpanRecord {
            id: span_record_id,
            project_id: event.project_id,
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
        }])
        .await?;
    let blobs = Arc::new(
        LocalBlobStore::new(
            root,
            LocalBlobConfig {
                capacity_bytes: 32 * 1024 * 1024,
                reserve_bytes: 1024 * 1024,
                max_object_bytes: 16 * 1024 * 1024,
            },
        )
        .await?,
    );
    let blob_port: Arc<dyn BlobStore> = blobs.clone();
    let service = ArchiveService::new(
        Arc::new(control.archive_store(codec)),
        blob_port,
        Arc::new(FixedClock(now)),
        ArchiveConfig {
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
            write_chunk_bytes: 4096,
            poll_interval: Duration::from_secs(60),
            hot_copy_delay: Duration::from_secs(60),
            orphan_grace: Duration::from_secs(24 * 60 * 60),
            cleanup_max_pages: 1,
        },
    )?;
    let report = service.run_once().await?;
    assert_eq!(report.claimed_records, 3);
    assert_eq!(report.archived_records, 3);
    assert!(report.stored_bytes > 0);

    let mut manifests = database
        .collection::<Document>("archive_manifests")
        .find(doc! { "project_id": 7_i32 })
        .await?;
    let mut kinds = std::collections::BTreeSet::new();
    while let Some(manifest) = manifests.try_next().await? {
        assert_eq!(manifest.get_str("state")?, "complete");
        assert!(manifest.get_bool("source_committed")?);
        kinds.insert(manifest.get_str("kind")?.to_owned());
        let object_key = BlobKey::new(manifest.get_str("object_key")?.to_owned())?;
        let mut reader = blobs.open(&object_key).await?;
        let mut archived = Vec::new();
        while let Some(chunk) = reader.read_chunk(4096).await? {
            archived.extend_from_slice(&chunk);
        }
        assert_eq!(&archived[..4], b"PAR1");
        assert_eq!(&archived[archived.len() - 4..], b"PAR1");
    }
    assert_eq!(
        kinds,
        std::collections::BTreeSet::from(["event".to_owned(), "log".to_owned(), "span".to_owned()])
    );

    let hot = database
        .collection::<Document>("error_events")
        .find_one(doc! { "_id": binary(event_key.as_bytes()) })
        .await?
        .unwrap();
    assert!(!hot.contains_key("h"));
    assert!(hot.contains_key("z"));
    assert_eq!(
        hot.get_datetime("x")?.timestamp_millis(),
        now.unix_millis() + 60_000
    );
    for (collection, id) in [
        ("logs", log_id.as_bytes()),
        ("spans", span_record_id.as_bytes()),
    ] {
        let hot = database
            .collection::<Document>(collection)
            .find_one(doc! { "_id": binary(id) })
            .await?
            .unwrap();
        assert!(!hot.contains_key("h"));
        assert!(hot.contains_key("z"));
        assert_eq!(
            hot.get_datetime("x")?.timestamp_millis(),
            now.unix_millis() + 60_000
        );
    }
    Ok(())
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
    Ok(client.database(&format!("metric_phase21_archive_e2e_{nonce}")))
}
