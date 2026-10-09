use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};
use metric_domain::{
    ProjectAcceptanceState,
    auth::{Actor, AuthContext, CredentialId, OrganizationRole, PermissionSet, UserId},
};
use metric_ports::{ProcessingProjectStore, ProjectStore};
use std::sync::Arc;

fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project_id(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn upload(project: i32, seed: u8) -> DebugUpload {
    DebugUpload {
        id: [seed; 16],
        project_id: project_id(project),
        organization_id: OrganizationId::new(project as u64).unwrap(),
        sha1: [seed; 20],
        name: "'; DROP TABLE metric.projects; -- Юникод.so".into(),
        debug_id: Some(DebugId::Uuid {
            uuid: [9; 16],
            appendix: 10,
        }),
        code_id: Some(CodeId::parse("abcdef1").unwrap()),
        chunks: vec![[seed; 20]],
        created_at: at(1_800_000_000_000),
        updated_at: at(1_800_000_000_000),
    }
}
fn file(upload: &DebugUpload, size: u64) -> DebugFile {
    DebugFile {
        id: DebugFileId::from_bytes([upload.id[0]; 16]),
        project_id: upload.project_id,
        debug_id: upload.debug_id.clone(),
        code_id: upload.code_id.clone(),
        checksum: [upload.id[0]; 32],
        file_type: DebugFileType::Elf,
        sha1: upload.sha1,
        size,
        name: upload.name.clone(),
        uploaded_at: upload.updated_at,
    }
}
async fn setup(quota: DebugFileQuota) -> (Fixture, PostgresDatabase, PostgresDebugFileStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let db = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(db.clone(), 32).await;
    let store = PostgresDebugFileStore::new(db.clone(), quota).unwrap();
    (fixture, db, store)
}
async fn counters(db: &PostgresDatabase, id: i32) -> (i64, i64, i64) {
    sqlx::query_as("SELECT debug_file_count,debug_file_bytes,debug_file_revision FROM metric.projects WHERE id=$1").bind(id).fetch_one(&db.pool).await.unwrap()
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn lifecycle_scope_lookup_recovery_and_idempotent_publication() {
    let (fixture, db, store) = setup(DebugFileQuota::default()).await;
    assert_eq!(
        store
            .project_organization(project_id(1))
            .await
            .unwrap()
            .get(),
        1
    );
    assert_eq!(
        store
            .resolve_project_slugs("org-1".into(), "same-project".into())
            .await
            .unwrap()
            .1
            .id,
        project_id(1)
    );
    assert_eq!(
        store
            .resolve_project_slugs("' OR true --".into(), "same-project".into())
            .await,
        Err(DebugFileStoreError::NotFound)
    );
    assert_eq!(
        store
            .resolve_project_slugs("org-2".into(), "' OR true --".into())
            .await,
        Err(DebugFileStoreError::NotFound)
    );
    let a = upload(1, 1);
    let first = store.upsert_upload(a.clone()).await.unwrap();
    assert_eq!(first.state, DebugUploadState::Pending);
    assert_eq!(store.upsert_upload(a.clone()).await.unwrap(), first);
    let mut collision = a.clone();
    collision.project_id = project_id(2);
    collision.organization_id = OrganizationId::new(2).unwrap();
    assert_eq!(
        store.upsert_upload(collision).await,
        Err(DebugFileStoreError::Conflict)
    );
    let mut wrong_org = upload(1, 3);
    wrong_org.organization_id = OrganizationId::new(2).unwrap();
    assert_eq!(
        store.upsert_upload(wrong_org).await,
        Err(DebugFileStoreError::NotFound)
    );
    store
        .set_upload_state(
            a.id,
            DebugUploadState::Assembling,
            at(a.updated_at.unix_millis() + 1),
            Some("temporary".into()),
        )
        .await
        .unwrap();
    let recovery = store.recoverable_uploads(1).await.unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].attempts, 1);
    assert_eq!(recovery[0].state, DebugUploadState::Assembling);
    store
        .set_upload_state(a.id, DebugUploadState::Pending, a.updated_at, None)
        .await
        .unwrap();
    assert!(
        store.recoverable_uploads(1).await.unwrap()[0]
            .error_code
            .is_none()
    );
    let original = file(&a, 3);
    assert_eq!(
        store
            .publish_debug_file(a.id, original.clone())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .publish_debug_file(a.id, original.clone())
            .await
            .unwrap(),
        1
    );
    assert_eq!(counters(&db, 1).await, (1, 3, 1));
    assert_eq!(
        store.load_by_sha1(project_id(1), a.sha1).await.unwrap(),
        Some(original.clone())
    );
    assert!(
        store
            .load_by_sha1(project_id(2), a.sha1)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.load_debug_file(project_id(2), original.id).await,
        Err(DebugFileStoreError::NotFound)
    );
    assert_eq!(
        store
            .delete_debug_file(project_id(2), original.id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .find_debug_files(project_id(1), original.debug_id.clone(), None, 20)
            .await
            .unwrap(),
        vec![original.clone()]
    );
    assert_eq!(
        store
            .find_debug_files(project_id(1), None, original.code_id.clone(), 1)
            .await
            .unwrap(),
        vec![original.clone()]
    );
    assert!(store.recoverable_uploads(20).await.unwrap().is_empty());
    let mut changed = original.clone();
    changed.checksum = [99; 32];
    assert_eq!(
        store.publish_debug_file(a.id, changed).await,
        Err(DebugFileStoreError::Conflict)
    );
    let mut missing = original.clone();
    missing.project_id = project_id(2);
    assert_eq!(
        store.publish_debug_file(a.id, missing).await,
        Err(DebugFileStoreError::Conflict)
    );
    assert_eq!(counters(&db, 1).await, (1, 3, 1));
    let second = upload(2, 2);
    store.upsert_upload(second.clone()).await.unwrap();
    let mut second_file = file(&second, 4);
    second_file.id = original.id;
    store
        .publish_debug_file(second.id, second_file.clone())
        .await
        .unwrap();
    assert_eq!(
        store
            .load_debug_file(project_id(2), original.id)
            .await
            .unwrap(),
        second_file
    );
    assert_eq!(
        store
            .delete_debug_file(project_id(1), original.id)
            .await
            .unwrap(),
        Some((original.clone(), 2))
    );
    assert_eq!(
        store
            .delete_debug_file(project_id(1), original.id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(counters(&db, 1).await, (0, 0, 2));
    assert_eq!(counters(&db, 2).await, (1, 4, 1));
    let processing =
        crate::PostgresEventStore::new(db.clone(), crate::EventCodecConfig::default()).unwrap();
    assert_eq!(
        processing
            .load_processing_project(project_id(1))
            .await
            .unwrap()
            .debug_file_revision,
        2
    );
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn concurrent_publications_enforce_byte_and_file_quotas() {
    let (fixture, db, store) = setup(DebugFileQuota {
        maximum_bytes_per_project: 3,
        maximum_files_per_project: 1,
    })
    .await;
    let a = upload(1, 1);
    let b = upload(1, 2);
    store.upsert_upload(a.clone()).await.unwrap();
    store.upsert_upload(b.clone()).await.unwrap();
    let (one, two) = tokio::join!(
        store.publish_debug_file(a.id, file(&a, 3)),
        store.publish_debug_file(b.id, file(&b, 3))
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    assert!(matches!(
        (one, two),
        (Ok(1), Err(DebugFileStoreError::Quota)) | (Err(DebugFileStoreError::Quota), Ok(1))
    ));
    assert_eq!(counters(&db, 1).await, (1, 3, 1));
    let recover = store.recoverable_uploads(10).await.unwrap();
    assert_eq!(recover.len(), 1);
    let persisted = store
        .find_debug_files(project_id(1), a.debug_id.clone(), a.code_id.clone(), 20)
        .await
        .unwrap();
    assert_eq!(persisted.len(), 1);
    let chosen = &persisted[0];
    let id = if chosen.sha1 == a.sha1 { a.id } else { b.id };
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let file = chosen.clone();
        tasks.push(tokio::spawn(async move {
            store.publish_debug_file(id, file).await
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap(), 1);
    }
    assert_eq!(counters(&db, 1).await, (1, 3, 1));
    let c = upload(2, 3);
    store.upsert_upload(c.clone()).await.unwrap();
    assert_eq!(
        store.publish_debug_file(c.id, file(&c, 4)).await,
        Err(DebugFileStoreError::Quota)
    );
    assert_eq!(counters(&db, 2).await, (0, 0, 0));
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn native_commit_reply_loss_preserves_publication_and_deletion_once() {
    let (fixture, db, _) = setup(DebugFileQuota::default()).await;
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let fault_db = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let store = PostgresDebugFileStore::new(fault_db.clone(), DebugFileQuota::default()).unwrap();
    let a = upload(1, 1);
    store.upsert_upload(a.clone()).await.unwrap();
    let f = file(&a, 3);
    proxy.drop_next_commit_reply();
    assert_eq!(
        store.publish_debug_file(a.id, f.clone()).await,
        Err(DebugFileStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert_eq!(counters(&db, 1).await, (1, 3, 1));
    assert_eq!(store.publish_debug_file(a.id, f.clone()).await.unwrap(), 1);
    proxy.drop_next_commit_reply();
    assert_eq!(
        store.delete_debug_file(project_id(1), f.id).await,
        Err(DebugFileStoreError::Unavailable)
    );
    assert_eq!(counters(&db, 1).await, (0, 0, 2));
    assert_eq!(
        store.delete_debug_file(project_id(1), f.id).await.unwrap(),
        None
    );
    fault_db.pool.close().await;
    proxy.close().await;
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn failed_publication_rolls_back_metadata_quota_and_upload_state() {
    let (fixture, db, store) = setup(DebugFileQuota::default()).await;
    let a = upload(1, 1);
    store.upsert_upload(a.clone()).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION metric.reject_debug_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state=2 THEN RAISE EXCEPTION 'synthetic fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_debug_completion BEFORE UPDATE ON metric.debug_uploads FOR EACH ROW EXECUTE FUNCTION metric.reject_debug_completion()")
        .execute(&db.pool).await.unwrap();
    assert_eq!(
        store.publish_debug_file(a.id, file(&a, 3)).await,
        Err(DebugFileStoreError::Unavailable)
    );
    assert_eq!(counters(&db, 1).await, (0, 0, 0));
    assert!(
        store
            .load_by_sha1(project_id(1), a.sha1)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.recoverable_uploads(1).await.unwrap()[0].state,
        DebugUploadState::Pending
    );
    sqlx::query("DROP TRIGGER reject_debug_completion ON metric.debug_uploads")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        store.publish_debug_file(a.id, file(&a, 3)).await.unwrap(),
        1
    );
    sqlx::query("UPDATE metric.projects SET debug_file_bytes=0 WHERE id=1")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        store.delete_debug_file(project_id(1), file(&a, 3).id).await,
        Err(DebugFileStoreError::InvalidData)
    );
    assert!(
        store
            .load_by_sha1(project_id(1), a.sha1)
            .await
            .unwrap()
            .is_some()
    );
    db.pool.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn bounded_manifests_corrupt_codecs_and_project_fences_are_checked() {
    let (fixture, db, store) = setup(DebugFileQuota::default()).await;
    let mut a = upload(1, 1);
    a.chunks.clear();
    assert_eq!(
        store.upsert_upload(a.clone()).await,
        Err(DebugFileStoreError::InvalidData)
    );
    a.chunks = vec![[1; 20]; MAX_DEBUG_CHUNKS + 1];
    assert_eq!(
        store.upsert_upload(a.clone()).await,
        Err(DebugFileStoreError::InvalidData)
    );
    a = upload(1, 1);
    a.name = "bad\0name".into();
    assert_eq!(
        store.upsert_upload(a).await,
        Err(DebugFileStoreError::InvalidData)
    );
    let a = upload(1, 1);
    store.upsert_upload(a.clone()).await.unwrap();
    for limit in [0, 1001] {
        assert_eq!(
            store.recoverable_uploads(limit).await,
            Err(DebugFileStoreError::InvalidData)
        );
    }
    for limit in [0, 21] {
        assert_eq!(
            store
                .find_debug_files(project_id(1), a.debug_id.clone(), None, limit)
                .await,
            Err(DebugFileStoreError::InvalidData)
        );
    }
    assert_eq!(
        store.find_debug_files(project_id(1), None, None, 1).await,
        Err(DebugFileStoreError::InvalidData)
    );
    assert_eq!(
        store
            .set_upload_state(
                a.id,
                DebugUploadState::Failed,
                a.updated_at,
                Some("x".repeat(65).into())
            )
            .await,
        Err(DebugFileStoreError::InvalidData)
    );
    assert_eq!(
        store.publish_debug_file([99; 16], file(&a, 3)).await,
        Err(DebugFileStoreError::NotFound)
    );
    let mut invalid = file(&a, 3);
    invalid.debug_id = None;
    invalid.code_id = None;
    assert_eq!(
        store.publish_debug_file(a.id, invalid).await,
        Err(DebugFileStoreError::InvalidData)
    );
    sqlx::query("ALTER TABLE metric.debug_uploads DROP CONSTRAINT debug_uploads_chunks_check")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE metric.debug_uploads SET chunks=$1 WHERE upload_id=$2")
        .bind(vec![1u8; 1024 * 1024])
        .bind(a.id.as_slice())
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        store.recoverable_uploads(1).await,
        Err(DebugFileStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.debug_uploads SET chunks=$1 WHERE upload_id=$2")
        .bind(vec![1u8; 20])
        .bind(a.id.as_slice())
        .execute(&db.pool)
        .await
        .unwrap();
    let projects = seed_store(db.clone());
    projects
        .set_project_acceptance(project_id(1), ProjectAcceptanceState::PendingDelete)
        .await
        .unwrap();
    assert_eq!(
        store.project_organization(project_id(1)).await,
        Err(DebugFileStoreError::NotFound)
    );
    assert!(store.recoverable_uploads(10).await.unwrap().is_empty());
    assert_eq!(
        store.upsert_upload(upload(1, 2)).await,
        Err(DebugFileStoreError::NotFound)
    );
    assert_eq!(
        store.publish_debug_file(a.id, file(&a, 3)).await,
        Err(DebugFileStoreError::NotFound)
    );
    db.pool.close().await;
    fixture.cleanup().await;
}
fn seed_store(db: PostgresDatabase) -> crate::PostgresProjectStore {
    crate::PostgresProjectStore::new(db, metric_domain::SecretBytes::new([42; 32]), 32).unwrap()
}

#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn indexed_lookup_and_recovery_plans_use_bounded_pages() {
    let (fixture, db, store) = setup(DebugFileQuota::default()).await;
    sqlx::query("INSERT INTO metric.debug_files(project_id,file_id,debug_id,file_type,checksum,sha1,size,name,uploaded_at) SELECT 1,decode(lpad(to_hex(n),32,'0'),'hex'),decode(lpad(to_hex(n),32,'0'),'hex'),0,decode(repeat('01',32),'hex'),decode(lpad(to_hex(n),40,'0'),'hex'),3,'synthetic.so',1800000000000+n FROM generate_series(1,4096) n")
        .execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO metric.debug_uploads(upload_id,project_id,organization_id,sha1,name,chunks,created_at,updated_at) SELECT decode(lpad(to_hex(n),32,'0'),'hex'),1,1,decode(lpad(to_hex(n),40,'0'),'hex'),'synthetic.so',decode(repeat('01',20),'hex'),1800000000000+n,1800000000000+n FROM generate_series(1,4096) n")
        .execute(&db.pool).await.unwrap();
    sqlx::raw_sql(
        "ANALYZE metric.debug_files; ANALYZE metric.debug_uploads; ANALYZE metric.projects",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let debug = DebugId::Uuid {
        uuid: 16u128.to_be_bytes(),
        appendix: 0,
    };
    assert_eq!(
        store
            .find_debug_files(project_id(1), Some(debug.clone()), None, 1)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.recoverable_uploads(2).await.unwrap().len(), 2);
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT file_id FROM metric.debug_files WHERE project_id=$1 AND debug_id=$2 ORDER BY uploaded_at DESC,file_id LIMIT $3")
        .bind(1i32).bind(debug.encode().into_vec()).bind(1i64).fetch_one(&db.pool).await.unwrap();
    assert!(plan.to_string().contains("debug_file_debug_lookup"));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,BUFFERS,FORMAT JSON) SELECT upload_id FROM metric.debug_uploads u WHERE state IN(0,1) AND EXISTS(SELECT 1 FROM metric.projects p WHERE p.id=u.project_id AND p.state=0) ORDER BY updated_at,upload_id LIMIT $1")
        .bind(2i64).fetch_one(&db.pool).await.unwrap();
    assert!(plan.to_string().contains("debug_upload_recovery"));
    db.pool.close().await;
    fixture.cleanup().await;
}

struct FixedClock(Timestamp);
impl metric_ports::Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}
#[tokio::test]
#[ignore = "requires real PostgreSQL Docker"]
async fn existing_debug_service_streams_local_blob_with_postgres_metadata() {
    use metric_application::debug_files::{
        AssembleDebugFile, AssembleState, DebugFileConfig, DebugFileError, DebugFileService,
    };
    let (fixture, db, store) = setup(DebugFileQuota::default()).await;
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("pg-debug-{}", uuid::Uuid::new_v4().simple()));
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
    let service = DebugFileService::new(
        Arc::new(store.clone()),
        blobs.clone(),
        Arc::new(FixedClock(at(1_800_000_000_000))),
        DebugFileConfig::default(),
    )
    .unwrap();
    let context = AuthContext {
        user_id: UserId::new(1).unwrap(),
        organization_id: OrganizationId::new(1).unwrap(),
        role: OrganizationRole::Owner,
        permissions: PermissionSet::from_role(OrganizationRole::Owner),
        actor: Actor::WebSession,
        credential_id: CredentialId::new(1).unwrap(),
    };
    let sha1 = [
        0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50, 0xc2,
        0x6c, 0x9c, 0xd0, 0xd8, 0x9d,
    ];
    let request = AssembleDebugFile {
        sha1,
        name: "synthetic.so".into(),
        debug_id: Some(DebugId::Uuid {
            uuid: [9; 16],
            appendix: 0,
        }),
        code_id: None,
        chunks: vec![sha1],
    };
    assert!(matches!(
        service
            .assemble(
                &context,
                context.organization_id,
                project_id(1),
                request.clone()
            )
            .await
            .unwrap(),
        AssembleState::Missing { .. }
    ));
    service
        .upload_chunk(
            &context,
            context.organization_id,
            sha1,
            b"abc".to_vec().into_boxed_slice(),
        )
        .await
        .unwrap();
    let AssembleState::Ok { file, revision } = service
        .assemble(
            &context,
            context.organization_id,
            project_id(1),
            request.clone(),
        )
        .await
        .unwrap()
    else {
        panic!("did not assemble")
    };
    assert_eq!(revision, 1);
    assert_eq!(file.size, 3);
    let (_, mut reader) = service.open(project_id(1), file.id).await.unwrap();
    assert_eq!(
        reader.read_chunk(1024).await.unwrap().unwrap().as_ref(),
        b"abc"
    );
    assert!(reader.read_chunk(1024).await.unwrap().is_none());
    drop(reader);
    assert!(matches!(
        service
            .assemble(&context, context.organization_id, project_id(1), request)
            .await
            .unwrap(),
        AssembleState::Ok { .. }
    ));
    assert!(matches!(
        service.open(project_id(2), file.id).await,
        Err(DebugFileError::NotFound)
    ));
    assert!(
        service
            .delete(&context, project_id(1), file.id)
            .await
            .unwrap()
    );
    assert_eq!(counters(&db, 1).await, (0, 0, 2));
    drop(service);
    drop(blobs);
    let resolved = std::fs::canonicalize(&root).unwrap();
    let target = std::fs::canonicalize(std::env::current_dir().unwrap().join("target")).unwrap();
    assert!(
        resolved.starts_with(target)
            && resolved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pg-debug-")
    );
    std::fs::remove_dir_all(resolved).unwrap();
    db.pool.close().await;
    fixture.cleanup().await;
}
