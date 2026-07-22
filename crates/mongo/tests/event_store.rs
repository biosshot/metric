use std::error::Error;

use faultkeep_domain::{
    AcceptedEvent, EventId, EventKey, ProjectId, ScrubbedEventPayload, SecretBytes, Timestamp,
};
use faultkeep_mongo::{
    EventCodecConfig, MongoBootstrapError, MongoEventStore, MongoProjectStore, decode_pending_event,
};
use faultkeep_ports::{EventStore, EventStoreError, EventWriteStatus, PreparedEvent};
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

async fn exercise(client: &Client, database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = MongoEventStore::from_database(database.clone(), EventCodecConfig::default());
    let collection = database.collection::<mongodb::bson::Document>("events");

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

async fn test_database() -> Result<(Client, Database), mongodb::error::Error> {
    let uri = std::env::var("FAULTKEEP_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://faultkeep:faultkeep-local-only@127.0.0.1:27018/?authSource=admin&retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    let database = client.database(&format!(
        "faultkeep_phase3_event_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    ));
    Ok((client, database))
}
