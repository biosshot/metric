//! Shared real-adapter lifecycle checks; current synthetic dates avoid TTL races.
use metric_domain::{
    ProjectId, Timestamp,
    finalization::derive_environment_id,
    monitors::{
        MonitorConfig, MonitorDefinition, MonitorId, MonitorRun, MonitorRunId, MonitorRunSource,
        MonitorRunStatus, MonitorSchedule, MonitorUpdate,
    },
};
use metric_ports::{DurableOutcome, MonitorStore, SignalStoreError};
use std::time::{SystemTime, UNIX_EPOCH};

pub async fn terminal_merge_order(store: &dyn MonitorStore, project: ProjectId) {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let at = |value| Timestamp::from_unix_millis(value).unwrap();
    let monitor = MonitorId::derive(project, "merge-order-conformance", "production");
    store
        .upsert_monitor(MonitorDefinition {
            id: monitor,
            project_id: project,
            slug: "merge-order-conformance".into(),
            name: "Synthetic merge ordering".into(),
            environment_id: derive_environment_id(project, "production"),
            environment: "production".into(),
            enabled: true,
            managed_by_web: true,
            revision: 1,
            config: MonitorConfig {
                schedule: MonitorSchedule::interval(1).unwrap(),
                checkin_margin_seconds: 5,
                max_runtime_seconds: 30,
            },
            uptime: None,
            next_expected_at: at(now + 60_000),
            last_run_id: None,
            last_status: None,
            last_check_in_at: None,
            created_at: at(now),
            updated_at: at(now),
        })
        .await
        .unwrap();
    let first = MonitorRun {
        id: MonitorRunId::sdk(monitor, [1; 16]),
        project_id: project,
        monitor_id: monitor,
        check_in_id: Some([1; 16]),
        status: MonitorRunStatus::InProgress,
        source: MonitorRunSource::Sdk,
        scheduled_for: None,
        started_at: at(now),
        finished_at: None,
        duration_ms: None,
        received_at: at(now),
        release_id: None,
        timeout_at: None,
        delete_at: Some(at(now + 30 * 86_400_000)),
        http_status: None,
        uptime_failure: None,
    };
    let update = |run| MonitorUpdate {
        definition: None,
        run,
    };
    assert_eq!(
        store
            .persist_monitors(vec![update(first.clone())])
            .await
            .unwrap(),
        vec![DurableOutcome::Accepted]
    );
    let before = store
        .list_monitor_runs(project, monitor, None, None, None, 100)
        .await
        .unwrap();
    let mut invalid = first.clone();
    invalid.status = MonitorRunStatus::Success;
    invalid.started_at = at(now - 2000);
    invalid.finished_at = Some(at(now - 1000));
    invalid.received_at = at(now - 1000);
    invalid.duration_ms = Some(1000);
    assert!(update(invalid.clone()).validate().is_ok());
    assert_eq!(
        store.persist_monitors(vec![update(invalid.clone())]).await,
        Err(SignalStoreError::InvalidData)
    );
    assert_eq!(
        store
            .list_monitor_runs(project, monitor, None, None, None, 100)
            .await
            .unwrap(),
        before
    );
    let mut valid = invalid.clone();
    valid.finished_at = Some(at(now + 2000));
    valid.received_at = at(now + 2000);
    valid.duration_ms = Some(4000);
    assert_eq!(
        store
            .persist_monitors(vec![update(valid.clone())])
            .await
            .unwrap(),
        vec![DurableOutcome::Accepted]
    );
    let completed = store
        .list_monitor_runs(project, monitor, None, None, None, 100)
        .await
        .unwrap();
    assert_eq!(completed.items[0].started_at, first.started_at);
    assert_eq!(completed.items[0].finished_at, valid.finished_at);
    assert_eq!(completed.items[0].status, MonitorRunStatus::Success);
    assert_eq!(
        store
            .persist_monitors(vec![update(invalid.clone())])
            .await
            .unwrap(),
        vec![DurableOutcome::Duplicate]
    );
    assert_eq!(
        store
            .list_monitor_runs(project, monitor, None, None, None, 100)
            .await
            .unwrap(),
        completed
    );
    // The same invalid transition inside one batch must not admit its initial run.
    let mut batch_first = first.clone();
    batch_first.id = MonitorRunId::sdk(monitor, [2; 16]);
    batch_first.check_in_id = Some([2; 16]);
    invalid.id = batch_first.id;
    invalid.check_in_id = batch_first.check_in_id;
    assert_eq!(
        store
            .persist_monitors(vec![update(batch_first.clone()), update(invalid)])
            .await,
        Err(SignalStoreError::InvalidData)
    );
    assert!(
        !store
            .list_monitor_runs(project, monitor, None, None, None, 100)
            .await
            .unwrap()
            .items
            .iter()
            .any(|run| run.id == batch_first.id)
    );
    let mut equal = batch_first.clone();
    equal.status = MonitorRunStatus::Success;
    equal.started_at = at(now - 1);
    equal.finished_at = Some(at(now));
    equal.duration_ms = Some(1);
    assert_eq!(
        store
            .persist_monitors(vec![update(batch_first.clone()), update(equal)])
            .await
            .unwrap(),
        vec![DurableOutcome::Accepted; 2]
    );
    let run = store
        .list_monitor_runs(project, monitor, None, None, None, 100)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|run| run.id == batch_first.id)
        .unwrap();
    assert_eq!(run.started_at, run.finished_at.unwrap());
}
