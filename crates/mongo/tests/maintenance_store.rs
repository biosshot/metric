use std::{
    error::Error,
    num::NonZeroU64,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use faultkeep_domain::{
    AcceptedEvent, DisplayName, EventId, EventKey, IpScrubPolicy, ItemCapabilities, OrganizationId,
    OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits,
    ScrubbedEventPayload, SecretBytes, Slug, Timestamp,
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, derive_issue_id,
    },
    issue::{IssueCulprit, IssueGroupingDetail, IssueOccurrence, IssueTitle},
};
use faultkeep_mongo::{
    EventCodecConfig, IssueCodecConfig, MongoEventStore, MongoIssueStore, MongoMaintenanceStore,
    MongoProjectStore,
};
use faultkeep_ports::{
    EventStore, EventWriteStatus, IssueStore, MaintenanceDisposition, MaintenanceRequest,
    MaintenanceStore, MaintenanceTask, ProjectStore,
};
use mongodb::{
    Client, Database,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by FAULTKEEP_TEST_MONGODB_URI"]
async fn infrastructure_retention_pending_safety_reconciliation_and_bounded_plans() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    control.insert_organization(organization()).await?;
    control.insert_project(project()).await?;

    let now_millis = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let day = 24 * 60 * 60 * 1_000_i64;
    let event_store = MongoEventStore::from_database(database.clone(), EventCodecConfig::default());
    let pending = event(1, now_millis - 40 * day);
    let processed_a = event(2, now_millis - day);
    let processed_b = event(3, now_millis - day);
    let failed = event(4, now_millis - 40 * day);
    let prepared = [pending, processed_a, processed_b, failed]
        .into_iter()
        .map(|event| event_store.prepare(event))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        event_store
            .insert_batch(&prepared)
            .await?
            .into_iter()
            .all(|status| status == EventWriteStatus::Inserted)
    );

    let occurrence = occurrence(now_millis - day);
    MongoIssueStore::from_database(database.clone(), IssueCodecConfig::default())
        .apply_occurrence(occurrence.clone())
        .await?;
    let events = database.collection::<Document>("error_events");
    for seed in [2_u8, 3] {
        events
            .update_one(
                doc! { "_id": event_key(seed) },
                doc! {
                    "$set": {
                        "u": binary(occurrence.issue_id.as_bytes()),
                        "x": DateTime::from_millis(now_millis + 30 * day),
                    },
                    "$unset": { "q": "" },
                },
            )
            .await?;
    }
    events
        .update_one(
            doc! { "_id": event_key(4) },
            doc! {
                "$set": { "q.s": 1_i32, "q.a": 1_i32, "q.c": 1_i32 },
                "$unset": { "q.n": "" },
            },
        )
        .await?;

    let environment_id = [9_u8; 16];
    database
        .collection::<Document>("environments")
        .insert_one(doc! {
            "_id": binary(environment_id),
            "project_id": 7_i32,
            "name": "production",
            "first_seen": DateTime::from_millis(now_millis - day),
            "last_seen": DateTime::from_millis(now_millis),
            "hidden": false,
            "source": "event",
        })
        .await?;
    database
        .collection::<Document>("issue_stats_hourly")
        .insert_one(doc! {
            "_id": binary([8_u8; 16]),
            "project_id": 7_i32,
            "issue_id": binary(occurrence.issue_id.as_bytes()),
            "bucket_start": DateTime::from_millis(now_millis - day),
            "occurrence_count": 2_i64,
            "expire_at": DateTime::from_millis(now_millis + 400 * day),
        })
        .await?;

    let maintenance = MongoMaintenanceStore::from_database(database.clone());
    let backlog = maintenance
        .run(request(MaintenanceTask::RetryBacklog, now_millis, 1))
        .await?;
    assert_eq!(backlog.scanned, 1);
    for task in [
        MaintenanceTask::UploadExpiry,
        MaintenanceTask::BlobOrphanRegistration,
    ] {
        assert_eq!(
            maintenance
                .run(request(task, now_millis, 1))
                .await?
                .disposition,
            MaintenanceDisposition::Disabled
        );
    }
    run_complete_pass(
        &maintenance,
        request(MaintenanceTask::EventRetention, now_millis, 1),
    )
    .await?;

    let pending = events
        .find_one(doc! { "_id": event_key(1) })
        .await?
        .unwrap();
    assert_eq!(pending.get_document("q")?.get_i32("s"), Ok(0));
    assert!(
        !pending.contains_key("x"),
        "pending Event received an expiration"
    );
    assert!(
        events
            .find_one(doc! { "_id": event_key(4) })
            .await?
            .is_none()
    );
    for seed in [2_u8, 3] {
        let processed = events
            .find_one(doc! { "_id": event_key(seed) })
            .await?
            .unwrap();
        assert_eq!(
            processed.get_datetime("x")?.timestamp_millis(),
            now_millis - day + 10 * day
        );
    }

    run_complete_pass(
        &maintenance,
        request(MaintenanceTask::HourlyRetention, now_millis, 1),
    )
    .await?;
    assert_eq!(
        database
            .collection::<Document>("issue_stats_hourly")
            .find_one(doc! { "_id": binary([8_u8; 16]) })
            .await?
            .unwrap()
            .get_datetime("expire_at")?
            .timestamp_millis(),
        now_millis - day + 20 * day
    );

    run_complete_pass(
        &maintenance,
        request(MaintenanceTask::CounterReconciliation, now_millis, 10),
    )
    .await?;
    let issue = database
        .collection::<Document>("issues")
        .find_one(doc! { "_id": binary(occurrence.issue_id.as_bytes()) })
        .await?
        .unwrap();
    assert_eq!(issue.get_i64("c"), Ok(2));
    let project = database
        .collection::<Document>("projects")
        .find_one(doc! { "_id": 7_i32 })
        .await?
        .unwrap();
    assert_eq!(project.get_document("catalog_usage")?.get_i32("ec"), Ok(1));

    assert_bounded_plans(database, occurrence.issue_id.as_bytes()).await?;
    Ok(())
}

