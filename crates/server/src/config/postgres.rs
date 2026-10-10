//! Typed native PostgreSQL settings; secrets are resolved only for the selected backend.
use super::{ConfigError, SecretReference};
use metric_postgres::{ConnectionPolicy, MigrationMode, MigrationPolicy, SslMode};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageBackend {
    #[default]
    #[serde(rename = "mongodb")]
    MongoDb,
    #[serde(rename = "postgres")]
    Postgres,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSettings {
    pub backend: StorageBackend,
}

#[derive(Debug, Clone)]
pub struct PostgresSettings {
    pub uri: Option<SecretReference>,
    pub migration_uri: Option<SecretReference>,
    pub connection: ConnectionPolicy,
    pub migrations: MigrationPolicy,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawPostgresSettings {
    uri: Option<SecretReference>,
    bootstrap_timeout: String,
    acquire_timeout: String,
    min_connections: u32,
    max_connections: u32,
    statement_timeout: String,
    lock_timeout: String,
    idle_in_transaction_timeout: String,
    ssl_mode: String,
    allow_insecure_connection: bool,
    ssl_root_cert: Option<PathBuf>,
    migrations: RawMigrations,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawMigrations {
    mode: String,
    lock_timeout: String,
    statement_timeout: String,
    uri: Option<SecretReference>,
}
impl Default for RawMigrations {
    fn default() -> Self {
        Self {
            mode: "auto".into(),
            lock_timeout: "30s".into(),
            statement_timeout: "60s".into(),
            uri: None,
        }
    }
}
impl Default for RawPostgresSettings {
    fn default() -> Self {
        Self {
            uri: None,
            bootstrap_timeout: "10s".into(),
            acquire_timeout: "5s".into(),
            min_connections: 0,
            max_connections: 10,
            statement_timeout: "10s".into(),
            lock_timeout: "2s".into(),
            idle_in_transaction_timeout: "30s".into(),
            ssl_mode: "verify-full".into(),
            allow_insecure_connection: false,
            ssl_root_cert: None,
            migrations: RawMigrations::default(),
        }
    }
}
impl RawPostgresSettings {
    pub(super) fn validate(
        self,
        selected: bool,
        allow_literals: bool,
    ) -> Result<PostgresSettings, ConfigError> {
        let duration =
            |s: &str| humantime::parse_duration(s).map_err(|_| ConfigError::InvalidPostgresConfig);
        let ssl_mode = match self.ssl_mode.as_str() {
            "disable" => SslMode::Disable,
            "require" => SslMode::Require,
            "verify-ca" => SslMode::VerifyCa,
            "verify-full" => SslMode::VerifyFull,
            _ => return Err(ConfigError::InvalidPostgresConfig),
        };
        let mode = match self.migrations.mode.as_str() {
            "auto" => MigrationMode::Auto,
            "check" => MigrationMode::Check,
            _ => return Err(ConfigError::InvalidPostgresConfig),
        };
        let connection = ConnectionPolicy {
            bootstrap_timeout: duration(&self.bootstrap_timeout)?,
            acquire_timeout: duration(&self.acquire_timeout)?,
            min_connections: self.min_connections,
            max_connections: self.max_connections,
            statement_timeout: duration(&self.statement_timeout)?,
            lock_timeout: duration(&self.lock_timeout)?,
            idle_in_transaction_timeout: duration(&self.idle_in_transaction_timeout)?,
            ssl_mode,
            allow_insecure_connection: self.allow_insecure_connection,
            ssl_root_cert: self.ssl_root_cert,
        };
        let migrations = MigrationPolicy {
            mode,
            lock_timeout: duration(&self.migrations.lock_timeout)?,
            statement_timeout: duration(&self.migrations.statement_timeout)?,
        };
        if selected {
            connection.validate().map_err(ConfigError::Postgres)?;
            if migrations.lock_timeout < Duration::from_millis(1)
                || migrations.lock_timeout > Duration::from_secs(300)
                || migrations.statement_timeout < Duration::from_millis(1)
                || migrations.statement_timeout > Duration::from_secs(1800)
            {
                return Err(ConfigError::InvalidPostgresConfig);
            }
            let uri = self.uri.as_ref().ok_or(ConfigError::MissingPostgresUri)?;
            uri.validate(allow_literals)?;
            if let Some(uri) = self.migrations.uri.as_ref() {
                uri.validate(allow_literals)?;
            }
            if connection
                .ssl_root_cert
                .as_ref()
                .is_some_and(|p| !p.is_file())
            {
                return Err(ConfigError::InvalidPostgresConfig);
            }
        }
        Ok(PostgresSettings {
            uri: self.uri,
            migration_uri: self.migrations.uri,
            connection,
            migrations,
        })
    }
}
impl PostgresSettings {
    pub(super) fn effective_redacted(&self, backend: StorageBackend) -> String {
        let backend = match backend {
            StorageBackend::MongoDb => "mongodb",
            StorageBackend::Postgres => "postgres",
        };
        let ssl = match self.connection.ssl_mode {
            SslMode::Disable => "disable",
            SslMode::Require => "require",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyFull => "verify-full",
        };
        let mode = match self.migrations.mode {
            MigrationMode::Auto => "auto",
            MigrationMode::Check => "check",
        };
        let origin = |s: &Option<SecretReference>| {
            s.as_ref()
                .map_or("<not-configured>", SecretReference::redacted_origin)
        };
        format!(
            "\n[storage]\nbackend = \"{backend}\"\n\n[postgres]\nuri = \"{}\"\nbootstrap_timeout = \"{}\"\nacquire_timeout = \"{}\"\nmin_connections = {}\nmax_connections = {}\nstatement_timeout = \"{}\"\nlock_timeout = \"{}\"\nidle_in_transaction_timeout = \"{}\"\nssl_mode = \"{ssl}\"\nallow_insecure_connection = {}\nssl_root_cert = \"{}\"\n\n[postgres.migrations]\nmode = \"{mode}\"\nlock_timeout = \"{}\"\nstatement_timeout = \"{}\"\nuri = \"{}\"\n",
            origin(&self.uri),
            humantime::format_duration(self.connection.bootstrap_timeout),
            humantime::format_duration(self.connection.acquire_timeout),
            self.connection.min_connections,
            self.connection.max_connections,
            humantime::format_duration(self.connection.statement_timeout),
            humantime::format_duration(self.connection.lock_timeout),
            humantime::format_duration(self.connection.idle_in_transaction_timeout),
            self.connection.allow_insecure_connection,
            self.connection
                .ssl_root_cert
                .as_ref()
                .map_or("<not-configured>", |_| "<redacted:file>"),
            humantime::format_duration(self.migrations.lock_timeout),
            humantime::format_duration(self.migrations.statement_timeout),
            origin(&self.migration_uri)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, RawConfig};
    use figment::{
        Figment,
        providers::{Format, Serialized, Toml},
    };
    fn parse(source: &str) -> Result<AppConfig, ConfigError> {
        let raw = Figment::from(Serialized::defaults(RawConfig::default()))
            .merge(Toml::string(source))
            .extract::<RawConfig>()?;
        AppConfig::try_from(raw)
    }
    fn selected(extra: &str) -> String {
        format!(
            "[storage]\nbackend='postgres'\n[development]\nallow_literal_secrets=true\n[projects]\nscrub_hmac_key={{literal='{}'}}\n[postgres]\nuri={{literal='postgres://operator:NEVER_PRINT_POSTGRES_SECRET@127.0.0.1:54329/metric_test'}}\n{extra}",
            "01".repeat(32)
        )
    }
    #[test]
    fn default_mongo_and_inactive_postgres_secrets_are_not_resolved() {
        let c=parse("[postgres]\nuri={env='METRIC_TEST_UNUSED_POSTGRES_MUST_NOT_RESOLVE'}\n[postgres.migrations]\nuri={file='/missing/not-selected-postgres-uri'}").unwrap();
        assert_eq!(c.storage.backend, StorageBackend::MongoDb);
        let secrets = c.validate_secrets().unwrap();
        assert!(secrets.postgres_uri.is_none());
        assert!(secrets.postgres_migration_uri.is_none());
    }
    #[test]
    fn postgres_without_mongo_is_native_and_effective_config_redacts_all_uris() {
        let source = selected(
            "ssl_mode='disable'\nallow_insecure_connection=true\n[postgres.migrations]\nuri={literal='postgres://migrator:MIGRATION_SECRET@127.0.0.1:54329/metric_test'}\n[mongodb]\nuri={env='METRIC_TEST_UNUSED_MONGO_MUST_NOT_RESOLVE'}",
        );
        let c = parse(&source).unwrap();
        let secrets = c.validate_secrets().unwrap();
        assert!(secrets.mongodb_uri.is_none());
        assert!(secrets.postgres_uri.is_some());
        assert!(secrets.postgres_migration_uri.is_some());
        let effective = c.effective_redacted();
        assert!(effective.contains("backend = \"postgres\""));
        assert!(effective.contains("ssl_mode = \"disable\""));
        for secret in [
            "NEVER_PRINT_POSTGRES_SECRET",
            "MIGRATION_SECRET",
            "METRIC_TEST_UNUSED_MONGO_MUST_NOT_RESOLVE",
        ] {
            assert!(!effective.contains(secret));
            assert!(!format!("{c:?}").contains(secret));
        }
    }
    #[test]
    fn selected_uri_and_shared_hmac_are_required_without_network_io() {
        assert!(matches!(
            parse("[storage]\nbackend='postgres'"),
            Err(ConfigError::MissingPostgresUri)
        ));
        let c=parse("[storage]\nbackend='postgres'\n[development]\nallow_literal_secrets=true\n[postgres]\nuri={literal='postgres://operator:secret@127.0.0.1:54329/metric_test'}").unwrap();
        assert!(matches!(
            c.validate_secrets(),
            Err(ConfigError::MissingScrubHmacKey)
        ));
    }
    #[test]
    fn archive_accepts_the_selected_postgres_backend_without_a_mongo_uri() {
        let c = parse(&selected("[archive]\nenabled=true")).unwrap();
        assert!(c.archive.enabled);
        assert!(c.mongodb.uri.is_none());
        assert!(c.validate_secrets().is_ok());
    }
    #[test]
    fn weak_modes_bounds_and_unknown_keys_are_closed() {
        for mode in ["disable", "require", "verify-ca"] {
            assert!(matches!(
                parse(&selected(&format!("ssl_mode='{mode}'"))),
                Err(ConfigError::Postgres(
                    metric_postgres::PostgresError::InsecureConnectionNotAcknowledged
                ))
            ));
            assert!(
                parse(&selected(&format!(
                    "ssl_mode='{mode}'\nallow_insecure_connection=true"
                )))
                .is_ok()
            );
        }
        for extra in [
            "ssl_mode='prefer'",
            "max_connections=129",
            "acquire_timeout='0s'",
            "lock_timeout='11s'",
            "[postgres.migrations]\nmode='disabled'",
            "[postgres.migrations]\nstatement_timeout='31m'",
            "unknown_pool_option=true",
        ] {
            assert!(parse(&selected(extra)).is_err(), "{extra}");
        }
        assert!(parse("[storage]\nbackend='mysql'").is_err());
    }
    #[test]
    fn migration_target_uri_policy_and_errors_do_not_reveal_credentials() {
        let c=parse(&selected("[postgres.migrations]\nuri={literal='postgres://migrator:MIGRATION_SECRET@different-host:54329/metric_test'}")).unwrap();
        let error = c.validate_secrets().err().unwrap();
        assert!(matches!(
            error,
            ConfigError::Postgres(metric_postgres::PostgresError::UriPolicyConflict)
        ));
        assert!(!format!("{error:?} {error}").contains("MIGRATION_SECRET"));
        let c = parse(
            &selected("").replace("/metric_test'", "/metric_test?options=NEVER_PRINT_OPTIONS'"),
        )
        .unwrap();
        let error = c.validate_secrets().err().unwrap();
        assert!(!format!("{error:?} {error}").contains("NEVER_PRINT_OPTIONS"));
    }
}
