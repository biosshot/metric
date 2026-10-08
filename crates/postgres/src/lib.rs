//! Native PostgreSQL storage adapter. Driver types stay within this crate.
mod connection;
mod error;
mod migrations;

pub use connection::{ConnectionPolicy, PostgresDatabase, SslMode};
pub use error::PostgresError;
pub use migrations::{MigrationMode, MigrationPolicy, PostgresSchemaManager, SchemaStatus};

#[cfg(test)]
mod test_support;
