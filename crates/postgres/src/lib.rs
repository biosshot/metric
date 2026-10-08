//! Native PostgreSQL storage adapter. Driver types stay within this crate.
mod connection;
mod error;
mod migrations;
mod projects;

pub use connection::{ConnectionPolicy, PostgresDatabase, SslMode};
pub use error::PostgresError;
pub use migrations::{MigrationMode, MigrationPolicy, PostgresSchemaManager, SchemaStatus};
pub use projects::PostgresProjectStore;

#[cfg(test)]
mod test_support;
