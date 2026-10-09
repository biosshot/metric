use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};
use metric_domain::{
    finalization::derive_environment_id,
    monitors::{
        MonitorConfig, MonitorSchedule, SealedUptimeHeaderValue, UptimeEndpoint, UptimeHeader,
        UptimeMethod, UptimeMonitorConfig,
    },
};
use std::sync::Arc;

fn time(v: i64) -> Timestamp {
    at(v).unwrap()
}
fn project(v: i32) -> ProjectId {
    ProjectId::new(v).unwrap()
}
fn definition(p: i32, slug: &str, uptime: bool) -> MonitorDefinition {
    let p = project(p);
    MonitorDefinition {
        id: if uptime {
            MonitorId::derive_uptime(p, slug, "production")
        } else {
            MonitorId::derive(p, slug, "production")
        },
        project_id: p,
        slug: slug.into(),
        name: "'; SELECT pg_sleep(999); -- Юникод".into(),
        environment_id: derive_environment_id(p, "production"),
        environment: "production".into(),
        enabled: true,
        managed_by_web: true,
        revision: 1,
        config: MonitorConfig {
            schedule: MonitorSchedule::interval(1).unwrap(),
            checkin_margin_seconds: 5,
            max_runtime_seconds: 30,
        },
        uptime: uptime.then(|| UptimeMonitorConfig {
            endpoint: UptimeEndpoint::new("http://example.com/health?q=';DROP%20TABLE").unwrap(),
            method: UptimeMethod::Get,
            expected_status_min: 200,
            expected_status_max: 299,
            timeout_seconds: 5,
            max_redirects: 2,
            headers: vec![UptimeHeader {
                name: "authorization".into(),
                value: SealedUptimeHeaderValue::new(vec![42; 64]).unwrap(),
                sensitive: true,
            }]
            .into_boxed_slice(),
        }),
        next_expected_at: time(60_000),
        last_run_id: None,
        last_status: None,
        last_check_in_at: None,
        created_at: time(0),
        updated_at: time(0),
    }
}
fn update(
    def: &MonitorDefinition,
    sequence: u128,
    started: i64,
    status: MonitorRunStatus,
) -> MonitorUpdate {
    let checkin = sequence.to_be_bytes();
    MonitorUpdate {
        definition: None,
        run: MonitorRun {
            id: MonitorRunId::sdk(def.id, checkin),
            project_id: def.project_id,
            monitor_id: def.id,
            check_in_id: Some(checkin),
            status,
            source: MonitorRunSource::Sdk,
            scheduled_for: None,
            started_at: time(started),
            finished_at: status.is_terminal().then(|| time(started + 1)),
            duration_ms: status.is_terminal().then_some(1),
            received_at: time(started + 1),
            release_id: None,
            timeout_at: None,
            delete_at: Some(time(86_400_000)),
            http_status: None,
            uptime_failure: None,
        },
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresMonitorStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store =
        PostgresMonitorStore::new(database.clone(), MonitorRetention { runs_days: 30 }).unwrap();
    (fixture, database, store)
}
async fn history(store: &PostgresMonitorStore, def: &MonitorDefinition) -> Vec<MonitorRun> {
    store
        .list_monitor_runs(def.project_id, def.id, None, None, None, 100_000)
        .await
        .unwrap()
        .items
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn shared_merge_order_and_native_constraint_keep_history_readable() {
    let (fixture, database, store) = setup().await;
    metric_testkit::monitor_conformance::terminal_merge_order(&store, project(1)).await;
    let error =
        sqlx::query("UPDATE metric.monitor_runs SET finished_at=started_at-1 WHERE status=1")
            .execute(&database.pool)
            .await
            .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    let id = MonitorId::derive(project(1), "merge-order-conformance", "production");
    assert_eq!(
        store
            .list_monitor_runs(project(1), id, None, None, None, 100)
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn scoped_crud_keysets_filters_and_atomic_terminal_lifecycle() {
    let (fixture, database, store) = setup().await;
    let first = definition(1, "one", false);
    let second = definition(1, "two", false);
    let foreign = definition(2, "one", false);
    for def in [&first, &second, &foreign] {
        assert_eq!(store.upsert_monitor(def.clone()).await.unwrap(), *def);
    }
    let page = store.list_monitors(project(1), None, 1).await.unwrap();
    let next = store.list_monitors(project(1), page.next, 1).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(next.items.len(), 1);
    assert_ne!(page.items[0].id, next.items[0].id);
    assert!(next.next.is_none());
    assert_eq!(
        store.load_monitor(project(2), first.id).await,
        Err(SignalStoreError::NotFound)
    );
    let mut stolen = first.clone();
    stolen.project_id = project(2);
    assert_eq!(
        store.upsert_monitor(stolen).await,
        Err(SignalStoreError::Conflict)
    );
    assert_eq!(
        store.delete_monitor(project(2), first.id).await,
        Err(SignalStoreError::NotFound)
    );
    let start = update(&first, 1, 1000, MonitorRunStatus::InProgress);
    let mut terminal = start.clone();
    terminal.run.status = MonitorRunStatus::Success;
    terminal.run.started_at = time(500);
    terminal.run.finished_at = Some(time(2000));
    terminal.run.received_at = time(2000);
    assert_eq!(
        store
            .persist_monitors(vec![
                start.clone(),
                start.clone(),
                terminal.clone(),
                terminal.clone()
            ])
            .await
            .unwrap(),
        vec![
            DurableOutcome::Accepted,
            DurableOutcome::Duplicate,
            DurableOutcome::Accepted,
            DurableOutcome::Duplicate
        ]
    );
    let mut expected = start.run.clone();
    expected.timeout_at = Some(time(31_000));
    expected.status = MonitorRunStatus::Success;
    expected.finished_at = terminal.run.finished_at;
    expected.received_at = terminal.run.received_at;
    assert_eq!(history(&store, &first).await, vec![expected.clone()]);
    let mut late = terminal.clone();
    late.run.status = MonitorRunStatus::Error;
    assert_eq!(
        store.persist_monitors(vec![late]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    assert_eq!(history(&store, &first).await, vec![expected]);
    let more: Vec<_> = (2..=5)
        .map(|i| update(&first, i, 1000, MonitorRunStatus::Success))
        .collect();
    store.persist_monitors(more).await.unwrap();
    let all = history(&store, &first).await;
    let mut paged = vec![];
    let mut before = None;
    loop {
        let page = store
            .list_monitor_runs(
                project(1),
                first.id,
                Some(time(1000)),
                Some(time(1000)),
                before,
                2,
            )
            .await
            .unwrap();
        paged.extend(page.items);
        before = page.next;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(paged, all);
    assert!(
        store
            .list_monitor_runs(project(2), first.id, None, None, None, 1)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let mut collision = update(&foreign, 100, 1000, MonitorRunStatus::Success);
    collision.run.id = start.run.id;
    assert_eq!(
        store.persist_monitors(vec![collision]).await,
        Err(SignalStoreError::Conflict)
    );
    let missing = definition(1, "missing", false);
    let original = store.load_monitor(project(1), first.id).await.unwrap();
    assert_eq!(
        store
            .persist_monitors(vec![
                update(&first, 99, 3000, MonitorRunStatus::Success),
                update(&missing, 1, 3000, MonitorRunStatus::Success)
            ])
            .await,
        Err(SignalStoreError::NotFound)
    );
    assert_eq!(history(&store, &first).await.len(), 5);
    assert_eq!(
        store.load_monitor(project(1), first.id).await.unwrap(),
        original
    );
    store.delete_monitor(project(1), first.id).await.unwrap();
    assert!(history(&store, &first).await.is_empty());
    assert_eq!(
        store.load_monitor(project(1), first.id).await,
        Err(SignalStoreError::NotFound)
    );
    assert!(store.load_monitor(project(2), foreign.id).await.is_ok());
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn sdk_definitions_web_precedence_sealed_headers_and_decode_budgets() {
    let (fixture, database, store) = setup().await;
    let mut sdk = definition(1, "sdk", false);
    sdk.managed_by_web = false;
    let mut admission = update(&sdk, 1, 1000, MonitorRunStatus::Success);
    admission.definition = Some(sdk.clone());
    store
        .persist_monitors(vec![admission.clone()])
        .await
        .unwrap();
    sdk.name = "sdk revision".into();
    sdk.created_at = time(900);
    sdk.updated_at = time(900);
    let mut refreshed = update(&sdk, 2, 2000, MonitorRunStatus::Success);
    refreshed.definition = Some(sdk.clone());
    store.persist_monitors(vec![refreshed]).await.unwrap();
    let stored = store.load_monitor(project(1), sdk.id).await.unwrap();
    assert_eq!(stored.revision, 2);
    assert_eq!(stored.created_at, time(0));
    assert_eq!(stored.name, sdk.name);
    let mut web = stored.clone();
    web.managed_by_web = true;
    web.name = "web-owned".into();
    web.revision = 3;
    store.upsert_monitor(web.clone()).await.unwrap();
    store.persist_monitors(vec![admission]).await.unwrap();
    assert_eq!(store.load_monitor(project(1), sdk.id).await.unwrap(), web);
    let uptime = definition(1, "uptime", true);
    store.upsert_monitor(uptime.clone()).await.unwrap();
    assert_eq!(
        store.load_monitor(project(1), uptime.id).await.unwrap(),
        uptime
    );
    let row: Vec<u8> = sqlx::query_scalar("SELECT config FROM metric.monitors WHERE monitor_id=$1")
        .bind(uptime.id.as_bytes().as_slice())
        .fetch_one(&database.pool)
        .await
        .unwrap();
    let decoded = crate::codec::decode_json(&row, MAX_CONFIG).unwrap();
    assert!(!String::from_utf8(decoded).unwrap().contains("Bearer "));
    let mut bad = uptime.clone();
    bad.name = "bad\0name".into();
    assert_eq!(
        store.upsert_monitor(bad).await,
        Err(SignalStoreError::InvalidData)
    );
    let mut bad = uptime.clone();
    bad.config.schedule = MonitorSchedule::Interval { minutes: 0 };
    assert_eq!(
        store.upsert_monitor(bad).await,
        Err(SignalStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.monitors SET config=$2 WHERE monitor_id=$1")
        .bind(uptime.id.as_bytes().as_slice())
        .bind(vec![2u8, 0, b'[', b']'])
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.load_monitor(project(1), uptime.id).await,
        Err(SignalStoreError::InvalidData)
    );
    assert!(store.load_monitor(project(2), uptime.id).await.is_err());
    store.upsert_monitor(uptime.clone()).await.unwrap_err();
    sqlx::query("UPDATE metric.monitors SET config=$2 WHERE monitor_id=$1")
        .bind(uptime.id.as_bytes().as_slice())
        .bind(row)
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.load_monitor(project(1), uptime.id).await.unwrap(),
        uptime
    );
    for limit in [0, 100_001] {
        assert_eq!(
            store.list_monitors(project(1), None, limit).await,
            Err(SignalStoreError::InvalidData)
        );
        assert_eq!(
            store
                .list_monitor_runs(project(1), sdk.id, None, None, None, limit)
                .await,
            Err(SignalStoreError::InvalidData)
        );
    }
    assert!(store.persist_monitors(vec![]).await.unwrap().is_empty());
    assert_eq!(
        store
            .persist_monitors(vec![
                update(&sdk, 1, 1000, MonitorRunStatus::Success);
                10_001
            ])
            .await,
        Err(SignalStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn deadline_workers_alerts_fences_and_retention_are_durable() {
    let (fixture, database, store) = setup().await;
    let cron = definition(1, "cron", false);
    store.upsert_monitor(cron.clone()).await.unwrap();
    let mut disabled = definition(1, "disabled", false);
    disabled.enabled = false;
    store.upsert_monitor(disabled.clone()).await.unwrap();
    let other = definition(2, "other", false);
    store.upsert_monitor(other.clone()).await.unwrap();
    store
        .persist_monitors(vec![
            update(&cron, 1, 1000, MonitorRunStatus::InProgress),
            update(&other, 1, 1000, MonitorRunStatus::InProgress),
        ])
        .await
        .unwrap();
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=2")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .terminalize_due_timeouts(time(30_999), 100)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .terminalize_due_timeouts(time(31_000), 100)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .terminalize_due_timeouts(time(31_000), 100)
            .await
            .unwrap(),
        0
    );
    let run = &history(&store, &cron).await[0];
    assert_eq!(run.status, MonitorRunStatus::Timeout);
    assert_eq!(run.duration_ms, Some(30_000));
    assert_eq!(run.finished_at, Some(time(31_000)));
    assert_eq!(
        history(&store, &other).await[0].status,
        MonitorRunStatus::InProgress
    );
    assert_eq!(
        store
            .materialize_due_missed(time(95_999), 100)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .materialize_due_missed(time(96_000), 100)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .materialize_due_missed(time(96_000), 100)
            .await
            .unwrap(),
        0
    );
    let alerts = store.pending_monitor_alerts(100).await.unwrap();
    assert_eq!(alerts.len(), 2);
    assert_eq!(alerts[0].status, MonitorRunStatus::Timeout);
    assert_eq!(alerts[1].status, MonitorRunStatus::Missed);
    assert_eq!(alerts[1].delete_at, Some(time(96_000 + 30 * 86_400_000)));
    store
        .mark_monitor_alert_evaluated(alerts[0].id, time(70_000))
        .await
        .unwrap();
    store
        .mark_monitor_alert_evaluated(alerts[0].id, time(71_000))
        .await
        .unwrap();
    assert_eq!(
        store.pending_monitor_alerts(100).await.unwrap(),
        vec![alerts[1].clone()]
    );
    assert_eq!(
        store
            .persist_monitors(vec![update(&other, 2, 80_000, MonitorRunStatus::Success)])
            .await,
        Err(SignalStoreError::NotFound)
    );
    assert_eq!(
        store
            .load_monitor(project(1), disabled.id)
            .await
            .unwrap()
            .last_run_id,
        None
    );
    for limit in [0, 10_001] {
        assert_eq!(
            store.terminalize_due_timeouts(time(0), limit).await,
            Err(SignalStoreError::InvalidData)
        );
        assert_eq!(
            store.materialize_due_missed(time(0), limit).await,
            Err(SignalStoreError::InvalidData)
        );
    }
    assert_eq!(
        store.pending_monitor_alerts(1001).await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store.claim_due_uptime(time(0), time(0), 1).await,
        Err(SignalStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_ingest_deadlines_and_leases_have_one_durable_owner() {
    let (fixture, database, store) = setup().await;
    let cron = definition(1, "race", false);
    store.upsert_monitor(cron.clone()).await.unwrap();
    let start = update(&cron, 1, 1000, MonitorRunStatus::InProgress);
    let (a, b) = tokio::join!(
        store.persist_monitors(vec![start.clone()]),
        store.persist_monitors(vec![start.clone()])
    );
    let outcomes = [a.unwrap()[0], b.unwrap()[0]];
    assert!(outcomes.contains(&DurableOutcome::Accepted));
    assert!(outcomes.contains(&DurableOutcome::Duplicate));
    let mut done = start.clone();
    done.run.status = MonitorRunStatus::Success;
    done.run.finished_at = Some(time(31_000));
    done.run.received_at = time(31_000);
    let (a, b) = tokio::join!(
        store.persist_monitors(vec![done]),
        store.terminalize_due_timeouts(time(31_000), 10)
    );
    assert!(a.is_ok());
    assert!(b.is_ok());
    assert_eq!(history(&store, &cron).await.len(), 1);
    assert!(history(&store, &cron).await[0].status.is_terminal());
    let (a, b) = tokio::join!(
        store.materialize_due_missed(time(96_000), 10),
        store.materialize_due_missed(time(96_000), 10)
    );
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    for i in 0..4 {
        store
            .upsert_monitor(definition(1, &format!("uptime-{i}"), true))
            .await
            .unwrap();
    }
    let (a, b) = tokio::join!(
        store.claim_due_uptime(time(60_000), time(80_000), 4),
        store.claim_due_uptime(time(60_000), time(80_000), 4)
    );
    let mut ids = a
        .unwrap()
        .into_iter()
        .chain(b.unwrap())
        .map(|d| d.id)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 4);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    assert!(
        store
            .claim_due_uptime(time(79_999), time(90_000), 4)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .claim_due_uptime(time(80_000), time(90_000), 4)
            .await
            .unwrap()
            .len(),
        4
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn lost_commit_reply_retry_and_second_chunk_failure_are_atomic() {
    let (fixture, database, store) = setup().await;
    let cron = definition(1, "atomic", false);
    store.upsert_monitor(cron.clone()).await.unwrap();
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxy_db = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied = PostgresMonitorStore::new(proxy_db.clone(), MonitorRetention::default()).unwrap();
    let first = update(&cron, 1, 1000, MonitorRunStatus::Success);
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.persist_monitors(vec![first.clone()]).await,
        Err(SignalStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert_eq!(history(&store, &cron).await.len(), 1);
    assert_eq!(
        proxied.persist_monitors(vec![first]).await.unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    let original = store.load_monitor(project(1), cron.id).await.unwrap();
    // Statement trigger fails only the second 1000-row run INSERT. All earlier
    // INSERTs and definition/projection changes belong to the same transaction.
    sqlx::raw_sql("CREATE SEQUENCE metric.monitor_fault; CREATE FUNCTION metric.fail_monitor_chunk() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN IF nextval(''metric.monitor_fault'')=2 THEN RAISE EXCEPTION ''synthetic chunk failure''; END IF; RETURN NULL; END'; CREATE TRIGGER fail_monitor_chunk BEFORE INSERT ON metric.monitor_runs FOR EACH STATEMENT EXECUTE FUNCTION metric.fail_monitor_chunk()").execute(&database.pool).await.unwrap();
    let batch: Vec<_> = (2..1003)
        .map(|i| update(&cron, i, i as i64, MonitorRunStatus::Success))
        .collect();
    assert_eq!(
        store.persist_monitors(batch.clone()).await,
        Err(SignalStoreError::Unavailable)
    );
    assert_eq!(history(&store, &cron).await.len(), 1);
    assert_eq!(
        store.load_monitor(project(1), cron.id).await.unwrap(),
        original
    );
    sqlx::query("DROP TRIGGER fail_monitor_chunk ON metric.monitor_runs")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.persist_monitors(batch).await.unwrap(),
        vec![DurableOutcome::Accepted; 1001]
    );
    assert_eq!(history(&store, &cron).await.len(), 1002);
    proxy_db.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

struct Executor;
impl metric_ports::UptimeCheckExecutor for Executor {
    fn execute(
        &self,
        monitor: MonitorDefinition,
    ) -> PortFuture<'_, Result<metric_ports::UptimeCheckResult>> {
        Box::pin(async move {
            assert_eq!(
                monitor.uptime.unwrap().headers[0].value.expose_ciphertext(),
                &[42; 64]
            );
            Ok(metric_ports::UptimeCheckResult {
                http_status: Some(200),
                duration_ms: 7,
                failure: None,
            })
        })
    }
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn existing_writer_and_uptime_scheduler_reuse_ports_and_release_lease() {
    use metric_application::{
        monitor_writer::{MonitorWriter, MonitorWriterConfig},
        shutdown::ShutdownRoot,
        uptime::{UptimeScheduler, UptimeSchedulerConfig},
    };
    use metric_ports::MonitorSink;
    let (fixture, database, store) = setup().await;
    let cron = definition(1, "writer", false);
    store.upsert_monitor(cron.clone()).await.unwrap();
    let root = ShutdownRoot::new();
    let shutdown = root.signal();
    let shared: Arc<dyn MonitorStore> = Arc::new(store.clone());
    let (writer, task) = MonitorWriter::start(
        shared.clone(),
        MonitorWriterConfig::default(),
        shutdown.clone(),
    )
    .unwrap();
    assert_eq!(
        writer
            .persist_monitors(vec![update(&cron, 1, 1000, MonitorRunStatus::Success)])
            .await
            .unwrap(),
        vec![DurableOutcome::Accepted]
    );
    root.begin();
    task.wait().await;
    let uptime = definition(1, "scheduler", true);
    store.upsert_monitor(uptime.clone()).await.unwrap();
    let scheduler = UptimeScheduler::new(
        shared,
        Arc::new(Executor),
        Arc::new(metric_testkit::FixedClock(time(60_000))),
        UptimeSchedulerConfig::default(),
    )
    .unwrap();
    assert_eq!(scheduler.run_once().await.unwrap(), 1);
    assert_eq!(scheduler.run_once().await.unwrap(), 0);
    let runs = history(&store, &uptime).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].http_status, Some(200));
    assert_eq!(runs[0].id, MonitorRunId::uptime(uptime.id, time(60_000)));
    assert_eq!(store.pending_monitor_alerts(100).await.unwrap(), runs);
    assert_eq!(
        store
            .claim_due_uptime(time(120_000), time(130_000), 10)
            .await
            .unwrap()
            .len(),
        1
    );
    database.close().await;
    fixture.cleanup().await;
}

#[test]
fn maximum_valid_uptime_configuration_roundtrips_without_exposing_headers() {
    let mut value = definition(1, "maximum", true);
    let config = value.uptime.as_mut().unwrap();
    config.headers = (0..16)
        .map(|i| UptimeHeader {
            name: format!("x-test-{i}").into(),
            value: SealedUptimeHeaderValue::new(vec![255; 560]).unwrap(),
            sensitive: false,
        })
        .collect();
    value.config.schedule = MonitorSchedule::crontab("0 0 * * *").unwrap();
    value.validate().unwrap();
    let (body, size) = encode_config(&value).unwrap();
    let (decoded, uptime) = codec::decode_config(&body, size as i32).unwrap();
    assert_eq!(decoded, value.config);
    assert_eq!(uptime, value.uptime);
    assert!(format!("{:?}", value.uptime).contains("<redacted>"));
    assert!(size < MAX_CONFIG);
    assert!(codec::decode_config(&body, size as i32 - 1).is_err());
    assert!(codec::decode_config(&[1, 1, 255, 0], 10).is_err());
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn native_page_budget_batch_limit_and_scheduler_indexes_are_bounded() {
    let (fixture, database, store) = setup().await;
    let mut large = definition(1, "large", true);
    large.uptime.as_mut().unwrap().headers = (0..16)
        .map(|i| UptimeHeader {
            name: format!("x-test-{i}").into(),
            value: SealedUptimeHeaderValue::new(vec![255; 560]).unwrap(),
            sensitive: false,
        })
        .collect();
    let values: Vec<_> = (0..260)
        .map(|i| {
            let mut def = large.clone();
            def.slug = format!("large-{i:04}").into();
            def.id = MonitorId::derive_uptime(project(1), &def.slug, "production");
            StoredMonitor {
                definition: def,
                lease: None,
            }
        })
        .collect();
    let mut connection = database.pool.acquire().await.unwrap();
    write_monitors(&mut connection, &values.iter().collect::<Vec<_>>())
        .await
        .unwrap();
    drop(connection);
    let page = store
        .list_monitors(project(1), None, 100_000)
        .await
        .unwrap();
    assert!(page.items.len() < 260);
    assert!(page.next.is_some());
    let tail = store
        .list_monitors(project(1), page.next, 100_000)
        .await
        .unwrap();
    assert!(tail.next.is_none());
    let ids: BTreeSet<_> = page.items.iter().chain(&tail.items).map(|d| d.id).collect();
    assert_eq!(ids.len(), 260);
    assert_eq!(page.items.len() + tail.items.len(), 260);
    let cron = definition(1, "bulk", false);
    store.upsert_monitor(cron.clone()).await.unwrap();
    let batch: Vec<_> = (0..10_000)
        .map(|i| {
            update(
                &cron,
                i,
                1000 + i as i64,
                if i % 2 == 0 {
                    MonitorRunStatus::InProgress
                } else {
                    MonitorRunStatus::Error
                },
            )
        })
        .collect();
    let started = std::time::Instant::now();
    assert_eq!(
        store.persist_monitors(batch).await.unwrap(),
        vec![DurableOutcome::Accepted; 10_000]
    );
    println!(
        "monitor 10000-row admission: {} ms",
        started.elapsed().as_millis()
    );
    assert_eq!(history(&store, &cron).await.len(), 10_000);
    // Representative extra future definitions keep selective due plans realistic.
    let values: Vec<_> = (0..3000)
        .map(|i| {
            let mut def = definition(1, &format!("future-{i}"), i % 2 == 0);
            def.next_expected_at = time(10_000_000);
            StoredMonitor {
                definition: def,
                lease: None,
            }
        })
        .collect();
    let mut connection = database.pool.acquire().await.unwrap();
    write_monitors(&mut connection, &values.iter().collect::<Vec<_>>())
        .await
        .unwrap();
    drop(connection);
    sqlx::raw_sql("ANALYZE metric.monitors; ANALYZE metric.monitor_runs")
        .execute(&database.pool)
        .await
        .unwrap();
    for (sql, index) in [
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT run_id FROM metric.monitor_runs WHERE project_id=1 AND monitor_id=$1 ORDER BY started_at DESC,run_id DESC LIMIT 10",
            "monitor_run_history",
        ),
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT run_id FROM metric.monitor_runs WHERE status=0 AND timeout_at<=32000 AND monitor_id=$1 ORDER BY timeout_at,run_id LIMIT 10",
            "monitor_run_timeout",
        ),
    ] {
        // Only fixed test SQL is selected. Reuse typed parameters on both shapes.
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(cron.id.as_bytes().as_slice())
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let plan: sqlx::types::Json<serde_json::Value> = row.get(0);
        assert!(plan.0.to_string().contains(index), "{}", plan.0);
    }
    for (sql, index) in [
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT monitor_id FROM metric.monitors WHERE kind=0 AND enabled AND due_at<=66000 ORDER BY due_at,monitor_id LIMIT 10",
            "monitor_cron_due",
        ),
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT monitor_id FROM metric.monitors WHERE kind=1 AND enabled AND due_at<=60000 AND (lease_until IS NULL OR lease_until<=60000) ORDER BY due_at,monitor_id LIMIT 10",
            "monitor_uptime_due",
        ),
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT run_id FROM metric.monitor_runs WHERE evaluated_at IS NULL AND (status IN(2,3,4) OR (status=1 AND http_status IS NOT NULL)) ORDER BY received_at,run_id LIMIT 10",
            "monitor_run_alerts",
        ),
    ] {
        let row = sqlx::query(sql).fetch_one(&database.pool).await.unwrap();
        let plan: sqlx::types::Json<serde_json::Value> = row.get(0);
        assert!(plan.0.to_string().contains(index), "{}", plan.0);
    }
    database.close().await;
    fixture.cleanup().await;
}
