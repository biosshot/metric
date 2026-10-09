//! Native PostgreSQL storage adapter. Driver types stay within this crate.
mod api;
mod auth;
mod codec;
mod connection;
mod dashboards;
mod error;
mod events;
mod explore;
mod feedback;
mod finalization;
mod issues;
mod metrics;
mod migrations;
mod monitors;
mod projects;
mod releases;
mod replays;
mod sessions;
mod signal_storage;
mod signals;

pub use api::PostgresInvestigationStore;
pub use auth::PostgresAuthStore;
pub use connection::{ConnectionPolicy, PostgresDatabase, SslMode};
pub use dashboards::PostgresDashboardStore;
pub use error::PostgresError;
pub use events::{EventCodecConfig, PostgresEventStore, PostgresPreparedEvent};
pub use explore::PostgresExploreStore;
pub use feedback::PostgresFeedbackStore;
pub use finalization::PostgresFinalizationStore;
pub use issues::{IssueCodecConfig, PostgresIssueStore};
pub use metrics::{MetricRetention, PostgresMetricStore};
pub use migrations::{MigrationMode, MigrationPolicy, PostgresSchemaManager, SchemaStatus};
pub use monitors::{MonitorRetention, PostgresMonitorStore};
pub use projects::PostgresProjectStore;
pub use releases::PostgresReleaseStore;
pub use replays::{PostgresReplayStore, ReplayRetention};
pub use sessions::{PostgresSessionStore, SessionRetention};
pub use signals::{PostgresSignalStore, SignalRetention};

#[cfg(test)]
mod test_support;
