use std::error::Error;
use std::time::Instant;

use metric_domain::{
    AcceptedEvent, DisplayName, EventId, EventKey, IpScrubPolicy, ItemCapabilities, OrganizationId,
    OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits,
    ScrubbedEventPayload, SecretBytes, Slug, Timestamp,
};
use metric_mongo::{
    EventCodecConfig, MongoBootstrapError, MongoEventStore, MongoProjectStore, decode_pending_event,
};
use metric_ports::{
    EventBacklog, EventStore, EventStoreError, EventWriteStatus, PreparedEvent, ProjectStore,
};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_event_schema_unordered_duplicates_and_retry_identity() {
    let (client, database) = test_database().await.unwrap();
    let result = exercise(&client, &database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "performance baseline requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn performance_dispatcher_mongodb_refill_rps() {
    let (_, database) = test_database().await.unwrap();
    let result = measure_refill(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn measure_refill(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    control.insert_organization(organization()).await?;
    control.insert_project(project()).await?;
    let store = MongoEventStore::from_database(database.clone(), EventCodecConfig::default());
    const EVENTS: u32 = 20_000;
    for start in (0..EVENTS).step_by(500) {
        let prepared = (start..(start + 500).min(EVENTS))
            .map(|index| store.prepare(refill_event(index)))
            .collect::<Result<Vec<_>, _>>()?;
        let statuses = store.insert_batch(&prepared).await?;
        assert!(
            statuses
                .iter()
                .all(|status| *status == EventWriteStatus::Inserted)
        );
    }
    let started = Instant::now();
    let loaded = store
        .load_due(
            Timestamp::from_unix_millis(1_000_000)?,
            EVENTS as usize,
            &[],
        )
        .await?;
    let elapsed = started.elapsed();
    assert_eq!(loaded.len(), EVENTS as usize);
    assert_eq!(
        loaded.first().unwrap().event.event_id,
        EventId::from_bytes([0; 16])
    );
    assert_eq!(
        loaded.last().unwrap().event.event_id,
        EventId::from_bytes(u128::from(EVENTS - 1).to_be_bytes())
    );
    let rps = f64::from(EVENTS) / elapsed.as_secs_f64();
    eprintln!(
        "Dispatcher MongoDB refill: {rps:.0} events/s, events={EVENTS}, elapsed_ms={}",
        elapsed.as_millis()
    );
    assert!(
        rps >= 7_500.0,
        "Dispatcher refill {rps:.0} RPS is below recovery gate"
    );
    Ok(())
}

async fn exercise(client: &Client, database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    control.insert_organization(organization()).await?;
    control.insert_project(project()).await?;
    let store = MongoEventStore::from_database(database.clone(), EventCodecConfig::default());
    let collection = database.collection::<mongodb::bson::Document>("error_events");

    let index_names = collection.list_index_names().await?;
    for required in [
        "event_pending_due",
        "event_project_timeline",
        "event_issue_timeline",
        "event_search_tokens",
        "event_expiration",
        "event_archive_due",
    ] {
        assert!(
            index_names.iter().any(|name| name == required),
            "{required}"
        );
    }
    assert!(
        collection
            .insert_one(doc! { "_id": "invalid" })
            .await
            .is_err(),
        "strict validator must reject malformed Events"
    );

    let first = store.prepare(event(1, 1_000))?;
    assert_eq!(
        store.insert_batch(std::slice::from_ref(&first)).await?,
        [EventWriteStatus::Inserted]
    );
    let second = store.prepare(event(2, 2_000))?;
    let duplicate = store.prepare(event(1, 3_000))?;
    let third = store.prepare(event(3, 4_000))?;
    let partial = store.insert_batch(&[second, duplicate, third]).await?;
    assert_eq!(
        partial,
        [
            EventWriteStatus::Inserted,
            EventWriteStatus::Duplicate,
            EventWriteStatus::Inserted
        ]
    );
    assert_eq!(collection.count_documents(doc! {}).await?, 3);

    let observation = store.observe(1_000).await?;
    assert_eq!(observation.pending_count, 3);
    assert_eq!(observation.oldest_pending_at.unwrap().unix_millis(), 1_000);
    let excluded = EventKey::new(ProjectId::new(42)?, EventId::from_bytes([1; 16]));
    let due = store
        .load_due(Timestamp::from_unix_millis(5_000)?, 10, &[excluded])
        .await?;
    assert_eq!(
        due.iter()
            .map(|event| event.event.event_id)
            .collect::<Vec<_>>(),
        [EventId::from_bytes([2; 16]), EventId::from_bytes([3; 16])]
    );
    collection
        .update_one(
            doc! { "_id": mongodb::bson::Binary {
                subtype: mongodb::bson::spec::BinarySubtype::Generic,
                bytes: EventKey::new(ProjectId::new(42)?, EventId::from_bytes([2; 16])).as_bytes().to_vec(),
            }},
            doc! { "$set": { "q.n": mongodb::bson::DateTime::from_millis(6_000) } },
        )
        .await?;
    let due = store
        .load_due(Timestamp::from_unix_millis(5_000)?, 10, &[])
        .await?;
    assert_eq!(
        due.iter()
            .map(|event| event.event.event_id)
            .collect::<Vec<_>>(),
        [EventId::from_bytes([1; 16]), EventId::from_bytes([3; 16])]
    );
    control
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::PendingDelete)
        .await?;
    assert!(
        store
            .load_due(Timestamp::from_unix_millis(10_000)?, 10, &[])
            .await?
            .is_empty()
    );

    let first_key = EventKey::new(ProjectId::new(42)?, EventId::from_bytes([1; 16]));
    let document = collection
        .find_one(doc! { "_id": mongodb::bson::Binary {
            subtype: mongodb::bson::spec::BinarySubtype::Generic,
            bytes: first_key.as_bytes().to_vec(),
        }})
        .await?
        .unwrap();
    assert_eq!(
        decode_pending_event(&document, EventCodecConfig::default())?.event_id,
        EventId::from_bytes([1; 16])
    );

    client
        .database("admin")
        .run_command(doc! {
            "configureFailPoint": "failCommand",
            "mode": { "times": 1 },
            "data": {
                "failCommands": ["insert"],
                "closeConnection": true,
            },
        })
        .await?;
    let ambiguous = store.prepare(event(4, 5_000))?;
    assert_eq!(
        store.insert_batch(std::slice::from_ref(&ambiguous)).await,
        Err(EventStoreError::Ambiguous)
    );
    let retry = store.insert_batch(std::slice::from_ref(&ambiguous)).await?;
    assert!(matches!(
        retry.as_slice(),
        [EventWriteStatus::Inserted] | [EventWriteStatus::Duplicate]
    ));
    assert_eq!(
        collection
            .count_documents(doc! { "_id": mongodb::bson::Binary {
                subtype: mongodb::bson::spec::BinarySubtype::Generic,
                bytes: ambiguous.key().as_bytes().to_vec(),
            }})
            .await?,
        1
    );
    assert_eq!(
        store.insert_batch(std::slice::from_ref(&ambiguous)).await?,
        [EventWriteStatus::Duplicate]
    );

    collection.drop_index("event_pending_due").await?;
    collection
        .create_index(
            mongodb::IndexModel::builder()
                .keys(doc! { "r": 1 })
                .options(
                    mongodb::options::IndexOptions::builder()
                        .name("event_pending_due".to_owned())
                        .build(),
                )
                .build(),
        )
        .await?;
    assert!(matches!(
        control.bootstrap_or_validate().await,
        Err(MongoBootstrapError::IncompatibleSchema)
    ));
    Ok(())
}

fn organization() -> OrganizationIdentity {
    OrganizationIdentity {
        id: OrganizationId::new(1).unwrap(),
        slug: Slug::new("phase4-org").unwrap(),
        display_name: DisplayName::new("Phase 4").unwrap(),
        created_at: Timestamp::from_unix_millis(1).unwrap(),
    }
}

fn project() -> ProjectIdentity {
    ProjectIdentity {
        id: ProjectId::new(42).unwrap(),
        organization_id: OrganizationId::new(1).unwrap(),
        slug: Slug::new("phase4-project").unwrap(),
        display_name: DisplayName::new("Phase 4 Project").unwrap(),
        state: ProjectAcceptanceState::Active,
        policy_revision: 1,
        ip_policy: IpScrubPolicy::Hmac,
        items: ItemCapabilities {
            error: true,
            client_report: true,
            log: true,
            transaction: true,
            span: true,
            feedback: true,
            check_in: true,
        },
        limits: ProjectIngestLimits::default(),
        grouping_revision: 1,
        created_at: Timestamp::from_unix_millis(1).unwrap(),
    }
}

fn event(byte: u8, received_at: i64) -> AcceptedEvent {
    AcceptedEvent {
        project_id: ProjectId::new(42).unwrap(),
        event_id: EventId::from_bytes([byte; 16]),
        received_at: Timestamp::from_unix_millis(received_at).unwrap(),
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(
                r#"{{"event_id":"{}","level":"error","platform":"rust","message":"event-{byte}"}}"#,
                format!("{byte:02x}").repeat(16)
            )
            .into_bytes(),
        ),
    }
}

fn refill_event(index: u32) -> AcceptedEvent {
    let event_id = EventId::from_bytes(u128::from(index).to_be_bytes());
    AcceptedEvent {
        project_id: ProjectId::new(42).unwrap(),
        event_id,
        received_at: Timestamp::from_unix_millis(1_000 + i64::from(index)).unwrap(),
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(
                r#"{{"event_id":"{event_id}","platform":"rust","level":"error","message":"refill-{index}"}}"#
            )
            .into_bytes(),
        ),
    }
}

async fn test_database() -> Result<(Client, Database), mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    let database = client.database(&format!(
        "metric_phase3_event_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    ));
    Ok((client, database))
}
