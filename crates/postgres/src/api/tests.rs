use super::*;
use crate::{
    MigrationPolicy, PostgresFinalizationStore, PostgresReleaseStore, PostgresSchemaManager,
    test_support::{Fixture, finalized, seed_projects},
};
use metric_domain::{
    AcceptedEvent, ScrubbedEventPayload,
    api::SearchStorageBranch,
    finalization::{FinalizationPolicy, FinalizeBatch, FinalizeEvent, SearchToken},
    issue::{IssueCommand, IssueCommandAction},
    releases::CreateRelease,
};
use metric_ports::{EventStore, FinalizationStore, IssueStore, ReleaseStore};
use serde_json::json;
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
fn event(project: i32, issue: u8, id: u128, occurred: i64) -> FinalizeEvent {
    finalized(
        project,
        issue,
        EventId::from_bytes(id.to_be_bytes()),
        occurred,
        occurred + 100,
        Some("r\0'; DROP TABLE metric.releases; -- 雪"),
        Some("prod\0雪"),
    )
}
fn issue_query(limit: usize) -> IssueListQuery {
    IssueListQuery {
        status: None,
        from: None,
        until: None,
        before: None,
        limit,
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresInvestigationStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let api = PostgresInvestigationStore::new(
        database.clone(),
        EventCodecConfig::default(),
        IssueCodecConfig::default(),
    )
    .unwrap();
    (fixture, database, api)
}
async fn insert(database: &PostgresDatabase, events: &[FinalizeEvent], finalize: bool) {
    let store = PostgresEventStore::new(database.clone(), EventCodecConfig::default()).unwrap();
    for batch in events.chunks(500) {
        let prepared: Vec<_> = batch
            .iter()
            .map(|event| {
                store
                    .prepare(AcceptedEvent {
                        project_id: event.project_id,
                        event_id: event.event_id,
                        received_at: event.received_at,
                        policy_revision: 1,
                        payload: ScrubbedEventPayload::new(
                            serde_json::to_vec(&json!({"event_id":event.event_id.to_string()}))
                                .unwrap(),
                        ),
                    })
                    .unwrap()
            })
            .collect();
        store.insert_batch(&prepared).await.unwrap();
    }
    if finalize {
        PostgresFinalizationStore::new(
            database.clone(),
            EventCodecConfig::default(),
            IssueCodecConfig::default(),
        )
        .unwrap()
        .finalize(
            FinalizeBatch {
                events: events.to_vec(),
            },
            FinalizationPolicy {
                event_retention: Duration::from_secs(86400),
                hourly_retention: Duration::from_secs(864000),
                archive_events: false,
                max_implicit_releases_per_project_day: 100,
                max_implicit_environments_per_project: 100,
            },
        )
        .await
        .unwrap();
    }
}
async fn command(
    database: &PostgresDatabase,
    event: &FinalizeEvent,
    action: IssueCommandAction,
    seed: u8,
    time: i64,
) {
    PostgresIssueStore::new(database.clone(), IssueCodecConfig::default())
        .unwrap()
        .apply_command(IssueCommand {
            project_id: event.project_id,
            issue_id: event.issue.issue_id,
            idempotency_key: [seed; 16],
            actor: ActorRef::system(),
            at: at(time),
            action,
        })
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn issue_event_timeline_pages_scope_states_filters_and_ties() {
    let (fixture, database, api) = setup().await;
    let mut events: Vec<_> = (1..=12)
        .map(|id| event(1, ((id - 1) / 4 + 1) as u8, id, 1000))
        .collect();
    events.push(event(2, 1, 1, 2000));
    events[0].payload = ProcessedEventPayload::new(
        serde_json::to_vec(
            &json!({"platform":"custom\0runtime","level":"fatal","message":"hello\0world"}),
        )
        .unwrap(),
    );
    insert(&database, &events, true).await;
    let pending = event(1, 4, 100, 3000);
    insert(&database, std::slice::from_ref(&pending), false).await;
    let failed = event(1, 4, 101, 3000);
    insert(&database, std::slice::from_ref(&failed), false).await;
    sqlx::query("UPDATE metric.error_events SET processing_state=1,next_attempt_at=NULL WHERE project_id=1 AND event_id=$1").bind(failed.event_id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    let first = api
        .list_events(project(1), None, at(0), at(2000), None, 5)
        .await
        .unwrap();
    assert_eq!(first.candidates_examined, 6);
    assert_eq!(first.items.len(), 5);
    assert!(first.next.is_some());
    let mut keys = vec![];
    let mut before = None;
    loop {
        let page = api
            .list_events(project(1), None, at(0), at(2000), before, 5)
            .await
            .unwrap();
        keys.extend(page.items.iter().map(|event| event.key));
        before = page.next;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(
        keys,
        events[..12]
            .iter()
            .rev()
            .map(|event| EventKey::new(event.project_id, event.event_id))
            .collect::<Vec<_>>()
    );
    let exact = api.load_event(project(1), keys[11]).await.unwrap();
    assert_eq!(
        exact.platform,
        EventPlatform::Custom("custom\0runtime".into())
    );
    assert_eq!(exact.level, EventLevel::Fatal);
    assert_eq!(
        api.load_event(project(2), keys[11]).await,
        Err(InvestigationStoreError::NotFound)
    );
    assert_eq!(
        api.load_event(project(1), EventKey::new(project(1), pending.event_id))
            .await,
        Err(InvestigationStoreError::NotFound)
    );
    assert_eq!(
        api.list_events(project(2), None, at(0), at(3000), None, 100)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let selected = api
        .list_events(
            project(1),
            Some(events[0].issue.issue_id),
            at(1000),
            at(1001),
            None,
            100,
        )
        .await
        .unwrap();
    assert_eq!(selected.items.len(), 4);
    assert!(selected.next.is_none());
    assert!(
        api.list_events(project(1), None, at(1001), at(2000), None, 100)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let page = api.list_issues(project(1), issue_query(2)).await.unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.next.is_some());
    let tail = api
        .list_issues(
            project(1),
            IssueListQuery {
                before: page.next,
                ..issue_query(2)
            },
        )
        .await
        .unwrap();
    assert_eq!(tail.items.len(), 1);
    assert!(tail.next.is_none());
    let point = PostgresIssueStore::new(database.clone(), IssueCodecConfig::default())
        .unwrap()
        .load(project(1), page.items[0].issue_id)
        .await
        .unwrap();
    assert_eq!(point, page.items[0]);
    command(&database, &events[0], IssueCommandAction::Resolve, 1, 5000).await;
    command(&database, &events[4], IssueCommandAction::Ignore, 2, 5000).await;
    for status in [
        IssueStatus::Open,
        IssueStatus::Resolved,
        IssueStatus::Ignored,
    ] {
        let page = api
            .list_issues(
                project(1),
                IssueListQuery {
                    status: Some(status),
                    from: Some(at(1000)),
                    until: Some(at(1001)),
                    ..issue_query(100)
                },
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].status, status);
    }
    for limit in [0, 101, usize::MAX] {
        assert_eq!(
            api.list_issues(project(1), issue_query(limit)).await,
            Err(InvestigationStoreError::InvalidData)
        );
        assert_eq!(
            api.list_events(project(1), None, at(0), at(1000), None, limit)
                .await,
            Err(InvestigationStoreError::InvalidData)
        );
    }
    assert_eq!(
        api.list_events(project(1), None, at(1000), at(1000), None, 1)
            .await,
        Err(InvestigationStoreError::InvalidData)
    );
    assert!(
        api.list_issues(project(99), issue_query(100))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn bounded_search_branch_union_tokens_candidates_and_foreign_cursors() {
    let (fixture, database, api) = setup().await;
    let mut events = vec![
        event(1, 1, 1, 1000),
        event(1, 2, 2, 1000),
        event(1, 3, 3, 2000),
        event(2, 1, 1, 1000),
    ];
    events[0].search_tokens = vec![SearchToken::from_stored(i64::MIN)];
    events[1].search_tokens = vec![SearchToken::from_stored(i64::MAX)];
    events[2].search_tokens = vec![];
    insert(&database, &events, true).await;
    let branch = |anchor| SearchStorageBranch {
        anchor,
        from: at(0),
        until: at(2000),
    };
    let request = |branches, limit| SearchStorageQuery {
        branches,
        before: None,
        candidate_limit: limit,
    };
    let rows = api
        .search_candidates(
            project(1),
            request(
                vec![
                    branch(SearchStorageAnchor::Token(SearchToken::from_stored(
                        i64::MIN,
                    ))),
                    branch(SearchStorageAnchor::Issue(events[1].issue.issue_id)),
                    branch(SearchStorageAnchor::Event(EventKey::new(
                        project(1),
                        events[0].event_id,
                    ))),
                ],
                100,
            ),
        )
        .await
        .unwrap();
    assert_eq!(rows.items.len(), 2);
    assert_eq!(rows.candidates_examined, 2);
    assert!(rows.next.is_none());
    assert_eq!(rows.items[0].key.event_id(), events[1].event_id);
    let one = api
        .search_candidates(
            project(1),
            request(vec![branch(SearchStorageAnchor::ProjectTimeline)], 1),
        )
        .await
        .unwrap();
    assert_eq!(one.items.len(), 1);
    assert_eq!(one.candidates_examined, 1);
    assert!(one.next.is_none());
    let foreign = EventKey::new(project(2), events[3].event_id);
    assert!(
        api.search_candidates(
            project(1),
            request(vec![branch(SearchStorageAnchor::Event(foreign))], 100)
        )
        .await
        .unwrap()
        .items
        .is_empty()
    );
    let before = api
        .list_events(
            project(1),
            None,
            at(0),
            at(2000),
            Some(EventAnchor {
                occurred_at: at(1000),
                event_key: foreign,
            }),
            100,
        )
        .await
        .unwrap();
    assert_eq!(before.items.len(), 2);
    let rows = api
        .search_candidates(
            project(1),
            SearchStorageQuery {
                branches: vec![branch(SearchStorageAnchor::ProjectTimeline)],
                before: Some(EventAnchor {
                    occurred_at: at(1000),
                    event_key: EventKey::new(project(1), events[1].event_id),
                }),
                candidate_limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.items.len(), 1);
    assert_eq!(
        api.search_candidates(project(1), request(vec![], 100))
            .await,
        Err(InvestigationStoreError::InvalidData)
    );
    assert_eq!(
        api.search_candidates(
            project(1),
            request(vec![branch(SearchStorageAnchor::ProjectTimeline); 9], 100)
        )
        .await,
        Err(InvestigationStoreError::InvalidData)
    );
    for limit in [0, 10_001, usize::MAX] {
        assert_eq!(
            api.search_candidates(
                project(1),
                request(vec![branch(SearchStorageAnchor::ProjectTimeline)], limit)
            )
            .await,
            Err(InvestigationStoreError::InvalidData)
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn activity_and_statistics_are_scoped_ordered_and_bounded() {
    let (fixture, database, api) = setup().await;
    let first = event(1, 1, 1, 1000);
    let latest = event(1, 1, 2, 3_601_000);
    let foreign = event(2, 1, 1, 1000);
    insert(&database, &[first.clone(), latest, foreign], true).await;
    for (index, action) in [
        IssueCommandAction::Resolve,
        IssueCommandAction::Ignore,
        IssueCommandAction::Reopen,
        IssueCommandAction::Assign(Some(ActorRef::system())),
        IssueCommandAction::Assign(None),
        IssueCommandAction::Resolve,
    ]
    .into_iter()
    .enumerate()
    {
        command(&database, &first, action, index as u8, 5_000_000).await;
    }
    let next = event(1, 1, 3, 6_000_000);
    insert(&database, std::slice::from_ref(&next), true).await;
    let stats = api
        .issue_statistics(project(1), first.issue.issue_id, at(0), at(7_200_000), 100)
        .await
        .unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].bucket_start, at(0));
    assert_eq!(stats[0].occurrence_count.get(), 1);
    assert_eq!(stats[1].occurrence_count.get(), 2);
    assert_eq!(
        api.issue_statistics(
            project(1),
            first.issue.issue_id,
            at(3_600_000),
            at(7_200_000),
            1
        )
        .await
        .unwrap()
        .len(),
        1
    );
    assert!(
        api.issue_statistics(project(2), first.issue.issue_id, at(0), at(7_200_000), 100)
            .await
            .unwrap()
            .is_empty()
    );
    let all = api
        .issue_activity(project(1), first.issue.issue_id, None, 100)
        .await
        .unwrap();
    assert_eq!(all.items.len(), 7);
    assert_eq!(all.items[0].kind, IssueActivityKind::Regressed);
    assert_eq!(
        all.items[0].event_key,
        Some(EventKey::new(project(1), next.event_id))
    );
    for kind in [
        IssueActivityKind::Resolved,
        IssueActivityKind::Ignored,
        IssueActivityKind::Reopened,
        IssueActivityKind::Assigned,
        IssueActivityKind::Unassigned,
    ] {
        assert!(all.items.iter().any(|item| item.kind == kind));
    }
    let mut items = vec![];
    let mut before = None;
    loop {
        let page = api
            .issue_activity(project(1), first.issue.issue_id, before, 2)
            .await
            .unwrap();
        items.extend(page.items);
        before = page.next;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(items, all.items);
    assert!(
        api.issue_activity(project(2), first.issue.issue_id, None, 100)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    for limit in [0, 101] {
        assert_eq!(
            api.issue_activity(project(1), first.issue.issue_id, None, limit)
                .await,
            Err(InvestigationStoreError::InvalidData)
        );
        assert_eq!(
            api.issue_statistics(project(1), first.issue.issue_id, at(0), at(1000), limit)
                .await,
            Err(InvestigationStoreError::InvalidData)
        );
    }
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn catalog_pages_preserve_binary_values_scope_hidden_state_and_ties() {
    let (fixture, database, api) = setup().await;
    let mut events = vec![];
    for id in 1..=4 {
        let mut row = event(1, 1, id, 1000);
        row.issue.release =
            Some(metric_domain::issue::IssueRelease::new(format!("r\0{id}")).unwrap());
        row.environment = Some(format!("env\0{id}").into());
        events.push(row);
    }
    events.push(event(2, 1, 1, 2000));
    insert(&database, &events, true).await;
    let control = PostgresReleaseStore::new(database.clone());
    let explicit = control
        .create_release(CreateRelease {
            organization_id: org(1),
            project_ids: vec![project(1)],
            version: "explicit\0雪".into(),
            url: None,
            reference: None,
            repositories: vec![],
            created_at: at(1000),
        })
        .await
        .unwrap();
    let all = api
        .list_releases(org(1), project(1), None, 100)
        .await
        .unwrap();
    assert_eq!(all.items.len(), 5);
    let view = all
        .items
        .iter()
        .find(|view| view.id == explicit.id)
        .unwrap();
    assert!(view.explicit);
    assert_eq!(view.first_seen, None);
    assert_eq!(view.version.as_ref(), "explicit\0雪");
    let mut items = vec![];
    let mut before = None;
    loop {
        let page = api
            .list_releases(org(1), project(1), before, 2)
            .await
            .unwrap();
        items.extend(page.items);
        before = page.next;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(items, all.items);
    assert!(
        api.list_releases(org(2), project(1), None, 100)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        api.list_releases(org(1), project(2), None, 100)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let hidden = derive_environment_id(project(1), "env\x001");
    sqlx::query(
        "UPDATE metric.environments SET hidden=true WHERE project_id=1 AND environment_id=$1",
    )
    .bind(hidden.as_bytes().as_slice())
    .execute(&database.pool)
    .await
    .unwrap();
    let all = api.list_environments(project(1), None, 100).await.unwrap();
    assert_eq!(all.items.len(), 3);
    assert!(
        all.items
            .iter()
            .all(|view| view.name.contains('\0') && view.id != hidden)
    );
    let mut items = vec![];
    let mut before = None;
    loop {
        let page = api.list_environments(project(1), before, 2).await.unwrap();
        items.extend(page.items);
        before = page.next;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(items, all.items);
    assert_eq!(
        api.list_environments(project(2), None, 100)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    for limit in [0, 101] {
        assert_eq!(
            api.list_environments(project(1), None, limit).await,
            Err(InvestigationStoreError::InvalidData)
        );
        assert_eq!(
            api.list_releases(org(1), project(1), None, limit).await,
            Err(InvestigationStoreError::InvalidData)
        );
    }
    sqlx::query("UPDATE metric.environments SET name=$1 WHERE project_id=1 AND environment_id=$2")
        .bind(b"wrong-name".as_slice())
        .bind(all.items[0].id.as_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        api.list_environments(project(1), None, 100).await,
        Err(InvestigationStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.releases SET version=$1 WHERE organization_id=1 AND release_id=$2")
        .bind(b"wrong-version".as_slice())
        .bind(explicit.id.as_bytes().as_slice())
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        api.list_releases(org(1), project(1), None, 100).await,
        Err(InvestigationStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn corruption_and_codec_limits_fail_before_unbounded_materialization() {
    let (fixture, database, api) = setup().await;
    let event = event(1, 1, 1, 1000);
    insert(&database, std::slice::from_ref(&event), true).await;
    let key = EventKey::new(project(1), event.event_id);
    let narrow = PostgresInvestigationStore::new(
        database.clone(),
        EventCodecConfig {
            max_decoded_body_bytes: 32,
            ..Default::default()
        },
        IssueCodecConfig::default(),
    )
    .unwrap();
    assert_eq!(
        narrow.load_event(project(1), key).await,
        Err(InvestigationStoreError::InvalidData)
    );
    let body: Vec<u8> =
        sqlx::query_scalar("SELECT body FROM metric.error_events WHERE project_id=1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    for invalid_body in [
        vec![2, 0, b'{', b'}'],
        vec![1, 1, 0, 0],
        vec![1, 0, 0xff, 0xff],
        vec![1, 0, b'[', b']'],
    ] {
        sqlx::query("UPDATE metric.error_events SET body=$1 WHERE project_id=1")
            .bind(invalid_body)
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            api.load_event(project(1), key).await,
            Err(InvestigationStoreError::InvalidData)
        );
    }
    let (invalid_level, _) =
        codec::encode_json(&json!({"level":"invalid","platform":"rust"}), 3, 64, 1024).unwrap();
    sqlx::query("UPDATE metric.error_events SET body=$1 WHERE project_id=1")
        .bind(invalid_level)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        api.load_event(project(1), key).await,
        Err(InvestigationStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.error_events SET body=$1 WHERE project_id=1")
        .bind(vec![1; 3 * 1024 * 1024])
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        api.load_event(project(1), key).await,
        Err(InvestigationStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.error_events SET body=$1 WHERE project_id=1")
        .bind(body)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(api.load_event(project(1), key).await.unwrap().key, key);
    sqlx::query("UPDATE metric.issues SET grouping_body=$1 WHERE project_id=1")
        .bind(vec![1, 0, b'{', b'}'])
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        api.list_issues(project(1), issue_query(100)).await,
        Err(InvestigationStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn selective_timelines_tokens_and_environments_use_native_indexes() {
    let (fixture, database, api) = setup().await;
    let mut rows: Vec<_> = (1..=10_000).map(|id| event(1, 1, id, id as i64)).collect();
    rows[5000].search_tokens = vec![SearchToken::from_stored(i64::MIN)];
    insert(&database, &rows, true).await;
    sqlx::query("VACUUM ANALYZE metric.error_events")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,FORMAT JSON) SELECT event_id FROM metric.error_events WHERE project_id=1 AND processing_state=2 AND occurred_at>=9000 AND occurred_at<10001 ORDER BY occurred_at DESC,event_id DESC LIMIT 10").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("event_project_timeline"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(10.0));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,FORMAT JSON) SELECT event_id FROM metric.error_events WHERE project_id=1 AND processing_state=2 AND issue_id=$1 AND occurred_at>=9000 ORDER BY occurred_at DESC,event_id DESC LIMIT 10").bind(rows[0].issue.issue_id.as_bytes().as_slice()).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("event_issue_timeline"));
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,FORMAT JSON) SELECT event_id FROM metric.error_events WHERE project_id=1 AND processing_state=2 AND search_tokens @> $1 ORDER BY occurred_at DESC,event_id DESC LIMIT 10").bind(vec![i64::MIN]).fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("event_search_tokens"));
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
    let candidates = api
        .search_candidates(
            project(1),
            SearchStorageQuery {
                branches: vec![SearchStorageBranch {
                    anchor: SearchStorageAnchor::Token(SearchToken::from_stored(i64::MIN)),
                    from: at(0),
                    until: at(10001),
                }],
                before: None,
                candidate_limit: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(candidates.items.len(), 1);
    assert_eq!(
        api.list_events(
            project(1),
            Some(rows[0].issue.issue_id),
            at(9000),
            at(10001),
            None,
            10
        )
        .await
        .unwrap()
        .items
        .len(),
        10
    );
    sqlx::query("INSERT INTO metric.environments(project_id,environment_id,name,first_seen,last_seen) SELECT 2,decode(lpad(to_hex(n),32,'0'),'hex'),convert_to('noise-'||n,'UTF8'),0,n FROM generate_series(1,10000) n").execute(&database.pool).await.unwrap();
    sqlx::query("ANALYZE metric.environments")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:serde_json::Value=sqlx::query_scalar("EXPLAIN(ANALYZE,FORMAT JSON) SELECT environment_id FROM metric.environments WHERE project_id=1 AND NOT hidden ORDER BY last_seen DESC,environment_id DESC LIMIT 10").fetch_one(&database.pool).await.unwrap();
    assert!(plan.to_string().contains("environment_project_timeline"));
    assert_eq!(
        api.list_environments(project(1), None, 10)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    database.close().await;
    fixture.cleanup().await;
}
