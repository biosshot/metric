//! Storage objects used by the shared service assembly.
use crate::{MongoReadiness, config::AppConfig, http::DependencyReadiness};
use metric_mongo::{EventCodecConfig, IssueCodecConfig, MongoEventStore, MongoProjectStore};
use std::sync::Arc;
pub(super) struct StorageBundle<E> {
    pub(super) events: Arc<E>,
    pub(super) readiness: Arc<dyn DependencyReadiness>,
    pub(super) notification: Arc<dyn metric_ports::NotificationStore>,
    pub(super) auth: Arc<dyn metric_ports::AuthStore>,
    pub(super) archive: Arc<dyn metric_ports::ArchiveStore>,
    pub(super) issues: Arc<dyn metric_ports::IssueStore>,
    pub(super) investigation: Arc<dyn metric_ports::InvestigationStore>,
    pub(super) releases: Arc<dyn metric_ports::ReleaseStore>,
    pub(super) signals: Arc<dyn metric_ports::SignalStore>,
    pub(super) metrics: Arc<dyn metric_ports::MetricStore>,
    pub(super) replays: Arc<dyn metric_ports::ReplayStore>,
    pub(super) sessions: Arc<dyn metric_ports::SessionStore>,
    pub(super) monitors: Arc<dyn metric_ports::MonitorStore>,
    pub(super) explore: Arc<dyn metric_ports::ExploreStore>,
    pub(super) dashboards: Arc<dyn metric_ports::DashboardStore>,
    pub(super) debug_files: Arc<dyn metric_ports::DebugFileStore>,
    pub(super) artifacts: Arc<dyn metric_ports::ArtifactStore>,
    pub(super) finalization: Arc<dyn metric_ports::FinalizationStore>,
    pub(super) maintenance: Arc<dyn metric_ports::MaintenanceStore>,
    pub(super) projects: Arc<dyn metric_ports::ProjectStore>,
    pub(super) deletion: Arc<dyn metric_ports::ProjectDeletionStore>,
    pub(super) feedback_sink: Arc<dyn metric_ports::FeedbackSink>,
    pub(super) feedback: Arc<dyn metric_ports::FeedbackStore>,
}
impl StorageBundle<MongoEventStore> {
    pub(super) fn from_mongo(store: MongoProjectStore, config: &AppConfig) -> Self {
        let event_codec = EventCodecConfig {
            compression_level: config.ingest.event_codec.compression_level,
            compression_min_savings: config.ingest.event_codec.compression_min_savings,
            max_decoded_body_bytes: config.ingest.max_event_bytes,
            max_encoded_document_bytes: config.ingest.max_event_bytes.saturating_add(64 * 1024),
        };
        let issue_codec = IssueCodecConfig::default();
        let feedback = Arc::new(store.feedback_store());
        Self {
            projects: Arc::new(store.clone()),
            deletion: Arc::new(store.clone()),
            events: Arc::new(store.event_store(event_codec)),
            readiness: Arc::new(MongoReadiness {
                store: store.clone(),
                timeout: config
                    .server
                    .request_timeout
                    .get()
                    .min(std::time::Duration::from_secs(2)),
            }),
            feedback_sink: feedback.clone(),
            feedback,
            notification: std::sync::Arc::new(store.notification_store()),
            auth: std::sync::Arc::new(store.auth_store()),
            archive: std::sync::Arc::new(store.archive_store(event_codec)),
            issues: std::sync::Arc::new(store.issue_store(issue_codec)),
            investigation: std::sync::Arc::new(store.investigation_store(event_codec, issue_codec)),
            releases: std::sync::Arc::new(store.release_store(issue_codec)),
            signals: std::sync::Arc::new(store.signal_store_with_retention(
                metric_mongo::SignalRetention {
                    logs_days: config.retention.logs_days,
                    spans_days: config.retention.spans_days,
                    span_stats_hourly_days: config.retention.span_stats_hourly_days,
                    archive: config.archive.enabled,
                },
            )),
            metrics: std::sync::Arc::new(store.metric_store(metric_mongo::MetricRetention {
                days: config.retention.metrics_days,
                max_series_per_project: config.retention.metric_max_series_per_project,
                archive: config.archive.enabled && config.retention.metric_archive,
            })),
            replays: std::sync::Arc::new(store.replay_store(metric_mongo::ReplayRetention {
                days: config.retention.replays_days,
                archive: config.retention.replay_archive,
            })),
            sessions: std::sync::Arc::new(store.session_store(metric_mongo::SessionRetention {
                sessions_days: config.retention.sessions_days,
                session_stats_hourly_days: config.retention.session_stats_hourly_days,
                archive: config.archive.enabled,
            })),
            monitors: std::sync::Arc::new(store.monitor_store(metric_mongo::MonitorRetention {
                runs_days: config.retention.monitor_runs_days,
            })),
            explore: std::sync::Arc::new(store.explore_store()),
            dashboards: std::sync::Arc::new(store.dashboard_store()),
            debug_files: std::sync::Arc::new(
                store.debug_file_store(metric_mongo::DebugFileQuota::default()),
            ),
            artifacts: std::sync::Arc::new(store.artifact_store(metric_mongo::ArtifactQuota {
                maximum_bytes_per_organization: config.artifacts.maximum_bytes_per_organization,
                maximum_bundles_per_organization: config.artifacts.maximum_bundles_per_organization,
            })),
            finalization: std::sync::Arc::new(store.finalization_store(event_codec, issue_codec)),
            maintenance: std::sync::Arc::new(store.maintenance_store()),
        }
    }
}
