use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};

fn time(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn update(
    project_id: i32,
    seed: u8,
    state: SessionState,
    sequence: u64,
    updated: i64,
) -> SessionUpdate {
    let project_id = project(project_id);
    SessionUpdate {
        id: SessionId::derive(project_id, [seed; 16]),
        project_id,
        release_id: ReleaseId::from_bytes([10; 16]),
        environment_id: EnvironmentId::from_bytes([20; 16]),
        started_at: time(1000),
        updated_at: time(updated),
        state,
        sequence: Some(sequence),
        duration_ms: Some(500),
        user_digest: Some([seed; 16]),
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresSessionStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresSessionStore::new(database.clone(), SessionRetention::default()).unwrap();
    (fixture, database, store)
}
async fn health(store: &PostgresSessionStore, project_id: i32) -> Vec<ReleaseHealthBucket> {
    store
        .release_health(
            project(project_id),
            ReleaseId::from_bytes([10; 16]),
            time(0),
            time(7_200_000),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn shared_whole_hour_ranges_preserve_counts_sketches_and_repeatability() {
    let (fixture, database, store) = setup().await;
    metric_testkit::session_conformance::whole_hour_rebuild(&store, project(1))
        .await
        .unwrap();
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lifecycle_merge_order_duplicates_scope_unsigned_sequence_and_retention() {
    let (fixture, database, store) = setup().await;
    let changes = vec![
        update(1, 1, SessionState::Ok, 1, 1100),
        update(1, 1, SessionState::Exited, 3, 1300),
        update(1, 1, SessionState::Crashed, 2, 1200),
        update(1, 1, SessionState::Exited, 3, 1300),
    ];
    assert_eq!(
        store.persist_sessions(changes.clone()).await.unwrap(),
        vec![
            DurableOutcome::Accepted,
            DurableOutcome::Accepted,
            DurableOutcome::Accepted,
            DurableOutcome::Duplicate
        ]
    );
    let mut expected = SessionRecord::from_update(changes[0].clone()).unwrap();
    for update in &changes[1..] {
        expected.merge(update.clone()).unwrap();
    }
    assert_eq!(
        store.load_session(project(1), changes[0].id).await.unwrap(),
        expected
    );
    assert_eq!(
        store.persist_sessions(changes.clone()).await.unwrap(),
        vec![DurableOutcome::Duplicate; 4]
    );
    let original_health = health(&store, 1).await;
    assert_eq!(original_health[0].sessions, 1);
    assert_eq!(original_health[0].crashed, 1);
    assert_eq!(original_health[0].exited, 0);
    for index in [1, 3, 0, 2] {
        let mut row = changes[index].clone();
        row.id = SessionId::derive(project(1), [2; 16]);
        row.user_digest = Some([2; 16]);
        store.persist_sessions(vec![row]).await.unwrap();
    }
    let reordered = store
        .load_session(project(1), SessionId::derive(project(1), [2; 16]))
        .await
        .unwrap();
    assert_eq!(reordered.state, expected.state);
    assert_eq!(reordered.finished_at, expected.finished_at);
    assert_eq!(reordered.sequence, expected.sequence);
    let maximum = update(1, 3, SessionState::Exited, u64::MAX, 2000);
    store.persist_sessions(vec![maximum.clone()]).await.unwrap();
    assert_eq!(
        store
            .load_session(project(1), maximum.id)
            .await
            .unwrap()
            .sequence,
        Some(u64::MAX)
    );
    assert_eq!(
        store.load_session(project(2), maximum.id).await,
        Err(SignalStoreError::NotFound)
    );
    assert!(health(&store, 2).await.is_empty());
    let values: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT expire_at,archive_due FROM metric.sessions WHERE project_id=1 AND session_id=$1",
    )
    .bind(maximum.id.as_bytes().as_slice())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(values, (Some(2000 + 7 * DAY), None));
    let archive = PostgresSessionStore::new(
        database.clone(),
        SessionRetention {
            archive: true,
            ..Default::default()
        },
    )
    .unwrap();
    let row = update(2, 1, SessionState::Crashed, 1, 2000);
    archive.persist_sessions(vec![row.clone()]).await.unwrap();
    let values: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT expire_at,archive_due FROM metric.sessions WHERE project_id=2 AND session_id=$1",
    )
    .bind(row.id.as_bytes().as_slice())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(values, (None, Some(2000 + 7 * DAY)));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_retry_merge_and_rebuild_preserve_authoritative_counts() {
    let (fixture, database, store) = setup().await;
    let value = update(1, 1, SessionState::Ok, 1, 1100);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let store = store.clone();
        let value = value.clone();
        tasks.spawn(async move { store.persist_sessions(vec![value]).await });
    }
    let mut accepted = 0;
    while let Some(result) = tasks.join_next().await {
        accepted += usize::from(result.unwrap().unwrap()[0] == DurableOutcome::Accepted);
    }
    assert_eq!(accepted, 1);
    assert_eq!(health(&store, 1).await[0].sessions, 1);
    for seed in 2..=40 {
        let store = store.clone();
        tasks.spawn(async move {
            store
                .persist_sessions(vec![
                    update(1, seed, SessionState::Exited, 1, 2000),
                    update(2, seed, SessionState::Crashed, 1, 2000),
                ])
                .await
        });
    }
    for _ in 0..12 {
        let store = store.clone();
        tasks.spawn(async move {
            store
                .rebuild_session_stats(project(1), time(500), time(1500))
                .await
                .map(|_| vec![])
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
    }
    for state in [
        SessionState::Exited,
        SessionState::Abnormal,
        SessionState::Crashed,
    ] {
        let store = store.clone();
        tasks.spawn(async move {
            store
                .persist_sessions(vec![update(1, 1, state, 3, 3000)])
                .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
    }
    let before = health(&store, 1).await;
    assert_eq!(before[0].sessions, 40);
    assert_eq!(before[0].crashed, 1);
    assert_eq!(before[0].exited, 39);
    assert_eq!(health(&store, 2).await[0].sessions, 39);
    assert_eq!(
        store
            .rebuild_session_stats(project(1), time(1), time(3599))
            .await
            .unwrap(),
        1
    );
    assert_eq!(health(&store, 1).await, before);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn session_and_repair_failures_rollback_all_effects_and_allow_recovery() {
    let (fixture, database, store) = setup().await;
    sqlx::raw_sql("CREATE FUNCTION metric.fail_session_stats() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic stats failure''; END'; CREATE TRIGGER fail_session_stats BEFORE UPDATE ON metric.session_stats_hourly FOR EACH ROW EXECUTE FUNCTION metric.fail_session_stats()").execute(&database.pool).await.unwrap();
    let value = update(1, 1, SessionState::Exited, 1, 2000);
    assert_eq!(
        store.persist_sessions(vec![value.clone()]).await,
        Err(SignalStoreError::Unavailable)
    );
    assert_eq!(
        store.load_session(project(1), value.id).await,
        Err(SignalStoreError::NotFound)
    );
    assert!(health(&store, 1).await.is_empty());
    sqlx::query("DROP TRIGGER fail_session_stats ON metric.session_stats_hourly")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_sessions(vec![value.clone()]).await.unwrap();
    let before = health(&store, 1).await;
    sqlx::query("CREATE TRIGGER fail_session_stats BEFORE INSERT ON metric.session_stats_hourly FOR EACH ROW EXECUTE FUNCTION metric.fail_session_stats()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .rebuild_session_stats(project(1), time(500), time(2000))
            .await,
        Err(SignalStoreError::Unavailable)
    );
    assert_eq!(health(&store, 1).await, before);
    sqlx::query("DROP TRIGGER fail_session_stats ON metric.session_stats_hourly")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE metric.session_stats_hourly SET sessions=999,user_sketch=B'0'::bit(1024) WHERE project_id=1").execute(&database.pool).await.unwrap();
    store
        .rebuild_session_stats(project(1), time(500), time(2000))
        .await
        .unwrap();
    assert_eq!(health(&store, 1).await, before);
    // A removed source leaves no stale derived bucket after an explicit repair.
    sqlx::query("DELETE FROM metric.sessions WHERE project_id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .rebuild_session_stats(project(1), time(500), time(2000))
            .await
            .unwrap(),
        0
    );
    assert!(health(&store, 1).await.is_empty());
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_reply_retry_does_not_double_session_or_health() {
    let (fixture, database, store) = setup().await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxy_database = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied =
        PostgresSessionStore::new(proxy_database.clone(), SessionRetention::default()).unwrap();
    let value = update(1, 1, SessionState::Crashed, 1, 2000);
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.persist_sessions(vec![value.clone()]).await,
        Err(SignalStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert_eq!(
        store.persist_sessions(vec![value.clone()]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    assert_eq!(health(&store, 1).await[0].sessions, 1);
    assert_eq!(health(&store, 1).await[0].crashed, 1);
    proxy_database.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn stale_terminalization_rechecks_locked_state_and_skips_fenced_projects() {
    let (fixture, database, store) = setup().await;
    let first = update(1, 1, SessionState::Ok, 1, 2000);
    let second = update(1, 2, SessionState::Ok, 1, 9000);
    store
        .persist_sessions(vec![first.clone(), second.clone()])
        .await
        .unwrap();
    assert_eq!(
        store
            .terminalize_stale_sessions(time(10000), Duration::from_millis(5000))
            .await
            .unwrap(),
        1
    );
    let record = store.load_session(project(1), first.id).await.unwrap();
    assert_eq!(record.state, SessionState::Abnormal);
    assert_eq!(record.finished_at, Some(time(7000)));
    assert_eq!(record.duration_ms, Some(6000));
    assert_eq!(
        store
            .terminalize_stale_sessions(time(10000), Duration::from_millis(5000))
            .await
            .unwrap(),
        0
    );
    // Simulate a stale worker snapshot after a fresh update has already committed.
    let stale = update(1, 2, SessionState::Abnormal, 1, 9000);
    store
        .persist_sessions(vec![update(1, 2, SessionState::Ok, 2, 15000)])
        .await
        .unwrap();
    assert_eq!(
        store
            .persist(vec![stale], Some((10000, 5000)))
            .await
            .unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    assert_eq!(
        store
            .load_session(project(1), second.id)
            .await
            .unwrap()
            .state,
        SessionState::Ok
    );
    let fenced = update(2, 1, SessionState::Ok, 1, 2000);
    store.persist_sessions(vec![fenced.clone()]).await.unwrap();
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=2")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .terminalize_stale_sessions(time(20000), Duration::from_millis(5000))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .load_session(project(2), fenced.id)
            .await
            .unwrap()
            .state,
        SessionState::Ok
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn identity_validation_fences_corruption_and_overflow_are_atomic() {
    let (fixture, database, store) = setup().await;
    let value = update(1, 1, SessionState::Exited, 1, 2000);
    store.persist_sessions(vec![value.clone()]).await.unwrap();
    let mut conflict = value.clone();
    conflict.release_id = ReleaseId::from_bytes([99; 16]);
    assert_eq!(
        store
            .persist_sessions(vec![update(1, 2, SessionState::Exited, 1, 2000), conflict])
            .await,
        Err(SignalStoreError::Conflict)
    );
    assert_eq!(health(&store, 1).await[0].sessions, 1);
    let mut invalid_value = value.clone();
    invalid_value.updated_at = time(999);
    assert_eq!(
        store.persist_sessions(vec![invalid_value]).await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store
            .persist_sessions(vec![value.clone(); MAX_BATCH + 1])
            .await,
        Err(SignalStoreError::InvalidData)
    );
    assert!(store.persist_sessions(vec![]).await.unwrap().is_empty());
    for state in [2_i16, 3, 4] {
        sqlx::query("UPDATE metric.projects SET state=$1 WHERE id=1")
            .bind(state)
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            store.persist_sessions(vec![value.clone()]).await,
            Err(SignalStoreError::NotFound)
        );
        assert_eq!(
            store
                .rebuild_session_stats(project(1), time(0), time(1000))
                .await,
            Err(SignalStoreError::NotFound)
        );
    }
    sqlx::query("UPDATE metric.projects SET state=1 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_sessions(vec![value.clone()]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    assert_eq!(
        store
            .rebuild_session_stats(project(1), time(1000), time(1000))
            .await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store
            .terminalize_stale_sessions(time(2000), Duration::ZERO)
            .await,
        Err(SignalStoreError::InvalidData)
    );
    sqlx::query(
        "UPDATE metric.session_stats_hourly SET sessions=9223372036854775807 WHERE project_id=1",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        store
            .persist_sessions(vec![update(1, 2, SessionState::Exited, 1, 2000)])
            .await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store
            .load_session(project(1), SessionId::derive(project(1), [2; 16]))
            .await,
        Err(SignalStoreError::NotFound)
    );
    store
        .rebuild_session_stats(project(1), time(0), time(1000))
        .await
        .unwrap();
    let missing = update(99, 1, SessionState::Exited, 1, 2000);
    assert_eq!(
        store.persist_sessions(vec![missing]).await,
        Err(SignalStoreError::NotFound)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn indexed_health_rebuild_and_stale_selection_remain_project_scoped() {
    let (fixture, database, store) = setup().await;
    let first = update(1, 1, SessionState::Exited, 1, 2000);
    store.persist_sessions(vec![first.clone()]).await.unwrap();
    sqlx::query("INSERT INTO metric.sessions(project_id,session_id,release_id,environment_id,started_at,last_update,state) SELECT 2,decode(lpad(to_hex(n),32,'0'),'hex'),$1,$2,n,n,0 FROM generate_series(1,10000) n").bind(first.release_id.as_bytes().as_slice()).bind(first.environment_id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    sqlx::query("ANALYZE metric.sessions")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,FORMAT JSON) SELECT session_id FROM metric.sessions WHERE project_id=1 AND started_at>=0 AND started_at<3600000").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("session_project_started"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
    assert_eq!(
        store
            .rebuild_session_stats(project(1), time(500), time(1500))
            .await
            .unwrap(),
        1
    );
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,FORMAT JSON) SELECT session_id FROM metric.sessions WHERE state=0 AND last_update<=10 ORDER BY last_update,project_id,session_id LIMIT 10").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("session_stale_active"));
    let mut users = UserSketch::default();
    users.insert([1; 16]);
    assert_eq!(health(&store, 1).await[0].user_sketch, users);
    database.close().await;
    fixture.cleanup().await;
}

#[test]
fn hour_bounds_cover_negative_and_final_domain_timestamps() {
    assert_eq!(
        rebuild_bounds(time(-1800000), time(1800000)).unwrap(),
        (-3600000, 3600000)
    );
    assert_eq!(
        rebuild_bounds(time(1), time(3600000)).unwrap(),
        (0, 3600000)
    );
    assert_eq!(
        rebuild_bounds(time(253402300799998), time(253402300799999)).unwrap(),
        (253402297200000, 253402300800000)
    );
}
