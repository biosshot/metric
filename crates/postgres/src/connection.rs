use crate::PostgresError;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use std::{collections::BTreeSet, fmt, path::PathBuf, str::FromStr, time::Duration};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SslMode {
    #[default]
    VerifyFull,
    VerifyCa,
    Require,
    Disable,
}

impl SslMode {
    const fn driver(self) -> PgSslMode {
        match self {
            Self::VerifyFull => PgSslMode::VerifyFull,
            Self::VerifyCa => PgSslMode::VerifyCa,
            Self::Require => PgSslMode::Require,
            Self::Disable => PgSslMode::Disable,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::VerifyFull => "verify-full",
            Self::VerifyCa => "verify-ca",
            Self::Require => "require",
            Self::Disable => "disable",
        }
    }
}

/// Driver-free validated settings; credentials are passed separately and never Debug.
#[derive(Clone)]
pub struct ConnectionPolicy {
    pub bootstrap_timeout: Duration,
    pub acquire_timeout: Duration,
    pub min_connections: u32,
    pub max_connections: u32,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
    pub idle_in_transaction_timeout: Duration,
    pub ssl_mode: SslMode,
    pub allow_insecure_connection: bool,
    pub ssl_root_cert: Option<PathBuf>,
}

impl fmt::Debug for ConnectionPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionPolicy")
            .field("bootstrap_timeout", &self.bootstrap_timeout)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("min_connections", &self.min_connections)
            .field("max_connections", &self.max_connections)
            .field("statement_timeout", &self.statement_timeout)
            .field("lock_timeout", &self.lock_timeout)
            .field(
                "idle_in_transaction_timeout",
                &self.idle_in_transaction_timeout,
            )
            .field("ssl_mode", &self.ssl_mode)
            .field("allow_insecure_connection", &self.allow_insecure_connection)
            .field(
                "ssl_root_cert",
                &self.ssl_root_cert.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            bootstrap_timeout: Duration::from_secs(10),
            acquire_timeout: Duration::from_secs(5),
            min_connections: 0,
            max_connections: 10,
            statement_timeout: Duration::from_secs(10),
            lock_timeout: Duration::from_secs(2),
            idle_in_transaction_timeout: Duration::from_secs(30),
            ssl_mode: SslMode::VerifyFull,
            allow_insecure_connection: false,
            ssl_root_cert: None,
        }
    }
}

impl ConnectionPolicy {
    pub fn validate(&self) -> Result<(), PostgresError> {
        let bounded = |duration: Duration, maximum: u64| {
            duration >= Duration::from_millis(1) && duration <= Duration::from_secs(maximum)
        };
        if self.max_connections == 0
            || self.max_connections > 128
            || self.min_connections > self.max_connections
            || !bounded(self.bootstrap_timeout, 60)
            || !bounded(self.acquire_timeout, 60)
            || !bounded(self.statement_timeout, 300)
            || !bounded(self.lock_timeout, 300)
            || self.lock_timeout > self.statement_timeout
            || !bounded(self.idle_in_transaction_timeout, 300)
        {
            return Err(PostgresError::InvalidPolicy);
        }
        if self.ssl_mode != SslMode::VerifyFull && !self.allow_insecure_connection {
            return Err(PostgresError::InsecureConnectionNotAcknowledged);
        }
        Ok(())
    }

    /// Validate before SQLx can log unknown URI query values.
    pub fn validate_uri(&self, uri: &str) -> Result<(), PostgresError> {
        self.validate()?;
        let parsed = url::Url::parse(uri).map_err(|_| PostgresError::InvalidUri)?;
        if !matches!(parsed.scheme(), "postgres" | "postgresql")
            || parsed.host_str().is_none()
            || parsed.fragment().is_some()
            || parsed.username().is_empty()
            || parsed.path().trim_matches('/').is_empty()
        {
            return Err(PostgresError::InvalidUri);
        }
        let mut keys = BTreeSet::new();
        for (key, value) in parsed.query_pairs() {
            if !keys.insert(key.to_string()) {
                return Err(PostgresError::UriPolicyConflict);
            }
            match key.as_ref() {
                "sslmode" if value == self.ssl_mode.name() => {}
                _ => return Err(PostgresError::UriPolicyConflict),
            }
        }
        Ok(())
    }

