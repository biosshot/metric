use std::{error::Error, num::NonZeroU64, time::Instant};

use metric_domain::{
    EventId, ProjectId, SecretBytes, Timestamp,
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, derive_issue_id,
    },
    issue::{
        ActorKind, ActorRef, IssueCommand, IssueCommandAction, IssueCulprit, IssueGroupingDetail,
        IssueMutationKind, IssueOccurrence, IssueRelease, IssueSearchQuery, IssueStatus,
        IssueTitle,
    },
};
use metric_mongo::{IssueCodecConfig, MongoIssueStore, MongoProjectStore};
use metric_ports::{IssueStore, IssueStoreError};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_issue_atomic_lifecycle_contention_and_query_plans() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "performance baseline requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn performance_issue_upsert_hot_and_distributed_rps() {
    let database = test_database().await.unwrap();
    let result = measure_rps(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    bootstrap(database).await?;
    let store = MongoIssueStore::from_database(database.clone(), IssueCodecConfig::default());
    let issues = database.collection::<mongodb::bson::Document>("issues");

    for expected in [
        "issue_project_timeline",
        "issue_status_timeline",
        "issue_notification_ready",
        "issue_title_text",
    ] {
        assert!(
            issues
                .list_index_names()
                .await?
                .iter()
                .any(|name| name == expected)
        );
    }
    assert!(issues.insert_one(doc! { "_id": "invalid" }).await.is_err());

    let first = occurrence(1, 1, 1_000, 2_000, Some("1.0.0"));
    let created = store.apply_occurrence(first.clone()).await?;
    assert_eq!(created.kind, IssueMutationKind::Created);
    assert_eq!(created.issue.occurrence_count.get(), 1);
    let retry = store.apply_occurrence(first.clone()).await?;
    assert_eq!(
        retry.issue.occurrence_count.get(),
        2,
        "accepted retry drift"
    );

    let latest_missing = occurrence(1, 2, 2_000, 3_000, None);
    let updated = store.apply_occurrence(latest_missing).await?;
    assert_eq!(
        updated.issue.first_release.as_ref().unwrap().as_str(),
        "1.0.0"
    );
    assert_eq!(updated.issue.last_release, None);
    let raw = issues
        .find_one(doc! { "_id": binary(first.issue_id.as_bytes()) })
        .await?
        .unwrap();
    assert_eq!(raw.get_bool("m"), Ok(true));
    assert!(!raw.contains_key("lr"));

    let earlier_without_release = occurrence(1, 0, 500, 3_500, None);
    let updated = store.apply_occurrence(earlier_without_release).await?;
    assert_eq!(
        updated.issue.first_event_id,
        EventId::from_bytes(0_u128.to_be_bytes())
    );
    assert_eq!(updated.issue.first_release, None);
    assert_eq!(updated.issue.last_release, None);

    let mut same_issue_tasks = Vec::new();
    for index in 10..42 {
        let store = store.clone();
        same_issue_tasks.push(tokio::spawn(async move {
            store
                .apply_occurrence(occurrence(
                    1,
                    index,
                    4_000 + index as i64,
                    5_000 + index as i64,
                    Some("2.0"),
                ))
                .await
        }));
    }
    for task in same_issue_tasks {
        task.await??;
    }
    let snapshot = store.load(first.project_id, first.issue_id).await?;
    assert_eq!(snapshot.occurrence_count.get(), 36);

    let actor = ActorRef::new(ActorKind::User, [8; 16]);
    let resolve = command(&first, 1, 10_000, actor, IssueCommandAction::Resolve);
    assert!(store.apply_command(resolve).await?.applied);
    assert!(
        !store.apply_command(resolve).await?.applied,
        "command retry is a no-op"
    );

    let accepted_before_resolve = occurrence(1, 50, 11_000, 9_000, None);
    let no_regression = store.apply_occurrence(accepted_before_resolve).await?;
    assert_eq!(no_regression.issue.status, IssueStatus::Resolved);
    let regression = store
        .apply_occurrence(occurrence(1, 51, 12_000, 10_001, None))
        .await?;
    assert_eq!(regression.kind, IssueMutationKind::Regressed);
    assert_eq!(regression.issue.status, IssueStatus::Open);
    assert_eq!(regression.issue.regression.as_ref().unwrap().count.get(), 1);

    let resolve_again = command(&first, 2, 20_000, actor, IssueCommandAction::Resolve);
    assert!(store.apply_command(resolve_again).await?.applied);
    let mut regression_tasks = Vec::new();
    for index in 60..76 {
        let store = store.clone();
        regression_tasks.push(tokio::spawn(async move {
            store
                .apply_occurrence(occurrence(
                    1,
                    index,
                    21_000 + index as i64,
                    20_001 + index as i64,
                    None,
                ))
                .await
        }));
    }
    for task in regression_tasks {
        task.await??;
    }
    let snapshot = store.load(first.project_id, first.issue_id).await?;
    assert_eq!(snapshot.status, IssueStatus::Open);
    assert_eq!(snapshot.regression.as_ref().unwrap().count.get(), 2);

    let ignore = command(&first, 3, 30_000, actor, IssueCommandAction::Ignore);
    assert!(store.apply_command(ignore).await?.applied);
    let ignored = store
        .apply_occurrence(occurrence(1, 80, 31_000, 31_000, None))
        .await?;
    assert_eq!(ignored.issue.status, IssueStatus::Ignored);
    assert_eq!(ignored.issue.regression.as_ref().unwrap().count.get(), 2);
    assert!(
        store
            .apply_command(command(
                &first,
                4,
                32_000,
                actor,
                IssueCommandAction::Reopen
            ))
            .await?
            .applied
    );
    assert!(
        store
            .apply_command(command(
                &first,
                5,
                33_000,
                actor,
                IssueCommandAction::Assign(Some(actor)),
            ))
            .await?
            .applied
    );

    let mut distributed_tasks = Vec::new();
    for seed in 10..74_u8 {
        let store = store.clone();
        distributed_tasks.push(tokio::spawn(async move {
            store
                .apply_occurrence(occurrence(seed, u64::from(seed), 40_000, 40_000, None))
                .await
        }));
    }
    for task in distributed_tasks {
        task.await??;
    }

    let results = store
        .search_titles(
            first.project_id,
            IssueSearchQuery::new("bounded failure", 10)?,
        )
        .await?;
    assert!(!results.is_empty());
    assert!(results.len() <= 10);
    assert_plan_uses(database, timeline_explain(), "issue_project_timeline").await?;
    assert_plan_uses(database, title_explain(), "issue_title_text").await?;

    let collision = occurrence(90, 1, 50_000, 50_000, None);
    store.apply_occurrence(collision.clone()).await?;
    let alternate = key(91);
    issues
        .update_one(
            doc! { "_id": binary(collision.issue_id.as_bytes()) },
            doc! { "$set": { "g": binary(alternate.to_bytes()) } },
        )
        .await?;
    assert_eq!(
        store.apply_occurrence(collision).await,
        Err(IssueStoreError::IdentityCollision)
    );

    database
        .collection::<mongodb::bson::Document>("issue_activities")
        .drop()
        .await?;
    let best_effort = command(&first, 6, 60_000, actor, IssueCommandAction::Resolve);
    let result = store.apply_command(best_effort).await?;
    assert!(result.applied);
    assert_eq!(result.issue.status, IssueStatus::Resolved);
    Ok(())
}

