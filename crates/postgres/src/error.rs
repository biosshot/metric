use thiserror::Error;

/// Redacted boundary errors: never retain a driver error, URI, query or row value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PostgresError {
    #[error("invalid PostgreSQL connection URI")]
    InvalidUri,
    #[error("unsupported or conflicting PostgreSQL URI option")]
    UriPolicyConflict,
    #[error("invalid PostgreSQL pool or timeout settings")]
    InvalidPolicy,
    #[error("PostgreSQL insecure connection requires explicit acknowledgement")]
    InsecureConnectionNotAcknowledged,
    #[error("PostgreSQL is temporarily unavailable")]
    Unavailable,
    #[error("PostgreSQL operation exceeded its deadline")]
    Timeout,
    #[error("PostgreSQL permission denied")]
    PermissionDenied,
    #[error("PostgreSQL integrity constraint rejected the operation")]
    Constraint,
    #[error("PostgreSQL schema is not recognized")]
    UnknownSchema,
    #[error("PostgreSQL schema requires migration")]
    MigrationRequired,
    #[error("PostgreSQL schema is newer than this application")]
    NewerSchema,
    #[error("PostgreSQL migration checksum or registry does not match")]
    MigrationMismatch,
    #[error("PostgreSQL migration ownership is busy")]
    MigrationBusy,
    #[error("PostgreSQL stored data is invalid")]
    InvalidData,
}

impl From<sqlx::Error> for PostgresError {
    fn from(error: sqlx::Error) -> Self {
        match error {
            sqlx::Error::PoolTimedOut => Self::Timeout,
            sqlx::Error::Database(ref database) => match database.code().as_deref() {
                Some("57014" | "55P03") => Self::Timeout,
                Some("42501") => Self::PermissionDenied,
                Some(code) if code.starts_with("23") => Self::Constraint,
                _ => Self::Unavailable,
            },
            sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => Self::InvalidData,
            _ => Self::Unavailable,
        }
    }
}
