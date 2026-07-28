use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use metric_domain::{
    EventId, ProjectId, SecretBytes, Timestamp,
    blob::{BlobChecksum, BlobKey, BlobKind, BlobObject},
    replays::{ReplayMetadata, ReplaySegment, ReplaySegmentCommit},
};
use metric_mongo::{MongoProjectStore, ReplayRetention};
use metric_ports::{DurableOutcome, ReplayQuery, ReplayStore};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn replay_manifest_order_retention_archive_and_project_scope_are_bounded() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let project = ProjectId::new(42)?;
    let replay_id = EventId::from_bytes([3; 16]);
    let store = control.replay_store(ReplayRetention {
        days: 30,
        archive: false,
    });

    assert_eq!(
        store
            .persist_replay_segment(commit(project, replay_id, 2))
            .await?,
        DurableOutcome::Accepted
    );
    assert_eq!(
        store
            .persist_replay_segment(commit(project, replay_id, 0))
            .await?,
        DurableOutcome::Accepted
    );
    assert_eq!(
        store
            .persist_replay_segment(commit(project, replay_id, 0))
            .await?,
        DurableOutcome::Duplicate
    );

    let record = store.load_replay(project, replay_id).await?;
    assert_eq!(
        record
            .segments
            .iter()
            .map(|segment| segment.segment_id)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert_eq!(record.error_ids, vec![EventId::from_bytes([8; 16])]);
    assert_eq!(
        store
            .list_replays(
                project,
                ReplayQuery {
                    from: None,
                    until: None,
                    before: None,
                    limit: 10,
                },
            )
            .await?
            .items
            .len(),
        1
    );
    assert!(
        store
            .list_replays(
                ProjectId::new(43)?,
                ReplayQuery {
                    from: None,
                    until: None,
                    before: None,
                    limit: 10,
                },
            )
            .await?
            .items
            .is_empty()
    );

    let replays = database.collection::<mongodb::bson::Document>("replays");
    assert_eq!(
        replays
            .count_documents(doc! {
                "p": project.get(),
                "z": { "$type": "date" },
                "sg": { "$size": 2 },
            })
            .await?,
        1
    );

    let archived_project = ProjectId::new(44)?;
    control
        .replay_store(ReplayRetention {
            days: 30,
            archive: true,
        })
        .persist_replay_segment(commit(archived_project, EventId::from_bytes([4; 16]), 0))
        .await?;
    assert_eq!(
        replays
            .count_documents(doc! {
                "p": archived_project.get(),
                "h": { "$type": "date" },
                "z": { "$exists": false },
            })
            .await?,
        1
    );
    let index_names = replays.list_index_names().await?;
    assert!(index_names.iter().any(|name| name == "replay_retention"));
    assert!(index_names.iter().any(|name| name == "replay_archive_due"));
    Ok(())
}

fn commit(project_id: ProjectId, replay_id: EventId, segment_id: u32) -> ReplaySegmentCommit {
    let started_at = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
    let ended_at = Timestamp::from_unix_millis(1_700_000_001_000 + i64::from(segment_id)).unwrap();
    ReplaySegmentCommit {
        metadata: ReplayMetadata {
            project_id,
            replay_id,
            segment_id,
            started_at,
            ended_at,
            received_at: ended_at,
            environment: Some("production".into()),
            release: Some("web@1.0.0".into()),
            url: Some("https://example.test/checkout".into()),
            error_ids: vec![EventId::from_bytes([8; 16])],
            trace_ids: Vec::new(),
        },
        segment: ReplaySegment {
            segment_id,
            object: BlobObject {
                key: BlobKey::replay_recording(project_id, replay_id, segment_id),
                kind: BlobKind::ReplayRecording,
                size: 64,
                checksum: BlobChecksum::from_bytes([u8::try_from(segment_id).unwrap_or(0); 32]),
                created_at: ended_at,
            },
            decompressed_bytes: 128,
            event_count: 4,
        },
    }
}

async fn test_database() -> Result<Database, Box<dyn Error>> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_owned());
    let client = Client::with_uri_str(&uri).await?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(client.database(&format!("metric_replays_test_{nonce}")))
}