    pub(crate) fn connect_options(&self, uri: &str) -> Result<PgConnectOptions, PostgresError> {
        self.validate_uri(uri)?;
        // SQLx Require skips CA validation even with a supplied root. Honor our
        // documented native-libpq-compatible policy by using VerifyCa in that case.
        let ssl_mode = if self.ssl_mode == SslMode::Require
            && (self.ssl_root_cert.is_some() || std::env::var_os("PGSSLROOTCERT").is_some())
        {
            PgSslMode::VerifyCa
        } else {
            self.ssl_mode.driver()
        };
        // SQLx's VerifyCa verifier uses Rustls's process-level provider. S3 can
        // enable aws-lc alongside SQLx's ring feature, making auto-selection panic.
        // Respect a provider installed by the embedding application; a racing
        // installer winning install_default is likewise intentionally retained.
        if self.ssl_mode != SslMode::Disable
            && rustls::crypto::CryptoProvider::get_default().is_none()
        {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let mut options = PgConnectOptions::from_str(uri)
            .map_err(|_| PostgresError::InvalidUri)?
            .ssl_mode(ssl_mode)
            .application_name("metric")
            .disable_statement_logging()
            .options([
                ("search_path", "pg_catalog,metric".to_owned()),
                (
                    "statement_timeout",
                    self.statement_timeout.as_millis().to_string(),
                ),
                ("lock_timeout", self.lock_timeout.as_millis().to_string()),
                (
                    "idle_in_transaction_session_timeout",
                    self.idle_in_transaction_timeout.as_millis().to_string(),
                ),
            ]);
        if let Some(path) = &self.ssl_root_cert {
            options = options.ssl_root_cert(path);
        }
        Ok(options)
    }

    /// Separate DDL credentials must address the same explicit database target.
    pub fn validate_migration_target(
        &self,
        runtime_uri: &str,
        migration_uri: &str,
    ) -> Result<(), PostgresError> {
        let runtime = self.connect_options(runtime_uri)?;
        let migration = self.connect_options(migration_uri)?;
        if runtime.get_host() != migration.get_host()
            || runtime.get_port() != migration.get_port()
            || runtime.get_database() != migration.get_database()
        {
            return Err(PostgresError::UriPolicyConflict);
        }
        Ok(())
    }
}

/// Cloneable connection pool. No public accessor leaks the driver into services.
#[derive(Clone)]
pub struct PostgresDatabase {
    pub(crate) pool: PgPool,
}

impl PostgresDatabase {
    pub async fn connect(uri: &str, policy: &ConnectionPolicy) -> Result<Self, PostgresError> {
        let options = policy.connect_options(uri)?;
        for key in ["PGOPTIONS", "PGSSLROOTCERT", "PGSSLCERT", "PGSSLKEY"] {
            if std::env::var_os(key).is_some() {
                tracing::warn!(
                    variable = key,
                    "PostgreSQL driver environment default is operator-controlled"
                );
            }
        }
        if policy.ssl_mode != SslMode::VerifyFull {
            tracing::warn!("PostgreSQL weaker transport mode explicitly acknowledged");
        }
        let pool = tokio::time::timeout(
            policy.bootstrap_timeout,
            PgPoolOptions::new()
                .min_connections(policy.min_connections)
                .max_connections(policy.max_connections)
                .acquire_timeout(policy.acquire_timeout)
                .connect_with(options),
        )
        .await
        .map_err(|_| PostgresError::Timeout)??;
        Ok(Self { pool })
    }

    pub async fn ping(&self) -> Result<(), PostgresError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;

