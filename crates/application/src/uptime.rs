//! Isolated, bounded Uptime scheduling. It does not share queues or permits with ingest.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures_util::{StreamExt, stream};
use metric_domain::{
    Timestamp,
    monitors::{
        MonitorDefinition, MonitorRun, MonitorRunId, MonitorRunSource, MonitorRunStatus,
        MonitorUpdate,
    },
};
use metric_ports::{Clock, MonitorStore, SignalStoreError, UptimeCheckExecutor};
use thiserror::Error;
use tokio::{
    sync::Semaphore,
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};

use crate::shutdown::ShutdownSignal;

const DAY_MILLIS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy)]
pub struct UptimeSchedulerConfig {
    pub poll_interval: Duration,
    pub lease: Duration,
    pub batch_size: usize,
    pub global_concurrency: usize,
    pub per_host_concurrency: usize,
    pub retention_days: u32,
}

impl Default for UptimeSchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            lease: Duration::from_secs(180),
            batch_size: 100,
            global_concurrency: 16,
            per_host_concurrency: 2,
            retention_days: 90,
        }
    }
}

#[derive(Debug, Error)]
pub enum UptimeSchedulerError {
    #[error("invalid Uptime scheduler configuration")]
    InvalidConfiguration,
    #[error("Uptime storage is unavailable")]
    Storage,
}

pub struct UptimeScheduler {
    store: Arc<dyn MonitorStore>,
    executor: Arc<dyn UptimeCheckExecutor>,
    clock: Arc<dyn Clock>,
    config: UptimeSchedulerConfig,
}

impl UptimeScheduler {
    pub fn new(
        store: Arc<dyn MonitorStore>,
        executor: Arc<dyn UptimeCheckExecutor>,
        clock: Arc<dyn Clock>,
        config: UptimeSchedulerConfig,
    ) -> Result<Arc<Self>, UptimeSchedulerError> {
        if config.poll_interval.is_zero()
            || config.lease <= config.poll_interval
            || !(1..=1_000).contains(&config.batch_size)
            || !(1..=256).contains(&config.global_concurrency)
            || !(1..=32).contains(&config.per_host_concurrency)
            || config.retention_days == 0
        {
            return Err(UptimeSchedulerError::InvalidConfiguration);
        }
        Ok(Arc::new(Self {
            store,
            executor,
            clock,
            config,
        }))
    }

    pub async fn run_once(&self) -> Result<usize, UptimeSchedulerError> {
        let now = self.clock.now();
        let lease_until = add_duration(now, self.config.lease);
        let claimed = self
            .store
            .claim_due_uptime(now, lease_until, self.config.batch_size)
            .await
            .map_err(|_| UptimeSchedulerError::Storage)?;
        if claimed.is_empty() {
            return Ok(0);
        }
        let claimed = fair_by_host(claimed);
        let mut host_limits = BTreeMap::new();
        for monitor in &claimed {
            let host = monitor
                .uptime
                .as_ref()
                .and_then(|config| config.endpoint.host_key().ok())
                .ok_or(UptimeSchedulerError::Storage)?;
            host_limits
                .entry(host)
                .or_insert_with(|| Arc::new(Semaphore::new(self.config.per_host_concurrency)));
        }
        let completed = stream::iter(claimed.into_iter().map(|monitor| {
            let executor = Arc::clone(&self.executor);
            let permit = Arc::clone(
                host_limits
                    .get(
                        &monitor
                            .uptime
                            .as_ref()
                            .expect("claimed Uptime monitor has config")
                            .endpoint
                            .host_key()
                            .expect("validated endpoint"),
                    )
                    .expect("host semaphore exists"),
            );
            async move {
                let _permit = permit
                    .acquire_owned()
                    .await
                    .map_err(|_| SignalStoreError::Unavailable)?;
                let result = executor.execute(monitor.clone()).await?;
                Ok::<_, SignalStoreError>((monitor, result))
            }
        }))
        .buffer_unordered(self.config.global_concurrency)
        .collect::<Vec<_>>()
        .await;
        let finished_at = self.clock.now();
        let delete_at = add_days(finished_at, self.config.retention_days)
            .ok_or(UptimeSchedulerError::Storage)?;
        let mut updates = Vec::with_capacity(completed.len());
        for completed in completed {
            let (monitor, result) = completed.map_err(|_| UptimeSchedulerError::Storage)?;
            let status = if result.failure.is_none() {
                MonitorRunStatus::Success
            } else if result.failure == Some(metric_domain::monitors::UptimeFailure::Timeout) {
                MonitorRunStatus::Timeout
            } else {
                MonitorRunStatus::Error
            };
            updates.push(MonitorUpdate {
                definition: None,
                run: MonitorRun {
                    id: MonitorRunId::uptime(monitor.id, monitor.next_expected_at),
                    project_id: monitor.project_id,
                    monitor_id: monitor.id,
                    check_in_id: None,
                    status,
                    source: MonitorRunSource::Scheduler,
                    scheduled_for: Some(monitor.next_expected_at),
                    started_at: now,
                    finished_at: Some(finished_at),
                    duration_ms: Some(result.duration_ms),
                    received_at: finished_at,
                    release_id: None,
                    timeout_at: None,
                    delete_at: Some(delete_at),
                    http_status: result.http_status,
                    uptime_failure: result.failure,
                },
            });
        }
        let count = updates.len();
        self.store
            .persist_monitors(updates)
            .await
            .map_err(|_| UptimeSchedulerError::Storage)?;
        Ok(count)
    }

