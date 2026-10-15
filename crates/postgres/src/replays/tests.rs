use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};

fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn id(seed: u8) -> EventId {
    EventId::from_bytes([seed; 16])
}
fn commit(project_id: i32, seed: u8, segment_id: u32) -> ReplaySegmentCommit {
    let project_id = project(project_id);
    let replay_id = id(seed);
    ReplaySegmentCommit {
        metadata: ReplayMetadata {
            project_id,
            replay_id,
            segment_id,
            started_at: at(1000),
            ended_at: at(2000 + i64::from(segment_id)),
            received_at: at(3000 + i64::from(segment_id)),
            environment: Some("production".into()),
            release: Some("web@1.0.0".into()),
            url: Some("https://example.test/checkout".into()),
            error_ids: vec![id(8)],
            trace_ids: vec![TraceId::from_bytes([9; 16])],
        },
        segment: ReplaySegment {
            segment_id,
            object: BlobObject {
                key: BlobKey::replay_recording(project_id, replay_id, segment_id),
                kind: BlobKind::ReplayRecording,
                size: 64,
                checksum: BlobChecksum::from_bytes([segment_id as u8; 32]),
                created_at: at(3000 + i64::from(segment_id)),
            },
            decompressed_bytes: 128,
            event_count: 4,
        },
    }
}
fn query(limit: usize) -> ReplayQuery {
    ReplayQuery {
        from: None,
        until: None,
        error_id: None,
        trace_id: None,
        before: None,
        limit,
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresReplayStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresReplayStore::new(database.clone(), ReplayRetention::default()).unwrap();
    (fixture, database, store)
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn manifest_order_duplicates_metadata_retention_and_scope() {
    let (fixture, database, store) = setup().await;
    let first = commit(1, 1, 2);
    assert_eq!(
        store.persist_replay_segment(first.clone()).await.unwrap(),
        DurableOutcome::Accepted
    );
    let mut newer = commit(1, 1, 0);
    newer.metadata.started_at = at(500);
    newer.metadata.environment = None;
    newer.metadata.release = Some("'; DROP TABLE metric.projects; -- 雪".into());
    newer.metadata.error_ids = vec![id(4), id(4), id(5)];
    newer.metadata.trace_ids = vec![TraceId::from_bytes([5; 16])];
    store.persist_replay_segment(newer.clone()).await.unwrap();
    let before = store.load_replay(project(1), id(1)).await.unwrap();
    assert_eq!(
        before.segments,
        vec![newer.segment.clone(), first.segment.clone()]
    );
    assert_eq!(
        (before.started_at, before.ended_at, before.received_at),
        (at(500), at(2002), at(3000))
    );
    assert_eq!(before.environment, first.metadata.environment);
    assert_eq!(before.release, newer.metadata.release);
    assert_eq!(before.error_ids, newer.metadata.error_ids);
    assert_eq!(before.trace_ids, newer.metadata.trace_ids);
    assert_eq!(before.expires_at, Some(at(2002 + 30 * DAY)));
    let mut retry = newer.clone();
    retry.metadata.received_at = at(10000);
    retry.metadata.release = Some("ignored retry metadata".into());
    assert_eq!(
        store.persist_replay_segment(retry).await.unwrap(),
        DurableOutcome::Duplicate
    );
    assert_eq!(store.load_replay(project(1), id(1)).await.unwrap(), before);
    let mut conflict = newer.clone();
    conflict.segment.object.checksum = BlobChecksum::from_bytes([99; 32]);
    assert_eq!(
        store.persist_replay_segment(conflict).await,
        Err(SignalStoreError::Conflict)
    );
    let mut size_conflict = newer.clone();
    size_conflict.segment.object.size += 1;
    assert_eq!(
        store.persist_replay_segment(size_conflict).await,
        Err(SignalStoreError::Conflict)
    );
    assert_eq!(
        store.load_replay(project(2), id(1)).await,
        Err(SignalStoreError::NotFound)
    );
    let foreign = commit(2, 1, 0);
    store.persist_replay_segment(foreign.clone()).await.unwrap();
    assert_eq!(
        store.load_replay(project(2), id(1)).await.unwrap().segments,
        vec![foreign.segment]
    );
    assert!(
        store
            .references_replay_blob(&first.segment.object.key)
            .await
            .unwrap()
    );
    assert!(
        !store
            .references_replay_blob(&BlobKey::replay_recording(project(2), id(1), 2))
            .await
            .unwrap()
    );
    let archived = PostgresReplayStore::new(
        database.clone(),
        ReplayRetention {
            days: 14,
            archive: true,
        },
    )
    .unwrap();
    archived
        .persist_replay_segment(commit(1, 2, 0))
        .await
        .unwrap();
    let row = sqlx::query(
        "SELECT expire_at,archive_due FROM metric.replays WHERE project_id=1 AND replay_id=$1",
    )
    .bind(id(2).as_bytes().as_slice())
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(row.try_get::<Option<i64>, _>("expire_at").unwrap(), None);
    assert_eq!(
        row.try_get::<i64, _>("archive_due").unwrap(),
        2000 + 14 * DAY
    );
    assert_eq!(
        archived
            .load_replay(project(1), id(2))
            .await
            .unwrap()
            .expires_at,
        Some(at(2000 + 14 * DAY))
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn bounded_filters_pages_ties_and_foreign_cursors() {
    let (fixture, database, store) = setup().await;
    for seed in 1..=6 {
        let mut value = commit(1, seed, 0);
        value.metadata.received_at = at(4000 + i64::from(seed / 2));
        if seed % 2 == 0 {
            value.metadata.error_ids = vec![id(6)];
        }
        store.persist_replay_segment(value).await.unwrap();
    }
    store
        .persist_replay_segment(commit(2, 99, 0))
        .await
        .unwrap();
    let mut q = query(2);
    let mut seen = vec![];
    loop {
        let page = store.list_replays(project(1), q).await.unwrap();
        seen.extend(page.items.iter().map(|record| record.replay_id));
        match page.next {
            Some(before) => q.before = Some(before),
            None => break,
        }
    }
    assert_eq!(seen, vec![id(6), id(5), id(4), id(3), id(2), id(1)]);
    let mut q = query(100);
    q.from = Some(at(4001));
    q.until = Some(at(4002));
    assert_eq!(
        store
            .list_replays(project(1), q)
            .await
            .unwrap()
            .items
            .iter()
            .map(|r| r.replay_id)
            .collect::<Vec<_>>(),
        vec![id(3), id(2)]
    );
    q.from = None;
    q.until = None;
    q.error_id = Some(id(6));
    q.trace_id = Some(TraceId::from_bytes([9; 16]));
    assert_eq!(
        store.list_replays(project(1), q).await.unwrap().items.len(),
        3
    );
    q.trace_id = Some(TraceId::from_bytes([0; 16]));
    assert!(
        store
            .list_replays(project(1), q)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut q = query(100);
    q.before = Some(ReplayCursor {
        received_at: at(4002),
        replay_id: id(99),
    });
    let page = store.list_replays(project(1), q).await.unwrap();
    assert!(
        page.items
            .iter()
            .all(|record| record.project_id == project(1) && record.replay_id != id(99))
    );
    for limit in [0, 101] {
        assert_eq!(
            store.list_replays(project(1), query(limit)).await,
            Err(SignalStoreError::InvalidData)
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_first_segments_and_snapshot_reads_are_consistent() {
    let (fixture, database, store) = setup().await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let store = store.clone();
        tasks.spawn(async move { store.persist_replay_segment(commit(1, 1, 0)).await.unwrap() });
    }
    let mut accepted = 0;
    while let Some(result) = tasks.join_next().await {
        accepted += usize::from(result.unwrap() == DurableOutcome::Accepted);
    }
    assert_eq!(accepted, 1);
    for segment in 1..100 {
        let store = store.clone();
        tasks.spawn(async move {
            store
                .persist_replay_segment(commit(1, 1, segment))
                .await
                .unwrap()
        });
    }
    while !tasks.is_empty() {
        let record = store.load_replay(project(1), id(1)).await.unwrap();
        assert_eq!(
            record.ended_at,
            at(2000 + i64::from(record.segments.last().unwrap().segment_id))
        );
        if let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap(), DurableOutcome::Accepted);
        }
    }
    let record = store.load_replay(project(1), id(1)).await.unwrap();
    assert_eq!(
        record
            .segments
            .iter()
            .map(|s| s.segment_id)
            .collect::<Vec<_>>(),
        (0..100).collect::<Vec<_>>()
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn failed_manifest_update_rolls_back_segments_and_references() {
    let (fixture, database, store) = setup().await;
    sqlx::query("CREATE FUNCTION metric.fail_replay_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic replay fault'; END $$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_replay_update BEFORE UPDATE ON metric.replays FOR EACH ROW EXECUTE FUNCTION metric.fail_replay_update()").execute(&database.pool).await.unwrap();
    let first = commit(1, 1, 0);
    assert_eq!(
        store.persist_replay_segment(first.clone()).await,
        Err(SignalStoreError::Unavailable)
    );
    assert_eq!(
        store.load_replay(project(1), id(1)).await,
        Err(SignalStoreError::NotFound)
    );
    assert!(
        !store
            .references_replay_blob(&first.segment.object.key)
            .await
            .unwrap()
    );
    sqlx::query("DROP TRIGGER fail_replay_update ON metric.replays")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_replay_segment(first).await.unwrap();
    let before = store.load_replay(project(1), id(1)).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_replay_update BEFORE UPDATE ON metric.replays FOR EACH ROW EXECUTE FUNCTION metric.fail_replay_update()").execute(&database.pool).await.unwrap();
    let second = commit(1, 1, 1);
    assert_eq!(
        store.persist_replay_segment(second.clone()).await,
        Err(SignalStoreError::Unavailable)
    );
    assert_eq!(store.load_replay(project(1), id(1)).await.unwrap(), before);
    assert!(
        !store
            .references_replay_blob(&second.segment.object.key)
            .await
            .unwrap()
    );
    sqlx::query("DROP TRIGGER fail_replay_update ON metric.replays")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_replay_segment(second).await.unwrap(),
        DurableOutcome::Accepted
    );
    // The lifecycle owner can remove a manifest without leaving live segment refs.
    sqlx::query("DELETE FROM metric.replays WHERE project_id=1 AND replay_id=$1")
        .bind(id(1).as_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        !store
            .references_replay_blob(&commit(1, 1, 0).segment.object.key)
            .await
            .unwrap()
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_reply_retry_preserves_one_immutable_recording() {
    let (fixture, database, store) = setup().await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxied_database = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied =
        PostgresReplayStore::new(proxied_database.clone(), ReplayRetention::default()).unwrap();
    let value = commit(1, 1, 0);
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.persist_replay_segment(value.clone()).await,
        Err(SignalStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert!(
        store
            .references_replay_blob(&value.segment.object.key)
            .await
            .unwrap()
    );
    let before = store.load_replay(project(1), id(1)).await.unwrap();
    assert_eq!(
        proxied.persist_replay_segment(value).await.unwrap(),
        DurableOutcome::Duplicate
    );
    assert_eq!(store.load_replay(project(1), id(1)).await.unwrap(), before);
    proxied_database.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn validation_project_fences_and_corrupt_decoding_fail_closed() {
    let (fixture, database, store) = setup().await;
    let mut invalids = vec![];
    let mut value = commit(1, 1, 0);
    value.metadata.segment_id = 100;
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.metadata.release = Some("nul\0value".into());
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.metadata.error_ids = vec![id(1); 101];
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.metadata.ended_at = at(1000 + 86_400_001);
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.segment.object.size = u64::MAX;
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.segment.decompressed_bytes = u64::MAX;
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.segment.object.kind = BlobKind::EventAttachment;
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.segment.object.key = BlobKey::replay_recording(project(2), id(1), 0);
    invalids.push(value);
    let mut value = commit(1, 1, 0);
    value.metadata.ended_at = at(253402300799999);
    value.metadata.started_at = value.metadata.ended_at;
    invalids.push(value);
    for value in invalids {
        assert_eq!(
            store.persist_replay_segment(value).await,
            Err(SignalStoreError::InvalidData)
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.replays")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(matches!(
        PostgresReplayStore::new(
            database.clone(),
            ReplayRetention {
                days: 0,
                archive: false
            }
        ),
        Err(PostgresError::InvalidPolicy)
    ));
    assert_eq!(
        store.persist_replay_segment(commit(99, 1, 0)).await,
        Err(SignalStoreError::NotFound)
    );
    sqlx::query("UPDATE metric.projects SET state=1 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    store.persist_replay_segment(commit(1, 1, 0)).await.unwrap();
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_replay_segment(commit(1, 1, 1)).await,
        Err(SignalStoreError::NotFound)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.replay_segments")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("UPDATE metric.replays SET environment=$1 WHERE project_id=1")
        .bind(vec![255_u8])
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.load_replay(project(1), id(1)).await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store.list_replays(project(1), query(100)).await,
        Err(SignalStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.replays SET environment=NULL WHERE project_id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE metric.replay_segments SET blob_key=$1 WHERE project_id=1")
        .bind(BlobKey::replay_recording(project(2), id(1), 0).as_str())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.load_replay(project(1), id(1)).await,
        Err(SignalStoreError::InvalidData)
    );
    for ids in [vec![vec![1_u8; 16]; 101], vec![vec![1_u8; 15]]] {
        assert!(
            sqlx::query("UPDATE metric.replays SET trace_ids=$1 WHERE project_id=1")
                .bind(ids)
                .execute(&database.pool)
                .await
                .is_err()
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn timeline_correlations_and_reference_lookups_use_native_indexes() {
    let (fixture, database, store) = setup().await;
    store
        .persist_replay_segment(commit(1, 255, 0))
        .await
        .unwrap();
    sqlx::query("INSERT INTO metric.replays(project_id,replay_id,started_at,ended_at,received_at,expire_at) SELECT 2,decode(lpad(to_hex(n),32,'0'),'hex'),1000,2000,3000,3000000 FROM generate_series(1,10000) n").execute(&database.pool).await.unwrap();
    sqlx::query("INSERT INTO metric.replay_segments(project_id,replay_id,segment_id,blob_key,size,checksum,created_at,decompressed_bytes,event_count) SELECT 2,replay_id,0,'projects/2/replays/'||encode(replay_id,'hex')||'/0.rrweb',64,$1,3000,128,4 FROM metric.replays WHERE project_id=2").bind(vec![0_u8;32]).execute(&database.pool).await.unwrap();
    sqlx::query("ANALYZE metric.replays")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE metric.replay_segments")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT replay_id FROM metric.replays WHERE project_id=1 ORDER BY received_at DESC,replay_id DESC LIMIT 101").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("replay_project_received"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT replay_id FROM metric.replays WHERE project_id=2 AND error_ids @> $1::bytea[]").bind(vec![id(8).as_bytes().to_vec()]).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("replay_errors"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT replay_id FROM metric.replays WHERE project_id=2 AND trace_ids @> $1::bytea[]").bind(vec![vec![9_u8;16]]).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("replay_traces"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    let key = commit(1, 255, 0).segment.object.key;
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT 1 FROM metric.replay_segments WHERE blob_key=$1").bind(key.as_str()).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("replay_segment_blob_key"));
    assert!(store.references_replay_blob(&key).await.unwrap());
    assert_eq!(
        store
            .list_replays(project(1), query(100))
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    database.close().await;
    fixture.cleanup().await;
}
