use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{Fixture, seed_projects},
};
use metric_domain::{grouping::derive_issue_id, issue::ActorKind};

fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn occurrence(
    project_id: i32,
    seed: u8,
    event: u8,
    occurred: i64,
    received: i64,
    release: Option<&str>,
) -> IssueOccurrence {
    let project_id = project(project_id);
    let mut bytes = [seed; 34];
    bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    let grouping_key = GroupingKey::parse(&bytes).unwrap();
    IssueOccurrence {
        project_id,
        grouping_key,
        issue_id: derive_issue_id(project_id, grouping_key),
        event_id: EventId::from_bytes([event; 16]),
        occurred_at: at(occurred),
        received_at: at(received),
        release: release.map(|value| IssueRelease::new(value).unwrap()),
        title: IssueTitle::new("Panic: bounded failure").unwrap(),
        culprit: Some(IssueCulprit::new("crate::serve").unwrap()),
        grouping: IssueGroupingDetail {
            strategy: GroupingStrategy::Message,
            explanation: GroupingExplanation {
                summary: "message grouping".into(),
                components: vec![GroupingComponent {
                    kind: GroupingComponentKind::Message,
                    value: "bounded failure".into(),
                }],
            },
        },
        increment: NonZeroU64::MIN,
    }
}
fn command(
    value: &IssueOccurrence,
    key: u8,
    time: i64,
    action: IssueCommandAction,
) -> IssueCommand {
    IssueCommand {
        project_id: value.project_id,
        issue_id: value.issue_id,
        idempotency_key: [key; 16],
        actor: ActorRef::new(ActorKind::User, [8; 16]),
        at: at(time),
        action,
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresIssueStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresIssueStore::new(database.clone(), IssueCodecConfig::default()).unwrap();
    (fixture, database, store)
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn occurrence_pairs_workflow_receipt_regression_and_retry_conformance() {
    let (fixture, database, store) = setup().await;
    let first = occurrence(1, 1, 1, 1000, 2000, Some("1.0"));
    let created = store.apply_occurrence(first.clone()).await.unwrap();
    assert_eq!(created.kind, IssueMutationKind::Created);
    assert_eq!(created.issue.representative_event_id, first.event_id);
    assert_eq!(
        store
            .apply_occurrence(first.clone())
            .await
            .unwrap()
            .issue
            .occurrence_count
            .get(),
        2,
        "standalone approximate count allows positive retry drift"
    );
    let later = occurrence(1, 1, 2, 2000, 3000, None);
    let updated = store.apply_occurrence(later.clone()).await.unwrap();
    assert_eq!(updated.issue.first_release, first.release);
    assert_eq!(updated.issue.last_release, None);
    assert_eq!(updated.issue.representative_event_id, later.event_id);
    let mut earlier = occurrence(1, 1, 0, 500, 3500, None);
    earlier.title = IssueTitle::new("replacement must not win").unwrap();
    earlier.culprit = None;
    earlier.grouping.explanation.summary = "different explanation".into();
    let updated = store.apply_occurrence(earlier).await.unwrap();
    assert_eq!(updated.issue.first_seen, at(500));
    assert_eq!(updated.issue.first_release, None);
    assert_eq!(updated.issue.last_release, None);
    assert_eq!(updated.issue.title, first.title);
    assert_eq!(updated.issue.culprit, first.culprit);
    assert_eq!(updated.issue.grouping, first.grouping);
    let tied = occurrence(1, 1, 3, 2000, 4000, Some("latest"));
    assert_eq!(
        store
            .apply_occurrence(tied.clone())
            .await
            .unwrap()
            .issue
            .latest_event_id,
        tied.event_id
    );
    let assignee = ActorRef::new(ActorKind::ApiCredential, [9; 16]);
    let assign = command(&first, 1, 5000, IssueCommandAction::Assign(Some(assignee)));
    assert!(store.apply_command(assign).await.unwrap().applied);
    assert!(!store.apply_command(assign).await.unwrap().applied);
    let resolve = command(&first, 2, 10000, IssueCommandAction::Resolve);
    assert!(store.apply_command(resolve).await.unwrap().applied);
    assert!(!store.apply_command(resolve).await.unwrap().applied);
    for received in [9000, 10000] {
        assert_eq!(
            store
                .apply_occurrence(occurrence(1, 1, 4, 11000, received, None))
                .await
                .unwrap()
                .issue
                .status,
            IssueStatus::Resolved
        );
    }
    let regression = occurrence(1, 1, 5, 100, 10001, Some("2.0"));
    let reopened = store.apply_occurrence(regression.clone()).await.unwrap();
    assert_eq!(
        reopened.kind,
        IssueMutationKind::Regressed,
        "receipt time governs regression even for older occurrence"
    );
    assert_eq!(reopened.issue.status, IssueStatus::Open);
    assert_eq!(reopened.issue.assignee, Some(assignee));
    assert!(reopened.issue.workflow.is_none());
    assert_eq!(reopened.issue.regression.as_ref().unwrap().count.get(), 1);
    let retried = store.apply_occurrence(regression).await.unwrap();
    assert_eq!(retried.kind, IssueMutationKind::Regressed);
    assert_eq!(retried.issue.regression.as_ref().unwrap().count.get(), 1);
    assert!(
        store
            .apply_command(command(&first, 3, 20000, IssueCommandAction::Resolve))
            .await
            .unwrap()
            .applied
    );
    assert_eq!(
        store
            .apply_occurrence(occurrence(1, 1, 6, 21000, 20001, None))
            .await
            .unwrap()
            .issue
            .regression
            .as_ref()
            .unwrap()
            .count
            .get(),
        2
    );
    let ignore = command(&first, 4, 30000, IssueCommandAction::Ignore);
    assert!(store.apply_command(ignore).await.unwrap().applied);
    assert!(!store.apply_command(ignore).await.unwrap().applied);
    let ignored = store
        .apply_occurrence(occurrence(1, 1, 7, 31000, 31000, None))
        .await
        .unwrap()
        .issue;
    assert_eq!(ignored.status, IssueStatus::Ignored);
    assert_eq!(ignored.regression.as_ref().unwrap().count.get(), 2);
    assert!(
        store
            .apply_command(command(&first, 5, 32000, IssueCommandAction::Reopen))
            .await
            .unwrap()
            .applied
    );
    assert!(
        !store
            .apply_command(command(&first, 5, 32000, IssueCommandAction::Reopen))
            .await
            .unwrap()
            .applied
    );
    assert!(
        store
            .apply_command(command(&first, 6, 33000, IssueCommandAction::Assign(None)))
            .await
            .unwrap()
            .applied
    );
    assert!(
        !store
            .apply_command(command(&first, 6, 33000, IssueCommandAction::Assign(None)))
            .await
            .unwrap()
            .applied
    );
    let activity_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM metric.issue_activities WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        activity_count, 8,
        "command/regression retries keep deterministic activity IDs"
    );
    let transitions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM metric.issue_transitions WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        transitions, 5,
        "new, two resolved and two regressed transitions"
    );
    assert_eq!(
        store.load(project(2), first.issue_id).await,
        Err(IssueStoreError::NotFound)
    );
    assert_eq!(
        store
            .apply_command(IssueCommand {
                project_id: project(2),
                ..resolve
            })
            .await,
        Err(IssueStoreError::NotFound)
    );
    let fresh = PostgresIssueStore::new(
        PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap(),
        IssueCodecConfig::default(),
    )
    .unwrap();
    assert_eq!(
        fresh.load(first.project_id, first.issue_id).await.unwrap(),
        store.load(first.project_id, first.issue_id).await.unwrap()
    );
    fresh.database.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_create_updates_and_regression_are_atomic() {
    let (fixture, database, store) = setup().await;
    let base = occurrence(1, 1, 0, 0, 0, None);
    let mut tasks = Vec::new();
    for byte in 1..=64_u8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .apply_occurrence(occurrence(
                    1,
                    1,
                    byte,
                    i64::from(byte),
                    i64::from(byte),
                    Some("concurrent"),
                ))
                .await
        }));
    }
    let mut created = 0;
    for task in tasks {
        if task.await.unwrap().unwrap().kind == IssueMutationKind::Created {
            created += 1;
        }
    }
    assert_eq!(created, 1);
    let snapshot = store.load(base.project_id, base.issue_id).await.unwrap();
    assert_eq!(snapshot.occurrence_count.get(), 64);
    assert_eq!(snapshot.first_event_id, EventId::from_bytes([1; 16]));
    assert_eq!(snapshot.latest_event_id, EventId::from_bytes([64; 16]));
    store
        .apply_command(command(&base, 1, 100, IssueCommandAction::Resolve))
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for byte in 65..=96_u8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .apply_occurrence(occurrence(1, 1, byte, i64::from(byte), 200, None))
                .await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let snapshot = store.load(base.project_id, base.issue_id).await.unwrap();
    assert_eq!(snapshot.occurrence_count.get(), 96);
    assert_eq!(snapshot.regression.as_ref().unwrap().count.get(), 1);
    assert_eq!(snapshot.status, IssueStatus::Open);
    let transitions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM metric.issue_transitions WHERE project_id=1 AND kind=2",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(transitions, 1);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lossless_nul_utf8_identity_and_title_search_preserve_originals() {
    let (fixture, database, store) = setup().await;
    let mut value = occurrence(
        1,
        1,
        1,
        10,
        20,
        Some("release\0'; DROP TABLE metric.issues; -- 雪"),
    );
    value.title = IssueTitle::new("PANIC\0Café\0Ошибка 雪").unwrap();
    value.culprit = Some(IssueCulprit::new("module\0function").unwrap());
    value.grouping.explanation.summary = "summary\0雪".into();
    value.grouping.explanation.components[0].value = "component\0'; --".into();
    let created = store.apply_occurrence(value.clone()).await.unwrap().issue;
    assert_eq!(created.title, value.title);
    assert_eq!(created.culprit, value.culprit);
    assert_eq!(created.first_release, value.release);
    assert_eq!(created.last_release, value.release);
    assert_eq!(created.grouping, value.grouping);
    assert_eq!(
        store.load(value.project_id, value.issue_id).await.unwrap(),
        created
    );
    for text in [
        "panic",
        "CAFÉ",
        "cafe",
        "ошибка",
        "雪",
        "\"panic cafe\"",
        "panic absent",
        "panic -absent",
        "cafe\0ошибка",
    ] {
        let found = store
            .search_titles(project(1), IssueSearchQuery::new(text, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "query {text:?}");
        assert_eq!(found[0].title, value.title);
    }
    for text in [
        "\"cafe panic\"",
        "panic -cafe",
        "-panic",
        "\0",
        "\"'; DROP TABLE metric.issues; --\"",
    ] {
        assert!(
            store
                .search_titles(project(1), IssueSearchQuery::new(text, 10).unwrap())
                .await
                .unwrap()
                .is_empty(),
            "query {text:?}"
        );
    }
    assert!(
        store
            .search_titles(project(2), IssueSearchQuery::new("panic", 10).unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    let mut second = occurrence(
        2,
        1,
        1,
        10,
        20,
        Some("release\0'; DROP TABLE metric.issues; -- 雪"),
    );
    second.title = value.title.clone();
    store.apply_occurrence(second.clone()).await.unwrap();
    assert_ne!(second.issue_id, value.issue_id);
    assert_eq!(
        store
            .search_titles(project(2), IssueSearchQuery::new("panic", 100).unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT first_release FROM metric.issues WHERE project_id=1 AND issue_id=$1",
    )
    .bind(value.issue_id.as_bytes().as_slice())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(raw, value.release.unwrap().as_str().as_bytes());
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('metric.issues') IS NOT NULL")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert!(exists, "malicious text remains bound data");
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn shared_term_phrase_unicode_and_negation_search_corpus() {
    let (fixture, database, store) = setup().await;
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../testkit/fixtures/issue-title-search.json"
    ))
    .unwrap();
    let mut identities = std::collections::BTreeMap::new();
    for title in corpus["titles"].as_array().unwrap() {
        let seed = title["id"].as_u64().unwrap() as u8;
        let mut value = occurrence(
            title["project"].as_i64().unwrap() as i32,
            seed,
            seed,
            10,
            20,
            None,
        );
        value.title = IssueTitle::new(title["title"].as_str().unwrap()).unwrap();
        identities.insert(value.issue_id, u64::from(seed));
        store.apply_occurrence(value).await.unwrap();
    }
    for query in corpus["queries"].as_array().unwrap() {
        let text = query["text"].as_str().unwrap();
        let mut actual: Vec<_> = store
            .search_titles(project(1), IssueSearchQuery::new(text, 100).unwrap())
            .await
            .unwrap()
            .iter()
            .map(|result| identities[&result.issue_id])
            .collect();
        actual.sort_unstable();
        let expected: Vec<_> = query["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_u64().unwrap())
            .collect();
        assert_eq!(actual, expected, "shared search query {text:?}");
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn critical_transition_rollback_and_best_effort_activity_failure() {
    let (fixture, database, store) = setup().await;
    let value = occurrence(1, 1, 1, 10, 20, None);
    sqlx::query("CREATE FUNCTION metric.reject_issue_transition() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'synthetic failure'; END$$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_issue_transition BEFORE INSERT ON metric.issue_transitions FOR EACH ROW EXECUTE FUNCTION metric.reject_issue_transition()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store.apply_occurrence(value.clone()).await,
        Err(IssueStoreError::Unavailable)
    );
    assert_eq!(
        store.load(value.project_id, value.issue_id).await,
        Err(IssueStoreError::NotFound)
    );
    sqlx::query("DROP TRIGGER reject_issue_transition ON metric.issue_transitions")
        .execute(&database.pool)
        .await
        .unwrap();
    store.apply_occurrence(value.clone()).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_issue_transition BEFORE INSERT ON metric.issue_transitions FOR EACH ROW EXECUTE FUNCTION metric.reject_issue_transition()").execute(&database.pool).await.unwrap();
    let resolve = command(&value, 1, 100, IssueCommandAction::Resolve);
    assert_eq!(
        store.apply_command(resolve).await,
        Err(IssueStoreError::Unavailable)
    );
    assert_eq!(
        store
            .load(value.project_id, value.issue_id)
            .await
            .unwrap()
            .status,
        IssueStatus::Open
    );
    sqlx::query("DROP TRIGGER reject_issue_transition ON metric.issue_transitions")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_issue_activity BEFORE INSERT ON metric.issue_activities FOR EACH ROW EXECUTE FUNCTION metric.reject_issue_transition()").execute(&database.pool).await.unwrap();
    assert!(store.apply_command(resolve).await.unwrap().applied);
    assert_eq!(
        store
            .load(value.project_id, value.issue_id)
            .await
            .unwrap()
            .status,
        IssueStatus::Resolved
    );
    assert_eq!(
        store
            .apply_occurrence(occurrence(1, 1, 2, 200, 101, None))
            .await
            .unwrap()
            .issue
            .status,
        IssueStatus::Open
    );
    let activities: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.issue_activities")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(activities, 0);
    let transitions: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.issue_transitions")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        transitions, 3,
        "best-effort history failure cannot lose authoritative transitions"
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn corrupt_identity_utf8_body_bounds_and_overflow_reject_without_mutation() {
    let (fixture, database, store) = setup().await;
    let value = occurrence(1, 1, 1, 10, 20, None);
    let mut invalid = value.clone();
    invalid.issue_id = IssueId::from_bytes([9; 16]);
    assert_eq!(
        store.apply_occurrence(invalid).await,
        Err(IssueStoreError::InvalidData)
    );
    let mut overflow = value.clone();
    overflow.increment = NonZeroU64::new(u64::MAX).unwrap();
    assert_eq!(
        store.apply_occurrence(overflow).await,
        Err(IssueStoreError::InvalidData)
    );
    let mut too_big = value.clone();
    too_big.grouping.explanation.summary = "x".repeat(65536).into();
    assert_eq!(
        store.apply_occurrence(too_big).await,
        Err(IssueStoreError::InvalidData)
    );
    let mut compressed = value.clone();
    compressed.grouping.explanation.summary = "repeated component".repeat(1000).into();
    store.apply_occurrence(compressed.clone()).await.unwrap();
    let codec: Vec<u8> =
        sqlx::query_scalar("SELECT grouping_body FROM metric.issues WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(&codec[..2], &[1, 1]);
    assert_eq!(
        store
            .load(value.project_id, value.issue_id)
            .await
            .unwrap()
            .grouping,
        compressed.grouping
    );
    let bounded = PostgresIssueStore::new(
        database.clone(),
        IssueCodecConfig {
            max_decoded_body_bytes: 100,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        bounded.load(value.project_id, value.issue_id).await,
        Err(IssueStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.issues SET occurrence_count=$1 WHERE project_id=1")
        .bind(i64::MAX)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.apply_occurrence(value.clone()).await,
        Err(IssueStoreError::InvalidData)
    );
    let count: i64 =
        sqlx::query_scalar("SELECT occurrence_count FROM metric.issues WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(count, i64::MAX);
    sqlx::query("UPDATE metric.issues SET grouping_key=$1 WHERE project_id=1")
        .bind(
            occurrence(1, 2, 1, 10, 20, None)
                .grouping_key
                .to_bytes()
                .as_slice(),
        )
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.apply_occurrence(value.clone()).await,
        Err(IssueStoreError::IdentityCollision)
    );
    assert_eq!(
        store.load(value.project_id, value.issue_id).await,
        Err(IssueStoreError::InvalidData)
    );
    sqlx::query(
        "UPDATE metric.issues SET grouping_key=$1,title=decode('ff','hex') WHERE project_id=1",
    )
    .bind(value.grouping_key.to_bytes().as_slice())
    .execute(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        store.load(value.project_id, value.issue_id).await,
        Err(IssueStoreError::InvalidData)
    );
    sqlx::query(
        "UPDATE metric.issues SET title=$1,grouping_body=decode('0200','hex') WHERE project_id=1",
    )
    .bind(value.title.as_str().as_bytes())
    .execute(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        store.load(value.project_id, value.issue_id).await,
        Err(IssueStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn batch_helper_pairs_and_indexed_bounded_title_search() {
    let (fixture, database, store) = setup().await;
    let a = occurrence(1, 1, 2, 20, 30, Some("middle"));
    let b = occurrence(1, 1, 1, 10, 100, None);
    let c = occurrence(1, 1, 3, 20, 40, Some("latest"));
    let mut transaction = database.pool.begin().await.unwrap();
    lock_project(&mut transaction, project(1)).await.unwrap();
    let (result, activity) = store
        .apply_batch(&mut transaction, &[a.clone(), b.clone(), c.clone()])
        .await
        .unwrap();
    assert!(activity.is_none());
    assert_eq!(result.issue.occurrence_count.get(), 3);
    assert_eq!(result.issue.title, a.title);
    assert_eq!(result.issue.first_event_id, b.event_id);
    assert_eq!(result.issue.latest_event_id, c.event_id);
    assert_eq!(result.issue.first_release, None);
    assert_eq!(result.issue.last_release, c.release);
    transaction.commit().await.unwrap();
    // Enough unrelated scoped data to require an actual selective GIN plan.
    sqlx::query("INSERT INTO metric.issues(project_id,issue_id,grouping_key,title,search_projection,first_seen,last_seen,first_event_id,latest_event_id,representative_event_id,occurrence_count,grouping_body) SELECT 1,substring(sha256(int4send(i)) FROM 1 FOR 16),decode('0001','hex')||sha256(int4send(i)),convert_to('ordinary noise','UTF8'),'ordinary noise',10,10,decode(repeat('00',16),'hex'),decode(repeat('00',16),'hex'),decode(repeat('00',16),'hex'),1,decode('01007b7d','hex') FROM generate_series(1000,10999) i").execute(&database.pool).await.unwrap();
    sqlx::query("VACUUM ANALYZE metric.issues")
        .execute(&database.pool)
        .await
        .unwrap();
    let plans:Value = sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT issue_id,title,status,last_seen,occurrence_count FROM metric.issues WHERE project_id=$1 AND title_search @@ websearch_to_tsquery('pg_catalog.simple'::regconfig,$2) ORDER BY ts_rank(title_search,websearch_to_tsquery('pg_catalog.simple'::regconfig,$2)) DESC,issue_id LIMIT 10")
        .bind(1_i32).bind("bounded OR failure").fetch_one(&database.pool).await.unwrap();
    assert!(plans.to_string().contains("issue_title_text"), "{plans}");
    assert_eq!(
        store
            .search_titles(
                project(1),
                IssueSearchQuery::new("bounded failure", 10).unwrap()
            )
            .await
            .unwrap()
            .len(),
        1
    );
    database.close().await;
    fixture.cleanup().await;
}
