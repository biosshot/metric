use std::{error::Error, time::Duration};

use metric_domain::{
    AcceptedEvent, DisplayName, DsnKey, EventId, IpScrubPolicy, ItemCapabilities, OrganizationId,
    OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits,
    ProjectKeyIdentity, ProjectKeyLabel, ProjectKeyState, ScrubbedEventPayload, SecretBytes, Slug,
    Timestamp,
    auth::UserId,
    deletion::{ProjectDeletionOperationId, ProjectDeletionPhase, ProjectDeletionRequest},
};
use metric_mongo::{EventCodecConfig, MongoProjectStore};
use metric_ports::{EventStore, ProjectDeletionStore, ProjectPurgeRequest, ProjectStore};
use mongodb::{
    Client, Database,
    bson::{Binary, DateTime, doc, spec::BinarySubtype},
};

#[tokio::test]
#[ignore = "requires a real MongoDB"]
async fn infrastructure_project_deletion_cancel_restart_rescan_and_tombstone() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "Phase 15 purge RPS baseline requires a real MongoDB"]
async fn performance_project_deletion_bounded_purge_rps() {
    let database = test_database().await.unwrap();
    let result = measure_purge_rps(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn measure_purge_rps(database: &Database) -> Result<(), Box<dyn Error>> {
    const DELETED_EVENTS: u32 = 20_000;
    const ACTIVE_EVENTS: u32 = 2_000;
    let store = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    store.bootstrap_or_validate().await?;
    store.insert_organization(organization()).await?;
    store.insert_project(project(42, "delete-load")).await?;
    store.insert_project(project(43, "active-load")).await?;
    let events = store.event_store(EventCodecConfig::default());
    for start in (0..DELETED_EVENTS).step_by(250) {
        let prepared = (start..(start + 250).min(DELETED_EVENTS))
            .map(|index| events.prepare(indexed_event(42, index)))
            .collect::<Result<Vec<_>, _>>()?;
        events.insert_batch(&prepared).await?;
    }
    store
        .request_deletion(deletion_request(
            ProjectDeletionOperationId::from_bytes([5; 16]),
            2_000,
            2_000,
        ))
        .await?;

    let active_store = events.clone();
    let active = tokio::spawn(async move {
        for start in (0..ACTIVE_EVENTS).step_by(250) {
            let prepared = (start..(start + 250).min(ACTIVE_EVENTS))
                .map(|index| active_store.prepare(indexed_event(43, index)))
                .collect::<Result<Vec<_>, _>>()?;
            active_store.insert_batch(&prepared).await?;
        }
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });
    let started = std::time::Instant::now();
    let mut samples = Vec::new();
    for _ in 0..64 {
        let batch_started = std::time::Instant::now();
        let status = store
            .purge_next(ProjectPurgeRequest {
                now: Timestamp::from_unix_millis(3_000)?,
                batch_size: 1_000,
                retry_base: Duration::from_millis(1),
                retry_max: Duration::from_secs(1),
                completed_retention: Duration::from_secs(60),
                slug_reservation: Duration::from_secs(30),
            })
            .await?;
        samples.push(batch_started.elapsed());
        if status
            .as_ref()
            .is_some_and(|value| value.phase == ProjectDeletionPhase::Deleted)
        {
            break;
        }
    }
    active.await.unwrap().unwrap();
    let elapsed = started.elapsed();
    samples.sort_unstable();
    let p95 = samples[(samples.len() - 1) * 95 / 100];
    let rps = f64::from(DELETED_EVENTS) / elapsed.as_secs_f64();
    let deleted_remaining = database
        .collection::<mongodb::bson::Document>("error_events")
        .count_documents(doc! { "p": 42 })
        .await?;
    let active_count = database
        .collection::<mongodb::bson::Document>("error_events")
        .count_documents(doc! { "p": 43 })
        .await?;
    eprintln!(
        "project deletion purge: {rps:.2} RPS, documents={DELETED_EVENTS}, elapsed_ms={}, batch_p95_ms={}, active_project_documents={active_count}",
        elapsed.as_millis(),
        p95.as_millis(),
    );
    assert_eq!(deleted_remaining, 0);
    assert_eq!(active_count, u64::from(ACTIVE_EVENTS));
    Ok(())
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let store = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    store.bootstrap_or_validate().await?;
    store.insert_organization(organization()).await?;
    store.insert_project(project(42, "backend")).await?;
    store.insert_project(project(43, "frontend")).await?;
    let active = DsnKey::from_bytes([3; 16]);
    let disabled = DsnKey::from_bytes([4; 16]);
    store
        .insert_project_key(project_key(42, active, ProjectKeyState::Active))
        .await?;
    store
        .insert_project_key(project_key(42, disabled, ProjectKeyState::Disabled))
        .await?;

    let first = ProjectDeletionOperationId::from_bytes([1; 16]);
    let request = deletion_request(first, 2_000, 20_000);
    let accepted = store.request_deletion(request).await?;
    assert_eq!(accepted.status.phase, ProjectDeletionPhase::PendingGrace);
    let repeated = store.request_deletion(request).await?;
    assert_eq!(repeated.status.operation_id, first);
    let keys = store.list_project_keys(ProjectId::new(42)?).await?;
    assert_eq!(
        keys.iter().find(|key| key.key == active).unwrap().state,
        ProjectKeyState::SuspendedByDeletion
    );
    assert_eq!(
        keys.iter().find(|key| key.key == disabled).unwrap().state,
        ProjectKeyState::Disabled
    );

    let cancelled = store
        .cancel_deletion(
            ProjectId::new(42)?,
            first,
            Timestamp::from_unix_millis(3_000)?,
            Duration::from_secs(60),
        )
        .await?;
    assert_eq!(cancelled.status.phase, ProjectDeletionPhase::Cancelled);
    let keys = store.list_project_keys(ProjectId::new(42)?).await?;
    assert_eq!(
        keys.iter().find(|key| key.key == active).unwrap().state,
        ProjectKeyState::Active
    );
    assert_eq!(
        keys.iter().find(|key| key.key == disabled).unwrap().state,
        ProjectKeyState::Disabled
    );

    let event_store = store.event_store(EventCodecConfig::default());
    let deleted_event = event_store.prepare(event(42, 9))?;
    let active_event = event_store.prepare(event(43, 8))?;
    event_store
        .insert_batch(&[deleted_event, active_event])
        .await?;
    database
        .collection::<mongodb::bson::Document>("releases")
        .insert_one(doc! {
            "_id": generic_binary(&[7; 16]),
            "organization_id": 1_i64,
            "version": "shared@1.0.0",
            "status": "open",
            "project_ids": [42_i32, 43_i32],
            "first_seen": DateTime::from_millis(1_000),
            "last_seen": DateTime::from_millis(2_000),
            "first_event_id": generic_binary(&[1; 20]),
            "latest_event_id": generic_binary(&[2; 20]),
            "created_at": DateTime::from_millis(1_000),
            "source": "event",
        })
        .await?;

    let second = ProjectDeletionOperationId::from_bytes([2; 16]);
    store
        .request_deletion(deletion_request(second, 4_000, 4_000))
        .await?;

    // A new adapter instance simulates process restart while the durable job remains.
    let mut late_inflight_inserted = false;
    for _ in 0..40 {
        // Recreate the adapter before every bounded step: every persisted phase/cursor
        // must be sufficient to resume after a process crash.
        let restarted =
            MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
        let status = restarted
            .purge_next(ProjectPurgeRequest {
                now: Timestamp::from_unix_millis(5_000)?,
                batch_size: 1,
                retry_base: Duration::from_millis(1),
                retry_max: Duration::from_secs(1),
                completed_retention: Duration::from_secs(60),
                slug_reservation: Duration::from_secs(30),
            })
            .await?;
        if status.as_ref().is_some_and(|value| {
            value.phase == ProjectDeletionPhase::Purging
                && value.reconciliation_pass
                && value.dataset_code == 10
        }) && !late_inflight_inserted
        {
            let late = event_store.prepare(event(42, 6))?;
            event_store.insert_batch(&[late]).await?;
            late_inflight_inserted = true;
        }
        if status
            .as_ref()
            .is_some_and(|value| value.phase == ProjectDeletionPhase::Deleted)
        {
            break;
        }
    }
    let restarted =
        MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    let status = restarted.deletion_status(ProjectId::new(42)?).await?;
    assert!(late_inflight_inserted);
    assert_eq!(status.phase, ProjectDeletionPhase::Deleted);
    assert!(status.reconciliation_pass);
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("error_events")
            .count_documents(doc! { "p": 42 })
            .await?,
        0
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("error_events")
            .count_documents(doc! { "p": 43 })
            .await?,
        1
    );
    let shared_release = database
        .collection::<mongodb::bson::Document>("releases")
        .find_one(doc! { "_id": generic_binary(&[7; 16]) })
        .await?
        .unwrap();
    assert_eq!(shared_release.get_array("project_ids")?, &[43_i32.into()]);
    assert_eq!(
        store.load_project_by_id(ProjectId::new(43)?).await?.state,
        ProjectAcceptanceState::Active
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("project_keys")
            .count_documents(doc! { "project_id": 42 })
            .await?,
        0
    );
    let tombstone = database
        .collection::<mongodb::bson::Document>("projects")
        .find_one(doc! { "_id": 42 })
        .await?
        .unwrap();
    assert_eq!(tombstone.get_str("state")?, "deleted");
    assert!(!tombstone.contains_key("policy"));
    assert!(tombstone.contains_key("slug"));

    restarted
        .purge_next(ProjectPurgeRequest {
            now: Timestamp::from_unix_millis(40_000)?,
            batch_size: 1,
            retry_base: Duration::from_millis(1),
            retry_max: Duration::from_secs(1),
            completed_retention: Duration::from_secs(60),
            slug_reservation: Duration::from_secs(30),
        })
        .await?;
    let tombstone = database
        .collection::<mongodb::bson::Document>("projects")
        .find_one(doc! { "_id": 42 })
        .await?
        .unwrap();
    assert!(!tombstone.contains_key("slug"));
    Ok(())
}

fn deletion_request(
    operation_id: ProjectDeletionOperationId,
    requested_at: i64,
    purge_after: i64,
) -> ProjectDeletionRequest {
    ProjectDeletionRequest {
        operation_id,
        project_id: ProjectId::new(42).unwrap(),
        organization_id: OrganizationId::new(1).unwrap(),
        requested_by: UserId::new(7).unwrap(),
        requested_at: Timestamp::from_unix_millis(requested_at).unwrap(),
        purge_after: Timestamp::from_unix_millis(purge_after).unwrap(),
    }
}

fn organization() -> OrganizationIdentity {
    OrganizationIdentity {
        id: OrganizationId::new(1).unwrap(),
        slug: Slug::new("acme").unwrap(),
        display_name: DisplayName::new("Acme").unwrap(),
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}

fn project(id: i32, slug: &str) -> ProjectIdentity {
    ProjectIdentity {
        id: ProjectId::new(id).unwrap(),
        organization_id: OrganizationId::new(1).unwrap(),
        slug: Slug::new(slug).unwrap(),
        display_name: DisplayName::new("Backend").unwrap(),
        state: ProjectAcceptanceState::Active,
        policy_revision: 1,
        ip_policy: IpScrubPolicy::Hmac,
        items: ItemCapabilities {
            error: true,
            client_report: true,
            log: true,
            transaction: true,
            span: true,
        },
        limits: ProjectIngestLimits::default(),
        grouping_revision: 1,
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}

fn project_key(project_id: i32, key: DsnKey, state: ProjectKeyState) -> ProjectKeyIdentity {
    ProjectKeyIdentity {
        key,
        project_id: ProjectId::new(project_id).unwrap(),
        state,
        label: ProjectKeyLabel::new("test").unwrap(),
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}

fn event(project_id: i32, byte: u8) -> AcceptedEvent {
    AcceptedEvent {
        project_id: ProjectId::new(project_id).unwrap(),
        event_id: EventId::from_bytes([byte; 16]),
        received_at: Timestamp::from_unix_millis(1_500).unwrap(),
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(
                r#"{{"event_id":"{}","message":"project isolation"}}"#,
                format!("{byte:02x}").repeat(16)
            )
            .into_bytes(),
        ),
    }
}

fn indexed_event(project_id: i32, index: u32) -> AcceptedEvent {
    let mut id = [0_u8; 16];
    id[12..].copy_from_slice(&index.to_be_bytes());
    let encoded_id = id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    AcceptedEvent {
        project_id: ProjectId::new(project_id).unwrap(),
        event_id: EventId::from_bytes(id),
        received_at: Timestamp::from_unix_millis(1_500).unwrap(),
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(r#"{{"event_id":"{encoded_id}","message":"phase 15 purge baseline"}}"#)
                .into_bytes(),
        ),
    }
}

fn generic_binary(bytes: &[u8]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://127.0.0.1:27017/?serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "metric_phase15_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
