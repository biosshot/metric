//! Native PostgreSQL storage adapter. Driver types stay within this crate.
mod auth;
mod connection;
mod error;
mod events;
mod migrations;
mod projects;

pub use auth::PostgresAuthStore;
pub use connection::{ConnectionPolicy, PostgresDatabase, SslMode};
pub use error::PostgresError;
pub use events::{EventCodecConfig, PostgresEventStore, PostgresPreparedEvent};
pub use migrations::{MigrationMode, MigrationPolicy, PostgresSchemaManager, SchemaStatus};
pub use projects::PostgresProjectStore;

#[cfg(test)]
mod test_support;
