use std::{
    error::Error,
    num::NonZeroU64,
    time::{Duration, Instant},
};

use metric_domain::{
    AcceptedEvent, DisplayName, EventId, IpScrubPolicy, ItemCapabilities, OrganizationId,
    OrganizationIdentity, ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits,
    ScrubbedEventPayload, SecretBytes, Slug, Timestamp,
    event::{EventLevel, EventPlatform},
    finalization::{
        FinalizationPolicy, FinalizeBatch, FinalizeEvent, ProcessedEventPayload, SearchToken,
        derive_environment_id, derive_hour_bucket_id, derive_release_id, hour_start,
    },
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, derive_issue_id,
    },
    issue::{IssueCulprit, IssueGroupingDetail, IssueOccurrence, IssueRelease, IssueTitle},
    releases::{CreateDeploy, CreateRelease, derive_deploy_id},
};
use metric_mongo::{
    EventCodecConfig, IssueCodecConfig, MongoEventStore, MongoFinalizationStore, MongoProjectStore,
    MongoReleaseStore, decode_finalized_event,
};
use metric_ports::{EventStore, EventWriteStatus, FinalizationStore, ProjectStore, ReleaseStore};
use mongodb::{
    Client, Database,
    bson::{Bson, Document, doc},
};

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_finalizer_event_issue_bucket_catalog_limits_and_explains() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn crash_between_acknowledged_steps_is_retryable_without_identity_duplication() {
    let database = test_database().await.unwrap();
    let result = exercise_crash_boundaries(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "performance baseline requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn performance_finalize_batch_rps() {
    let database = test_database().await.unwrap();
    let result = measure_rps(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let event_store = bootstrap(database).await?;
    let finalizer = MongoFinalizationStore::from_database(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    );
    let first = finalized(1, 1, 1_700_000_100_000, "backend@1.0", "production");
    let second = finalized(1, 2, 1_700_000_200_000, "backend@1.0", "production");
    insert_pending(&event_store, &[first.clone(), second.clone()]).await?;
    let policy = policy(1, 1);
    let result = finalizer
        .finalize(
            FinalizeBatch {
                events: vec![first.clone(), second.clone()],
            },
            policy,
        )
        .await?;
    assert_eq!(result.finalized, 2);

    let events = database.collection::<Document>("error_events");
    for item in [&first, &second] {
        let document = events
            .find_one(doc! { "_id": binary(item.key().as_bytes()) })
            .await?
            .unwrap();
        let decoded = decode_finalized_event(&document, EventCodecConfig::default())?;
        assert_eq!(decoded.issue_id, item.issue.issue_id);
        assert_eq!(decoded.search_tokens.len(), 2);
        assert_eq!(
            decoded.expire_at.unwrap().unix_millis(),
            item.received_at.unix_millis() + 30 * 24 * 60 * 60 * 1_000
        );
        assert!(serde_json::from_slice::<serde_json::Value>(decoded.payload.as_bytes()).is_ok());
    }

    let issue = database
        .collection::<Document>("issues")
        .find_one(doc! { "_id": binary(first.issue.issue_id.as_bytes()) })
        .await?
        .unwrap();
    assert_eq!(issue.get_i64("c"), Ok(2));
    assert_eq!(issue.get_bool("j"), Ok(true));
    assert_eq!(issue.get_array("n")?.len(), 1);

    let bucket_start = hour_start(first.occurred_at);
    let bucket_id = derive_hour_bucket_id(first.project_id, first.issue.issue_id, bucket_start);
    let bucket = database
        .collection::<Document>("issue_stats_hourly")
        .find_one(doc! { "_id": binary(bucket_id.as_bytes()) })
        .await?
        .unwrap();
    assert_eq!(bucket.get_i64("occurrence_count"), Ok(2));
    assert_eq!(
        bucket.get_datetime("expire_at")?.timestamp_millis(),
        bucket_start.unix_millis() + 400 * 24 * 60 * 60 * 1_000
    );

    let release_id = derive_release_id(OrganizationId::new(42)?, "backend@1.0");
    let release = database
        .collection::<Document>("releases")
        .find_one(doc! { "_id": binary(release_id.as_bytes()) })
        .await?
        .unwrap();
    assert_eq!(release.get_str("version"), Ok("backend@1.0"));
    assert_eq!(release.get_array("project_ids")?, &[Bson::Int32(7)]);
    assert_eq!(release.get_binary_generic("first_event_id")?.len(), 20);
    assert!(!release.contains_key("expire_at"));
    let release_store =
        MongoReleaseStore::from_database(database.clone(), IssueCodecConfig::default());
    let explicit = release_store
        .create_release(CreateRelease {
            organization_id: OrganizationId::new(42)?,
            project_ids: vec![ProjectId::new(7)?],
            version: "backend@1.0".into(),
            url: Some("https://ci.example/build/1".into()),
            reference: None,
            repositories: Vec::new(),
            created_at: Timestamp::from_unix_millis(1_700_000_250_000)?,
        })
        .await?;
    assert_eq!(explicit.id, release_id);
    assert!(explicit.explicit);
    assert_eq!(
        database
            .collection::<Document>("releases")
            .count_documents(doc! { "_id": binary(release_id.as_bytes()) })
            .await?,
        1
    );
    let deploy_id = derive_deploy_id(OrganizationId::new(42)?, release_id, [3; 16]);
    let deploy = CreateDeploy {
        deploy_id,
        organization_id: OrganizationId::new(42)?,
        release_id,
        project_ids: vec![ProjectId::new(7)?],
        environment: "production".into(),
        name: Some("rollout".into()),
        url: None,
        started_at: Timestamp::from_unix_millis(1_700_000_300_000)?,
        finished_at: Some(Timestamp::from_unix_millis(1_700_000_360_000)?),
        created_at: Timestamp::from_unix_millis(1_700_000_300_000)?,
    };
    assert_eq!(
        release_store.create_deploy(deploy.clone()).await?,
        release_store.create_deploy(deploy).await?
    );
    let environment_id = derive_environment_id(ProjectId::new(7)?, "production");
    assert!(
        database
            .collection::<Document>("environments")
            .find_one(doc! { "_id": binary(environment_id.as_bytes()) })
            .await?
            .is_some()
    );

    let retry = finalizer
        .finalize(
            FinalizeBatch {
                events: vec![first.clone(), second.clone()],
            },
            policy,
        )
        .await?;
    assert_eq!(retry.pending, 0);
    assert_eq!(retry.skipped_completed, 2);
    assert_eq!(
        database
            .collection::<Document>("issue_stats_hourly")
            .find_one(doc! { "_id": binary(bucket_id.as_bytes()) })
            .await?
            .unwrap()
            .get_i64("occurrence_count"),
        Ok(2)
    );

    let limited = finalized(2, 3, 1_700_000_300_000, "backend@2.0", "staging");
    insert_pending(&event_store, std::slice::from_ref(&limited)).await?;
    finalizer
        .finalize(
            FinalizeBatch {
                events: vec![limited.clone()],
            },
            policy,
        )
        .await?;
    assert!(
        database
            .collection::<Document>("releases")
            .find_one(doc! { "_id": binary(derive_release_id(OrganizationId::new(42)?, "backend@2.0").as_bytes()) })
            .await?
            .is_none()
    );
    assert!(
        database
            .collection::<Document>("environments")
            .find_one(doc! { "_id": binary(derive_environment_id(ProjectId::new(7)?, "staging").as_bytes()) })
            .await?
            .is_none()
    );

    let next_day = finalized(3, 4, 1_700_086_500_000, "backend@3.0", "qa");
    insert_pending(&event_store, std::slice::from_ref(&next_day)).await?;
    finalizer
        .finalize(
            FinalizeBatch {
                events: vec![next_day.clone()],
            },
            policy,
        )
        .await?;
    assert!(
        database
            .collection::<Document>("releases")
            .find_one(doc! { "_id": binary(derive_release_id(OrganizationId::new(42)?, "backend@3.0").as_bytes()) })
            .await?
            .is_some()
    );
    assert!(
        database
            .collection::<Document>("environments")
            .find_one(
                doc! { "_id": binary(derive_environment_id(ProjectId::new(7)?, "qa").as_bytes()) }
            )
            .await?
            .is_none()
    );

    assert_plan_uses(
        database,
        stats_explain(first.issue.issue_id),
        "issue_stats_issue_timeline",
    )
    .await?;
    assert_plan_uses(database, release_explain(), "release_organization_timeline").await?;
    assert_plan_uses(
        database,
        deploy_explain(release_id),
        "deploy_project_release_timeline",
    )
    .await?;
    assert_plan_uses(
        database,
        environment_explain(),
        "environment_project_timeline",
    )
    .await?;
    Ok(())
}

async fn exercise_crash_boundaries(database: &Database) -> Result<(), Box<dyn Error>> {
    let event_store = bootstrap(database).await?;
    let finalizer = MongoFinalizationStore::from_database(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    );
    let boundaries = ["issues", "issue_stats_hourly", "releases", "error_events"];

    for (offset, collection) in boundaries.into_iter().enumerate() {
        let seed = u8::try_from(20 + offset)?;
        let event = finalized(
            seed,
            u64::from(seed),
            1_700_100_000_000 + i64::try_from(offset)? * 1_000,
            &format!("backend@crash-{seed}"),
            &format!("crash-{seed}"),
        );
        insert_pending(&event_store, std::slice::from_ref(&event)).await?;

        let validator = collection_validator(database, collection).await?;
        replace_validator(database, collection, doc! { "$expr": false }).await?;
        let failed = finalizer
            .finalize(
                FinalizeBatch {
                    events: vec![event.clone()],
                },
                policy(100, 100),
            )
            .await;
        assert_eq!(
            failed,
            Err(metric_ports::FinalizationStoreError::Unavailable),
            "failure boundary before {collection} did not stop the batch"
        );
        replace_validator(database, collection, validator).await?;

        let retry = finalizer
            .finalize(
                FinalizeBatch {
                    events: vec![event.clone()],
                },
                policy(100, 100),
            )
            .await?;
        assert_eq!(retry.finalized, 1);
        assert_eq!(
            database
                .collection::<Document>("error_events")
                .count_documents(doc! { "_id": binary(event.key().as_bytes()) })
                .await?,
            1
        );
        assert_eq!(
            database
                .collection::<Document>("issues")
                .count_documents(doc! { "_id": binary(event.issue.issue_id.as_bytes()) })
                .await?,
            1
        );
        let terminal = database
            .collection::<Document>("error_events")
            .find_one(doc! { "_id": binary(event.key().as_bytes()) })
            .await?
            .unwrap();
        assert!(!terminal.contains_key("q"));
        assert_eq!(
            database
                .collection::<Document>("issues")
                .find_one(doc! { "_id": binary(event.issue.issue_id.as_bytes()) })
                .await?
                .unwrap()
                .get_array("n")?
                .len(),
            1
        );

        let completed_retry = finalizer
            .finalize(
                FinalizeBatch {
                    events: vec![event],
                },
                policy(100, 100),
            )
            .await?;
        assert_eq!(completed_retry.pending, 0);
    }
    Ok(())
}

async fn measure_rps(database: &Database) -> Result<(), Box<dyn Error>> {
    let event_store = bootstrap(database).await?;
    let finalizer = MongoFinalizationStore::from_database(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    );
    const EVENTS: u64 = 1_000;
    const ISSUES: u8 = 100;
    let batch = (0..EVENTS)
        .map(|index| {
            finalized(
                u8::try_from(index % u64::from(ISSUES) + 1).unwrap(),
                index + 1,
                1_700_000_000_000 + index as i64,
                "benchmark@1.0",
                "production",
            )
        })
        .collect::<Vec<_>>();
    for chunk in batch.chunks(250) {
        insert_pending(&event_store, chunk).await?;
    }
    let started = Instant::now();
    let result = finalizer
        .finalize(FinalizeBatch { events: batch }, policy(1_000, 100))
        .await?;
    let elapsed = started.elapsed();
    assert_eq!(result.finalized, EVENTS as usize);
    let rps = EVENTS as f64 / elapsed.as_secs_f64();
    eprintln!(
        "Finalizer Phase 9: rps={rps:.0},events={EVENTS},issues={ISSUES},elapsed_ms={}",
        elapsed.as_millis()
    );
    assert!(rps >= 150.0, "FinalizeBatch {rps:.0} RPS below local gate");
    Ok(())
}

async fn bootstrap(database: &Database) -> Result<MongoEventStore, Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    control.insert_organization(organization()).await?;
    control.insert_project(project()).await?;
    Ok(MongoEventStore::from_database(
        database.clone(),
        EventCodecConfig::default(),
    ))
}

async fn insert_pending(
    store: &MongoEventStore,
    events: &[FinalizeEvent],
) -> Result<(), Box<dyn Error>> {
    let prepared = events
        .iter()
        .map(|event| store.prepare(accepted(event)))
        .collect::<Result<Vec<_>, _>>()?;
    let statuses = store.insert_batch(&prepared).await?;
    assert!(
        statuses
            .iter()
            .all(|status| *status == EventWriteStatus::Inserted)
    );
    Ok(())
}

fn accepted(event: &FinalizeEvent) -> AcceptedEvent {
    let event_id = event.event_id;
    AcceptedEvent {
        project_id: event.project_id,
        event_id,
        received_at: event.received_at,
        policy_revision: 1,
        payload: ScrubbedEventPayload::new(
            serde_json::to_vec(&serde_json::json!({
                "event_id": event_id.to_string(),
                "platform": "rust",
                "level": "error",
                "timestamp": event.occurred_at.unix_millis() as f64 / 1000.0,
                "message": "pending finalization",
            }))
            .unwrap(),
        ),
    }
}

fn finalized(
    seed: u8,
    event_number: u64,
    timestamp: i64,
    release: &str,
    environment: &str,
) -> FinalizeEvent {
    let project_id = ProjectId::new(7).unwrap();
    let grouping_key = key(seed);
    let event_id = EventId::from_bytes(u128::from(event_number).to_be_bytes());
    let occurred_at = Timestamp::from_unix_millis(timestamp).unwrap();
    let received_at = Timestamp::from_unix_millis(timestamp + 1_000).unwrap();
    FinalizeEvent {
        project_id,
        event_id,
        received_at,
        occurred_at,
        level: EventLevel::Error,
        platform: EventPlatform::Rust,
        issue: IssueOccurrence {
            project_id,
            issue_id: derive_issue_id(project_id, grouping_key),
            grouping_key,
            event_id,
            occurred_at,
            received_at,
            release: Some(IssueRelease::new(release).unwrap()),
            title: IssueTitle::new(format!("Panic: finalizer {seed}")).unwrap(),
            culprit: Some(IssueCulprit::new("crate::finalize").unwrap()),
            grouping: IssueGroupingDetail {
                strategy: GroupingStrategy::Message,
                explanation: GroupingExplanation {
                    summary: "normalized message".into(),
                    components: vec![GroupingComponent {
                        kind: GroupingComponentKind::Message,
                        value: format!("finalizer {seed}").into_boxed_str(),
                    }],
                },
            },
            increment: NonZeroU64::MIN,
        },
        environment: Some(environment.into()),
        search_tokens: vec![
            SearchToken::environment(environment),
            SearchToken::release(release),
        ],
        payload: ProcessedEventPayload::new(
            serde_json::to_vec(&serde_json::json!({
                "_metric": { "symbolication": { "status": "not_required" } },
                "environment": environment,
                "message": "pending finalization",
                "platform": "rust",
                "release": release,
                "timestamp": timestamp,
            }))
            .unwrap(),
        ),
    }
}

fn key(seed: u8) -> GroupingKey {
    let mut bytes = [seed; 34];
    bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    GroupingKey::parse(&bytes).unwrap()
}

fn policy(releases: u32, environments: u32) -> FinalizationPolicy {
    FinalizationPolicy {
        event_retention: Duration::from_secs(30 * 24 * 60 * 60),
        hourly_retention: Duration::from_secs(400 * 24 * 60 * 60),
        archive_events: false,
        max_implicit_releases_per_project_day: releases,
        max_implicit_environments_per_project: environments,
    }
}

fn organization() -> OrganizationIdentity {
    OrganizationIdentity {
        id: OrganizationId::new(42).unwrap(),
        slug: Slug::new("acme").unwrap(),
        display_name: DisplayName::new("Acme").unwrap(),
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}

fn project() -> ProjectIdentity {
    ProjectIdentity {
        id: ProjectId::new(7).unwrap(),
        organization_id: OrganizationId::new(42).unwrap(),
        slug: Slug::new("backend").unwrap(),
        display_name: DisplayName::new("Backend").unwrap(),
        state: ProjectAcceptanceState::Active,
        policy_revision: 1,
        ip_policy: IpScrubPolicy::Remove,
        items: ItemCapabilities {
            error: true,
            client_report: false,
            log: true,
            transaction: true,
            span: true,
            feedback: true,
            check_in: true,
            metric: true,
        },
        limits: ProjectIngestLimits::default(),
        grouping_revision: 1,
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}

fn binary<const N: usize>(bytes: [u8; N]) -> mongodb::bson::Binary {
    mongodb::bson::Binary {
        subtype: mongodb::bson::spec::BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn stats_explain(issue_id: metric_domain::grouping::IssueId) -> Document {
    doc! { "explain": { "find": "issue_stats_hourly", "filter": {
        "project_id": 7_i32,
        "issue_id": binary(issue_id.as_bytes()),
        "bucket_start": { "$gte": mongodb::bson::DateTime::from_millis(0) },
    }, "sort": { "bucket_start": 1 } }, "verbosity": "queryPlanner" }
}

fn release_explain() -> Document {
    doc! { "explain": { "find": "releases", "filter": { "organization_id": 42_i64 }, "sort": { "activity_at": -1, "_id": -1 } }, "verbosity": "queryPlanner" }
}

fn deploy_explain(release_id: metric_domain::finalization::ReleaseId) -> Document {
    doc! { "explain": { "find": "deploys", "filter": {
        "organization_id": 42_i64,
        "project_ids": 7_i32,
        "release_id": binary(release_id.as_bytes()),
    }, "sort": { "started_at": -1, "_id": -1 } }, "verbosity": "queryPlanner" }
}

fn environment_explain() -> Document {
    doc! { "explain": { "find": "environments", "filter": { "project_id": 7_i32, "hidden": false }, "sort": { "last_seen": -1, "_id": -1 } }, "verbosity": "queryPlanner" }
}

async fn assert_plan_uses(
    database: &Database,
    command: Document,
    index: &str,
) -> Result<(), Box<dyn Error>> {
    let explain = database.run_command(command).await?;
    let rendered = format!("{explain:?}");
    assert!(
        rendered.contains(index),
        "query plan did not use {index}: {rendered}"
    );
    Ok(())
}

async fn collection_validator(
    database: &Database,
    collection: &str,
) -> Result<Document, Box<dyn Error>> {
    let response = database
        .run_command(doc! {
            "listCollections": 1,
            "filter": { "name": collection },
            "nameOnly": false,
        })
        .await?;
    Ok(response
        .get_document("cursor")?
        .get_array("firstBatch")?
        .first()
        .and_then(Bson::as_document)
        .ok_or("collection metadata is missing")?
        .get_document("options")?
        .get_document("validator")?
        .clone())
}

async fn replace_validator(
    database: &Database,
    collection: &str,
    validator: Document,
) -> Result<(), Box<dyn Error>> {
    database
        .run_command(doc! {
            "collMod": collection,
            "validator": validator,
            "validationLevel": "strict",
            "validationAction": "error",
        })
        .await?;
    Ok(())
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&retryWrites=false&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "metric_phase9_finalizer_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
