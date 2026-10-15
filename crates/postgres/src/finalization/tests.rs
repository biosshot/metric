use super::*;
use crate::{
    MigrationPolicy, PostgresEventStore, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, finalized, project_identity, seed_projects},
};
use metric_domain::{
    AcceptedEvent, EventId, ProjectId, ScrubbedEventPayload,
    finalization::{ProcessedEventPayload, SearchToken},
    issue::{ActorRef, IssueCommand, IssueCommandAction},
};
use metric_ports::{EventStore, EventWriteStatus, IssueStore, ProjectStore};
use serde_json::json;

fn event(project: i32, issue: u8, id: u128) -> FinalizeEvent {
    finalized(
        project,
        issue,
        EventId::from_bytes(id.to_be_bytes()),
        1000,
        2000,
        Some("backend@1"),
        Some("production"),
    )
}
fn policy(releases: u32, environments: u32) -> FinalizationPolicy {
    FinalizationPolicy {
        event_retention: Duration::from_secs(86400),
        hourly_retention: Duration::from_secs(864000),
        archive_events: false,
        max_implicit_releases_per_project_day: releases,
        max_implicit_environments_per_project: environments,
    }
}
async fn setup() -> (
    Fixture,
    PostgresDatabase,
    PostgresEventStore,
    PostgresFinalizationStore,
) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let events = PostgresEventStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    let finalizer = PostgresFinalizationStore::new(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    )
    .unwrap();
    (fixture, database, events, finalizer)
}
async fn insert(store: &PostgresEventStore, events: &[FinalizeEvent]) {
    for batch in events.chunks(500) {
        let prepared:Vec<_> = batch.iter().map(|value|store.prepare(AcceptedEvent { project_id:value.project_id,event_id:value.event_id,received_at:value.received_at,policy_revision:1,payload:ScrubbedEventPayload::new(serde_json::to_vec(&json!({"event_id":value.event_id.to_string(),"message":"accepted synthetic event"})).unwrap()) }).unwrap()).collect();
        assert!(
            store
                .insert_batch(&prepared)
                .await
                .unwrap()
                .iter()
                .all(|status| *status == EventWriteStatus::Inserted)
        );
    }
}
async fn counts(database: &PostgresDatabase) -> (i64, i64, i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM metric.error_events WHERE processing_state=2),(SELECT count(*) FROM metric.issues),(SELECT COALESCE(sum(occurrence_count),0)::bigint FROM metric.issue_stats_hourly),(SELECT count(*) FROM metric.releases),(SELECT count(*) FROM metric.environments),(SELECT count(*) FROM metric.issue_transitions)").fetch_one(&database.pool).await.unwrap()
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn event_issue_hourly_catalogs_tokens_and_retention_finalize_once() {
    let (fixture, database, events, store) = setup().await;
    let first = event(1, 1, 1);
    let mut latest = event(1, 1, 2);
    latest.occurred_at = Timestamp::from_unix_millis(1100).unwrap();
    latest.issue.occurred_at = latest.occurred_at;
    latest.level = EventLevel::Fatal;
    latest.platform = EventPlatform::Native;
    latest.search_tokens.extend([
        SearchToken::from_stored(i64::MIN),
        SearchToken::from_stored(i64::MAX),
    ]);
    let mut payload: Value = serde_json::from_slice(latest.payload.as_bytes()).unwrap();
    payload["contexts"] = json!({"trace":{"trace_id":"0123456789abcdef0123456789abcdef","span_id":"0123456789abcdef"}});
    latest.payload = ProcessedEventPayload::new(serde_json::to_vec(&payload).unwrap());
    insert(&events, &[first.clone(), latest.clone()]).await;
    let result = store
        .finalize(
            FinalizeBatch {
                events: vec![latest.clone(), first.clone()],
            },
            policy(10, 10),
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        FinalizeResult {
            requested: 2,
            pending: 2,
            finalized: 2,
            skipped_completed: 0
        }
    );
    assert_eq!(counts(&database).await, (2, 1, 2, 1, 1, 1));
    let issue = store
        .issues
        .load(first.project_id, first.issue.issue_id)
        .await
        .unwrap();
    assert_eq!(issue.occurrence_count.get(), 2);
    assert_eq!(issue.first_event_id, first.event_id);
    assert_eq!(issue.latest_event_id, latest.event_id);
    let row=sqlx::query("SELECT body,search_tokens,level,platform,occurred_at,expire_at,archive_due,trace_id,span_id FROM metric.error_events WHERE project_id=$1 AND event_id=$2")
        .bind(latest.project_id.get()).bind(latest.event_id.as_bytes().as_slice()).fetch_one(&database.pool).await.unwrap();
    assert_eq!(
        codec::decode_json(&row.get::<Vec<u8>, _>("body"), 1024 * 1024).unwrap(),
        latest.payload.as_bytes()
    );
    assert_eq!(
        row.get::<Vec<i64>, _>("search_tokens"),
        latest
            .search_tokens
            .iter()
            .map(|value| value.stored())
            .collect::<Vec<_>>()
    );
    assert_eq!(row.get::<i16, _>("level"), 4);
    assert_eq!(row.get::<i16, _>("platform"), 3);
    assert_eq!(row.get::<i64, _>("occurred_at"), 1100);
    assert_eq!(row.get::<Option<i64>, _>("expire_at"), Some(86_402_000));
    assert_eq!(row.get::<Option<i64>, _>("archive_due"), None);
    assert_eq!(
        row.get::<Vec<u8>, _>("trace_id"),
        TraceId::parse("0123456789abcdef0123456789abcdef")
            .unwrap()
            .as_bytes()
    );
    assert_eq!(
        row.get::<Vec<u8>, _>("span_id"),
        SpanId::parse("0123456789abcdef").unwrap().as_bytes()
    );
    let release=sqlx::query("SELECT first_seen,last_seen,first_event_key,latest_event_key,created_at,activity_at FROM metric.releases WHERE organization_id=1").fetch_one(&database.pool).await.unwrap();
    assert_eq!(
        release.get::<Vec<u8>, _>("first_event_key"),
        first.key().as_bytes()
    );
    assert_eq!(
        release.get::<Vec<u8>, _>("latest_event_key"),
        latest.key().as_bytes()
    );
    assert_eq!(release.get::<i64, _>("first_seen"), 1000);
    assert_eq!(release.get::<i64, _>("last_seen"), 1100);
    let expiry: i64 =
        sqlx::query_scalar("SELECT expire_at FROM metric.issue_stats_hourly WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(expiry, 864_000_000);
    let retried = store
        .finalize(
            FinalizeBatch {
                events: vec![first, latest],
            },
            policy(10, 10),
        )
        .await
        .unwrap();
    assert_eq!(
        retried,
        FinalizeResult {
            requested: 2,
            pending: 0,
            finalized: 0,
            skipped_completed: 2
        }
    );
    assert_eq!(counts(&database).await, (2, 1, 2, 1, 1, 1));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn critical_failure_rolls_back_all_derived_effects_and_recovers() {
    let (fixture, database, events, store) = setup().await;
    let value = event(1, 1, 1);
    insert(&events, std::slice::from_ref(&value)).await;
    sqlx::query("CREATE FUNCTION metric.reject_finalization() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN IF NEW.processing_state=2 THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END$$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_finalization BEFORE UPDATE ON metric.error_events FOR EACH ROW EXECUTE FUNCTION metric.reject_finalization()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![value.clone()]
                },
                policy(10, 10)
            )
            .await,
        Err(FinalizationStoreError::Unavailable)
    );
    assert_eq!(counts(&database).await, (0, 0, 0, 0, 0, 0));
    let usage: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.project_catalog_usage")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(usage, 0);
    let state: i16 =
        sqlx::query_scalar("SELECT processing_state FROM metric.error_events WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(state, 0);
    sqlx::query("DROP TRIGGER reject_finalization ON metric.error_events")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![value]
                },
                policy(10, 10)
            )
            .await
            .unwrap()
            .finalized,
        1
    );
    assert_eq!(counts(&database).await, (1, 1, 1, 1, 1, 1));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_reply_retries_without_double_counters_or_transitions() {
    let (fixture, database, events, direct) = setup().await;
    let value = event(1, 1, 1);
    insert(&events, std::slice::from_ref(&value)).await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxied = PostgresDatabase::connect(
        &proxy.uri,
        &crate::ConnectionPolicy {
            max_connections: 1,
            ..fixture.policy.clone()
        },
    )
    .await
    .unwrap();
    let store = PostgresFinalizationStore::new(
        proxied.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    )
    .unwrap();
    proxy.drop_next_commit_reply();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        store.finalize(
            FinalizeBatch {
                events: vec![value.clone()],
            },
            policy(10, 10),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result, Err(FinalizationStoreError::Unavailable));
    assert!(proxy.did_drop_reply());
    assert_eq!(counts(&database).await, (1, 1, 1, 1, 1, 1));
    let result = store
        .finalize(
            FinalizeBatch {
                events: vec![value],
            },
            policy(10, 10),
        )
        .await
        .unwrap();
    assert_eq!(result.pending, 0);
    assert_eq!(result.skipped_completed, 1);
    assert_eq!(counts(&database).await, (1, 1, 1, 1, 1, 1));
    assert_eq!(
        direct
            .issues
            .load(ProjectId::new(1).unwrap(), event(1, 1, 1).issue.issue_id)
            .await
            .unwrap()
            .occurrence_count
            .get(),
        1
    );
    proxied.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_identical_and_opposing_project_batches_do_not_double_apply() {
    let (fixture, database, events, store) = setup().await;
    let first = event(1, 1, 1);
    let other = event(2, 1, 1);
    insert(&events, &[first.clone(), other.clone()]).await;
    let mut tasks = Vec::new();
    for index in 0..32 {
        let store = store.clone();
        let a = first.clone();
        let b = other.clone();
        tasks.push(tokio::spawn(async move {
            store
                .finalize(
                    FinalizeBatch {
                        events: if index % 2 == 0 {
                            vec![a, b]
                        } else {
                            vec![b, a]
                        },
                    },
                    policy(10, 10),
                )
                .await
        }));
    }
    let mut total = 0;
    for task in tasks {
        total += task.await.unwrap().unwrap().finalized;
    }
    assert_eq!(total, 2);
    assert_eq!(counts(&database).await, (2, 2, 2, 2, 2, 2));
    for value in [&first, &other] {
        assert_eq!(
            store
                .issues
                .load(value.project_id, value.issue.issue_id)
                .await
                .unwrap()
                .occurrence_count
                .get(),
            1
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn catalog_limits_existing_identity_and_nul_values_are_scoped() {
    let (fixture, database, events, store) = setup().await;
    let first = finalized(
        1,
        1,
        EventId::from_bytes([1; 16]),
        10,
        20,
        Some("release\0'; -- 雪"),
        Some("env\0'; -- 雪"),
    );
    insert(&events, std::slice::from_ref(&first)).await;
    store
        .finalize(
            FinalizeBatch {
                events: vec![first.clone()],
            },
            policy(1, 1),
        )
        .await
        .unwrap();
    let release: Vec<u8> =
        sqlx::query_scalar("SELECT version FROM metric.releases WHERE organization_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        release,
        first.issue.release.as_ref().unwrap().as_str().as_bytes()
    );
    let environment: Vec<u8> =
        sqlx::query_scalar("SELECT name FROM metric.environments WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        environment,
        first.environment.as_deref().unwrap().as_bytes()
    );
    let second = finalized(
        1,
        1,
        EventId::from_bytes([2; 16]),
        30,
        40,
        Some("other"),
        Some("other"),
    );
    insert(&events, std::slice::from_ref(&second)).await;
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![second.clone()]
                },
                policy(1, 1)
            )
            .await
            .unwrap()
            .finalized,
        1,
        "catalog admission limit cannot lose an Event"
    );
    assert_eq!(counts(&database).await, (2, 1, 2, 1, 1, 1));
    let existing = finalized(
        1,
        1,
        EventId::from_bytes([3; 16]),
        50,
        60,
        first.issue.release.as_ref().map(|value| value.as_str()),
        first.environment.as_deref(),
    );
    insert(&events, std::slice::from_ref(&existing)).await;
    store
        .finalize(
            FinalizeBatch {
                events: vec![existing],
            },
            policy(1, 1),
        )
        .await
        .unwrap();
    let usage:(i64,i64)=sqlx::query_as("SELECT release_count,environment_count FROM metric.project_catalog_usage WHERE project_id=1").fetch_one(&database.pool).await.unwrap();
    assert_eq!(usage, (1, 1));
    let next_day = finalized(
        1,
        1,
        EventId::from_bytes([4; 16]),
        90_000_000,
        90_001_000,
        Some("next-day"),
        Some("other"),
    );
    insert(&events, std::slice::from_ref(&next_day)).await;
    store
        .finalize(
            FinalizeBatch {
                events: vec![next_day],
            },
            policy(1, 1),
        )
        .await
        .unwrap();
    assert_eq!(counts(&database).await, (4, 1, 4, 2, 1, 1));
    let tenant = finalized(
        2,
        1,
        EventId::from_bytes([1; 16]),
        10,
        20,
        first.issue.release.as_ref().map(|value| value.as_str()),
        first.environment.as_deref(),
    );
    insert(&events, std::slice::from_ref(&tenant)).await;
    store
        .finalize(
            FinalizeBatch {
                events: vec![tenant],
            },
            policy(1, 1),
        )
        .await
        .unwrap();
    assert_eq!(counts(&database).await, (5, 2, 5, 3, 2, 2));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn same_organization_projects_share_release_and_global_pair_order() {
    let (fixture, database, events, store) = setup().await;
    let control = crate::PostgresProjectStore::new(
        database.clone(),
        metric_domain::SecretBytes::new([42; 32]),
        32,
    )
    .unwrap();
    let mut third_project = project_identity(3, 1);
    third_project.slug = metric_domain::Slug::new("third-project").unwrap();
    control.insert_project(third_project).await.unwrap();
    let a = finalized(
        1,
        1,
        EventId::from_bytes([2; 16]),
        10,
        20,
        Some("shared"),
        Some("same"),
    );
    let b = finalized(
        3,
        1,
        EventId::from_bytes([1; 16]),
        10,
        20,
        Some("shared"),
        Some("same"),
    );
    insert(&events, &[a.clone(), b.clone()]).await;
    let s1 = store.clone();
    let s2 = store.clone();
    let a_copy = a.clone();
    let b_copy = b.clone();
    let (ra, rb) = tokio::join!(
        s1.finalize(
            FinalizeBatch {
                events: vec![a_copy]
            },
            policy(1, 1)
        ),
        s2.finalize(
            FinalizeBatch {
                events: vec![b_copy]
            },
            policy(1, 1)
        )
    );
    assert_eq!(ra.unwrap().finalized, 1);
    assert_eq!(rb.unwrap().finalized, 1);
    let row = sqlx::query(
        "SELECT first_event_key,latest_event_key FROM metric.releases WHERE organization_id=1",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        row.get::<Vec<u8>, _>("first_event_key"),
        a.key().as_bytes(),
        "project bytes break occurrence-time ties"
    );
    assert_eq!(
        row.get::<Vec<u8>, _>("latest_event_key"),
        b.key().as_bytes()
    );
    let links:Vec<i32>=sqlx::query_scalar("SELECT project_id FROM metric.release_projects WHERE organization_id=1 ORDER BY project_id").fetch_all(&database.pool).await.unwrap();
    assert_eq!(links, vec![1, 3]);
    let cross_organization = sqlx::query(
        "INSERT INTO metric.release_projects(organization_id,release_id,project_id) VALUES(1,$1,2)",
    )
    .bind(
        derive_release_id(OrganizationId::new(1).unwrap(), "shared")
            .as_bytes()
            .as_slice(),
    )
    .execute(&database.pool)
    .await
    .unwrap_err();
    assert_eq!(
        cross_organization
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23503"),
        "composite foreign key must reject a project belonging to another organization"
    );
    let quota: i64 =
        sqlx::query_scalar("SELECT sum(release_count)::bigint FROM metric.project_catalog_usage")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(quota, 1, "only the creator reserves a release slot");
    assert_eq!(counts(&database).await, (2, 2, 2, 1, 2, 2));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn supported_maximum_single_issue_batch_uses_bounded_bulk_arrays() {
    let (fixture, database, events, store) = setup().await;
    let batch: Vec<_> = (0..10_000_u128)
        .map(|id| {
            finalized(
                1,
                1,
                EventId::from_bytes(id.to_be_bytes()),
                1000,
                2000,
                (id % 2 == 0).then_some("release"),
                (id % 3 == 0).then_some("production"),
            )
        })
        .collect();
    insert(&events, &batch).await;
    let mut oversized = batch.clone();
    oversized.push(event(1, 1, 10_001));
    assert_eq!(
        store
            .finalize(FinalizeBatch { events: oversized }, policy(10, 10))
            .await,
        Err(FinalizationStoreError::InvalidData)
    );
    assert_eq!(counts(&database).await, (0, 0, 0, 0, 0, 0));
    let result = store
        .finalize(FinalizeBatch { events: batch }, policy(10, 10))
        .await
        .unwrap();
    assert_eq!(result.finalized, 10_000);
    assert_eq!(counts(&database).await, (10_000, 1, 10_000, 1, 1, 1));
    let count: i64 =
        sqlx::query_scalar("SELECT occurrence_count FROM metric.issues WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(count, 10_000);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn archive_hold_disabled_fenced_missing_and_failed_events() {
    let (fixture, database, events, store) = setup().await;
    let a = event(1, 1, 1);
    let b = event(2, 1, 1);
    let failed = event(1, 1, 2);
    insert(&events, &[a.clone(), b.clone(), failed.clone()]).await;
    sqlx::query("UPDATE metric.projects SET state=CASE WHEN id=1 THEN 1 ELSE 2 END")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE metric.error_events SET processing_state=1,next_attempt_at=NULL WHERE project_id=1 AND event_id=$1").bind(failed.event_id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    let mut archive = policy(10, 10);
    archive.archive_events = true;
    let result = store
        .finalize(
            FinalizeBatch {
                events: vec![a.clone(), b, failed, event(1, 1, 99)],
            },
            archive,
        )
        .await
        .unwrap();
    assert_eq!(
        result,
        FinalizeResult {
            requested: 4,
            pending: 1,
            finalized: 1,
            skipped_completed: 3
        }
    );
    let row=sqlx::query("SELECT expire_at,archive_due,archive_segment FROM metric.error_events WHERE project_id=1 AND event_id=$1").bind(a.event_id.as_bytes().as_slice()).fetch_one(&database.pool).await.unwrap();
    assert!(row.get::<Option<i64>, _>("expire_at").is_none());
    assert_eq!(row.get::<Option<i64>, _>("archive_due"), Some(86_402_000));
    assert!(row.get::<Option<Vec<u8>>, _>("archive_segment").is_none());
    assert_eq!(counts(&database).await, (1, 1, 1, 1, 1, 1));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn validation_receipt_mismatch_bounds_collision_and_overflow_are_atomic() {
    let (fixture, database, events, store) = setup().await;
    let value = event(1, 1, 1);
    insert(&events, std::slice::from_ref(&value)).await;
    assert_eq!(
        store
            .finalize(FinalizeBatch { events: vec![] }, policy(10, 10))
            .await,
        Err(FinalizationStoreError::InvalidData)
    );
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![value.clone(), value.clone()]
                },
                policy(10, 10)
            )
            .await,
        Err(FinalizationStoreError::InvalidData)
    );
    let mut invalids = Vec::new();
    let mut wrong = value.clone();
    wrong.issue.project_id = ProjectId::new(2).unwrap();
    invalids.push(wrong);
    let mut wrong = value.clone();
    wrong.search_tokens.push(wrong.search_tokens[0]);
    invalids.push(wrong);
    let mut wrong = value.clone();
    wrong.search_tokens = (0..17).map(SearchToken::from_stored).collect();
    invalids.push(wrong);
    let mut wrong = value.clone();
    wrong.environment = Some("x".repeat(65).into());
    invalids.push(wrong);
    let mut wrong = value.clone();
    wrong.payload = ProcessedEventPayload::new(b"{ \"message\": \"noncanonical\" }".as_slice());
    invalids.push(wrong);
    let mut wrong = value.clone();
    wrong.payload = ProcessedEventPayload::new(b"[]".as_slice());
    invalids.push(wrong);
    let mut wrong = value.clone();
    wrong.received_at = Timestamp::from_unix_millis(2001).unwrap();
    wrong.issue.received_at = wrong.received_at;
    invalids.push(wrong);
    for wrong in invalids {
        assert_eq!(
            store
                .finalize(
                    FinalizeBatch {
                        events: vec![wrong]
                    },
                    policy(10, 10)
                )
                .await,
            Err(FinalizationStoreError::InvalidData)
        );
        assert_eq!(counts(&database).await, (0, 0, 0, 0, 0, 0));
    }
    let mut bad_policy = policy(10, 10);
    bad_policy.event_retention = Duration::MAX;
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![value.clone()]
                },
                bad_policy
            )
            .await,
        Err(FinalizationStoreError::InvalidData)
    );
    let small = PostgresFinalizationStore::new(
        database.clone(),
        EventCodecConfig {
            max_decoded_body_bytes: 10,
            ..Default::default()
        },
        IssueCodecConfig::default(),
    )
    .unwrap();
    assert_eq!(
        small
            .finalize(
                FinalizeBatch {
                    events: vec![value.clone()]
                },
                policy(10, 10)
            )
            .await,
        Err(FinalizationStoreError::InvalidData)
    );
    store
        .issues
        .apply_occurrence(value.issue.clone())
        .await
        .unwrap();
    sqlx::query("UPDATE metric.issues SET grouping_key=$1 WHERE project_id=1")
        .bind(event(1, 2, 1).issue.grouping_key.to_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![value.clone()]
                },
                policy(10, 10)
            )
            .await,
        Err(FinalizationStoreError::IdentityCollision)
    );
    sqlx::query("UPDATE metric.issues SET grouping_key=$1,occurrence_count=$2 WHERE project_id=1")
        .bind(value.issue.grouping_key.to_bytes().as_slice())
        .bind(i64::MAX)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![value]
                },
                policy(10, 10)
            )
            .await,
        Err(FinalizationStoreError::InvalidData)
    );
    assert_eq!(counts(&database).await, (0, 1, 0, 0, 0, 1));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn regression_activity_remains_best_effort_after_authoritative_commit() {
    let (fixture, database, events, store) = setup().await;
    let first = event(1, 1, 1);
    insert(&events, std::slice::from_ref(&first)).await;
    store
        .finalize(
            FinalizeBatch {
                events: vec![first.clone()],
            },
            policy(10, 10),
        )
        .await
        .unwrap();
    store
        .issues
        .apply_command(IssueCommand {
            project_id: first.project_id,
            issue_id: first.issue.issue_id,
            idempotency_key: [1; 16],
            actor: ActorRef::system(),
            at: Timestamp::from_unix_millis(2500).unwrap(),
            action: IssueCommandAction::Resolve,
        })
        .await
        .unwrap();
    sqlx::query("CREATE FUNCTION metric.reject_activity() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'synthetic failure'; END$$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_activity BEFORE INSERT ON metric.issue_activities FOR EACH ROW EXECUTE FUNCTION metric.reject_activity()").execute(&database.pool).await.unwrap();
    let regressed = finalized(1, 1, EventId::from_bytes([2; 16]), 900, 2501, None, None);
    insert(&events, std::slice::from_ref(&regressed)).await;
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![regressed.clone()]
                },
                policy(10, 10)
            )
            .await
            .unwrap()
            .finalized,
        1
    );
    let issue = store
        .issues
        .load(first.project_id, first.issue.issue_id)
        .await
        .unwrap();
    assert_eq!(issue.regression.unwrap().count.get(), 1);
    assert_eq!(counts(&database).await, (2, 1, 2, 1, 1, 3));
    assert_eq!(
        store
            .finalize(
                FinalizeBatch {
                    events: vec![regressed]
                },
                policy(10, 10)
            )
            .await
            .unwrap()
            .pending,
        0
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM metric.issue_activities WHERE kind=6")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    database.close().await;
    fixture.cleanup().await;
}