async fn measure_rps(database: &Database) -> Result<(), Box<dyn Error>> {
    bootstrap(database).await?;
    let store = MongoIssueStore::from_database(database.clone(), IssueCodecConfig::default());
    const OPERATIONS: u64 = 4_000;
    const CONCURRENCY: usize = 64;

    let hot_started = Instant::now();
    for start in (0..OPERATIONS).step_by(CONCURRENCY) {
        let mut tasks = Vec::new();
        for index in start..(start + CONCURRENCY as u64).min(OPERATIONS) {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                store
                    .apply_occurrence(occurrence(1, index + 1, index as i64, index as i64, None))
                    .await
            }));
        }
        for task in tasks {
            task.await??;
        }
    }
    let hot_elapsed = hot_started.elapsed();
    let hot_rps = OPERATIONS as f64 / hot_elapsed.as_secs_f64();

    let distributed_started = Instant::now();
    for start in (0..OPERATIONS).step_by(CONCURRENCY) {
        let mut tasks = Vec::new();
        for index in start..(start + CONCURRENCY as u64).min(OPERATIONS) {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                let seed = u8::try_from(index % 250 + 2).unwrap();
                store
                    .apply_occurrence(occurrence(
                        seed,
                        index + 1,
                        index as i64,
                        index as i64,
                        None,
                    ))
                    .await
            }));
        }
        for task in tasks {
            task.await??;
        }
    }
    let distributed_elapsed = distributed_started.elapsed();
    let distributed_rps = OPERATIONS as f64 / distributed_elapsed.as_secs_f64();
    eprintln!(
        "IssueStore Phase 8: hot_rps={hot_rps:.0},distributed_rps={distributed_rps:.0},operations={OPERATIONS},concurrency={CONCURRENCY}"
    );
    assert!(
        hot_rps >= 250.0,
        "hot Issue RPS {hot_rps:.0} below local gate"
    );
    assert!(
        distributed_rps >= 500.0,
        "distributed Issue RPS {distributed_rps:.0} below local gate"
    );
    Ok(())
}

