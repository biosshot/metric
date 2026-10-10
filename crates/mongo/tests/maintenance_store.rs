use std::{
    error::Error,
    num::NonZeroU64,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use metric_domain::{
    AcceptedEvent, DisplayName, EventId, EventKey, IpScrubPolicy, ItemCapabilities, OrganizationId,
    OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits,
    ScrubbedEventPayload, SecretBytes, Slug, Timestamp,
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, derive_issue_id,
    },
    issue::{IssueCulprit, IssueGroupingDetail, IssueOccurrence, IssueTitle},
};
use metric_mongo::{
    EventCodecConfig, IssueCodecConfig, MongoEventStore, MongoIssueStore, MongoMaintenanceStore,
    MongoProjectStore,
};
use metric_ports::{
    EventStore, EventWriteStatus, IssueStore, MaintenanceDisposition, MaintenanceRequest,
    MaintenanceStore, MaintenanceTask, ProjectStore,
};
use mongodb::{
    Client, Database,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
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

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn catalog_counter_counts_above_page_size_and_preserves_reservations() {
    let database = test_database().await.unwrap();
    let result = exercise_catalog_counter(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise_catalog_counter(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    control.insert_organization(organization()).await?;
    control.insert_project(project()).await?;
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let environments = database.collection::<Document>("environments");
    for seed in 1_u8..=5 {
        environments
            .insert_one(doc! {
                "_id": binary([seed; 16]), "project_id": 7_i32,
                "name": format!("synthetic-{seed}"), "first_seen": DateTime::from_millis(now),
                "last_seen": DateTime::from_millis(now), "hidden": false, "source": "event",
            })
            .await?;
    }
    let projects = database.collection::<Document>("projects");
    let maintenance = MongoMaintenanceStore::from_database(database.clone());
    // Exact count, including repair of an already truncated value. A reservation
    // may precede materialization, so a larger counter must never be lowered.
    for (initial, expected) in [(5_i32, 5_i32), (1, 5), (0, 5), (6, 6)] {
        let update = if initial == 0 {
            doc! { "$unset": { "catalog_usage.ec": "" } }
        } else {
            doc! { "$set": { "catalog_usage.ec": initial } }
        };
        projects.update_one(doc! { "_id": 7_i32 }, update).await?;
        run_complete_pass(
            &maintenance,
            request(MaintenanceTask::CounterReconciliation, now, 1),
        )
        .await?;
        let project = projects.find_one(doc! { "_id": 7_i32 }).await?.unwrap();
        assert_eq!(
            project.get_document("catalog_usage")?.get_i32("ec")?,
            expected
        );
        assert_eq!(
            environments
                .count_documents(doc! { "project_id": 7_i32 })
                .await?,
            5
        );
    }
    // Concurrent foreground reservations must survive every maintenance pass.
    let (reserved, reconciled) = tokio::join!(
        async {
            for _ in 0..20 {
                projects
                    .update_one(
                        doc! { "_id": 7_i32 },
                        doc! { "$inc": { "catalog_usage.ec": 1_i32 } },
                    )
                    .await?;
            }
            Ok::<_, Box<dyn Error>>(())
        },
        async {
            for _ in 0..20 {
                run_complete_pass(
                    &maintenance,
                    request(MaintenanceTask::CounterReconciliation, now, 1),
                )
                .await?;
            }
            Ok::<_, Box<dyn Error>>(())
        }
    );
    reserved?;
    reconciled?;
    let project = projects.find_one(doc! { "_id": 7_i32 }).await?.unwrap();
    assert_eq!(project.get_document("catalog_usage")?.get_i32("ec")?, 26);
    let explain = database.run_command(doc! {
        "explain": {
            "aggregate": "environments", "pipeline": [
                { "$match": { "project_id": 7_i32 } }, { "$group": { "_id": 1_i32, "n": { "$sum": 1_i32 } } }
            ], "cursor": {}, "hint": "environment_project_timeline", "maxTimeMS": 10_000_i32,
        }, "verbosity": "executionStats",
    }).await?;
    assert!(format!("{explain:?}").contains("environment_project_timeline"));
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
            feedback: true,
            check_in: true,
            metric: true,
            replay: true,
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
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://127.0.0.1:27017/?retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000"
            .to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "metric_phase14_maintenance_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn artifact_publication_preserves_concurrent_bindings_and_orphan_rescue() {
    let database = test_database().await.unwrap();
    let result = exercise_artifact_publication(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise_artifact_publication(database: &Database) -> Result<(), Box<dyn Error>> {
    use metric_domain::{
        artifacts::{ArtifactBinding, ArtifactBundle, ArtifactBundleId, ArtifactUpload},
        debug_files::DebugId,
        finalization::ReleaseId,
    };
    use metric_mongo::{ArtifactQuota, MongoArtifactStore};
    use metric_ports::{ArtifactStore, ArtifactStoreError};
    use std::sync::Arc;

    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    control.insert_organization(organization()).await?;
    control.insert_project(project()).await?;
    let store = MongoArtifactStore::from_database(database.clone(), ArtifactQuota::default());
    let now = Timestamp::from_unix_millis(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)?;
    let original = ArtifactBinding::new(ProjectId::new(7)?, None, None)?;
    let upload = ArtifactUpload {
        id: [1; 16],
        organization_id: OrganizationId::new(1)?,
        sha1: [1; 20],
        chunks: vec![[1; 20]],
        bindings: vec![original.clone()],
        created_at: now,
        updated_at: now,
    };
    store.upsert_upload(upload.clone()).await?;
    let bundle = ArtifactBundle {
        id: ArtifactBundleId::from_bytes([1; 16]),
        organization_id: upload.organization_id,
        bindings: upload.bindings.clone(),
        bundle_debug_id: DebugId::Uuid {
            uuid: [9; 16],
            appendix: 0,
        },
        debug_id_tokens: Vec::new(),
        checksum: [1; 32],
        sha1: upload.sha1,
        size: 3,
        uploaded_at: now,
        generation: 0,
    };
    store.publish_bundle(upload.id, bundle.clone()).await?;
    let start = Arc::new(tokio::sync::Barrier::new(32));
    let mut publishers = Vec::new();
    for seed in 1..=32_u8 {
        let store = store.clone();
        let mut candidate = bundle.clone();
        candidate.bindings = vec![ArtifactBinding::new(
            original.project_id,
            Some(ReleaseId::from_bytes([seed; 16])),
            None,
        )?];
        let start = start.clone();
        publishers.push(tokio::spawn(async move {
            start.wait().await;
            // A caller may retry Busy after the adapter's bounded CAS attempts.
            for _ in 0..8 {
                match store.publish_bundle([1; 16], candidate.clone()).await {
                    Err(ArtifactStoreError::Busy) => tokio::task::yield_now().await,
                    result => return result,
                }
            }
            Err(ArtifactStoreError::Busy)
        }));
    }
    let mut revisions = std::collections::BTreeSet::new();
    let mut failure = None;
    // Join every worker before cleanup, including when one fails.
    for publisher in publishers {
        match publisher.await {
            Ok(Ok(values)) => {
                assert_eq!(values.len(), 1);
                assert_eq!(values[0].0, original.project_id);
                revisions.insert(values[0].1);
            }
            result => failure = Some(format!("publication failed: {result:?}")),
        }
    }
    if let Some(failure) = failure {
        return Err(failure.into());
    }
    assert_eq!(revisions.len(), 32);
    let stored = store
        .load_for_project(original.project_id, bundle.id)
        .await?;
    assert_eq!(stored.bindings.len(), 33);
    assert!(stored.bindings.contains(&original));
    for seed in 1..=32_u8 {
        assert!(stored.bindings.contains(&ArtifactBinding::new(
            original.project_id,
            Some(ReleaseId::from_bytes([seed; 16])),
            None,
        )?));
    }
    // Duplicate publication must not advance revisions or charge a second bundle.
    assert!(
        store
            .publish_bundle(upload.id, bundle.clone())
            .await?
            .is_empty()
    );
    let projects = database.collection::<Document>("projects");
    assert_eq!(
        projects
            .find_one(doc! { "_id": 7_i32 })
            .await?
            .unwrap()
            .get_i64("ar")?,
        33
    );
    let organizations = database.collection::<Document>("organizations");
    let quota = organizations
        .find_one(doc! { "_id": 1_i64 })
        .await?
        .unwrap();
    assert_eq!(quota.get_i64("ab")?, 3);
    assert_eq!(quota.get_i64("ac")?, 1);

    // Remove every association through the port: the final one removes field b.
    for binding in stored.bindings {
        assert!(
            store
                .remove_binding(bundle.organization_id, bundle.id, binding, now)
                .await?
                .is_some()
        );
    }
    let bundles = database.collection::<Document>("artifact_bundles");
    let orphan = bundles
        .find_one(doc! { "_id": binary(bundle.id.as_bytes()) })
        .await?
        .unwrap();
    assert!(!orphan.contains_key("b"));
    assert!(orphan.contains_key("e"));
    // The upload cache was removed with its final binding, so recreate it normally.
    store.upsert_upload(upload.clone()).await?;
    assert_eq!(
        store.publish_bundle(upload.id, bundle.clone()).await?.len(),
        1
    );
    let rescued = bundles
        .find_one(doc! { "_id": binary(bundle.id.as_bytes()) })
        .await?
        .unwrap();
    assert!(!rescued.contains_key("e"));
    assert_eq!(
        store
            .load_for_project(original.project_id, bundle.id)
            .await?
            .bindings,
        bundle.bindings
    );
    assert!(store.claim_gc(now, now, [2; 16], 1).await?.is_empty());
    let quota = organizations
        .find_one(doc! { "_id": 1_i64 })
        .await?
        .unwrap();
    assert_eq!(quota.get_i64("ab")?, 3);
    assert_eq!(quota.get_i64("ac")?, 1);
    Ok(())
}
