use super::*;
use crate::{
    EventCodecConfig, IssueCodecConfig, MigrationPolicy, PostgresEventStore,
    PostgresFinalizationStore, PostgresIssueStore, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, finalized, project_identity, seed_projects},
};
use metric_domain::{
    AcceptedEvent, EventId, ScrubbedEventPayload, Slug,
    finalization::{FinalizationPolicy, FinalizeBatch},
    issue::{ActorRef, IssueCommand, IssueCommandAction},
    releases::derive_deploy_id,
};
use metric_ports::{EventStore, FinalizationStore, IssueStore, ProjectStore};
use std::time::Duration;

fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn org(value: u64) -> OrganizationId {
    OrganizationId::new(value).unwrap()
}
fn release(version: &str) -> CreateRelease {
    CreateRelease {
        organization_id: org(1),
        project_ids: vec![project(1)],
        version: version.into(),
        url: None,
        reference: None,
        repositories: vec![],
        created_at: at(100),
    }
}
fn deploy(release: ReleaseId, seed: u8, start: i64) -> CreateDeploy {
    CreateDeploy {
        deploy_id: derive_deploy_id(org(1), release, [seed; 16]),
        organization_id: org(1),
        release_id: release,
        project_ids: vec![project(1)],
        environment: "production".into(),
        name: None,
        url: None,
        started_at: at(start),
        finished_at: None,
        created_at: at(100),
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresReleaseStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    let control = seed_projects(database.clone(), 32).await;
    let mut third = project_identity(3, 1);
    third.slug = Slug::new("third-project").unwrap();
    control.insert_project(third).await.unwrap();
    let store = PostgresReleaseStore::new(database.clone());
    (fixture, database, store)
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn organization_project_resolution_is_scoped_bounded_and_fenced() {
    let (fixture, database, store) = setup().await;
    assert_eq!(
        store
            .resolve_projects(
                "org-1".into(),
                vec!["third-project".into(), "same-project".into()]
            )
            .await
            .unwrap(),
        (org(1), vec![project(1), project(3)])
    );
    assert_eq!(
        store
            .resolve_projects("org-2".into(), vec!["same-project".into()])
            .await
            .unwrap(),
        (org(2), vec![project(2)])
    );
    for (organization, slugs) in [
        ("org-1", vec!["same-project", "same-project"]),
        ("org-1", vec![]),
    ] {
        assert_eq!(
            store
                .resolve_projects(
                    organization.into(),
                    slugs.into_iter().map(Into::into).collect()
                )
                .await,
            Err(ReleaseStoreError::InvalidData)
        );
    }
    for (organization, slug) in [
        ("absent", "same-project"),
        ("org-1", "missing"),
        ("org-1\0suffix", "same-project"),
        ("org-1", "'; DROP TABLE metric.projects; --"),
    ] {
        assert_eq!(
            store
                .resolve_projects(organization.into(), vec![slug.into()])
                .await,
            Err(ReleaseStoreError::NotFound)
        );
    }
    sqlx::query(
        "UPDATE metric.projects SET state=CASE WHEN id=1 THEN 1 ELSE 2 END WHERE organization_id=1",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    assert!(
        store
            .resolve_projects("org-1".into(), vec!["same-project".into()])
            .await
            .is_ok()
    );
    assert_eq!(
        store
            .resolve_projects("org-1".into(), vec!["third-project".into()])
            .await,
        Err(ReleaseStoreError::NotFound)
    );
    let mut wrong = release("cross-tenant");
    wrong.project_ids = vec![project(2)];
    assert_eq!(
        store.create_release(wrong).await,
        Err(ReleaseStoreError::NotFound)
    );
    let mut fenced = release("fenced");
    fenced.project_ids = vec![project(3)];
    assert_eq!(
        store.create_release(fenced).await,
        Err(ReleaseStoreError::NotFound)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.releases")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn implicit_to_explicit_release_preserves_pairs_and_lossless_metadata() {
    let (fixture, database, store) = setup().await;
    let version = "release\0'; DROP TABLE metric.releases; -- 雪";
    let event = finalized(
        1,
        1,
        EventId::from_bytes([1; 16]),
        10,
        20,
        Some(version),
        None,
    );
    let events = PostgresEventStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    let prepared = events
        .prepare(AcceptedEvent {
            project_id: project(1),
            event_id: event.event_id,
            received_at: event.received_at,
            policy_revision: 1,
            payload: ScrubbedEventPayload::new(
                serde_json::to_vec(&json!({"event_id":event.event_id.to_string()})).unwrap(),
            ),
        })
        .unwrap();
    events.insert_batch(&[prepared]).await.unwrap();
    let finalizer = PostgresFinalizationStore::new(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    )
    .unwrap();
    finalizer
        .finalize(
            FinalizeBatch {
                events: vec![event.clone()],
            },
            FinalizationPolicy {
                event_retention: Duration::from_secs(86400),
                hourly_retention: Duration::from_secs(86400),
                archive_events: false,
                max_implicit_releases_per_project_day: 1,
                max_implicit_environments_per_project: 1,
            },
        )
        .await
        .unwrap();
    let id = derive_release_id(org(1), version);
    let implicit = store.load_release(org(1), id).await.unwrap();
    assert!(!implicit.explicit);
    assert_eq!(implicit.created_at, at(20));
    assert_eq!(implicit.first_event_id, Some(event.event_id));
    let mut command = release(version);
    command.project_ids = vec![project(3), project(1)];
    command.created_at = at(200);
    command.url = Some("https://synthetic.invalid/\0雪".into());
    command.reference = Some("ref\0value".into());
    command.repositories = vec![RepositoryReference {
        repository: "repo\0'; -- 雪".into(),
        commit_from: None,
        commit_to: Some("commit\0raw".into()),
    }];
    let explicit = store.create_release(command.clone()).await.unwrap();
    assert_eq!(explicit.id, id);
    assert!(explicit.explicit);
    assert_eq!(explicit.project_ids, vec![project(1), project(3)]);
    assert_eq!(explicit.created_at, implicit.created_at);
    assert_eq!(explicit.first_seen, implicit.first_seen);
    assert_eq!(explicit.first_event_id, implicit.first_event_id);
    assert_eq!(explicit.url, command.url);
    assert_eq!(explicit.reference, command.reference);
    assert_eq!(explicit.repositories, command.repositories);
    let mut preserve = release(version);
    preserve.created_at = at(300);
    let preserved = store.create_release(preserve).await.unwrap();
    assert_eq!(preserved.url, command.url);
    assert_eq!(preserved.repositories, command.repositories);
    let released = store
        .finalize_release(FinalizeRelease {
            organization_id: org(1),
            release_id: id,
            released_at: at(500),
        })
        .await
        .unwrap();
    assert_eq!(released.released_at, Some(at(500)));
    let retried = store
        .finalize_release(FinalizeRelease {
            organization_id: org(1),
            release_id: id,
            released_at: at(600),
        })
        .await
        .unwrap();
    assert_eq!(retried.released_at, Some(at(500)));
    assert_eq!(retried.activity_at, at(600));
    assert_eq!(
        store.load_release(org(2), id).await,
        Err(ReleaseStoreError::NotFound)
    );
    assert_eq!(
        store
            .finalize_release(FinalizeRelease {
                organization_id: org(2),
                release_id: id,
                released_at: at(700)
            })
            .await,
        Err(ReleaseStoreError::NotFound)
    );
    let fresh = store
        .create_release(release("explicit-only"))
        .await
        .unwrap();
    assert!(fresh.first_seen.is_none());
    assert!(fresh.first_event_id.is_none());
    let count: i64 = sqlx::query_scalar(
        "SELECT release_count FROM metric.project_catalog_usage WHERE project_id=1",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        count, 1,
        "explicit control-plane writes do not consume implicit admission slots"
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn deploy_order_identity_concurrent_retry_finish_and_scope() {
    let (fixture, database, store) = setup().await;
    let release = store
        .create_release(release("deploy-release"))
        .await
        .unwrap();
    let mut command = deploy(release.id, 1, 1000);
    command.project_ids = vec![project(3), project(1)];
    command.environment = "env\0'; -- 雪".into();
    command.name = Some("name\0raw".into());
    command.url = Some("https://synthetic.invalid/\0".into());
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let store = store.clone();
        let value = command.clone();
        tasks.push(tokio::spawn(
            async move { store.create_deploy(value).await },
        ));
    }
    for task in tasks {
        let value = task.await.unwrap().unwrap();
        assert_eq!(value.project_ids, command.project_ids);
        assert_eq!(value.environment, command.environment);
        assert_eq!(value.name, command.name);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.deploys")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut conflict = command.clone();
    conflict.project_ids.reverse();
    assert_eq!(
        store.create_deploy(conflict).await,
        Err(ReleaseStoreError::Conflict)
    );
    assert_eq!(
        store
            .finish_deploy(org(1), command.deploy_id, at(999))
            .await,
        Err(ReleaseStoreError::InvalidData)
    );
    let (a, b) = tokio::join!(
        store.finish_deploy(org(1), command.deploy_id, at(2000)),
        store.finish_deploy(org(1), command.deploy_id, at(2000))
    );
    assert_eq!(a.unwrap().finished_at, Some(at(2000)));
    assert_eq!(b.unwrap().finished_at, Some(at(2000)));
    assert_eq!(
        store
            .finish_deploy(org(1), command.deploy_id, at(2001))
            .await,
        Err(ReleaseStoreError::Conflict)
    );
    assert_eq!(
        store
            .finish_deploy(org(2), command.deploy_id, at(2000))
            .await,
        Err(ReleaseStoreError::NotFound)
    );
    assert!(
        store
            .list_deploys(org(2), project(1), release.id, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_deploys(org(1), project(2), release.id, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    let mut unknown = deploy(derive_release_id(org(1), "missing"), 2, 1000);
    unknown.project_ids = vec![project(1)];
    assert_eq!(
        store.create_deploy(unknown).await,
        Err(ReleaseStoreError::NotFound)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn deploy_keyset_pages_are_stable_and_use_project_release_index() {
    let (fixture, database, store) = setup().await;
    let release = store.create_release(release("timeline")).await.unwrap();
    let mut expected = Vec::new();
    for seed in 1..=7 {
        let value = store
            .create_deploy(deploy(release.id, seed, 1000 + i64::from(seed / 3)))
            .await
            .unwrap();
        expected.push((value.started_at, value.id.as_bytes()));
    }
    expected.sort_unstable_by(|a, b| b.cmp(a));
    let mut cursor = None;
    let mut actual = Vec::new();
    loop {
        let values = store
            .list_deploys(org(1), project(1), release.id, cursor, 2)
            .await
            .unwrap();
        if values.is_empty() {
            break;
        }
        actual.extend(
            values
                .iter()
                .map(|value| (value.started_at, value.id.as_bytes())),
        );
        let last = values.last().unwrap();
        cursor = Some((last.started_at, last.id));
    }
    assert_eq!(actual, expected);
    for limit in [0, 101] {
        assert_eq!(
            store
                .list_deploys(org(1), project(1), release.id, None, limit)
                .await,
            Err(ReleaseStoreError::InvalidData)
        );
    }
    sqlx::query("INSERT INTO metric.deploys(organization_id,deploy_id,release_id,environment,started_at,created_at) SELECT 1,substring(sha256(int4send(i)) FROM 1 FOR 16),$1,convert_to('noise','UTF8'),10000+i,100 FROM generate_series(1000,10999) i")
        .bind(release.id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    sqlx::query("INSERT INTO metric.deploy_projects(organization_id,deploy_id,release_id,started_at,project_id,ordinal) SELECT organization_id,deploy_id,release_id,started_at,1,0 FROM metric.deploys WHERE started_at>=11000").execute(&database.pool).await.unwrap();
    sqlx::query("ANALYZE metric.deploy_projects")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE metric.deploys")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:Value=sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT d.deploy_id FROM metric.deploy_projects q JOIN metric.deploys d ON d.organization_id=q.organization_id AND d.deploy_id=q.deploy_id WHERE q.organization_id=1 AND q.project_id=1 AND q.release_id=$1 ORDER BY q.started_at DESC,q.deploy_id DESC LIMIT 10")
        .bind(release.id.as_bytes().as_slice()).fetch_one(&database.pool).await.unwrap();
    assert!(
        plan.to_string().contains("deploy_project_release_timeline"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(10.0));
    assert_eq!(
        store
            .list_deploys(org(1), project(1), release.id, None, 10)
            .await
            .unwrap()
            .len(),
        10
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn release_issue_pages_use_first_or_regression_time_and_exact_bytes() {
    let (fixture, database, store) = setup().await;
    let issues = PostgresIssueStore::new(database.clone(), IssueCodecConfig::default()).unwrap();
    let version = "first\0release";
    let a = finalized(
        1,
        1,
        EventId::from_bytes([1; 16]),
        10,
        20,
        Some(version),
        None,
    );
    let b = finalized(
        1,
        2,
        EventId::from_bytes([2; 16]),
        20,
        30,
        Some(version),
        None,
    );
    issues.apply_occurrence(a.issue.clone()).await.unwrap();
    issues.apply_occurrence(b.issue.clone()).await.unwrap();
    let later = finalized(1, 1, EventId::from_bytes([3; 16]), 1000, 40, None, None);
    issues.apply_occurrence(later.issue).await.unwrap();
    let first = store
        .list_release_issues(project(1), version.into(), ReleaseIssueKind::New, None, 1)
        .await
        .unwrap();
    assert_eq!(first[0].issue_id, b.issue.issue_id);
    let next = store
        .list_release_issues(
            project(1),
            version.into(),
            ReleaseIssueKind::New,
            Some((at(20), b.issue.issue_id)),
            1,
        )
        .await
        .unwrap();
    assert_eq!(next[0].issue_id, a.issue.issue_id);
    assert_eq!(next[0].last_release, None);
    for value in [&a, &b] {
        issues
            .apply_command(IssueCommand {
                project_id: value.project_id,
                issue_id: value.issue.issue_id,
                idempotency_key: [1; 16],
                actor: ActorRef::system(),
                at: at(100),
                action: IssueCommandAction::Resolve,
            })
            .await
            .unwrap();
    }
    let reg_a = finalized(
        1,
        1,
        EventId::from_bytes([4; 16]),
        10000,
        101,
        Some("reg\0release"),
        None,
    );
    let reg_b = finalized(
        1,
        2,
        EventId::from_bytes([5; 16]),
        30,
        102,
        Some("reg\0release"),
        None,
    );
    issues.apply_occurrence(reg_a.issue).await.unwrap();
    issues.apply_occurrence(reg_b.issue).await.unwrap();
    let first = store
        .list_release_issues(
            project(1),
            "reg\0release".into(),
            ReleaseIssueKind::Regressed,
            None,
            1,
        )
        .await
        .unwrap();
    assert_eq!(first[0].issue_id, b.issue.issue_id);
    let next = store
        .list_release_issues(
            project(1),
            "reg\0release".into(),
            ReleaseIssueKind::Regressed,
            Some((at(102), b.issue.issue_id)),
            1,
        )
        .await
        .unwrap();
    assert_eq!(next[0].issue_id, a.issue.issue_id);
    assert!(
        store
            .list_release_issues(project(2), version.into(), ReleaseIssueKind::New, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
    for limit in [0, 101] {
        assert_eq!(
            store
                .list_release_issues(
                    project(1),
                    version.into(),
                    ReleaseIssueKind::New,
                    None,
                    limit
                )
                .await,
            Err(ReleaseStoreError::InvalidData)
        );
    }
    sqlx::query("INSERT INTO metric.issues(project_id,issue_id,grouping_key,title,search_projection,first_seen,last_seen,first_event_id,latest_event_id,representative_event_id,occurrence_count,first_release,regression_at,regression_event_id,regression_count,regression_release,grouping_body) SELECT 1,substring(sha256(int4send(i)) FROM 1 FOR 16),decode('0001','hex')||sha256(int4send(i)),convert_to('noise','UTF8'),'noise',10,10,decode(repeat('00',16),'hex'),decode(repeat('00',16),'hex'),decode(repeat('00',16),'hex'),1,convert_to('noise','UTF8'),20,decode(repeat('00',16),'hex'),1,convert_to('noise','UTF8'),decode('01007b7d','hex') FROM generate_series(1000,10999) i").execute(&database.pool).await.unwrap();
    sqlx::query("VACUUM ANALYZE metric.issues")
        .execute(&database.pool)
        .await
        .unwrap();
    let new_plan:Value=sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT issue_id FROM metric.issues WHERE project_id=1 AND first_release=$1 ORDER BY first_seen DESC,issue_id DESC LIMIT 10")
        .bind(version.as_bytes()).fetch_one(&database.pool).await.unwrap();
    assert!(
        new_plan.to_string().contains("issue_release_new_timeline"),
        "{new_plan}"
    );
    let regression_plan:Value=sqlx::query_scalar("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) SELECT issue_id FROM metric.issues WHERE project_id=1 AND regression_release=$1 ORDER BY regression_at DESC,issue_id DESC LIMIT 10")
        .bind(b"reg\0release".as_slice()).fetch_one(&database.pool).await.unwrap();
    assert!(
        regression_plan
            .to_string()
            .contains("issue_release_regression_timeline"),
        "{regression_plan}"
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn release_transaction_rollback_ambiguous_retry_and_corrupt_decoding() {
    let (fixture, database, store) = setup().await;
    sqlx::query("CREATE FUNCTION metric.reject_release_link() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'synthetic failure'; END$$").execute(&database.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_release_link BEFORE INSERT ON metric.release_projects FOR EACH ROW EXECUTE FUNCTION metric.reject_release_link()").execute(&database.pool).await.unwrap();
    let command = release("rollback");
    assert_eq!(
        store.create_release(command.clone()).await,
        Err(ReleaseStoreError::Unavailable)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.releases")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER reject_release_link ON metric.release_projects")
        .execute(&database.pool)
        .await
        .unwrap();
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
    let through = PostgresReleaseStore::new(proxied.clone());
    proxy.drop_next_commit_reply();
    assert_eq!(
        through.create_release(command.clone()).await,
        Err(ReleaseStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    let result = through.create_release(command.clone()).await.unwrap();
    assert_eq!(store.load_release(org(1), result.id).await.unwrap(), result);
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.release_projects")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(links, 1);
    let mut bad = command.clone();
    bad.project_ids = vec![project(1), project(1)];
    assert_eq!(
        store.create_release(bad).await,
        Err(ReleaseStoreError::InvalidData)
    );
    sqlx::query(
        "UPDATE metric.releases SET repositories_body=decode('0200','hex') WHERE organization_id=1",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    assert_eq!(
        store.load_release(org(1), result.id).await,
        Err(ReleaseStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.releases SET repositories_body=NULL,version=decode('ff','hex') WHERE organization_id=1").execute(&database.pool).await.unwrap();
    assert_eq!(
        store.load_release(org(1), result.id).await,
        Err(ReleaseStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.releases SET version=convert_to('changed identity','UTF8') WHERE organization_id=1").execute(&database.pool).await.unwrap();
    assert_eq!(
        store.load_release(org(1), result.id).await,
        Err(ReleaseStoreError::InvalidData)
    );
    assert_eq!(
        store.create_release(command).await,
        Err(ReleaseStoreError::Conflict)
    );
    proxied.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}