async fn bootstrap(database: &Database) -> Result<(), Box<dyn Error>> {
    MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32)
        .bootstrap_or_validate()
        .await?;
    Ok(())
}

fn occurrence(
    seed: u8,
    event_number: u64,
    occurred_at: i64,
    received_at: i64,
    release: Option<&str>,
) -> IssueOccurrence {
    let project_id = ProjectId::new(7).unwrap();
    let grouping_key = key(seed);
    IssueOccurrence {
        project_id,
        issue_id: derive_issue_id(project_id, grouping_key),
        grouping_key,
        event_id: EventId::from_bytes(u128::from(event_number).to_be_bytes()),
        occurred_at: Timestamp::from_unix_millis(occurred_at).unwrap(),
        received_at: Timestamp::from_unix_millis(received_at).unwrap(),
        release: release.map(|value| IssueRelease::new(value).unwrap()),
        title: IssueTitle::new(format!("Panic: bounded failure {seed}")).unwrap(),
        culprit: Some(IssueCulprit::new("crate::serve").unwrap()),
        grouping: IssueGroupingDetail {
            strategy: GroupingStrategy::Message,
            explanation: GroupingExplanation {
                summary: "logger plus normalized message".into(),
                components: vec![GroupingComponent {
                    kind: GroupingComponentKind::Message,
                    value: format!("bounded failure {seed}").into_boxed_str(),
                }],
            },
        },
        increment: NonZeroU64::MIN,
    }
}

fn command(
    occurrence: &IssueOccurrence,
    key: u8,
    at: i64,
    actor: ActorRef,
    action: IssueCommandAction,
) -> IssueCommand {
    IssueCommand {
        project_id: occurrence.project_id,
        issue_id: occurrence.issue_id,
        idempotency_key: [key; 16],
        actor,
        at: Timestamp::from_unix_millis(at).unwrap(),
        action,
    }
}

fn key(seed: u8) -> GroupingKey {
    let mut bytes = [seed; 34];
    bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    GroupingKey::parse(&bytes).unwrap()
}

fn binary<const N: usize>(bytes: [u8; N]) -> mongodb::bson::Binary {
    mongodb::bson::Binary {
        subtype: mongodb::bson::spec::BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn timeline_explain() -> mongodb::bson::Document {
    doc! {
        "explain": {
            "find": "issues",
            "filter": { "p": 7_i32 },
            "sort": { "l": -1, "_id": -1 },
            "limit": 100_i64,
        },
        "verbosity": "queryPlanner",
    }
}

fn title_explain() -> mongodb::bson::Document {
    doc! {
        "explain": {
            "find": "issues",
            "filter": {
                "p": 7_i32,
                "$text": { "$search": "bounded failure", "$language": "none" },
            },
            "projection": { "_id": 1, "t": 1, "s": 1, "l": 1, "c": 1 },
            "sort": { "score": { "$meta": "textScore" }, "_id": 1 },
            "limit": 100_i64,
        },
        "verbosity": "queryPlanner",
    }
}

async fn assert_plan_uses(
    database: &Database,
    command: mongodb::bson::Document,
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
        "metric_phase8_issue_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