async fn run_complete_pass(
    store: &MongoMaintenanceStore,
    mut request: MaintenanceRequest,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..32 {
        let result = store.run(request.clone()).await?;
        assert_eq!(result.disposition, MaintenanceDisposition::Completed);
        assert!(result.scanned <= request.batch_size);
        let Some(cursor) = result.next_cursor else {
            return Ok(());
        };
        request.cursor = Some(cursor);
    }
    Err("bounded maintenance pass did not terminate".into())
}

async fn assert_bounded_plans(
    database: &Database,
    issue_id: [u8; 16],
) -> Result<(), Box<dyn Error>> {
    for (command, index) in [
        (
            doc! {
                "explain": {
                    "find": "error_events",
                    "filter": {},
                    "sort": { "_id": 1 },
                    "hint": "_id_",
                    "limit": 1_i64,
                },
                "verbosity": "executionStats",
            },
            "_id_",
        ),
        (
            doc! {
                "explain": {
                    "find": "error_events",
                    "filter": { "p": 7_i32, "u": binary(issue_id) },
                    "hint": "event_issue_timeline",
                    "limit": 10_i64,
                },
                "verbosity": "executionStats",
            },
            "event_issue_timeline",
        ),
        (
            doc! {
                "explain": {
                    "find": "environments",
                    "filter": { "project_id": 7_i32 },
                    "hint": "environment_project_timeline",
                    "limit": 10_i64,
                },
                "verbosity": "executionStats",
            },
            "environment_project_timeline",
        ),
    ] {
        let explain = database.run_command(command).await?;
        let rendered = format!("{explain:?}");
        assert!(
            rendered.contains(index),
            "bounded query plan did not use {index}: {rendered}"
        );
    }
    Ok(())
}

fn request(task: MaintenanceTask, now_millis: i64, batch_size: usize) -> MaintenanceRequest {
    MaintenanceRequest {
        task,
        now: Timestamp::from_unix_millis(now_millis).unwrap(),
        cursor: None,
        batch_size,
        event_retention: Duration::from_secs(10 * 24 * 60 * 60),
        hourly_retention: Duration::from_secs(20 * 24 * 60 * 60),
        archive_events: false,
    }
}

fn organization() -> OrganizationIdentity {
    OrganizationIdentity {
        id: OrganizationId::new(1).unwrap(),
        slug: Slug::new("phase14-org").unwrap(),
        display_name: DisplayName::new("Phase 14").unwrap(),
        created_at: Timestamp::from_unix_millis(1).unwrap(),
    }
}

fn project() -> ProjectIdentity {
    ProjectIdentity {
        id: ProjectId::new(7).unwrap(),
        organization_id: OrganizationId::new(1).unwrap(),
        slug: Slug::new("phase14-project").unwrap(),
        display_name: DisplayName::new("Phase 14 Project").unwrap(),
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
        created_at: Timestamp::from_unix_millis(1).unwrap(),
    }
}

fn event(seed: u8, received_at: i64) -> AcceptedEvent {
    let event_id = EventId::from_bytes([seed; 16]);
    AcceptedEvent {
        project_id: ProjectId::new(7).unwrap(),
        event_id,
        received_at: Timestamp::from_unix_millis(received_at).unwrap(),
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            format!(r#"{{"event_id":"{event_id}","platform":"rust","message":"phase14-{seed}"}}"#)
                .into_bytes(),
        ),
    }
}

fn occurrence(at: i64) -> IssueOccurrence {
    let project_id = ProjectId::new(7).unwrap();
    let mut bytes = [5_u8; 34];
    bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    let grouping_key = GroupingKey::parse(&bytes).unwrap();
    IssueOccurrence {
        project_id,
        issue_id: derive_issue_id(project_id, grouping_key),
        grouping_key,
        event_id: EventId::from_bytes([2; 16]),
        occurred_at: Timestamp::from_unix_millis(at).unwrap(),
        received_at: Timestamp::from_unix_millis(at).unwrap(),
        release: None,
        title: IssueTitle::new("Phase 14 retained issue").unwrap(),
        culprit: Some(IssueCulprit::new("phase14::exercise").unwrap()),
        grouping: IssueGroupingDetail {
            strategy: GroupingStrategy::Message,
            explanation: GroupingExplanation {
                summary: "phase14 reconciliation fixture".into(),
                components: vec![GroupingComponent {
                    kind: GroupingComponentKind::Message,
                    value: "phase14 retained issue".into(),
                }],
            },
        },
        increment: NonZeroU64::MIN,
    }
}

fn event_key(seed: u8) -> Bson {
    Bson::Binary(binary(
        EventKey::new(ProjectId::new(7).unwrap(), EventId::from_bytes([seed; 16])).as_bytes(),
    ))
}

fn binary<const N: usize>(bytes: [u8; N]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("FAULTKEEP_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://127.0.0.1:27017/?retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000"
            .to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "faultkeep_phase14_maintenance_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
