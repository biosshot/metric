use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use metric_domain::{
    EventId, ProjectId, SecretBytes, Timestamp,
    blob::{
        AttachmentFilename, BlobChecksum, BlobContentType, BlobKey, BlobKind, BlobObject,
        BlobObjectId, EventAttachment,
    },
    feedback::{FeedbackRecord, FeedbackStatus},
    signals::TraceId,
};
use metric_mongo::{EventCodecConfig, MongoProjectStore};
use metric_ports::{
    BlobReference, BlobReferenceStore, DurableOutcome, FeedbackQuery, FeedbackSink, FeedbackStore,
};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn feedback_metadata_workflow_and_blob_reference_are_durable() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = control.feedback_store();
    let project_id = ProjectId::new(42)?;
    let feedback_id = EventId::from_bytes([3; 16]);
    let object_id = BlobObjectId::from_bytes([4; 16]);
    let received_at = Timestamp::from_unix_millis(1_700_000_000_000)?;
    let record = FeedbackRecord {
        project_id,
        feedback_id,
        received_at,
        status: FeedbackStatus::Open,
        status_changed_at: received_at,
        message: "Checkout did not respond".into(),
        name: Some("Ada".into()),
        contact_email: Some("ada@example.com".into()),
        url: Some("https://example.test/checkout".into()),
        associated_event_id: Some(EventId::from_bytes([8; 16])),
        issue_id: None,
        trace_id: Some(TraceId::from_bytes([9; 16])),
        replay_id: Some(EventId::from_bytes([10; 16])),
        attachments: vec![EventAttachment {
            attachment_id: object_id,
            blob: BlobObject {
                key: BlobKey::event_owned(project_id, feedback_id, object_id),
                kind: BlobKind::EventAttachment,
                size: 24,
                checksum: BlobChecksum::from_bytes([5; 32]),
                created_at: received_at,
            },
            filename: AttachmentFilename::sanitized("context.txt")?,
            content_type: BlobContentType::new("text/plain")?,
            attachment_type: "event.attachment".into(),
        }],
        expires_at: Timestamp::from_unix_millis(1_707_776_000_000)?,
    };
    assert_eq!(
        store.persist_feedback(record.clone()).await?,
        DurableOutcome::Accepted
    );
    let mut retry = record.clone();
    retry.received_at = Timestamp::from_unix_millis(1_700_000_000_100)?;
    retry.status_changed_at = retry.received_at;
    retry.expires_at = Timestamp::from_unix_millis(1_707_776_000_100)?;
    retry.attachments[0].blob.created_at = retry.received_at;
    assert_eq!(
        store.persist_feedback(retry).await?,
        DurableOutcome::Duplicate
    );
    assert_eq!(store.load_feedback(project_id, feedback_id).await?, record);
    let page = store
        .list_feedback(
            project_id,
            FeedbackQuery {
                status: Some(FeedbackStatus::Open),
                event_id: None,
                trace_id: None,
                replay_id: None,
                before: None,
                limit: 10,
            },
        )
        .await?;
    assert_eq!(page.items, vec![record]);
    for query in [
        FeedbackQuery {
            status: None,
            event_id: Some(EventId::from_bytes([8; 16])),
            trace_id: None,
            replay_id: None,
            before: None,
            limit: 10,
        },
        FeedbackQuery {
            status: None,
            event_id: None,
            trace_id: Some(TraceId::from_bytes([9; 16])),
            replay_id: None,
            before: None,
            limit: 10,
        },
        FeedbackQuery {
            status: None,
            event_id: None,
            trace_id: None,
            replay_id: Some(EventId::from_bytes([10; 16])),
            before: None,
            limit: 10,
        },
    ] {
        assert_eq!(store.list_feedback(project_id, query).await?.items.len(), 1);
    }
    assert_eq!(
        store
            .update_feedback_status(
                project_id,
                feedback_id,
                FeedbackStatus::Resolved,
                Timestamp::from_unix_millis(1_700_000_000_100)?,
            )
            .await?
            .status,
        FeedbackStatus::Resolved
    );
    assert!(
        control
            .event_store(EventCodecConfig::default())
            .is_referenced(BlobReference {
                project_id,
                event_id: feedback_id,
                object_id,
            })
            .await?
    );
    Ok(())
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(client.database(&format!(
        "metric_phase31_feedback_{}_{}",
        std::process::id(),
        nonce
    )))
}
