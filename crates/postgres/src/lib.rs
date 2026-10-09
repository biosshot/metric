//! Native PostgreSQL storage adapter. Driver types stay within this crate.
mod api;
mod auth;
mod codec;
mod connection;
mod dashboards;
mod error;
mod events;
mod finalization;
mod issues;
mod migrations;
mod projects;
mod releases;
mod replays;
mod sessions;

pub use api::PostgresInvestigationStore;
pub use auth::PostgresAuthStore;
pub use connection::{ConnectionPolicy, PostgresDatabase, SslMode};
pub use dashboards::PostgresDashboardStore;
pub use error::PostgresError;
pub use events::{EventCodecConfig, PostgresEventStore, PostgresPreparedEvent};
pub use finalization::PostgresFinalizationStore;
pub use issues::{IssueCodecConfig, PostgresIssueStore};
pub use migrations::{MigrationMode, MigrationPolicy, PostgresSchemaManager, SchemaStatus};
pub use projects::PostgresProjectStore;
pub use releases::PostgresReleaseStore;
pub use replays::{PostgresReplayStore, ReplayRetention};
pub use sessions::{PostgresSessionStore, SessionRetention};

#[cfg(test)]
mod test_support;
