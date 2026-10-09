use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, project_identity, seed_projects},
};
use metric_domain::{
    ProjectAcceptanceState, Slug,
    auth::{Actor, AuthContext, CredentialId, OrganizationRole, PermissionSet, UserId},
};
use metric_ports::{MaintenanceStore, ProjectStore};
use std::sync::Arc;
fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn pid(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn org(value: u64) -> OrganizationId {
    OrganizationId::new(value).unwrap()
}
fn binding(project: i32) -> ArtifactBinding {
    ArtifactBinding::new(pid(project), None, None).unwrap()
}
fn debug() -> DebugId {
    DebugId::Uuid {
        uuid: [9; 16],
        appendix: 10,
    }
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn concurrent_binding_additions_preserve_every_acknowledged_association() {
    let (fixture, db, store) = setup(ArtifactQuota::default()).await;
    let a = upload(1, 1, 1);
    store.upsert_upload(a.clone()).await.unwrap();
    let original = bundle(&a);
    store.publish_bundle(a.id, original.clone()).await.unwrap();
    let mut tasks = Vec::new();
    for seed in 1_u8..=32 {
        let store = store.clone();
        let mut value = original.clone();
        value.bindings = vec![
            ArtifactBinding::new(pid(1), Some(ReleaseId::from_bytes([seed; 16])), None).unwrap(),
        ];
        tasks.push(tokio::spawn(async move {
            store.publish_bundle([1; 16], value).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let stored = store.load_for_project(pid(1), original.id).await.unwrap();
    assert_eq!(stored.bindings.len(), 33);
    assert_eq!(revision_of(&db, 1).await, 33);
    assert_eq!(counters(&db, 1).await, (1, 3));
    db.pool.close().await;
    fixture.cleanup().await;
}
fn upload(seed: u8, organization: u64, project: i32) -> ArtifactUpload {
    ArtifactUpload {
        id: [seed; 16],
        organization_id: org(organization),
        sha1: [seed; 20],
        chunks: vec![[seed; 20]],
        bindings: vec![binding(project)],
        created_at: at(1_800_000_000_000),
        updated_at: at(1_800_000_000_000),
    }
}
fn bundle(upload: &ArtifactUpload) -> ArtifactBundle {
    ArtifactBundle {
        id: ArtifactBundleId::from_bytes(upload.id),
        organization_id: upload.organization_id,
        bindings: upload.bindings.clone(),
        bundle_debug_id: debug(),
        debug_id_tokens: vec![ArtifactDebugIdToken::derive(
            upload.organization_id,
            &debug(),
        )],
        checksum: [upload.id[0]; 32],
        sha1: upload.sha1,
        size: 3,
        uploaded_at: upload.updated_at,
        generation: 0,
    }
}
fn lookup(project: i32, organization: u64) -> ArtifactLookup {
    ArtifactLookup {
        project_id: pid(project),
        organization_id: org(organization),
        debug_ids: vec![debug()],
        release_id: None,
        dist: None,
        limit: 20,
    }
}
async fn setup(quota: ArtifactQuota) -> (Fixture, PostgresDatabase, PostgresArtifactStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let db = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    let projects = seed_projects(db.clone(), 32).await;
    let mut third = project_identity(3, 1);
    third.slug = Slug::new("third-project").unwrap();
    projects.insert_project(third).await.unwrap();
    let store = PostgresArtifactStore::new(db.clone(), quota).unwrap();
    (fixture, db, store)
}
async fn counters(db: &PostgresDatabase, organization: i64) -> (i64, i64) {
    sqlx::query_as("SELECT artifact_count,artifact_bytes FROM metric.organizations WHERE id=$1")
        .bind(organization)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}
async fn revision_of(db: &PostgresDatabase, project: i32) -> i64 {
    sqlx::query_scalar("SELECT artifact_revision FROM metric.projects WHERE id=$1")
        .bind(project)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn scoped_merge_publication_lookup_exact_binding_and_revisions() {
    let (fixture, db, store) = setup(ArtifactQuota::default()).await;
    assert_eq!(store.project_organization(pid(1)).await.unwrap(), org(1));
    assert_eq!(
        store
            .resolve_projects(
                "org-1".into(),
                vec!["third-project".into(), "same-project".into()]
            )
            .await
            .unwrap()
            .1
            .iter()
            .map(|view| view.id.get())
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(
        store
            .resolve_projects("' OR true --".into(), vec!["same-project".into()])
            .await,
        Err(ArtifactStoreError::NotFound)
    );
    assert_eq!(
        store
            .resolve_projects("org-1".into(), vec!["' OR true --".into()])
            .await,
        Err(ArtifactStoreError::NotFound)
    );
    let a = upload(1, 1, 1);
    let first = store.upsert_upload(a.clone()).await.unwrap();
    assert_eq!(store.upsert_upload(a.clone()).await.unwrap(), first);
    assert_eq!(store.recoverable_uploads(1).await.unwrap(), vec![first]);
    let b = bundle(&a);
    assert_eq!(
        store.publish_bundle(a.id, b.clone()).await.unwrap(),
        vec![(pid(1), 1)]
    );
    assert!(
        store
            .publish_bundle(a.id, b.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(counters(&db, 1).await, (1, 3));
    let mut extended = a.clone();
    extended.bindings = vec![binding(3)];
    let merged = store.upsert_upload(extended).await.unwrap();
    assert_eq!(merged.upload.bindings, vec![binding(1), binding(3)]);
    let expanded = bundle(&merged.upload);
    assert_eq!(
        store.publish_bundle(a.id, expanded.clone()).await.unwrap(),
        vec![(pid(3), 1)]
    );
    assert_eq!(
        store.load_for_project(pid(3), b.id).await.unwrap().bindings,
        vec![binding(1), binding(3)]
    );
    assert_eq!(
        store.load_for_project(pid(2), b.id).await,
        Err(ArtifactStoreError::NotFound)
    );
    assert!(store.load_by_sha1(org(2), a.sha1).await.unwrap().is_none());
    assert!(store.lookup(lookup(2, 1)).await.unwrap().is_empty());
    assert!(store.lookup(lookup(1, 2)).await.unwrap().is_empty());
    let mut request = lookup(1, 1);
    request.dist = Some("' OR true --".into());
    assert_eq!(
        store.lookup(request).await.unwrap()[0].resolved_with,
        ArtifactResolution::DebugId
    );
    let release = ReleaseId::from_bytes([7; 16]);
    let exact = ArtifactBinding::new(
        pid(1),
        Some(release),
        Some("'; DROP TABLE metric.projects; -- Ю".into()),
    )
    .unwrap();
    let mut extended = a.clone();
    extended.bindings = vec![exact.clone()];
    let merged = store.upsert_upload(extended).await.unwrap();
    assert_eq!(
        store
            .publish_bundle(a.id, bundle(&merged.upload))
            .await
            .unwrap(),
        vec![(pid(1), 2)]
    );
    let mut request = lookup(1, 1);
    request.debug_ids.clear();
    request.release_id = Some(release);
    request.dist = exact.dist.clone();
    assert_eq!(
        store.lookup(request.clone()).await.unwrap()[0].resolved_with,
        ArtifactResolution::Release
    );
    request.dist = None;
    assert!(store.lookup(request).await.unwrap().is_empty());
    assert_eq!(
        store
            .remove_binding(org(1), b.id, binding(1), a.updated_at)
            .await
            .unwrap(),
        Some(3)
    );
    assert!(
        store.load_for_project(pid(1), b.id).await.is_ok(),
        "other exact binding remains"
    );
    assert_eq!(
        store
            .remove_binding(org(1), b.id, binding(1), a.updated_at)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .remove_binding(org(1), b.id, exact, a.updated_at)
            .await
            .unwrap(),
        Some(4)
    );
    assert_eq!(
        store.load_for_project(pid(1), b.id).await,
        Err(ArtifactStoreError::NotFound)
    );
    assert!(store.load_for_project(pid(3), b.id).await.is_ok());
    assert!(
        store
            .claim_gc(
                a.updated_at,
                at(a.updated_at.unix_millis() + 1000),
                [9; 16],
                100
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(counters(&db, 1).await, (1, 3));
    assert!(store.recoverable_uploads(10).await.unwrap().is_empty());
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn concurrent_quota_admission_merge_and_duplicate_publication_are_atomic() {
    let (fixture, db, store) = setup(ArtifactQuota {
        maximum_bytes_per_organization: 3,
        maximum_bundles_per_organization: 1,
    })
    .await;
    let a = upload(1, 1, 1);
    let b = upload(2, 1, 3);
    store.upsert_upload(a.clone()).await.unwrap();
    store.upsert_upload(b.clone()).await.unwrap();
    let (one, two) = tokio::join!(
        store.publish_bundle(a.id, bundle(&a)),
        store.publish_bundle(b.id, bundle(&b))
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert!(matches!(
        (one, two),
        (Ok(_), Err(ArtifactStoreError::Quota)) | (Err(ArtifactStoreError::Quota), Ok(_))
    ));
    assert_eq!(counters(&db, 1).await, (1, 3));
    let chosen = if store.load_by_sha1(org(1), a.sha1).await.unwrap().is_some() {
        a.clone()
    } else {
        b.clone()
    };
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let chosen = chosen.clone();
        tasks.push(tokio::spawn(async move {
            store.publish_bundle(chosen.id, bundle(&chosen)).await
        }));
    }
    for task in tasks {
        assert!(task.await.unwrap().unwrap().is_empty());
    }
    assert_eq!(counters(&db, 1).await, (1, 3));
    let c = upload(3, 2, 2);
    store.upsert_upload(c.clone()).await.unwrap();
    store.publish_bundle(c.id, bundle(&c)).await.unwrap();
    assert_eq!(counters(&db, 2).await, (1, 3));
    let mut first = chosen.clone();
    first.bindings = vec![binding(1)];
    let mut second = chosen.clone();
    second.bindings = vec![binding(3)];
    let (one, two) = tokio::join!(store.upsert_upload(first), store.upsert_upload(second));
    one.unwrap();
    two.unwrap();
    let record = store.upsert_upload(chosen.clone()).await.unwrap();
    assert_eq!(record.upload.bindings, vec![binding(1), binding(3)]);
    store
        .publish_bundle(chosen.id, bundle(&record.upload))
        .await
        .unwrap();
    assert_eq!(revision_of(&db, 1).await, 1);
    assert_eq!(revision_of(&db, 3).await, 1);
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn orphan_rescue_expired_claim_and_post_tombstone_generation_fence() {
    let (fixture, db, store) = setup(ArtifactQuota::default()).await;
    let a = upload(1, 1, 1);
    let mut b = bundle(&a);
    let now = a.updated_at;
    store.upsert_upload(a.clone()).await.unwrap();
    store.publish_bundle(a.id, b.clone()).await.unwrap();
    store
        .remove_binding(org(1), b.id, binding(1), now)
        .await
        .unwrap();
    assert_eq!(
        store
            .publication_generation(org(1), a.sha1, a.id, at(now.unix_millis() + 1000))
            .await
            .unwrap(),
        0
    );
    store.upsert_upload(a.clone()).await.unwrap();
    store.publish_bundle(a.id, b.clone()).await.unwrap();
    assert_eq!(counters(&db, 1).await, (1, 3));
    assert!(
        store
            .claim_gc(now, at(now.unix_millis() + 1000), [1; 16], 10)
            .await
            .unwrap()
            .is_empty()
    );
    store
        .remove_binding(org(1), b.id, binding(1), now)
        .await
        .unwrap();
    store.upsert_upload(a.clone()).await.unwrap();
    let claims = store
        .claim_gc(now, at(now.unix_millis() + 1000), [1; 16], 10)
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert!(
        store
            .validate_gc_claim(b.id, 0, [1; 16], at(now.unix_millis() + 500))
            .await
            .unwrap()
    );
    assert!(
        !store
            .validate_gc_claim(b.id, 0, [1; 16], at(now.unix_millis() + 1000))
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .publication_generation(org(1), a.sha1, a.id, at(now.unix_millis() + 1000))
            .await,
        Err(ArtifactStoreError::Busy)
    );
    assert_eq!(
        store.publish_bundle(a.id, b.clone()).await,
        Err(ArtifactStoreError::Busy)
    );
    let later = at(now.unix_millis() + 1001);
    let next = store
        .claim_gc(later, at(later.unix_millis() + 1000), [2; 16], 10)
        .await
        .unwrap();
    assert_eq!(next.len(), 1);
    assert!(
        !store
            .validate_gc_claim(b.id, 0, [1; 16], now)
            .await
            .unwrap()
    );
    assert!(
        !store
            .finish_gc(b.id, 0, [1; 16], at(later.unix_millis() + 2000))
            .await
            .unwrap()
    );
    assert!(
        store
            .finish_gc(b.id, 0, [2; 16], at(later.unix_millis() + 2000))
            .await
            .unwrap()
    );
    assert!(
        !store
            .finish_gc(b.id, 0, [2; 16], at(later.unix_millis() + 2000))
            .await
            .unwrap()
    );
    assert_eq!(counters(&db, 1).await, (0, 0));
    assert!(store.load_by_sha1(org(1), a.sha1).await.unwrap().is_none());
    for _ in 0..2 {
        assert_eq!(
            store
                .publication_generation(org(1), a.sha1, a.id, at(later.unix_millis() + 3000))
                .await
                .unwrap(),
            1
        );
    }
    assert_eq!(
        store.publish_bundle(a.id, b.clone()).await,
        Err(ArtifactStoreError::Conflict)
    );
    b.generation = 1;
    store.publish_bundle(a.id, b.clone()).await.unwrap();
    assert_eq!(counters(&db, 1).await, (1, 3));
    assert!(
        !store
            .finish_gc(b.id, 0, [2; 16], at(later.unix_millis() + 2000))
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .load_for_project(pid(1), b.id)
            .await
            .unwrap()
            .generation,
        1
    );
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn native_lost_commit_replies_preserve_all_generation_and_quota_boundaries() {
    let (fixture, db, _) = setup(ArtifactQuota::default()).await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let fault_db = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let store = PostgresArtifactStore::new(fault_db.clone(), ArtifactQuota::default()).unwrap();
    let a = upload(1, 1, 1);
    let mut b = bundle(&a);
    let now = a.updated_at;
    store.upsert_upload(a.clone()).await.unwrap();
    proxy.drop_next_commit_reply();
    assert_eq!(
        store.publish_bundle(a.id, b.clone()).await,
        Err(ArtifactStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert_eq!(counters(&db, 1).await, (1, 3));
    assert!(
        store
            .publish_bundle(a.id, b.clone())
            .await
            .unwrap()
            .is_empty()
    );
    proxy.drop_next_commit_reply();
    assert_eq!(
        store.remove_binding(org(1), b.id, binding(1), now).await,
        Err(ArtifactStoreError::Unavailable)
    );
    assert_eq!(
        store
            .remove_binding(org(1), b.id, binding(1), now)
            .await
            .unwrap(),
        None
    );
    proxy.drop_next_commit_reply();
    assert_eq!(
        store
            .claim_gc(now, at(now.unix_millis() + 1000), [1; 16], 1)
            .await,
        Err(ArtifactStoreError::Unavailable)
    );
    assert!(
        store
            .claim_gc(now, at(now.unix_millis() + 1000), [2; 16], 1)
            .await
            .unwrap()
            .is_empty()
    );
    let later = at(now.unix_millis() + 1001);
    let claims = store
        .claim_gc(later, at(later.unix_millis() + 1000), [2; 16], 1)
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);
    proxy.drop_next_commit_reply();
    assert_eq!(
        store
            .finish_gc(b.id, 0, [2; 16], at(later.unix_millis() + 2000))
            .await,
        Err(ArtifactStoreError::Unavailable)
    );
    assert_eq!(counters(&db, 1).await, (0, 0));
    assert!(
        !store
            .finish_gc(b.id, 0, [2; 16], at(later.unix_millis() + 2000))
            .await
            .unwrap()
    );
    store.upsert_upload(a.clone()).await.unwrap();
    proxy.drop_next_commit_reply();
    assert_eq!(
        store
            .publication_generation(org(1), a.sha1, a.id, at(later.unix_millis() + 3000))
            .await,
        Err(ArtifactStoreError::Unavailable)
    );
    assert_eq!(
        store
            .publication_generation(org(1), a.sha1, a.id, at(later.unix_millis() + 3000))
            .await
            .unwrap(),
        1
    );
    b.generation = 1;
    store.publish_bundle(a.id, b).await.unwrap();
    assert_eq!(counters(&db, 1).await, (1, 3));
    assert_eq!(revision_of(&db, 1).await, 3);
    fault_db.pool.close().await;
    proxy.close().await;
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn publication_rollback_conflicts_and_fenced_projects_leave_no_partial_effects() {
    let (fixture, db, store) = setup(ArtifactQuota::default()).await;
    let a = upload(1, 1, 1);
    let b = bundle(&a);
    store.upsert_upload(a.clone()).await.unwrap();
    let mut conflict = a.clone();
    conflict.chunks = vec![[2; 20]];
    assert_eq!(
        store.upsert_upload(conflict).await,
        Err(ArtifactStoreError::Conflict)
    );
    assert_eq!(
        store.upsert_upload(upload(2, 1, 2)).await,
        Err(ArtifactStoreError::NotFound)
    );
    sqlx::raw_sql("CREATE FUNCTION metric.reject_artifact_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state=2 THEN RAISE EXCEPTION 'synthetic fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_artifact_completion BEFORE UPDATE ON metric.artifact_uploads FOR EACH ROW EXECUTE FUNCTION metric.reject_artifact_completion()")
        .execute(&db.pool).await.unwrap();
    assert_eq!(
        store.publish_bundle(a.id, b.clone()).await,
        Err(ArtifactStoreError::Unavailable)
    );
    assert_eq!(counters(&db, 1).await, (0, 0));
    assert_eq!(revision_of(&db, 1).await, 0);
    assert!(store.load_by_sha1(org(1), a.sha1).await.unwrap().is_none());
    assert_eq!(
        store.recoverable_uploads(1).await.unwrap()[0].state,
        ArtifactUploadState::Pending
    );
    sqlx::query("DROP TRIGGER reject_artifact_completion ON metric.artifact_uploads")
        .execute(&db.pool)
        .await
        .unwrap();
    store.publish_bundle(a.id, b.clone()).await.unwrap();
    let mut collision = b.clone();
    collision.checksum = [9; 32];
    assert_eq!(
        store.publish_bundle(a.id, collision).await,
        Err(ArtifactStoreError::Conflict)
    );
    let project =
        crate::PostgresProjectStore::new(db.clone(), metric_domain::SecretBytes::new([42; 32]), 32)
            .unwrap();
    project
        .set_project_acceptance(pid(1), ProjectAcceptanceState::PendingDelete)
        .await
        .unwrap();
    assert_eq!(
        store.publish_bundle(a.id, b.clone()).await,
        Err(ArtifactStoreError::NotFound)
    );
    assert_eq!(
        store.upsert_upload(a.clone()).await,
        Err(ArtifactStoreError::NotFound)
    );
    assert_eq!(counters(&db, 1).await, (1, 3));
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn upload_recovery_bounds_corrupt_metadata_and_native_expiry_are_safe() {
    let (fixture, db, store) = setup(ArtifactQuota::default()).await;
    let a = upload(1, 1, 1);
    store.upsert_upload(a.clone()).await.unwrap();
    store
        .set_upload_state(
            a.id,
            ArtifactUploadState::Assembling,
            a.updated_at,
            None,
            Some(1),
        )
        .await
        .unwrap();
    let record = store.recoverable_uploads(1).await.unwrap().remove(0);
    assert_eq!(record.attempts, 1);
    assert_eq!(record.error_code, Some(1));
    store
        .set_upload_state(a.id, ArtifactUploadState::Pending, a.updated_at, None, None)
        .await
        .unwrap();
    assert!(
        store.recoverable_uploads(1).await.unwrap()[0]
            .error_code
            .is_none()
    );
    let mut invalid = upload(2, 1, 1);
    invalid.chunks = vec![[2; 20]; MAX_ARTIFACT_CHUNKS + 1];
    assert_eq!(
        store.upsert_upload(invalid).await,
        Err(ArtifactStoreError::InvalidData)
    );
    for limit in [0, 1001] {
        assert_eq!(
            store.recoverable_uploads(limit).await,
            Err(ArtifactStoreError::InvalidData)
        );
    }
    for limit in [0, 101] {
        assert_eq!(
            store
                .claim_gc(
                    a.updated_at,
                    at(a.updated_at.unix_millis() + 1),
                    [1; 16],
                    limit
                )
                .await,
            Err(ArtifactStoreError::InvalidData)
        );
    }
    assert_eq!(
        store.claim_gc(a.updated_at, a.updated_at, [1; 16], 1).await,
        Err(ArtifactStoreError::InvalidData)
    );
    let mut invalid = lookup(1, 1);
    invalid.limit = 21;
    assert_eq!(
        store.lookup(invalid).await,
        Err(ArtifactStoreError::InvalidData)
    );
    sqlx::query(
        "ALTER TABLE metric.artifact_uploads DROP CONSTRAINT artifact_uploads_chunks_check",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE metric.artifact_uploads SET chunks=$1 WHERE upload_id=$2")
        .bind(vec![1u8; 1024 * 1024])
        .bind(a.id.as_slice())
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        store.recoverable_uploads(1).await,
        Err(ArtifactStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.artifact_uploads SET chunks=$1 WHERE upload_id=$2")
        .bind(vec![1u8; 20])
        .bind(a.id.as_slice())
        .execute(&db.pool)
        .await
        .unwrap();
    let b = bundle(&a);
    store.publish_bundle(a.id, b.clone()).await.unwrap();
    store
        .remove_binding(org(1), b.id, binding(1), a.updated_at)
        .await
        .unwrap();
    let until = at(a.updated_at.unix_millis() + 1000);
    store
        .claim_gc(a.updated_at, until, [1; 16], 1)
        .await
        .unwrap();
    store.finish_gc(b.id, 0, [1; 16], until).await.unwrap();
    let failed = upload(2, 1, 3);
    let pending = upload(3, 1, 3);
    store.upsert_upload(failed.clone()).await.unwrap();
    store.upsert_upload(pending.clone()).await.unwrap();
    store
        .set_upload_state(
            failed.id,
            ArtifactUploadState::Failed,
            a.updated_at,
            None,
            Some(1),
        )
        .await
        .unwrap();
    let maintenance = crate::PostgresMaintenanceStore::new(db.clone());
    let mut request = metric_ports::MaintenanceRequest {
        task: metric_ports::MaintenanceTask::EventRetention,
        now: at(a.updated_at.unix_millis() + 8 * 86_400_000),
        cursor: None,
        batch_size: 1,
        event_retention: std::time::Duration::from_secs(86400),
        hourly_retention: std::time::Duration::from_secs(86400),
        archive_events: false,
    };
    let mut finished = false;
    for _ in 0..64 {
        let result = maintenance.run(request.clone()).await.unwrap();
        let Some(cursor) = result.next_cursor else {
            finished = true;
            break;
        };
        request.cursor = Some(cursor);
    }
    assert!(finished);
    assert!(store.load_by_sha1(org(1), a.sha1).await.unwrap().is_none());
    assert_eq!(
        store
            .recoverable_uploads(10)
            .await
            .unwrap()
            .iter()
            .map(|record| record.upload.id)
            .collect::<Vec<_>>(),
        vec![pending.id]
    );
    assert_eq!(counters(&db, 1).await, (0, 0));
    let tombstones: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.artifact_bundles")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(tombstones, 0);
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn lookup_and_gc_use_native_scoped_indexes_and_bounded_claims() {
    let (fixture, db, store) = setup(ArtifactQuota::default()).await;
    sqlx::query("INSERT INTO metric.artifact_bundles(bundle_id,organization_id,sha1,bundle_debug_id,debug_tokens,checksum,size,uploaded_at) SELECT decode(lpad(to_hex(n),32,'0'),'hex'),1,decode(lpad(to_hex(n),40,'0'),'hex'),decode(repeat('09',16),'hex'),ARRAY[n::bigint],decode(repeat('01',32),'hex'),3,1800000000000+n FROM generate_series(1,4096) n").execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO metric.artifact_bundle_bindings(organization_id,bundle_id,project_id,release_id,dist) SELECT 1,bundle_id,1,decode(lpad(to_hex((uploaded_at-1800000000000)%512),32,'0'),'hex'),'web' FROM metric.artifact_bundles").execute(&db.pool).await.unwrap();
    sqlx::raw_sql("ANALYZE metric.artifact_bundles; ANALYZE metric.artifact_bundle_bindings")
        .execute(&db.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT bundle_id FROM metric.artifact_bundles b WHERE organization_id=$1 AND state=0 AND debug_tokens && $2 AND EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id AND r.project_id=$3) ORDER BY uploaded_at DESC,bundle_id DESC LIMIT $4")
        .bind(1i64).bind(vec![100i64]).bind(1i32).bind(1i64).fetch_one(&db.pool).await.unwrap();
    assert!(plan.to_string().contains("artifact_debug_tokens"));
    let release = ReleaseId::from_bytes(100u128.to_be_bytes());
    let mut query = lookup(1, 1);
    query.debug_ids.clear();
    query.release_id = Some(release);
    query.dist = Some("web".into());
    query.limit = 2;
    assert_eq!(store.lookup(query).await.unwrap().len(), 2);
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT bundle_id FROM metric.artifact_bundle_bindings WHERE organization_id=$1 AND project_id=$2 AND release_id=$3 AND dist=$4 LIMIT $5")
        .bind(1i64).bind(1i32).bind(release.as_bytes().as_slice()).bind("web").bind(2i64).fetch_one(&db.pool).await.unwrap();
    assert!(plan.to_string().contains("artifact_project_release"));
    sqlx::query("DELETE FROM metric.artifact_bundle_bindings")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE metric.artifact_bundles SET due_at=uploaded_at")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE metric.artifact_bundles")
        .execute(&db.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT bundle_id FROM metric.artifact_bundles WHERE state IN(0,1) AND due_at<=$1 ORDER BY due_at,bundle_id LIMIT $2")
        .bind(1_800_000_010_000i64).bind(2i64).fetch_one(&db.pool).await.unwrap();
    assert!(plan.to_string().contains("artifact_gc_due"));
    let now = at(1_800_000_010_000);
    let until = at(now.unix_millis() + 1000);
    let (one, two) = tokio::join!(
        store.claim_gc(now, until, [1; 16], 2),
        store.claim_gc(now, until, [2; 16], 2)
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert!(one.len() <= 2 && two.len() <= 2);
    let ids: Vec<_> = one
        .iter()
        .chain(&two)
        .map(|claim| claim.bundle.id)
        .collect();
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), ids.len());
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn existing_artifact_service_streams_real_source_bundle_and_gc_with_postgres() {
    use metric_application::{
        artifacts::{ArtifactConfig, ArtifactService, AssembleArtifact, AssembleArtifactState},
        debug_files::{DebugFileConfig, DebugFileService},
    };
    use sha1::{Digest as _, Sha1};
    use std::io::{Cursor, Write};
    let (fixture, db, store) = setup(ArtifactQuota::default()).await;
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("pg-artifact-{}", uuid::Uuid::new_v4().simple()));
    let blobs = Arc::new(
        metric_blob::LocalBlobStore::new(
            &root,
            metric_blob::LocalBlobConfig {
                capacity_bytes: 1024 * 1024,
                reserve_bytes: 0,
                max_object_bytes: 64 * 1024,
            },
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(metric_testkit::FixedClock(at(1_800_000_000_000)));
    let random = Arc::new(metric_testkit::FixedRandom(7));
    let service = ArtifactService::new(
        Arc::new(store.clone()),
        blobs.clone(),
        clock.clone(),
        random,
        ArtifactConfig::default(),
    )
    .unwrap();
    let debug = DebugFileService::new(
        Arc::new(
            crate::PostgresDebugFileStore::new(db.clone(), crate::DebugFileQuota::default())
                .unwrap(),
        ),
        blobs.clone(),
        clock,
        DebugFileConfig::default(),
    )
    .unwrap();
    let context = AuthContext {
        actor: Actor::WebSession,
        user_id: UserId::new(1).unwrap(),
        organization_id: org(1),
        role: OrganizationRole::Owner,
        permissions: PermissionSet::from_role(OrganizationRole::Owner),
        credential_id: CredentialId::new(1).unwrap(),
    };
    let id = "67e9247c-814e-392b-a027-dbde6748fcbf";
    let mut cursor = Cursor::new(Vec::new());
    cursor.write_all(b"SYSB").unwrap();
    cursor.write_all(&2u32.to_le_bytes()).unwrap();
    let mut zip = zip::ZipWriter::new(cursor);
    zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(&serde_json::json!({"debug_id":"11111111-2222-3333-4444-555555555555","files":{"files/app.js":{"type":"minified_source","url":"~/app.js","headers":{"debug-id":id}}}})).unwrap()).unwrap();
    zip.start_file("files/app.js", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"function a(){}").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let sha1: [u8; 20] = Sha1::digest(&bytes).into();
    let request = AssembleArtifact {
        sha1,
        chunks: vec![sha1],
        project_slugs: vec!["same-project".into()],
        release: None,
        dist: None,
    };
    assert!(matches!(
        service
            .assemble(&context, "org-1", request.clone())
            .await
            .unwrap(),
        AssembleArtifactState::Missing { .. }
    ));
    debug
        .upload_chunk(&context, org(1), sha1, bytes.clone().into_boxed_slice())
        .await
        .unwrap();
    let AssembleArtifactState::Ok { bundle } = service
        .assemble(&context, "org-1", request.clone())
        .await
        .unwrap()
    else {
        panic!("did not assemble artifact")
    };
    assert_eq!(bundle.generation, 0);
    assert!(matches!(
        service.assemble(&context, "org-1", request).await.unwrap(),
        AssembleArtifactState::Ok { .. }
    ));
    let (_, mut reader) = service.open(pid(1), bundle.id).await.unwrap();
    let mut restored = Vec::new();
    while let Some(chunk) = reader.read_chunk(4096).await.unwrap() {
        restored.extend_from_slice(&chunk);
    }
    assert_eq!(restored, bytes);
    drop(reader);
    let lookup = store
        .lookup(ArtifactLookup {
            project_id: pid(1),
            organization_id: org(1),
            debug_ids: vec![DebugId::parse(id).unwrap()],
            release_id: None,
            dist: None,
            limit: 20,
        })
        .await
        .unwrap();
    assert_eq!(lookup.len(), 1);
    store
        .remove_binding(org(1), bundle.id, binding(1), bundle.uploaded_at)
        .await
        .unwrap();
    assert_eq!(service.gc_once().await.unwrap(), 1);
    assert_eq!(counters(&db, 1).await, (0, 0));
    assert!(store.load_by_sha1(org(1), sha1).await.unwrap().is_none());
    drop(service);
    drop(debug);
    drop(blobs);
    let resolved = std::fs::canonicalize(&root).unwrap();
    let target = std::fs::canonicalize(std::env::current_dir().unwrap().join("target")).unwrap();
    assert!(
        resolved.starts_with(target)
            && resolved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pg-artifact-")
    );
    std::fs::remove_dir_all(resolved).unwrap();
    db.pool.close().await;
    fixture.cleanup().await;
}
