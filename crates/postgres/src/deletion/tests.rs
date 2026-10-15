use super::*;
use crate::{
    EventCodecConfig, MigrationPolicy, PostgresEventStore, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, project_identity, seed_projects},
};
use metric_domain::{
    AcceptedEvent, EventId, ProjectKeyIdentity, ProjectKeyLabel, ProjectKeyState,
    ScrubbedEventPayload, Slug, auth::UserId,
};
use metric_ports::{EventStore, ProjectStore};
use std::sync::Arc;
fn pid(n: i32) -> ProjectId {
    ProjectId::new(n).unwrap()
}
fn at(n: i64) -> Timestamp {
    Timestamp::from_unix_millis(n).unwrap()
}
fn request(project: i32, organization: u64, seed: u8) -> ProjectDeletionRequest {
    ProjectDeletionRequest {
        operation_id: ProjectDeletionOperationId::from_bytes([seed; 16]),
        project_id: pid(project),
        organization_id: OrganizationId::new(organization).unwrap(),
        requested_by: UserId::new(1).unwrap(),
        requested_at: at(1_800_000_000_000),
        purge_after: at(1_800_000_010_000),
    }
}
fn purge(batch: usize) -> ProjectPurgeRequest {
    ProjectPurgeRequest {
        now: at(1_800_000_020_000),
        batch_size: batch,
        retry_base: Duration::from_millis(10),
        retry_max: Duration::from_millis(40),
        completed_retention: Duration::from_secs(1),
        slug_reservation: Duration::from_secs(60),
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresProjectDeletionStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let db = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(db.clone(), 32).await;
    let store = PostgresProjectDeletionStore::new(db.clone(), 32).unwrap();
    (fixture, db, store)
}
async fn count(db: &PostgresDatabase, table: &str, p: i32) -> i64 {
    let mut q = QueryBuilder::<Postgres>::new("SELECT count(*) FROM ");
    q.push(table).push(" WHERE project_id=").push_bind(p);
    q.build_query_scalar().fetch_one(&db.pool).await.unwrap()
}
async fn finish(
    store: &PostgresProjectDeletionStore,
    r: ProjectPurgeRequest,
) -> ProjectDeletionStatus {
    for _ in 0..256 {
        if let Some(status) = store.purge_next(r).await.unwrap()
            && status.phase == ProjectDeletionPhase::Deleted
        {
            return status;
        }
    }
    panic!("bounded deletion did not terminate")
}
async fn events(db: &PostgresDatabase, n: u8) {
    let store = PostgresEventStore::new(db.clone(), EventCodecConfig::default()).unwrap();
    let mut batch = Vec::new();
    for p in [1, 2] {
        for seed in 1..=n {
            batch.push(
                store
                    .prepare(AcceptedEvent {
                        project_id: pid(p),
                        event_id: EventId::from_bytes([seed; 16]),
                        received_at: at(1_800_000_000_000),
                        policy_revision: 1,
                        payload: ScrubbedEventPayload::new(serde_json::to_vec(&serde_json::json!({"event_id":EventId::from_bytes([seed;16]).to_string(),"platform":"rust","message":"synthetic deletion"})).unwrap()),
                    })
                    .unwrap(),
            );
        }
    }
    store.insert_batch(&batch).await.unwrap();
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn lifecycle_scope_cancel_keys_restart_rescan_and_slug_release() {
    let (fixture, db, store) = setup().await;
    let projects =
        crate::PostgresProjectStore::new(db.clone(), metric_domain::SecretBytes::new([42; 32]), 32)
            .unwrap();
    for (seed, state) in [(1, ProjectKeyState::Active), (2, ProjectKeyState::Disabled)] {
        projects
            .insert_project_key(ProjectKeyIdentity {
                key: DsnKey::from_bytes([seed; 16]),
                project_id: pid(1),
                state,
                label: ProjectKeyLabel::new("test").unwrap(),
                created_at: at(1_800_000_000_000),
            })
            .await
            .unwrap();
    }
    assert_eq!(
        store.request_deletion(request(1, 2, 1)).await,
        Err(ProjectDeletionStoreError::NotFound)
    );
    let r = request(1, 1, 1);
    let first = store.request_deletion(r).await.unwrap();
    assert_eq!(first.affected_keys.len(), 2);
    assert_eq!(store.request_deletion(r).await.unwrap(), first);
    assert_eq!(
        store.request_deletion(request(1, 1, 2)).await,
        Err(ProjectDeletionStoreError::Conflict)
    );
    let keys = projects.list_project_keys(pid(1)).await.unwrap();
    assert_eq!(keys[0].state, ProjectKeyState::SuspendedByDeletion);
    assert_eq!(keys[1].state, ProjectKeyState::Disabled);
    let mut early = purge(1);
    early.now = r.requested_at;
    assert!(store.purge_next(early).await.unwrap().is_none());
    assert_eq!(
        store
            .cancel_deletion(
                pid(2),
                r.operation_id,
                r.requested_at,
                Duration::from_secs(1)
            )
            .await,
        Err(ProjectDeletionStoreError::NotFound)
    );
    let cancelled = store
        .cancel_deletion(
            pid(1),
            r.operation_id,
            r.requested_at,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.status.phase, ProjectDeletionPhase::Cancelled);
    let keys = projects.list_project_keys(pid(1)).await.unwrap();
    assert_eq!(keys[0].state, ProjectKeyState::Active);
    assert_eq!(keys[1].state, ProjectKeyState::Disabled);
    events(&db, 5).await;
    let r = request(1, 1, 3);
    store.request_deletion(r).await.unwrap();
    // First call passes empty references; Event batches remove at most one row.
    store.purge_next(purge(1)).await.unwrap();
    store.purge_next(purge(1)).await.unwrap();
    assert_eq!(count(&db, "metric.error_events", 1).await, 4);
    assert_eq!(
        store
            .cancel_deletion(
                pid(1),
                r.operation_id,
                r.requested_at,
                Duration::from_secs(1)
            )
            .await,
        Err(ProjectDeletionStoreError::NotCancellable)
    );
    let restarted = PostgresProjectDeletionStore::new(db.clone(), 32).unwrap();
    while restarted
        .deletion_status(pid(1))
        .await
        .unwrap()
        .dataset_code
        == 11
    {
        restarted.purge_next(purge(1)).await.unwrap();
    }
    // Synthetic late write bypasses the public fence to prove the second pass.
    sqlx::query("INSERT INTO metric.error_events(project_id,event_id,received_at,policy_revision,body,next_attempt_at) SELECT 1,$1,$2,1,body,$2 FROM metric.error_events WHERE project_id=2 ORDER BY event_id LIMIT 1")
        .bind([99_u8;16].as_slice()).bind(r.requested_at.unix_millis()).execute(&db.pool).await.unwrap();
    let completed = finish(&restarted, purge(1)).await;
    assert!(completed.reconciliation_pass);
    assert_eq!(count(&db, "metric.error_events", 1).await, 0);
    assert_eq!(count(&db, "metric.error_events", 2).await, 5);
    for (_, table) in OWNED {
        assert_eq!(count(&db, table, 1).await, 0, "{table}");
    }
    assert_eq!(
        projects.load_project_by_id(pid(1)).await,
        Err(metric_ports::ProjectStoreError::NotFound)
    );
    let tombstone: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM metric.projects p WHERE id=1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(tombstone["display_name"], "Deleted");
    assert_eq!(tombstone["state"], 4);
    assert_eq!(tombstone["slug"], "same-project");
    // Job may expire before the slug reservation; release depends on the project.
    sqlx::query("DELETE FROM metric.project_deletions WHERE project_id=1 AND phase IN(2,3)")
        .execute(&db.pool)
        .await
        .unwrap();
    let mut later = purge(1);
    later.now = at(later.now.unix_millis() + 60_000);
    assert!(store.purge_next(later).await.unwrap().is_none());
    let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM metric.projects WHERE id=1")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(slug.is_none());
    let mut replacement = project_identity(3, 1);
    replacement.slug = Slug::new("same-project").unwrap();
    projects.insert_project(replacement).await.unwrap();
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn all_existing_archive_source_families_and_dependent_rows_are_purged() {
    let (fixture, db, store) = setup().await;
    crate::archive::tests::seed_all(&db, crate::archive::tests::now()).await;
    let active_log = count(&db, "metric.logs", 2).await;
    store.request_deletion(request(1, 1, 1)).await.unwrap();
    finish(&store, purge(1)).await;
    for (_, table) in OWNED {
        assert_eq!(count(&db, table, 1).await, 0, "{table}");
    }
    assert_eq!(count(&db, "metric.logs", 2).await, active_log);
    assert_eq!(count(&db, "metric.spans", 2).await, 1);
    assert_eq!(count(&db, "metric.sessions", 2).await, 1);
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn shared_artifacts_detach_preserve_other_projects_and_wait_for_gc() {
    use metric_domain::{
        artifacts::{ArtifactBinding, ArtifactBundle, ArtifactBundleId, ArtifactUpload},
        debug_files::DebugId,
    };
    use metric_ports::ArtifactStore;
    let (fixture, db, store) = setup().await;
    let projects =
        crate::PostgresProjectStore::new(db.clone(), metric_domain::SecretBytes::new([42; 32]), 32)
            .unwrap();
    let mut third = project_identity(3, 1);
    third.slug = Slug::new("third").unwrap();
    projects.insert_project(third).await.unwrap();
    let artifacts =
        crate::PostgresArtifactStore::new(db.clone(), crate::ArtifactQuota::default()).unwrap();
    let mut bundles = Vec::new();
    for seed in [1_u8, 2] {
        let mut bindings = vec![ArtifactBinding::new(pid(1), None, None).unwrap()];
        if seed == 1 {
            bindings.push(ArtifactBinding::new(pid(3), None, None).unwrap());
        }
        let upload = ArtifactUpload {
            id: [seed; 16],
            organization_id: OrganizationId::new(1).unwrap(),
            sha1: [seed; 20],
            chunks: vec![[seed; 20]],
            bindings: bindings.clone(),
            created_at: purge(1).now,
            updated_at: purge(1).now,
        };
        artifacts.upsert_upload(upload.clone()).await.unwrap();
        let bundle = ArtifactBundle {
            id: ArtifactBundleId::from_bytes(upload.id),
            organization_id: upload.organization_id,
            bindings,
            bundle_debug_id: DebugId::Uuid {
                uuid: [9; 16],
                appendix: 0,
            },
            debug_id_tokens: vec![],
            checksum: [seed; 32],
            sha1: upload.sha1,
            size: 3,
            uploaded_at: upload.updated_at,
            generation: 0,
        };
        artifacts
            .publish_bundle(upload.id, bundle.clone())
            .await
            .unwrap();
        bundles.push(bundle);
    }
    // Organization-shared Releases and Deploys have one shared and one sole-project parent.
    for seed in [1_u8, 2] {
        sqlx::query("INSERT INTO metric.releases(organization_id,release_id,version,created_at,activity_at) VALUES(1,$1,$2,1800000000000,1800000000000)")
            .bind([seed;16].as_slice()).bind(format!("release-{seed}").into_bytes()).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO metric.deploys(organization_id,deploy_id,release_id,environment,started_at,created_at) VALUES(1,$1,$1,$2,1800000000000,1800000000000)")
            .bind([seed;16].as_slice()).bind(b"prod".as_slice()).execute(&db.pool).await.unwrap();
        for (ordinal, p) in if seed == 1 {
            vec![(0_i16, 1), (1, 3)]
        } else {
            vec![(0_i16, 1)]
        } {
            sqlx::query("INSERT INTO metric.release_projects(organization_id,release_id,project_id) VALUES(1,$1,$2)")
                .bind([seed;16].as_slice()).bind(p).execute(&db.pool).await.unwrap();
            sqlx::query("INSERT INTO metric.deploy_projects(organization_id,deploy_id,release_id,started_at,project_id,ordinal) VALUES(1,$1,$1,1800000000000,$2,$3)")
                .bind([seed;16].as_slice()).bind(p).bind(ordinal).execute(&db.pool).await.unwrap();
        }
    }
    store.request_deletion(request(1, 1, 1)).await.unwrap();
    for _ in 0..128 {
        let status = store.purge_next(purge(1)).await.unwrap().unwrap();
        if i32::from(status.dataset_code) == LAST {
            break;
        }
    }
    let waiting = store.purge_next(purge(1)).await.unwrap().unwrap();
    assert_eq!(waiting.phase, ProjectDeletionPhase::Purging);
    assert!(!waiting.reconciliation_pass);
    let shared = artifacts
        .load_for_project(pid(3), bundles[0].id)
        .await
        .unwrap();
    assert_eq!(shared.bindings.len(), 1);
    assert_eq!(shared.bindings[0].project_id, pid(3));
    let claims = artifacts
        .claim_gc(
            purge(1).now,
            at(purge(1).now.unix_millis() + 60_000),
            [4; 16],
            10,
        )
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].bundle.id, bundles[1].id);
    assert!(
        artifacts
            .finish_gc(
                claims[0].bundle.id,
                0,
                [4; 16],
                at(purge(1).now.unix_millis() + 60_000)
            )
            .await
            .unwrap()
    );
    finish(&store, purge(1)).await;
    assert_eq!(
        artifacts
            .load_for_project(pid(3), bundles[0].id)
            .await
            .unwrap()
            .bindings
            .len(),
        1
    );
    let quota: (i64, i64) =
        sqlx::query_as("SELECT artifact_count,artifact_bytes FROM metric.organizations WHERE id=1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(quota, (1, 3));
    assert_eq!(count(&db, "metric.artifact_bundle_bindings", 1).await, 0);
    assert_eq!(count(&db, "metric.artifact_upload_bindings", 1).await, 0);
    assert_eq!(count(&db, "metric.release_projects", 1).await, 0);
    assert_eq!(count(&db, "metric.deploy_projects", 1).await, 0);
    assert_eq!(count(&db, "metric.release_projects", 3).await, 1);
    assert_eq!(count(&db, "metric.deploy_projects", 3).await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM metric.releases WHERE organization_id=1"
        )
        .fetch_one(&db.pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.deploys WHERE organization_id=1")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        1
    );
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn native_commit_reply_loss_request_cancel_and_purge_are_observable() {
    let (fixture, db, store) = setup().await;
    events(&db, 2).await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let fault = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let interrupted = PostgresProjectDeletionStore::new(fault.clone(), 32).unwrap();
    let r = request(1, 1, 1);
    proxy.drop_next_commit_reply();
    assert_eq!(
        interrupted.request_deletion(r).await,
        Err(ProjectDeletionStoreError::Unavailable)
    );
    assert_eq!(
        store.request_deletion(r).await.unwrap().status.phase,
        ProjectDeletionPhase::PendingGrace
    );
    proxy.drop_next_commit_reply();
    assert_eq!(
        interrupted
            .cancel_deletion(
                pid(1),
                r.operation_id,
                r.requested_at,
                Duration::from_secs(60)
            )
            .await,
        Err(ProjectDeletionStoreError::Unavailable)
    );
    assert_eq!(
        store.deletion_status(pid(1)).await.unwrap().phase,
        ProjectDeletionPhase::Cancelled
    );
    store.request_deletion(request(1, 1, 2)).await.unwrap();
    store.purge_next(purge(1)).await.unwrap();
    proxy.drop_next_commit_reply();
    assert_eq!(
        interrupted.purge_next(purge(1)).await,
        Err(ProjectDeletionStoreError::Unavailable)
    );
    assert_eq!(count(&db, "metric.error_events", 1).await, 1);
    assert_eq!(
        store.deletion_status(pid(1)).await.unwrap().dataset_code,
        11
    );
    finish(&store, purge(1)).await;
    assert_eq!(count(&db, "metric.error_events", 2).await, 2);
    fault.pool.close().await;
    proxy.close().await;
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn batch_failure_rolls_back_and_schedules_bounded_redacted_retry() {
    let (fixture, db, store) = setup().await;
    events(&db, 2).await;
    store.request_deletion(request(1, 1, 1)).await.unwrap();
    store.purge_next(purge(1)).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION metric.reject_purge() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic hidden credential'; END $$; CREATE TRIGGER reject_purge BEFORE DELETE ON metric.error_events FOR EACH ROW EXECUTE FUNCTION metric.reject_purge();").execute(&db.pool).await.unwrap();
    assert_eq!(
        store.purge_next(purge(1)).await,
        Err(ProjectDeletionStoreError::Unavailable)
    );
    assert_eq!(count(&db, "metric.error_events", 1).await, 2);
    let status = store.deletion_status(pid(1)).await.unwrap();
    assert_eq!(status.attempts, 1);
    assert_eq!(status.last_error.as_deref(), Some("dataset_batch_failed"));
    assert_eq!(status.next_attempt_at, at(purge(1).now.unix_millis() + 10));
    assert!(store.purge_next(purge(1)).await.unwrap().is_none());
    sqlx::raw_sql(
        "DROP TRIGGER reject_purge ON metric.error_events; DROP FUNCTION metric.reject_purge();",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let mut retry = purge(1);
    retry.now = status.next_attempt_at;
    let progress = store.purge_next(retry).await.unwrap().unwrap();
    assert_eq!(progress.attempts, 0);
    assert!(progress.last_error.is_none());
    finish(&store, retry).await;
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn concurrent_request_cancel_purge_and_ingest_preserve_fence_and_other_tenants() {
    let (fixture, db, store) = setup().await;
    events(&db, 3).await;
    let r = request(1, 1, 1);
    let (a, b) = tokio::join!(store.request_deletion(r), store.request_deletion(r));
    assert_eq!(a.unwrap(), b.unwrap());
    let shared = Arc::new(store.clone());
    let cancel = shared.clone();
    let worker = shared.clone();
    let (cancelled, purged) = tokio::join!(
        cancel.cancel_deletion(
            pid(1),
            r.operation_id,
            r.requested_at,
            Duration::from_secs(60)
        ),
        worker.purge_next(purge(1))
    );
    let phase = store.deletion_status(pid(1)).await.unwrap().phase;
    if phase == ProjectDeletionPhase::Cancelled {
        assert!(cancelled.is_ok());
        assert!(purged.unwrap().is_none());
    } else {
        assert_eq!(cancelled, Err(ProjectDeletionStoreError::NotCancellable));
        assert!(purged.unwrap().is_some());
    }
    if phase == ProjectDeletionPhase::Cancelled {
        store.request_deletion(request(1, 1, 2)).await.unwrap();
    }
    let events = PostgresEventStore::new(db.clone(), EventCodecConfig::default()).unwrap();
    let incoming = events
        .prepare(AcceptedEvent {
            project_id: pid(1),
            event_id: EventId::from_bytes([99; 16]),
            received_at: r.requested_at,
            policy_revision: 1,
            payload: ScrubbedEventPayload::new(serde_json::to_vec(&serde_json::json!({"event_id":EventId::from_bytes([99;16]).to_string(),"platform":"rust","message":"synthetic fenced deletion"})).unwrap()),
        })
        .unwrap();
    assert_eq!(
        events.insert_batch(&[incoming]).await.unwrap(),
        vec![metric_ports::EventWriteStatus::Rejected]
    );
    let mut workers = Vec::new();
    for _ in 0..4 {
        let worker = store.clone();
        workers.push(tokio::spawn(async move {
            for _ in 0..64 {
                worker.purge_next(purge(1)).await.unwrap();
            }
        }));
    }
    for worker in workers {
        worker.await.unwrap();
    }
    if store.deletion_status(pid(1)).await.unwrap().phase != ProjectDeletionPhase::Deleted {
        finish(&store, purge(1)).await;
    }
    assert_eq!(
        store.deletion_status(pid(1)).await.unwrap().phase,
        ProjectDeletionPhase::Deleted
    );
    assert_eq!(count(&db, "metric.error_events", 1).await, 0);
    assert_eq!(count(&db, "metric.error_events", 2).await, 3);
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn registry_covers_every_project_fk_and_native_query_uses_scoped_index() {
    let (fixture, db, store) = setup().await;
    let referencing:Vec<String>=sqlx::query_scalar("SELECT DISTINCT n.nspname||'.'||c.relname FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE k.contype='f' AND k.confrelid='metric.projects'::regclass").fetch_all(&db.pool).await.unwrap();
    for table in referencing {
        assert!(
            table == "metric.project_deletions"
                || OWNED.iter().any(|(_, t)| *t == table)
                || SHARED.iter().any(|(_, _, t, _)| *t == table),
            "unregistered project data {table}"
        );
    }
    events(&db, 1).await;
    sqlx::query("WITH sample AS(SELECT body FROM metric.error_events WHERE project_id=2 LIMIT 1) INSERT INTO metric.error_events(project_id,event_id,received_at,policy_revision,body,next_attempt_at) SELECT 2,decode(lpad(to_hex(n),32,'0'),'hex'),1800000000000,1,body,1800000000000 FROM sample CROSS JOIN generate_series(1,4096) n").execute(&db.pool).await.unwrap();
    sqlx::query("ANALYZE metric.error_events")
        .execute(&db.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT ctid FROM metric.error_events WHERE project_id=1 LIMIT 1 FOR UPDATE").fetch_one(&db.pool).await.unwrap();
    assert!(plan.to_string().contains("Index Cond") && plan.to_string().contains("project_id = 1"));
    let mut invalid = purge(0);
    assert_eq!(
        store.purge_next(invalid).await,
        Err(ProjectDeletionStoreError::InvalidData)
    );
    invalid = purge(10_001);
    assert_eq!(
        store.purge_next(invalid).await,
        Err(ProjectDeletionStoreError::InvalidData)
    );
    invalid = purge(1);
    invalid.now = at(253402300799999);
    assert_eq!(
        store.purge_next(invalid).await,
        Err(ProjectDeletionStoreError::InvalidData)
    );
    let mut bad = request(1, 1, 1);
    bad.purge_after = at(bad.requested_at.unix_millis() - 1);
    assert_eq!(
        store.request_deletion(bad).await,
        Err(ProjectDeletionStoreError::InvalidData)
    );
    db.pool.close().await;
    fixture.cleanup().await;
}
