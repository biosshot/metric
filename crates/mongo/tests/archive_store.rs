use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use faultkeep_domain::{
    AcceptedEvent, EventId, EventKey, ProjectId, ScrubbedEventPayload, SecretBytes, Timestamp,
    archive::ArchiveBatchState,
    blob::{BlobChecksum, BlobKind, BlobObject},
};
use faultkeep_mongo::{EventCodecConfig, MongoArchiveStore, MongoEventStore, MongoProjectStore};
use faultkeep_ports::{
    ArchiveClaimRequest, ArchiveCompleteRequest, ArchiveSourceCommitRequest, ArchiveStore,
    ArchiveStoreError, EventStore, EventWriteStatus,
};
use mongodb::{
    Client, Database,
    bson::{Binary, Document, doc, spec::BinarySubtype},
};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by FAULTKEEP_TEST_MONGODB_URI"]
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
            now,
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert_eq!(claim.state, ArchiveBatchState::Writing);
    assert_eq!(claim.events.len(), 1);
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
            now,
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert_eq!(resumed.state, ArchiveBatchState::Complete);
    assert!(resumed.events.is_empty());

    assert_eq!(
        archive
            .commit_sources(ArchiveSourceCommitRequest {
                segment_id: resumed.segment_id,
                event_keys: resumed.event_keys.clone(),
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
                segment_id: resumed.segment_id,
                event_keys: resumed.event_keys,
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
            now,
            maximum_events: 10,
            target_uncompressed_bytes: 1024 * 1024,
        })
        .await?
        .unwrap();
    assert_eq!(failed_claim.events.len(), 1);
    // Simulated BlobStore/permission failure: no complete call, therefore no x.
    assert_hot_event_waiting(database, event_key(&failed)).await?;
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
    let uri = std::env::var("FAULTKEEP_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://127.0.0.1:27017/?directConnection=true&serverSelectionTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(client.database(&format!("faultkeep_phase21_archive_{nonce}")))
}