    pub fn start(self: Arc<Self>, shutdown: ShutdownSignal) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(self.config.poll_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    _ = ticker.tick() => {
                        if self.run_once().await.is_err() {
                            metrics::counter!("metric_uptime_checks_total", "outcome" => "scheduler_error").increment(1);
                        }
                    }
                }
            }
        })
    }
}

fn fair_by_host(monitors: Vec<MonitorDefinition>) -> Vec<MonitorDefinition> {
    let mut groups = BTreeMap::<Box<str>, std::collections::VecDeque<_>>::new();
    for monitor in monitors {
        if let Some(host) = monitor
            .uptime
            .as_ref()
            .and_then(|config| config.endpoint.host_key().ok())
        {
            groups.entry(host).or_default().push_back(monitor);
        }
    }
    let mut result = Vec::new();
    while !groups.is_empty() {
        groups.retain(|_, monitors| {
            if let Some(monitor) = monitors.pop_front() {
                result.push(monitor);
            }
            !monitors.is_empty()
        });
    }
    result
}

fn add_duration(value: Timestamp, duration: Duration) -> Timestamp {
    let millis = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    Timestamp::from_unix_millis(value.unix_millis().saturating_add(millis)).unwrap_or(value)
}

fn add_days(value: Timestamp, days: u32) -> Option<Timestamp> {
    Timestamp::from_unix_millis(
        value
            .unix_millis()
            .checked_add(i64::from(days).checked_mul(DAY_MILLIS)?)?,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use metric_domain::{
        ProjectId,
        finalization::EnvironmentId,
        monitors::{
            MonitorConfig, MonitorId, MonitorSchedule, UptimeEndpoint, UptimeMethod,
            UptimeMonitorConfig,
        },
    };

    use super::*;

    fn monitor(slug: &str, endpoint: &str) -> MonitorDefinition {
        let now = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        let project_id = ProjectId::new(7).unwrap();
        MonitorDefinition {
            id: MonitorId::derive_uptime(project_id, slug, "production"),
            project_id,
            slug: slug.into(),
            name: slug.into(),
            environment_id: EnvironmentId::from_bytes([1; 16]),
            environment: "production".into(),
            enabled: true,
            managed_by_web: true,
            revision: 1,
            config: MonitorConfig {
                schedule: MonitorSchedule::interval(1).unwrap(),
                checkin_margin_seconds: 0,
                max_runtime_seconds: 10,
            },
            uptime: Some(UptimeMonitorConfig {
                endpoint: UptimeEndpoint::new(endpoint).unwrap(),
                method: UptimeMethod::Get,
                expected_status_min: 200,
                expected_status_max: 399,
                timeout_seconds: 10,
                max_redirects: 3,
                headers: Box::new([]),
            }),
            next_expected_at: now,
            last_run_id: None,
            last_status: None,
            last_check_in_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn host_fairness_is_round_robin_and_restart_id_is_deterministic() {
        let scheduled = Timestamp::from_unix_millis(1_700_000_000_000).unwrap();
        let a1 = monitor("a-1", "https://a.example/health");
        let a2 = monitor("a-2", "https://a.example/health");
        let b1 = monitor("b-1", "https://b.example/health");
        let ordered = fair_by_host(vec![a1.clone(), a2, b1]);
        assert_ne!(
            ordered[0]
                .uptime
                .as_ref()
                .unwrap()
                .endpoint
                .host_key()
                .unwrap(),
            ordered[1]
                .uptime
                .as_ref()
                .unwrap()
                .endpoint
                .host_key()
                .unwrap()
        );
        assert_eq!(
            MonitorRunId::uptime(a1.id, scheduled),
            MonitorRunId::uptime(a1.id, scheduled)
        );
    }
}