    #[test]
    fn uri_policy_rejects_values_before_driver_logging() {
        let policy = ConnectionPolicy::default();
        let with_root = ConnectionPolicy {
            ssl_root_cert: Some(PathBuf::from("private-ca-location-marker.pem")),
            ..policy.clone()
        };
        assert!(!format!("{with_root:?}").contains("private-ca-location-marker"));
        for uri in [
            "postgres://user:secret@localhost/metric?options=-c%20search_path=public",
            "postgres://user:secret@localhost/metric?password=secret",
            "postgres://user:secret@localhost/metric?sslmode=disable",
            "postgres://user:secret@localhost/metric?sslmode=verify-full&sslmode=verify-full",
        ] {
            let error = policy.validate_uri(uri).unwrap_err();
            assert_eq!(error, PostgresError::UriPolicyConflict);
            assert!(!format!("{error:?} {error}").contains("secret"));
        }
        assert!(
            policy
                .validate_uri("postgresql://user:secret@localhost/metric?sslmode=verify-full")
                .is_ok()
        );
    }

    #[test]
    fn weak_transport_requires_explicit_acknowledgement() {
        for ssl_mode in [SslMode::Disable, SslMode::Require, SslMode::VerifyCa] {
            let mut policy = ConnectionPolicy {
                ssl_mode,
                ..Default::default()
            };
            assert_eq!(
                policy.validate(),
                Err(PostgresError::InsecureConnectionNotAcknowledged)
            );
            policy.allow_insecure_connection = true;
            assert!(policy.validate().is_ok());
        }
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn pool_exhaustion_sql_timeout_and_transaction_settings_do_not_leak() {
        let fixture = Fixture::new().await;
        let policy = ConnectionPolicy {
            max_connections: 1,
            acquire_timeout: Duration::from_millis(100),
            statement_timeout: Duration::from_millis(200),
            lock_timeout: Duration::from_millis(100),
            ..fixture.policy.clone()
        };
        let database = PostgresDatabase::connect(&fixture.uri, &policy)
            .await
            .unwrap();
        let held = database.pool.acquire().await.unwrap();
        assert_eq!(database.ping().await, Err(PostgresError::Timeout));
        drop(held);
        let error = sqlx::query("SELECT pg_sleep(2)")
            .execute(&database.pool)
            .await
            .unwrap_err();
        assert_eq!(PostgresError::from(error), PostgresError::Timeout);
        database.ping().await.unwrap();
        let mut transaction = database.pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('statement_timeout', '5', true)")
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.rollback().await.unwrap();
        let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(timeout, "200ms");
        database.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn operator_environment_defaults_are_used_without_exposing_values() {
        const CHILD: &str = "METRIC_PG_ENV_PROBE";
        if std::env::var_os(CHILD).is_some() {
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::WARN)
                .without_time()
                .with_ansi(false)
                .init();
            let uri = std::env::var("METRIC_TEST_POSTGRES_URI").unwrap();
            let policy = ConnectionPolicy {
                ssl_mode: SslMode::Disable,
                allow_insecure_connection: true,
                ..Default::default()
            };
            let database = PostgresDatabase::connect(&uri, &policy).await.unwrap();
            let jit: String = sqlx::query_scalar("SHOW jit")
                .fetch_one(&database.pool)
                .await
                .unwrap();
            assert_eq!(jit, "off");
            database.close().await;
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "connection::tests::operator_environment_defaults_are_used_without_exposing_values",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PGOPTIONS", "-c jit=off")
            .env("PGSSLROOTCERT", "operator-private-root-marker.pem")
            .env("PGSSLCERT", "operator-private-cert-marker.pem")
            .env("PGSSLKEY", "operator-private-key-marker.pem")
            .output()
            .unwrap();
        let output_text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "isolated environment connection probe failed"
        );
        for key in ["PGOPTIONS", "PGSSLROOTCERT", "PGSSLCERT", "PGSSLKEY"] {
            assert!(output_text.contains(key));
        }
        for value in [
            "jit=off",
            "operator-private-root-marker",
            "operator-private-cert-marker",
            "operator-private-key-marker",
        ] {
            assert!(
                !output_text.contains(value),
                "operator environment value entered logs"
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL TLS profile and local test certificates"]
    async fn verified_local_tls_rejects_wrong_ca_and_hostname() {
        let uri = std::env::var("METRIC_TEST_POSTGRES_TLS_URI")
            .expect("TLS fixture URI required; no silent skip");
        let root =
            std::env::var_os("METRIC_TEST_POSTGRES_TLS_ROOT").expect("local test CA required");
        let wrong = std::env::var_os("METRIC_TEST_POSTGRES_TLS_WRONG_ROOT")
            .expect("wrong local test CA required");
        let policy = ConnectionPolicy {
            ssl_root_cert: Some(root.clone().into()),
            bootstrap_timeout: Duration::from_secs(10),
            acquire_timeout: Duration::from_secs(5),
            ..Default::default()
        };
        let database = PostgresDatabase::connect(&uri, &policy).await.unwrap();
        let encrypted: bool =
            sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert!(encrypted);
        database.close().await;
        let wrong_policy = ConnectionPolicy {
            ssl_root_cert: Some(wrong.clone().into()),
            ..policy.clone()
        };
        assert!(matches!(
            PostgresDatabase::connect(&uri, &wrong_policy).await,
            Err(PostgresError::TlsFailure)
        ));
        let mut bad_host = url::Url::parse(&uri).unwrap();
        bad_host.set_host(Some("localhost")).unwrap();
        // Prove an actual certificate-name rejection rather than accepting a DNS
        // or connect timeout as evidence. Windows may try IPv6 localhost first.
        use sqlx::{Connection, PgConnection};
        let hostname_result = tokio::time::timeout(
            Duration::from_secs(10),
            PgConnection::connect_with(&policy.connect_options(bad_host.as_str()).unwrap()),
        )
        .await
        .expect("hostname probe exceeded its connection budget");
        assert!(matches!(hostname_result, Err(sqlx::Error::Io(ref error))
            if error.kind() == std::io::ErrorKind::InvalidData
                && error.to_string().contains("not valid for name")));
        assert_eq!(
            PostgresError::from(hostname_result.unwrap_err()),
            PostgresError::TlsFailure
        );
        let no_root = ConnectionPolicy {
            ssl_root_cert: None,
            ..policy.clone()
        };
        assert!(matches!(
            PostgresDatabase::connect(&uri, &no_root).await,
            Err(PostgresError::TlsFailure)
        ));
        let ca_only = ConnectionPolicy {
            ssl_mode: SslMode::VerifyCa,
            allow_insecure_connection: true,
            ..policy.clone()
        };
        let database = PostgresDatabase::connect(bad_host.as_str(), &ca_only)
            .await
            .expect("verify-ca must validate the CA while accepting a mismatched hostname");
        database.close().await;
        let bad_ca_only = ConnectionPolicy {
            ssl_root_cert: Some(wrong.clone().into()),
            ..ca_only
        };
        assert!(matches!(
            PostgresDatabase::connect(&uri, &bad_ca_only).await,
            Err(PostgresError::TlsFailure)
        ));
        let require_ca = ConnectionPolicy {
            ssl_mode: SslMode::Require,
            allow_insecure_connection: true,
            ..policy.clone()
        };
        let database = PostgresDatabase::connect(&uri, &require_ca).await.unwrap();
        database.close().await;
        let bad_require = ConnectionPolicy {
            ssl_root_cert: Some(wrong.into()),
            ..require_ca
        };
        assert!(matches!(
            PostgresDatabase::connect(&uri, &bad_require).await,
            Err(PostgresError::TlsFailure)
        ));
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL TLS profile and local test certificates"]
    async fn operator_crypto_provider_is_retained_by_native_tls() {
        const CHILD: &str = "METRIC_PG_CRYPTO_PROVIDER_PROBE";
        if std::env::var_os(CHILD).is_some() {
            rustls::crypto::aws_lc_rs::default_provider()
                .install_default()
                .unwrap();
            let installed = rustls::crypto::CryptoProvider::get_default()
                .unwrap()
                .clone();
            let uri = std::env::var("METRIC_TEST_POSTGRES_TLS_URI").unwrap();
            let root = std::env::var_os("METRIC_TEST_POSTGRES_TLS_ROOT").unwrap();
            for ssl_mode in [SslMode::VerifyFull, SslMode::VerifyCa, SslMode::Require] {
                let policy = ConnectionPolicy {
                    ssl_mode,
                    allow_insecure_connection: ssl_mode != SslMode::VerifyFull,
                    ssl_root_cert: Some(root.clone().into()),
                    ..Default::default()
                };
                let database = PostgresDatabase::connect(&uri, &policy).await.unwrap();
                assert!(
                    sqlx::query_scalar::<_, bool>(
                        "SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()"
                    )
                    .fetch_one(&database.pool)
                    .await
                    .unwrap()
                );
                assert!(std::sync::Arc::ptr_eq(
                    &installed,
                    rustls::crypto::CryptoProvider::get_default().unwrap()
                ));
                database.close().await;
            }
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "connection::tests::operator_crypto_provider_is_retained_by_native_tls",
                "--ignored",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "isolated operator-provider TLS probe failed"
        );
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL without TLS"]
    async fn required_tls_on_plain_server_is_permanent_without_schema_mutation() {
        let fixture = Fixture::new().await;
        for ssl_mode in [SslMode::Require, SslMode::VerifyCa, SslMode::VerifyFull] {
            let policy = ConnectionPolicy {
                ssl_mode,
                allow_insecure_connection: ssl_mode != SslMode::VerifyFull,
                ..fixture.policy.clone()
            };
            assert!(matches!(
                PostgresDatabase::connect(&fixture.uri, &policy).await,
                Err(PostgresError::TlsFailure)
            ));
            assert_eq!(
                crate::PostgresSchemaManager::ensure(
                    &fixture.uri,
                    &policy,
                    &crate::MigrationPolicy::default()
                )
                .await,
                Err(PostgresError::TlsFailure)
            );
        }
        let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        database.ping().await.unwrap();
        let schema: bool = sqlx::query_scalar("SELECT to_regnamespace('metric') IS NOT NULL")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert!(!schema, "failed TLS negotiation must not mutate the schema");
        database.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL TLS profile and local test certificates"]
    async fn operator_tls_certificates_are_inherited_and_values_redacted() {
        const CHILD: &str = "METRIC_PG_TLS_ENV_PROBE";
        if std::env::var_os(CHILD).is_some() {
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::WARN)
                .without_time()
                .with_ansi(false)
                .init();
            let uri = std::env::var("METRIC_TEST_POSTGRES_TLS_URI").unwrap();
            // No explicit CA/client certificate paths: the driver inherits them.
            let policy = ConnectionPolicy {
                bootstrap_timeout: Duration::from_secs(2),
                acquire_timeout: Duration::from_secs(1),
                ..Default::default()
            };
            let database = PostgresDatabase::connect(&uri, &policy).await.unwrap();
            let encrypted: bool =
                sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid()")
                    .fetch_one(&database.pool)
                    .await
                    .unwrap();
            assert!(encrypted);
            let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
                .fetch_one(&database.pool)
                .await
                .unwrap();
            assert_eq!(timeout, "10s");
            database.close().await;
            return;
        }
        let root = std::env::var("METRIC_TEST_POSTGRES_TLS_ROOT").expect("TLS CA path required");
        let cert = std::env::var("METRIC_TEST_POSTGRES_TLS_CLIENT_CERT")
            .expect("TLS client certificate path required");
        let key = std::env::var("METRIC_TEST_POSTGRES_TLS_CLIENT_KEY")
            .expect("TLS client key path required");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "connection::tests::operator_tls_certificates_are_inherited_and_values_redacted",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PGOPTIONS", "-c statement_timeout=0")
            .env("PGSSLROOTCERT", &root)
            .env("PGSSLCERT", &cert)
            .env("PGSSLKEY", &key)
            .output()
            .unwrap();
        let logs = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "isolated inherited TLS connection probe failed"
        );
        for name in ["PGOPTIONS", "PGSSLROOTCERT", "PGSSLCERT", "PGSSLKEY"] {
            assert!(logs.contains(name));
        }
        for value in [&root, &cert, &key] {
            assert!(!logs.contains(value), "certificate path entered logs");
        }
        assert!(!logs.contains("statement_timeout=0"));
    }
}
