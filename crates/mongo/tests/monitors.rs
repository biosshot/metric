use std::{
    error::Error,
    time::{SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use metric_domain::{
    ProjectId, SecretBytes, Timestamp,
    finalization::EnvironmentId,
    monitors::{
        MonitorConfig, MonitorDefinition, MonitorId, MonitorRun, MonitorRunId, MonitorRunSource,
        MonitorRunStatus, MonitorSchedule, MonitorUpdate, UptimeEndpoint, UptimeMethod,
        UptimeMonitorConfig,
    },
};
use metric_mongo::{MongoProjectStore, MonitorRetention};
use metric_ports::{DurableOutcome, MonitorStore};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn check_in_timeout_missed_and_compact_retention_are_durable() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "performance evidence; requires METRIC_TEST_MONGODB_URI"]
async fn durable_monitor_run_writer_reports_rps() {
    const RUNS: u128 = 2_000;
    const BATCH: usize = 200;

    let database = test_database().await.unwrap();
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await.unwrap();
    let store = control.monitor_store(MonitorRetention { runs_days: 30 });
    let project_id = ProjectId::new(42).unwrap();
    let base = timestamp(1_700_000_000_000);
    let monitor_id = MonitorId::derive(project_id, "load-test", "performance");
    store
        .upsert_monitor(definition(project_id, monitor_id, "load-test", base, 300))
        .await
        .unwrap();

    let started = std::time::Instant::now();
    for first in (0..RUNS).step_by(BATCH) {
        let updates = (first..(first + BATCH as u128).min(RUNS))
            .map(|sequence| {
                let check_in_id = sequence.to_be_bytes();
                let received_at = timestamp(base.unix_millis() + sequence as i64);
                MonitorUpdate {
                    definition: None,
                    run: MonitorRun {
                        id: MonitorRunId::sdk(monitor_id, check_in_id),
                        project_id,
                        monitor_id,
                        check_in_id: Some(check_in_id),
                        status: MonitorRunStatus::Success,
                        source: MonitorRunSource::Sdk,
                        scheduled_for: None,
                        started_at: received_at,
                        finished_at: Some(received_at),
                        duration_ms: Some(1),
                        received_at,
                        release_id: None,
                        timeout_at: None,
                        delete_at: Some(timestamp(received_at.unix_millis() + 30 * 86_400_000)),
                        http_status: None,
                        uptime_failure: None,
                    },
                }
            })
            .collect::<Vec<_>>();
        let outcomes = store.persist_monitors(updates).await.unwrap();
        assert_eq!(outcomes.len(), BATCH.min((RUNS - first) as usize));
        assert!(
            outcomes
                .iter()
                .all(|outcome| *outcome == DurableOutcome::Accepted)
        );
    }
    let elapsed = started.elapsed();
    let rps = RUNS as f64 / elapsed.as_secs_f64();
    println!(
        "{{\"runs\":{RUNS},\"batch\":{BATCH},\"elapsed_ms\":{},\"rps\":{rps:.2}}}",
        elapsed.as_millis()
    );
    assert!(rps > 100.0);
    database.drop().await.unwrap();
}

#[tokio::test]
#[ignore = "performance evidence; requires METRIC_TEST_MONGODB_URI"]
async fn durable_uptime_run_writer_reports_rps() {
    const RUNS: u128 = 2_000;
    const BATCH: usize = 200;

    let database = test_database().await.unwrap();
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await.unwrap();
    let store = control.monitor_store(MonitorRetention { runs_days: 30 });
    let project_id = ProjectId::new(42).unwrap();
    let base = timestamp(1_700_000_000_000);
    let monitor_id = MonitorId::derive_uptime(project_id, "uptime-load", "performance");
    let mut monitor = definition(project_id, monitor_id, "uptime-load", base, 10);
    monitor.uptime = Some(UptimeMonitorConfig {
        endpoint: UptimeEndpoint::new("https://example.com/health").unwrap(),
        method: UptimeMethod::Get,
        expected_status_min: 200,
        expected_status_max: 399,
        timeout_seconds: 10,
        max_redirects: 3,
        headers: Box::new([]),
    });
    store.upsert_monitor(monitor).await.unwrap();

    let started = std::time::Instant::now();
    for first in (0..RUNS).step_by(BATCH) {
        let updates = (first..(first + BATCH as u128).min(RUNS))
            .map(|sequence| {
                let received_at = timestamp(base.unix_millis() + sequence as i64);
                MonitorUpdate {
                    definition: None,
                    run: MonitorRun {
                        id: MonitorRunId::uptime(monitor_id, received_at),
                        project_id,
                        monitor_id,
                        check_in_id: None,
                        status: MonitorRunStatus::Success,
                        source: MonitorRunSource::Scheduler,
                        scheduled_for: Some(received_at),
                        started_at: received_at,
                        finished_at: Some(received_at),
                        duration_ms: Some(8),
                        received_at,
                        release_id: None,
                        timeout_at: None,
                        delete_at: Some(timestamp(received_at.unix_millis() + 30 * 86_400_000)),
                        http_status: Some(200),
                        uptime_failure: None,
                    },
                }
            })
            .collect::<Vec<_>>();
        let outcomes = store.persist_monitors(updates).await.unwrap();
        assert_eq!(outcomes.len(), BATCH.min((RUNS - first) as usize));
    }
    let elapsed = started.elapsed();
    let rps = RUNS as f64 / elapsed.as_secs_f64();
    println!(
        "{{\"runs\":{RUNS},\"batch\":{BATCH},\"elapsed_ms\":{},\"rps\":{rps:.2}}}",
        elapsed.as_millis()
    );
    assert!(rps > 100.0);
    database.drop().await.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = control.monitor_store(MonitorRetention { runs_days: 30 });
    let project_id = ProjectId::new(42)?;
    let base = timestamp(1_700_000_000_000);
    let monitor_id = MonitorId::derive(project_id, "nightly-backup", "production");
    let monitor = definition(project_id, monitor_id, "nightly-backup", base, 5);
    store
        .upsert_monitor(monitor)
        .await
        .map_err(|error| format!("initial monitor upsert failed: {error:?}"))?;

    let check_in_id = [3; 16];
    let run_id = MonitorRunId::sdk(monitor_id, check_in_id);
    let started = MonitorRun {
        id: run_id,
        project_id,
        monitor_id,
        check_in_id: Some(check_in_id),
        status: MonitorRunStatus::InProgress,
        source: MonitorRunSource::Sdk,
        scheduled_for: None,
        started_at: base,
        finished_at: None,
        duration_ms: None,
        received_at: base,
        release_id: None,
        timeout_at: None,
        delete_at: Some(timestamp(base.unix_millis() + 30 * 86_400_000)),
        http_status: None,
        uptime_failure: None,
    };
    assert_eq!(
        store
            .persist_monitors(vec![MonitorUpdate {
                definition: None,
                run: started.clone(),
            }])
            .await
            .map_err(|error| format!("in-progress persist failed: {error:?}"))?,
        vec![DurableOutcome::Accepted]
    );
    assert_eq!(
        store
            .terminalize_due_timeouts(timestamp(base.unix_millis() + 5_001), 100)
            .await
            .map_err(|error| format!("timeout terminalization failed: {error:?}"))?,
        1
    );
    let runs = store
        .list_monitor_runs(project_id, monitor_id, None, None, None, 100)
        .await?;
    assert_eq!(runs.items[0].status, MonitorRunStatus::Timeout);

    let batch_check_in_id = [4; 16];
    let batch_run_id = MonitorRunId::sdk(monitor_id, batch_check_in_id);
    let mut batch_started = started.clone();
    batch_started.id = batch_run_id;
    batch_started.check_in_id = Some(batch_check_in_id);
    let mut batch_finished = batch_started.clone();
    batch_finished.status = MonitorRunStatus::Success;
    batch_finished.finished_at = Some(timestamp(base.unix_millis() + 1_000));
    batch_finished.received_at = timestamp(base.unix_millis() + 1_000);
    batch_finished.duration_ms = Some(1_000);
    batch_finished.timeout_at = None;
    assert_eq!(
        store
            .persist_monitors(vec![
                MonitorUpdate {
                    definition: None,
                    run: batch_started,
                },
                MonitorUpdate {
                    definition: None,
                    run: batch_finished.clone(),
                },
            ])
            .await?,
        vec![DurableOutcome::Accepted, DurableOutcome::Accepted]
    );
    assert!(
        store
            .list_monitor_runs(project_id, monitor_id, None, None, None, 100)
            .await?
            .items
            .iter()
            .any(|run| run.id == batch_run_id && run.status == MonitorRunStatus::Success)
    );
    let mut late_error = batch_finished;
    late_error.status = MonitorRunStatus::Error;
    assert_eq!(
        store
            .persist_monitors(vec![MonitorUpdate {
                definition: None,
                run: late_error,
            }])
            .await?,
        vec![DurableOutcome::Duplicate]
    );

    let missed_id = MonitorId::derive(project_id, "hourly-cleanup", "production");
    let mut missed = definition(project_id, missed_id, "hourly-cleanup", base, 5);
    missed.next_expected_at = timestamp(base.unix_millis() + 60_000);
    store
        .upsert_monitor(missed)
        .await
        .map_err(|error| format!("missed monitor upsert failed: {error:?}"))?;
    assert_eq!(
        store
            .materialize_due_missed(timestamp(base.unix_millis() + 66_000), 100)
            .await
            .map_err(|error| format!("missed materialization failed: {error:?}"))?,
        1
    );
    let missed_runs = store
        .list_monitor_runs(project_id, missed_id, None, None, None, 100)
        .await?;
    assert_eq!(missed_runs.items.len(), 1);
    assert_eq!(missed_runs.items[0].status, MonitorRunStatus::Missed);
    assert_eq!(missed_runs.items[0].source, MonitorRunSource::Scheduler);

    let document = database
        .collection::<mongodb::bson::Document>("monitor_runs")
        .find_one(doc! { "_id": { "$exists": true } })
        .await?
        .unwrap();
    assert!(mongodb::bson::to_vec(&document)?.len() < 256);
    assert!(document.contains_key("x"));
    assert!(!document.contains_key("raw"));

    let indexes = database
        .collection::<mongodb::bson::Document>("monitor_runs")
        .list_indexes()
        .await?
        .collect::<Vec<_>>()
        .await;
    assert!(indexes.into_iter().any(|index| {
        index
            .ok()
            .and_then(|value| value.options)
            .and_then(|value| value.expire_after)
            .is_some()
    }));

    let outside_window = store
        .list_monitor_runs(
            project_id,
            monitor_id,
            Some(timestamp(base.unix_millis() + 1)),
            Some(timestamp(base.unix_millis() + 10_000)),
            None,
            100,
        )
        .await?;
    assert!(outside_window.items.is_empty());

    store.delete_monitor(project_id, monitor_id).await?;
    assert!(matches!(
        store.load_monitor(project_id, monitor_id).await,
        Err(metric_ports::SignalStoreError::NotFound)
    ));
    assert!(
        store
            .list_monitor_runs(project_id, monitor_id, None, None, None, 100)
            .await?
            .items
            .is_empty()
    );
    Ok(())
}

fn definition(
    project_id: ProjectId,
    monitor_id: MonitorId,
    slug: &str,
    now: Timestamp,
    max_runtime_seconds: u32,
) -> MonitorDefinition {
    MonitorDefinition {
        id: monitor_id,
        project_id,
        slug: slug.into(),
        name: slug.into(),
        environment_id: EnvironmentId::from_bytes([2; 16]),
        environment: "production".into(),
        enabled: true,
        managed_by_web: true,
        revision: 1,
        config: MonitorConfig {
            schedule: MonitorSchedule::interval(1).unwrap(),
            checkin_margin_seconds: 5,
            max_runtime_seconds,
        },
        uptime: None,
        next_expected_at: timestamp(now.unix_millis() + 60_000),
        last_run_id: None,
        last_status: None,
        last_check_in_at: None,
        created_at: now,
        updated_at: now,
    }
}

fn timestamp(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(client.database(&format!(
        "metric_phase35_monitors_{}_{}",
        std::process::id(),
        nonce
    )))
}
