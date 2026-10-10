use thiserror::Error;

/// Redacted boundary errors: never retain a driver error, URI, query or row value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PostgresError {
    #[error("invalid PostgreSQL connection URI")]
    InvalidUri,
    #[error("unsupported or conflicting PostgreSQL URI option")]
    UriPolicyConflict,
    #[error("invalid PostgreSQL connection, pool or timeout settings")]
    InvalidPolicy,
    #[error("PostgreSQL insecure connection requires explicit acknowledgement")]
    InsecureConnectionNotAcknowledged,
    #[error("PostgreSQL TLS negotiation or certificate verification failed")]
    TlsFailure,
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
            sqlx::Error::Tls(_) => Self::TlsFailure,
            // Rustls handshake/certificate errors reach SQLx as IO errors. Use
            // their concrete source type, never certificate-message matching or
            // treating an ordinary network outage as a permanent TLS failure.
            sqlx::Error::Io(ref error)
                if error
                    .get_ref()
                    .is_some_and(|source| source.is::<rustls::Error>()) =>
            {
                Self::TlsFailure
            }
            sqlx::Error::Configuration(_) => Self::InvalidPolicy,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_sources_are_redacted_without_misclassifying_network_failures() {
        let certificate =
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        let error = PostgresError::from(sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            certificate,
        )));
        assert_eq!(error, PostgresError::TlsFailure);
        assert!(!format!("{error:?}: {error}").contains("UnknownIssuer"));
        assert_eq!(
            PostgresError::from(sqlx::Error::Tls("PRIVATE_CERTIFICATE_PATH".into())),
            PostgresError::TlsFailure
        );
        assert!(!PostgresError::TlsFailure.to_string().contains("PRIVATE"));
        for kind in [
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::TimedOut,
        ] {
            assert_eq!(
                PostgresError::from(sqlx::Error::Io(std::io::Error::from(kind))),
                PostgresError::Unavailable
            );
        }
        // Error text can resemble a certificate failure; it is not a typed TLS
        // source and must not change retry policy.
        assert_eq!(
            PostgresError::from(sqlx::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "PRIVATE_CERTIFICATE_PATH: not valid for name",
            ))),
            PostgresError::Unavailable
        );
    }
}
