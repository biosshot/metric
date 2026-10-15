use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};
use metric_domain::explore::*;

fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
fn project(value: i32) -> ProjectId {
    ProjectId::new(value).unwrap()
}
fn saved(seed: u8, project_id: i32) -> SavedQuery {
    SavedQuery {
        id: SavedQueryId::from_bytes([seed; 16]),
        project_id: project(project_id),
        name: format!("saved-{seed}").into(),
        query: ExploreQuery {
            dataset: ExploreDataset::Logs,
            from: at(0),
            until: at(1000),
            predicates: vec![],
            expression: Some(ExploreExpression::And(vec![])),
            aggregates: vec![],
            group_by: vec![],
            interval: None,
            cursor: None,
            limit: 50,
        },
        revision: 1,
        created_by: UserId::new(1).unwrap(),
        updated_by: UserId::new(1).unwrap(),
        created_at: at(100),
        updated_at: at(100),
    }
}
fn dashboard(seed: u8, project_id: i32) -> Dashboard {
    Dashboard {
        id: DashboardId::from_bytes([seed; 16]),
        project_id: project(project_id),
        name: format!("dashboard-{seed}").into(),
        widgets: vec![DashboardWidget {
            id: DashboardWidgetId::from_bytes([1; 16]),
            title: "Widget '; DROP TABLE metric.projects; -- 雪".into(),
            saved_query_id: SavedQueryId::from_bytes([1; 16]),
            shape: WidgetShape::Table,
        }],
        refresh_interval: DashboardRefreshInterval::Manual,
        revision: 1,
        created_by: UserId::new(1).unwrap(),
        updated_by: UserId::new(1).unwrap(),
        created_at: at(100),
        updated_at: at(100),
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresDashboardStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresDashboardStore::new(database.clone());
    (fixture, database, store)
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn scoped_crud_unique_names_and_dangling_widget_reference() {
    let (fixture, database, store) = setup().await;
    let mut first = saved(1, 1);
    first.name = "'; DROP TABLE metric.dashboard_configurations; -- 雪".into();
    store.insert_saved_query(first.clone()).await.unwrap();
    assert_eq!(
        store.load_saved_query(project(1), first.id).await.unwrap(),
        first
    );
    let mut foreign = first.clone();
    foreign.project_id = project(2);
    store.insert_saved_query(foreign.clone()).await.unwrap();
    assert_eq!(
        store.load_saved_query(project(2), first.id).await.unwrap(),
        foreign
    );
    let mut duplicate = first.clone();
    duplicate.id = SavedQueryId::from_bytes([2; 16]);
    assert_eq!(
        store.insert_saved_query(duplicate).await,
        Err(DashboardStoreError::Conflict)
    );
    assert_eq!(
        store.insert_saved_query(first.clone()).await,
        Err(DashboardStoreError::Conflict)
    );
    let mut dash = dashboard(1, 1);
    dash.name = first.name.clone();
    store.insert_dashboard(dash.clone()).await.unwrap();
    assert_eq!(
        store.load_dashboard(project(1), dash.id).await.unwrap(),
        dash
    );
    let mut new = first.clone();
    new.revision = 2;
    new.updated_at = at(200);
    new.updated_by = UserId::new(2).unwrap();
    new.name = "renamed".into();
    store.replace_saved_query(new.clone(), 1).await.unwrap();
    assert_eq!(
        store.load_saved_query(project(1), new.id).await.unwrap(),
        new
    );
    assert_eq!(
        store.replace_saved_query(first.clone(), 1).await,
        Err(DashboardStoreError::Conflict)
    );
    dash.revision = 2;
    dash.updated_at = at(200);
    dash.refresh_interval = DashboardRefreshInterval::FiveMinutes;
    dash.widgets.push(DashboardWidget {
        id: DashboardWidgetId::from_bytes([2; 16]),
        title: "Number".into(),
        saved_query_id: SavedQueryId::from_bytes([99; 16]),
        shape: WidgetShape::Number,
    });
    store.replace_dashboard(dash.clone(), 1).await.unwrap();
    assert_eq!(
        store.load_dashboard(project(1), dash.id).await.unwrap(),
        dash
    );
    store
        .delete_saved_query(project(1), first.id)
        .await
        .unwrap();
    assert_eq!(
        store.load_saved_query(project(1), first.id).await,
        Err(DashboardStoreError::NotFound)
    );
    assert_eq!(
        store.load_dashboard(project(1), dash.id).await.unwrap(),
        dash
    );
    assert_eq!(
        store.load_saved_query(project(2), first.id).await.unwrap(),
        foreign
    );
    assert_eq!(
        store.delete_saved_query(project(1), first.id).await,
        Err(DashboardStoreError::NotFound)
    );
    store.delete_dashboard(project(1), dash.id).await.unwrap();
    assert_eq!(
        store.load_dashboard(project(1), dash.id).await,
        Err(DashboardStoreError::NotFound)
    );
    assert_eq!(
        store.delete_dashboard(project(1), dash.id).await,
        Err(DashboardStoreError::NotFound)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_revision_cas_and_name_reservation_are_atomic() {
    let (fixture, database, store) = setup().await;
    let value = saved(1, 1);
    store.insert_saved_query(value.clone()).await.unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for id in 0..32 {
        let store = store.clone();
        let mut value = value.clone();
        value.revision = 2;
        value.updated_at = at(200);
        value.name = format!("winner-{id}").into();
        tasks.spawn(async move { store.replace_saved_query(value, 1).await });
    }
    let mut won = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(()) => won += 1,
            Err(DashboardStoreError::Conflict) => {}
            other => panic!("unexpected CAS result {other:?}"),
        }
    }
    assert_eq!(won, 1);
    assert_eq!(
        store
            .load_saved_query(project(1), value.id)
            .await
            .unwrap()
            .revision,
        2
    );
    for id in 2..=33 {
        let store = store.clone();
        let mut value = saved(id, 1);
        value.name = "reserved-name".into();
        tasks.spawn(async move { store.insert_saved_query(value).await });
    }
    let mut won = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(()) => won += 1,
            Err(DashboardStoreError::Conflict) => {}
            other => panic!("unexpected insert result {other:?}"),
        }
    }
    assert_eq!(won, 1);
    let value = dashboard(1, 1);
    store.insert_dashboard(value.clone()).await.unwrap();
    for id in 0..16 {
        let store = store.clone();
        let mut value = value.clone();
        value.revision = 2;
        value.updated_at = at(200);
        value.name = format!("dashboard-winner-{id}").into();
        tasks.spawn(async move { store.replace_dashboard(value, 1).await });
    }
    let mut won = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(()) => won += 1,
            Err(DashboardStoreError::Conflict) => {}
            other => panic!("unexpected dashboard result {other:?}"),
        }
    }
    assert_eq!(won, 1);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn query_types_expression_versions_and_maximum_depth_round_trip_losslessly() {
    let (fixture, database, store) = setup().await;
    for (index, dataset) in [
        ExploreDataset::Errors,
        ExploreDataset::Logs,
        ExploreDataset::Spans,
        ExploreDataset::Metrics,
    ]
    .into_iter()
    .enumerate()
    {
        let mut value = saved(index as u8 + 1, 1);
        value.query.dataset = dataset;
        value.query.predicates = vec![
            ExplorePredicate {
                field: ExploreField::Message,
                op: ExplorePredicateOp::Exact,
                value: Some(ExploreValue::String(
                    "message\0'; DROP TABLE metric.projects; -- 雪".into(),
                )),
                upper: None,
            },
            ExplorePredicate {
                field: ExploreField::MetricSum,
                op: ExplorePredicateOp::Range,
                value: Some(ExploreValue::Integer(i64::MIN)),
                upper: Some(ExploreValue::Integer(i64::MAX)),
            },
            ExplorePredicate {
                field: ExploreField::MetricMin,
                op: ExplorePredicateOp::Greater,
                value: Some(ExploreValue::Number(-0.125)),
                upper: None,
            },
            ExplorePredicate {
                field: ExploreField::IsSegment,
                op: ExplorePredicateOp::Exact,
                value: Some(ExploreValue::Bool(true)),
                upper: None,
            },
            ExplorePredicate {
                field: ExploreField::Name,
                op: ExplorePredicateOp::Exact,
                value: Some(ExploreValue::Null),
                upper: None,
            },
        ];
        value.query.expression = Some(ExploreExpression::Or(vec![
            ExploreExpression::And(
                value
                    .query
                    .predicates
                    .iter()
                    .cloned()
                    .map(ExploreExpression::Predicate)
                    .collect(),
            ),
            ExploreExpression::Not(Box::new(ExploreExpression::Predicate(
                value.query.predicates[0].clone(),
            ))),
        ]));
        value.query.aggregates = vec![
            ExploreAggregate {
                kind: ExploreAggregateKind::Count,
                field: None,
                alias: "count".into(),
            },
            ExploreAggregate {
                kind: ExploreAggregateKind::P99,
                field: Some(ExploreField::DurationMs),
                alias: "p99".into(),
            },
        ];
        value.query.group_by = vec![ExploreField::Level];
        value.query.interval = Some(ExploreInterval::Hour);
        store.insert_saved_query(value.clone()).await.unwrap();
        assert_eq!(
            store.load_saved_query(project(1), value.id).await.unwrap(),
            value
        );
        value.query.expression = None;
        value.revision = 2;
        value.updated_at = at(200);
        store.replace_saved_query(value.clone(), 1).await.unwrap();
        assert_eq!(
            store.load_saved_query(project(1), value.id).await.unwrap(),
            value
        );
    }
    let mut deep = saved(9, 1);
    let mut expression = ExploreExpression::Predicate(ExplorePredicate {
        field: ExploreField::Message,
        op: ExplorePredicateOp::Exact,
        value: Some(ExploreValue::String("deep".into())),
        upper: None,
    });
    for _ in 0..255 {
        expression = ExploreExpression::Not(Box::new(expression));
    }
    deep.query.expression = Some(expression);
    store.insert_saved_query(deep.clone()).await.unwrap();
    assert_eq!(
        store.load_saved_query(project(1), deep.id).await.unwrap(),
        deep
    );
    let mut largest = saved(10, 1);
    largest.query.expression = None;
    largest.query.predicates = vec![
        ExplorePredicate {
            field: ExploreField::Message,
            op: ExplorePredicateOp::Exact,
            value: Some(ExploreValue::String("\0".repeat(256).into())),
            upper: None
        };
        MAX_EXPLORE_PREDICATES
    ];
    store.insert_saved_query(largest.clone()).await.unwrap();
    assert_eq!(
        store
            .load_saved_query(project(1), largest.id)
            .await
            .unwrap(),
        largest
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn validation_and_project_fences_reject_without_side_effects() {
    let (fixture, database, store) = setup().await;
    let value = saved(1, 1);
    for case in 0..4 {
        let mut bad = value.clone();
        match case {
            0 => bad.name = "\0invalid".into(),
            1 => bad.name = "   ".into(),
            2 => bad.name = "a".repeat(121).into(),
            _ => bad.updated_at = at(99),
        }
        assert_eq!(
            store.insert_saved_query(bad).await,
            Err(DashboardStoreError::InvalidData)
        );
    }
    let mut bad = value.clone();
    bad.query.predicates = vec![ExplorePredicate {
        field: ExploreField::MetricSum,
        op: ExplorePredicateOp::Exact,
        value: Some(ExploreValue::Number(f64::NAN)),
        upper: None,
    }];
    assert_eq!(
        store.insert_saved_query(bad).await,
        Err(DashboardStoreError::InvalidData)
    );
    let mut bad = value.clone();
    bad.query.expression = Some(ExploreExpression::Or(vec![
        ExploreExpression::And(vec![]);
        MAX_EXPLORE_OR_ALTERNATIVES + 1
    ]));
    assert_eq!(
        store.insert_saved_query(bad).await,
        Err(DashboardStoreError::InvalidData)
    );
    let mut bad = value.clone();
    bad.query.cursor = Some(ExploreCursor {
        time: 0,
        id: [0; 20],
        id_len: 16,
    });
    assert_eq!(
        store.insert_saved_query(bad).await,
        Err(DashboardStoreError::InvalidData)
    );
    let mut bad = value.clone();
    bad.revision = u64::MAX;
    assert_eq!(
        store.insert_saved_query(bad).await,
        Err(DashboardStoreError::InvalidData)
    );
    let mut bad = dashboard(1, 1);
    bad.widgets = vec![bad.widgets[0].clone(); MAX_DASHBOARD_WIDGETS + 1];
    assert_eq!(
        store.insert_dashboard(bad).await,
        Err(DashboardStoreError::InvalidData)
    );
    for state in [2_i16, 3, 4] {
        sqlx::query("UPDATE metric.projects SET state=$1 WHERE id=1")
            .bind(state)
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            store.insert_saved_query(value.clone()).await,
            Err(DashboardStoreError::NotFound)
        );
        assert_eq!(
            store.insert_dashboard(dashboard(1, 1)).await,
            Err(DashboardStoreError::NotFound)
        );
    }
    sqlx::query("UPDATE metric.projects SET state=1 WHERE id=1")
        .execute(&database.pool)
        .await
        .unwrap();
    store.insert_saved_query(value.clone()).await.unwrap();
    assert_eq!(
        store.replace_saved_query(value.clone(), 0).await,
        Err(DashboardStoreError::InvalidData)
    );
    assert_eq!(
        store.replace_saved_query(saved(99, 1), 1).await,
        Err(DashboardStoreError::Conflict)
    );
    assert_eq!(
        store.load_saved_query(project(2), value.id).await,
        Err(DashboardStoreError::NotFound)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.dashboard_configurations")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn list_order_bounds_corruption_skipping_and_indexed_scope() {
    let (fixture, database, store) = setup().await;
    let first = saved(1, 1);
    let second = saved(2, 1);
    store.insert_saved_query(first.clone()).await.unwrap();
    store.insert_saved_query(second.clone()).await.unwrap();
    store.insert_saved_query(saved(1, 2)).await.unwrap();
    assert_eq!(
        store.list_saved_queries(project(1), 100).await.unwrap(),
        vec![second.clone(), first.clone()]
    );
    assert_eq!(
        store.list_saved_queries(project(1), 1).await.unwrap(),
        vec![second.clone()]
    );
    let first_dash = dashboard(1, 1);
    let second_dash = dashboard(2, 1);
    store.insert_dashboard(first_dash.clone()).await.unwrap();
    store.insert_dashboard(second_dash.clone()).await.unwrap();
    assert_eq!(
        store.list_dashboards(project(1), 100).await.unwrap(),
        vec![second_dash, first_dash]
    );
    for limit in [0, 101, usize::MAX] {
        assert_eq!(
            store.list_saved_queries(project(1), limit).await,
            Err(DashboardStoreError::InvalidData)
        );
        assert_eq!(
            store.list_dashboards(project(1), limit).await,
            Err(DashboardStoreError::InvalidData)
        );
    }
    let original:Vec<u8>=sqlx::query_scalar("SELECT body FROM metric.dashboard_configurations WHERE project_id=1 AND kind=0 AND configuration_id=$1").bind(second.id.as_bytes().as_slice()).fetch_one(&database.pool).await.unwrap();
    for body in [
        vec![2, 0, b'{', b'}'],
        vec![1, 1, 0, 0],
        codec::encode_json(&json!({"v":999}), 3, 64, MAX_BODY)
            .unwrap()
            .0,
    ] {
        sqlx::query("UPDATE metric.dashboard_configurations SET body=$1 WHERE project_id=1 AND kind=0 AND configuration_id=$2").bind(body).bind(second.id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
        assert_eq!(
            store.load_saved_query(project(1), second.id).await,
            Err(DashboardStoreError::InvalidData)
        );
        assert_eq!(
            store.list_saved_queries(project(1), 100).await.unwrap(),
            vec![first.clone()]
        );
        assert!(
            store
                .list_saved_queries(project(1), 1)
                .await
                .unwrap()
                .is_empty()
        );
    }
    let bomb = zstd::bulk::compress(&vec![b' '; MAX_BODY + 100], 3).unwrap();
    let mut body = vec![1, 1];
    body.extend(bomb);
    sqlx::query("UPDATE metric.dashboard_configurations SET body=$1 WHERE project_id=1 AND kind=0 AND configuration_id=$2").bind(body).bind(second.id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    assert_eq!(
        store.load_saved_query(project(1), second.id).await,
        Err(DashboardStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.dashboard_configurations SET body=$1 WHERE project_id=1 AND kind=0 AND configuration_id=$2").bind(original).bind(second.id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    sqlx::query("INSERT INTO metric.dashboard_configurations(project_id,kind,configuration_id,name,revision,created_by,updated_by,created_at,updated_at,body) SELECT 2,1,decode(lpad(to_hex(n),32,'0'),'hex'),'noise-'||n,1,1,1,0,n,$1 FROM generate_series(1,10000) n").bind(dashboard_configuration(&dashboard(1,1)).and_then(|value|codec::encode_json(&value.body,3,64,MAX_BODY).map(|value|value.0).map_err(invalid)).unwrap()).execute(&database.pool).await.unwrap();
    sqlx::query("ANALYZE metric.dashboard_configurations")
        .execute(&database.pool)
        .await
        .unwrap();
    let plan:Value=sqlx::query_scalar("EXPLAIN(ANALYZE,FORMAT JSON) SELECT configuration_id FROM metric.dashboard_configurations WHERE project_id=1 AND kind=1 ORDER BY updated_at DESC,configuration_id DESC LIMIT 10").fetch_one(&database.pool).await.unwrap();
    assert!(
        plan.to_string()
            .contains("dashboard_configuration_timeline")
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(2.0));
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_reply_and_failed_update_have_reviewable_durable_outcomes() {
    let (fixture, database, store) = setup().await;
    let value = saved(1, 1);
    store.insert_saved_query(value.clone()).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION metric.fail_configuration_update() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic configuration failure''; END'; CREATE TRIGGER fail_configuration_update BEFORE UPDATE ON metric.dashboard_configurations FOR EACH ROW EXECUTE FUNCTION metric.fail_configuration_update()").execute(&database.pool).await.unwrap();
    let mut next = value.clone();
    next.name = "new".into();
    next.revision = 2;
    next.updated_at = at(200);
    assert_eq!(
        store.replace_saved_query(next.clone(), 1).await,
        Err(DashboardStoreError::Unavailable)
    );
    assert_eq!(
        store.load_saved_query(project(1), value.id).await.unwrap(),
        value
    );
    sqlx::query("DROP TRIGGER fail_configuration_update ON metric.dashboard_configurations")
        .execute(&database.pool)
        .await
        .unwrap();
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxy_database = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied = PostgresDashboardStore::new(proxy_database.clone());
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.replace_saved_query(next.clone(), 1).await,
        Err(DashboardStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert_eq!(
        store.load_saved_query(project(1), value.id).await.unwrap(),
        next
    );
    assert_eq!(
        store.replace_saved_query(next.clone(), 1).await,
        Err(DashboardStoreError::Conflict)
    );
    proxy_database.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[test]
fn expression_decoder_rejects_cycles_orphans_aliasing_and_unknown_variants() {
    let valid = query_codec::encode(&saved(1, 1).query).unwrap();
    for expression in [
        json!([["not", [0]]]),
        json!([["and", []], ["and", []]]),
        json!([["and", [1, 1]], ["and", []]]),
        json!([["unknown", []]]),
    ] {
        let mut malformed = valid.clone();
        malformed["expression"] = expression;
        assert_eq!(
            query_codec::decode(&malformed),
            Err(DashboardStoreError::InvalidData)
        );
    }
}
