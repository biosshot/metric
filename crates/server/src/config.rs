use std::{env, fmt, fs, net::SocketAddr, path::PathBuf, str::FromStr};

use clap::{Parser, ValueEnum};
use faultkeep_domain::{BoundedDuration, ByteSize};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_SHUTDOWN_GRACE_MILLIS: u64 = 5 * 60 * 1_000;
const MAX_PROJECT_CACHE_TTL_MILLIS: u64 = 10 * 60 * 1_000;

pub type ShutdownGrace = BoundedDuration<MAX_SHUTDOWN_GRACE_MILLIS>;
pub type RequestTimeout = BoundedDuration<60_000>;
pub type ProjectCacheTtl = BoundedDuration<MAX_PROJECT_CACHE_TTL_MILLIS>;
pub type MongoBootstrapTimeout = BoundedDuration<60_000>;
pub type BatchWait = BoundedDuration<1_000>;
pub type DispatcherInterval = BoundedDuration<60_000>;
type ConfiguredBytes = ByteSize<{ 1024 * 1024 * 1024 }>;

#[derive(Debug, Clone, Parser)]
#[command(name = "faultkeep", version, about = "Faultkeep all-in-one server")]
pub struct Cli {
    /// TOML configuration file.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Deployment role. Version one accepts only `all`.
    #[arg(long, value_enum)]
    pub role: Option<Role>,
    /// Validate configuration and required secret references, then exit.
    #[arg(long, conflicts_with = "print_effective_config")]
    pub check_config: bool,
    /// Print validated effective configuration with secrets redacted, then exit.
    #[arg(long, conflicts_with = "check_config")]
    pub print_effective_config: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    All,
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => formatter.write_str("all"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub role: Role,
    pub server: ServerConfig,
    pub mongodb: MongoConfig,
    pub projects: ProjectConfig,
    pub development: DevelopmentConfig,
    pub ingest: IngestConfig,
    pub dispatcher: DispatcherSettings,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub http_address: SocketAddr,
    pub shutdown_grace: ShutdownGrace,
}

#[derive(Debug, Clone)]
pub struct MongoConfig {
    pub uri: Option<SecretReference>,
    pub database: String,
    pub bootstrap_timeout: MongoBootstrapTimeout,
}

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    pub scrub_hmac_key: Option<SecretReference>,
    pub identity_collision_retries: usize,
    pub max_keys_per_project: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct DevelopmentConfig {
    pub allow_literal_secrets: bool,
}

#[derive(Debug, Clone)]
pub struct IngestConfig {
    pub max_compressed_request_bytes: usize,
    pub max_decompressed_request_bytes: usize,
    pub max_event_bytes: usize,
    pub max_envelope_items: usize,
    pub max_active_requests: usize,
    pub max_parsing_tasks: usize,
    pub max_waiting_for_storage: usize,
    pub request_timeout: RequestTimeout,
    pub unsupported_backoff_seconds: u64,
    pub project_cache: ProjectCacheSettings,
    pub batch: BatchSettings,
    pub event_codec: EventCodecSettings,
}

#[derive(Debug, Clone, Copy)]
pub struct ProjectCacheSettings {
    pub capacity: usize,
    pub max_inflight: usize,
    pub positive_ttl: ProjectCacheTtl,
    pub negative_ttl: ProjectCacheTtl,
}

#[derive(Debug, Clone, Copy)]
pub struct BatchSettings {
    pub max_wait: BatchWait,
    pub max_documents: usize,
    pub max_bytes: usize,
}

impl Default for BatchSettings {
    fn default() -> Self {
        Self {
            max_wait: "20ms".parse().expect("default batch wait is valid"),
            max_documents: 250,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EventCodecSettings {
    pub compression_level: i32,
    pub compression_min_savings: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct DispatcherSettings {
    pub queue_capacity: usize,
    pub worker_concurrency: usize,
    pub low_watermark: usize,
    pub refill_target: usize,
    pub refill_batch_size: usize,
    pub poll_interval: DispatcherInterval,
    pub metrics_interval: DispatcherInterval,
    pub source_timeout: DispatcherInterval,
}

impl Default for DispatcherSettings {
    fn default() -> Self {
        Self {
            queue_capacity: 4_096,
            worker_concurrency: 32,
            low_watermark: 1_024,
            refill_target: 3_072,
            refill_batch_size: 512,
            poll_interval: "100ms".parse().expect("default poll interval is valid"),
            metrics_interval: "5s".parse().expect("default metrics interval is valid"),
            source_timeout: "5s".parse().expect("default source timeout is valid"),
        }
    }
}

impl Default for EventCodecSettings {
    fn default() -> Self {
        Self {
            compression_level: 3,
            compression_min_savings: 64,
        }
    }
}

impl Default for ProjectCacheSettings {
    fn default() -> Self {
        Self {
            capacity: 100_000,
            max_inflight: 512,
            positive_ttl: "60s".parse().expect("default positive TTL is valid"),
            negative_ttl: "5s".parse().expect("default negative TTL is valid"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SecretReference {
    Environment(EnvironmentReference),
    File(FileReference),
    Literal(LiteralReference),
}

impl SecretReference {
    fn validate(&self, allow_literal: bool) -> Result<(), ConfigError> {
        match self {
            Self::Environment(reference) => {
                let valid = !reference.env.is_empty()
                    && reference.env.len() <= 128
                    && reference
                        .env
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
                if !valid {
                    return Err(ConfigError::InvalidSecretReference);
                }
            }
            Self::File(reference) => {
                if reference.file.as_os_str().is_empty() {
                    return Err(ConfigError::InvalidSecretReference);
                }
            }
            Self::Literal(reference) => {
                if !allow_literal {
                    return Err(ConfigError::LiteralSecretForbidden);
                }
                validate_secret_bytes(reference.literal.as_bytes())?;
            }
        }
        Ok(())
    }

    pub fn resolve(&self) -> Result<SecretValue, ConfigError> {
        let bytes = match self {
            Self::Environment(reference) => env::var_os(&reference.env)
                .ok_or_else(|| ConfigError::MissingSecretEnvironment(reference.env.clone()))?
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
            Self::File(reference) => {
                fs::read(&reference.file).map_err(|source| ConfigError::ReadSecretFile {
                    path: reference.file.clone(),
                    source,
                })?
            }
            Self::Literal(reference) => reference.literal.as_bytes().to_vec(),
        };
        validate_secret_bytes(&bytes)?;
        let value = String::from_utf8(bytes).map_err(|_| ConfigError::SecretNotUtf8)?;
        let value = value.trim_end_matches(['\r', '\n']).to_owned();
        if value.is_empty() {
            return Err(ConfigError::EmptySecret);
        }
        Ok(SecretValue(value.into_boxed_str()))
    }

    fn redacted_origin(&self) -> &'static str {
        match self {
            Self::Environment(_) => "<redacted:env>",
            Self::File(_) => "<redacted:file>",
            Self::Literal(_) => "<redacted:literal>",
        }
    }
}

impl fmt::Debug for SecretReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.redacted_origin())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentReference {
    env: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReference {
    file: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiteralReference {
    literal: String,
}

pub struct SecretValue(Box<str>);

impl SecretValue {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    role: Role,
    server: RawServerConfig,
    mongodb: RawMongoConfig,
    projects: RawProjectConfig,
    development: RawDevelopmentConfig,
    ingest: RawIngestConfig,
    dispatcher: RawDispatcherSettings,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            role: Role::All,
            server: RawServerConfig::default(),
            mongodb: RawMongoConfig::default(),
            projects: RawProjectConfig::default(),
            development: RawDevelopmentConfig::default(),
            ingest: RawIngestConfig::default(),
            dispatcher: RawDispatcherSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawServerConfig {
    http_address: String,
    shutdown_grace: String,
}

impl Default for RawServerConfig {
    fn default() -> Self {
        Self {
            http_address: "127.0.0.1:3000".to_owned(),
            shutdown_grace: "10s".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawMongoConfig {
    uri: Option<SecretReference>,
    database: String,
    bootstrap_timeout: String,
}

impl Default for RawMongoConfig {
    fn default() -> Self {
        Self {
            uri: None,
            database: "faultkeep".to_owned(),
            bootstrap_timeout: "10s".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawProjectConfig {
    scrub_hmac_key: Option<SecretReference>,
    identity_collision_retries: usize,
    max_keys_per_project: usize,
}

impl Default for RawProjectConfig {
    fn default() -> Self {
        Self {
            scrub_hmac_key: None,
            identity_collision_retries: 16,
            max_keys_per_project: 32,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawDevelopmentConfig {
    allow_literal_secrets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawIngestConfig {
    max_compressed_request_bytes: String,
    max_decompressed_request_bytes: String,
    max_event_bytes: String,
    max_envelope_items: usize,
    max_active_requests: usize,
    max_parsing_tasks: usize,
    max_waiting_for_storage: usize,
    request_timeout: String,
    unsupported_backoff_seconds: u64,
    project_cache: RawProjectCacheSettings,
    batch: RawBatchSettings,
    event_codec: RawEventCodecSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawProjectCacheSettings {
    capacity: usize,
    max_inflight: usize,
    positive_ttl: String,
    negative_ttl: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawBatchSettings {
    max_wait: String,
    max_documents: usize,
    max_bytes: String,
}

impl Default for RawBatchSettings {
    fn default() -> Self {
        Self {
            max_wait: "20ms".to_owned(),
            max_documents: 250,
            max_bytes: "8 MiB".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawEventCodecSettings {
    compression_level: i32,
    compression_min_savings: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawDispatcherSettings {
    queue_capacity: usize,
    worker_concurrency: usize,
    low_watermark: usize,
    refill_target: usize,
    refill_batch_size: usize,
    poll_interval: String,
    metrics_interval: String,
    source_timeout: String,
}

impl Default for RawDispatcherSettings {
    fn default() -> Self {
        let defaults = DispatcherSettings::default();
        Self {
            queue_capacity: defaults.queue_capacity,
            worker_concurrency: defaults.worker_concurrency,
            low_watermark: defaults.low_watermark,
            refill_target: defaults.refill_target,
            refill_batch_size: defaults.refill_batch_size,
            poll_interval: "100ms".to_owned(),
            metrics_interval: "5s".to_owned(),
            source_timeout: "5s".to_owned(),
        }
    }
}

impl Default for RawEventCodecSettings {
    fn default() -> Self {
        Self {
            compression_level: 3,
            compression_min_savings: 64,
        }
    }
}

impl Default for RawProjectCacheSettings {
    fn default() -> Self {
        Self {
            capacity: 100_000,
            max_inflight: 512,
            positive_ttl: "60s".to_owned(),
            negative_ttl: "5s".to_owned(),
        }
    }
}

impl Default for RawIngestConfig {
    fn default() -> Self {
        Self {
            max_compressed_request_bytes: "20 MiB".to_owned(),
            max_decompressed_request_bytes: "100 MiB".to_owned(),
            max_event_bytes: "1 MiB".to_owned(),
            max_envelope_items: 100,
            max_active_requests: 512,
            max_parsing_tasks: 0,
            max_waiting_for_storage: 512,
            request_timeout: "10s".to_owned(),
            unsupported_backoff_seconds: 3600,
            project_cache: RawProjectCacheSettings::default(),
            batch: RawBatchSettings::default(),
            event_codec: RawEventCodecSettings::default(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration could not be loaded")]
    Load(#[source] Box<figment::Error>),
    #[error("server.http_address is invalid")]
    InvalidHttpAddress,
    #[error("configuration file does not exist or is not a regular file")]
    MissingConfigFile,
    #[error("server.shutdown_grace is invalid or exceeds five minutes")]
    InvalidShutdownGrace,
    #[error("ingest configuration is invalid or outside supported bounds")]
    InvalidIngestConfig,
    #[error("MongoDB configuration is invalid or outside supported bounds")]
    InvalidMongoConfig,
    #[error("project identity configuration is invalid or outside supported bounds")]
    InvalidProjectConfig,
    #[error("dispatcher configuration is invalid or outside supported bounds")]
    InvalidDispatcherConfig,
    #[error("projects.scrub_hmac_key is required when MongoDB is configured")]
    MissingScrubHmacKey,
    #[error(
        "projects.scrub_hmac_key must resolve to exactly 32 bytes encoded as 64 hexadecimal characters"
    )]
    InvalidScrubHmacKey,
    #[error("secret reference is invalid")]
    InvalidSecretReference,
    #[error("literal secrets require development.allow_literal_secrets=true")]
    LiteralSecretForbidden,
    #[error("secret is empty")]
    EmptySecret,
    #[error("secret exceeds 65536 bytes")]
    SecretTooLarge,
    #[error("secret is not valid UTF-8")]
    SecretNotUtf8,
    #[error("required secret environment variable is absent: {0}")]
    MissingSecretEnvironment(String),
    #[error("secret file cannot be read")]
    ReadSecretFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<figment::Error> for ConfigError {
    fn from(error: figment::Error) -> Self {
        Self::Load(Box::new(error))
    }
}

pub fn load(cli: &Cli) -> Result<AppConfig, ConfigError> {
    let mut figment = Figment::from(Serialized::defaults(RawConfig::default()));
    if let Some(path) = &cli.config {
        if !path.is_file() {
            return Err(ConfigError::MissingConfigFile);
        }
        figment = figment.merge(Toml::file(path));
    }
    figment = figment.merge(Env::prefixed("APP__").split("__"));
    let mut raw = figment.extract::<RawConfig>()?;
    if let Some(role) = cli.role {
        raw.role = role;
    }
    AppConfig::try_from(raw)
}

impl TryFrom<RawConfig> for AppConfig {
    type Error = ConfigError;

    fn try_from(raw: RawConfig) -> Result<Self, Self::Error> {
        let http_address = SocketAddr::from_str(&raw.server.http_address)
            .map_err(|_| ConfigError::InvalidHttpAddress)?;
        let shutdown_grace = ShutdownGrace::from_str(&raw.server.shutdown_grace)
            .map_err(|_| ConfigError::InvalidShutdownGrace)?;
        if let Some(reference) = &raw.mongodb.uri {
            reference.validate(raw.development.allow_literal_secrets)?;
        }
        if let Some(reference) = &raw.projects.scrub_hmac_key {
            reference.validate(raw.development.allow_literal_secrets)?;
        }
        let valid_database = !raw.mongodb.database.is_empty()
            && raw.mongodb.database.len() <= 64
            && !raw.mongodb.database.chars().any(char::is_control)
            && !raw
                .mongodb
                .database
                .contains(['/', '\\', '.', ' ', '"', '$']);
        let bootstrap_timeout = MongoBootstrapTimeout::from_str(&raw.mongodb.bootstrap_timeout)
            .map_err(|_| ConfigError::InvalidMongoConfig)?;
        if !valid_database || bootstrap_timeout.get().is_zero() {
            return Err(ConfigError::InvalidMongoConfig);
        }
        if raw.projects.identity_collision_retries == 0
            || !(1..=1024).contains(&raw.projects.max_keys_per_project)
        {
            return Err(ConfigError::InvalidProjectConfig);
        }
        let ingest = IngestConfig::try_from(raw.ingest)?;
        let dispatcher = DispatcherSettings::try_from(raw.dispatcher)?;
        Ok(Self {
            role: raw.role,
            server: ServerConfig {
                http_address,
                shutdown_grace,
            },
            mongodb: MongoConfig {
                uri: raw.mongodb.uri,
                database: raw.mongodb.database,
                bootstrap_timeout,
            },
            projects: ProjectConfig {
                scrub_hmac_key: raw.projects.scrub_hmac_key,
                identity_collision_retries: raw.projects.identity_collision_retries,
                max_keys_per_project: raw.projects.max_keys_per_project,
            },
            development: DevelopmentConfig {
                allow_literal_secrets: raw.development.allow_literal_secrets,
            },
            ingest,
            dispatcher,
        })
    }
}

impl AppConfig {
    pub fn validate_secrets(&self) -> Result<ResolvedSecrets, ConfigError> {
        let mongodb_uri = self
            .mongodb
            .uri
            .as_ref()
            .map(SecretReference::resolve)
            .transpose()?;
        let scrub_hmac_key = self
            .projects
            .scrub_hmac_key
            .as_ref()
            .map(SecretReference::resolve)
            .transpose()?;
        if mongodb_uri.is_some() && scrub_hmac_key.is_none() {
            return Err(ConfigError::MissingScrubHmacKey);
        }
        let scrub_hmac_key = scrub_hmac_key
            .map(|value| {
                let mut bytes = [0_u8; 32];
                hex::decode_to_slice(value.expose(), &mut bytes)
                    .map_err(|_| ConfigError::InvalidScrubHmacKey)?;
                Ok::<_, ConfigError>(faultkeep_domain::SecretBytes::new(bytes))
            })
            .transpose()?;
        Ok(ResolvedSecrets {
            mongodb_uri,
            scrub_hmac_key,
        })
    }

    #[must_use]
    pub fn effective_redacted(&self) -> String {
        let uri = self
            .mongodb
            .uri
            .as_ref()
            .map_or("<not-configured>", SecretReference::redacted_origin);
        let scrub_hmac_key = self
            .projects
            .scrub_hmac_key
            .as_ref()
            .map_or("<not-configured>", SecretReference::redacted_origin);
        format!(
            "role = \"{}\"\n\n[server]\nhttp_address = \"{}\"\nshutdown_grace = \"{}\"\n\n[mongodb]\nuri = \"{}\"\ndatabase = \"{}\"\nbootstrap_timeout = \"{}\"\n\n[projects]\nscrub_hmac_key = \"{}\"\nidentity_collision_retries = {}\nmax_keys_per_project = {}\n\n[development]\nallow_literal_secrets = {}\n\n[ingest]\nmax_compressed_request_bytes = {}\nmax_decompressed_request_bytes = {}\nmax_event_bytes = {}\nmax_envelope_items = {}\nmax_active_requests = {}\nmax_parsing_tasks = {}\nmax_waiting_for_storage = {}\nrequest_timeout = \"{}\"\nunsupported_backoff_seconds = {}\n\n[ingest.project_cache]\ncapacity = {}\nmax_inflight = {}\npositive_ttl = \"{}\"\nnegative_ttl = \"{}\"\n\n[ingest.batch]\nmax_wait = \"{}\"\nmax_documents = {}\nmax_bytes = {}\n\n[ingest.event_codec]\ncompression_level = {}\ncompression_min_savings = {}\n\n[dispatcher]\nqueue_capacity = {}\nworker_concurrency = {}\nlow_watermark = {}\nrefill_target = {}\nrefill_batch_size = {}\npoll_interval = \"{}\"\nmetrics_interval = \"{}\"\nsource_timeout = \"{}\"\n",
            self.role,
            self.server.http_address,
            humantime::format_duration(self.server.shutdown_grace.get()),
            uri,
            self.mongodb.database,
            humantime::format_duration(self.mongodb.bootstrap_timeout.get()),
            scrub_hmac_key,
            self.projects.identity_collision_retries,
            self.projects.max_keys_per_project,
            self.development.allow_literal_secrets,
            self.ingest.max_compressed_request_bytes,
            self.ingest.max_decompressed_request_bytes,
            self.ingest.max_event_bytes,
            self.ingest.max_envelope_items,
            self.ingest.max_active_requests,
            self.ingest.max_parsing_tasks,
            self.ingest.max_waiting_for_storage,
            humantime::format_duration(self.ingest.request_timeout.get()),
            self.ingest.unsupported_backoff_seconds,
            self.ingest.project_cache.capacity,
            self.ingest.project_cache.max_inflight,
            humantime::format_duration(self.ingest.project_cache.positive_ttl.get()),
            humantime::format_duration(self.ingest.project_cache.negative_ttl.get()),
            humantime::format_duration(self.ingest.batch.max_wait.get()),
            self.ingest.batch.max_documents,
            self.ingest.batch.max_bytes,
            self.ingest.event_codec.compression_level,
            self.ingest.event_codec.compression_min_savings,
            self.dispatcher.queue_capacity,
            self.dispatcher.worker_concurrency,
            self.dispatcher.low_watermark,
            self.dispatcher.refill_target,
            self.dispatcher.refill_batch_size,
            humantime::format_duration(self.dispatcher.poll_interval.get()),
            humantime::format_duration(self.dispatcher.metrics_interval.get()),
            humantime::format_duration(self.dispatcher.source_timeout.get()),
        )
    }

    #[must_use]
    pub fn has_literal_secret_warning(&self) -> bool {
        matches!(self.mongodb.uri, Some(SecretReference::Literal(_)))
            || matches!(
                self.projects.scrub_hmac_key,
                Some(SecretReference::Literal(_))
            )
    }
}

impl TryFrom<RawIngestConfig> for IngestConfig {
    type Error = ConfigError;

    fn try_from(raw: RawIngestConfig) -> Result<Self, Self::Error> {
        let compressed = ConfiguredBytes::from_str(&raw.max_compressed_request_bytes)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let decompressed = ConfiguredBytes::from_str(&raw.max_decompressed_request_bytes)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let event = ConfiguredBytes::from_str(&raw.max_event_bytes)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let request_timeout = RequestTimeout::from_str(&raw.request_timeout)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let positive_ttl = ProjectCacheTtl::from_str(&raw.project_cache.positive_ttl)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let negative_ttl = ProjectCacheTtl::from_str(&raw.project_cache.negative_ttl)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let batch_wait = BatchWait::from_str(&raw.batch.max_wait)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let batch_bytes = ConfiguredBytes::from_str(&raw.batch.max_bytes)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let valid = compressed.get() > 0
            && decompressed.get() >= compressed.get()
            && event.get() > 0
            && event.get() <= decompressed.get()
            && (1..=1000).contains(&raw.max_envelope_items)
            && raw.max_active_requests > 0
            && raw.max_waiting_for_storage > 0
            && !request_timeout.get().is_zero()
            && raw.unsupported_backoff_seconds > 0;
        let valid_cache = (1..=1_000_000).contains(&raw.project_cache.capacity)
            && (1..=4096).contains(&raw.project_cache.max_inflight)
            && !positive_ttl.get().is_zero()
            && !negative_ttl.get().is_zero();
        let valid_batch = !batch_wait.get().is_zero()
            && (100..=500).contains(&raw.batch.max_documents)
            && batch_bytes.get() > 0
            && batch_bytes.get() <= 64 * 1024 * 1024;
        let valid_codec = (-7..=22).contains(&raw.event_codec.compression_level)
            && (1..=4096).contains(&raw.event_codec.compression_min_savings);
        if !valid || !valid_cache || !valid_batch || !valid_codec {
            return Err(ConfigError::InvalidIngestConfig);
        }
        Ok(Self {
            max_compressed_request_bytes: usize::try_from(compressed.get())
                .map_err(|_| ConfigError::InvalidIngestConfig)?,
            max_decompressed_request_bytes: usize::try_from(decompressed.get())
                .map_err(|_| ConfigError::InvalidIngestConfig)?,
            max_event_bytes: usize::try_from(event.get())
                .map_err(|_| ConfigError::InvalidIngestConfig)?,
            max_envelope_items: raw.max_envelope_items,
            max_active_requests: raw.max_active_requests,
            max_parsing_tasks: raw.max_parsing_tasks,
            max_waiting_for_storage: raw.max_waiting_for_storage,
            request_timeout,
            unsupported_backoff_seconds: raw.unsupported_backoff_seconds,
            project_cache: ProjectCacheSettings {
                capacity: raw.project_cache.capacity,
                max_inflight: raw.project_cache.max_inflight,
                positive_ttl,
                negative_ttl,
            },
            batch: BatchSettings {
                max_wait: batch_wait,
                max_documents: raw.batch.max_documents,
                max_bytes: usize::try_from(batch_bytes.get())
                    .map_err(|_| ConfigError::InvalidIngestConfig)?,
            },
            event_codec: EventCodecSettings {
                compression_level: raw.event_codec.compression_level,
                compression_min_savings: raw.event_codec.compression_min_savings,
            },
        })
    }
}

impl TryFrom<RawDispatcherSettings> for DispatcherSettings {
    type Error = ConfigError;

    fn try_from(raw: RawDispatcherSettings) -> Result<Self, Self::Error> {
        let poll_interval = DispatcherInterval::from_str(&raw.poll_interval)
            .map_err(|_| ConfigError::InvalidDispatcherConfig)?;
        let metrics_interval = DispatcherInterval::from_str(&raw.metrics_interval)
            .map_err(|_| ConfigError::InvalidDispatcherConfig)?;
        let source_timeout = DispatcherInterval::from_str(&raw.source_timeout)
            .map_err(|_| ConfigError::InvalidDispatcherConfig)?;
        let valid = (1..=100_000).contains(&raw.queue_capacity)
            && (1..=4_096).contains(&raw.worker_concurrency)
            && raw.worker_concurrency <= raw.queue_capacity
            && raw.low_watermark < raw.refill_target
            && raw.refill_target <= raw.queue_capacity
            && (1..=raw.refill_target.min(32_768)).contains(&raw.refill_batch_size)
            && !poll_interval.get().is_zero()
            && !metrics_interval.get().is_zero()
            && !source_timeout.get().is_zero();
        if !valid {
            return Err(ConfigError::InvalidDispatcherConfig);
        }
        Ok(Self {
            queue_capacity: raw.queue_capacity,
            worker_concurrency: raw.worker_concurrency,
            low_watermark: raw.low_watermark,
            refill_target: raw.refill_target,
            refill_batch_size: raw.refill_batch_size,
            poll_interval,
            metrics_interval,
            source_timeout,
        })
    }
}

pub struct ResolvedSecrets {
    pub mongodb_uri: Option<SecretValue>,
    pub scrub_hmac_key: Option<faultkeep_domain::SecretBytes>,
}

fn validate_secret_bytes(bytes: &[u8]) -> Result<(), ConfigError> {
    if bytes.is_empty() {
        return Err(ConfigError::EmptySecret);
    }
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(ConfigError::SecretTooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_role_is_all() {
        let cli = Cli::parse_from(["faultkeep", "--check-config"]);
        let config = load(&cli).unwrap();
        assert_eq!(config.role, Role::All);
        assert_eq!(config.server.shutdown_grace.get().as_secs(), 10);
        assert!(config.dispatcher.low_watermark < config.dispatcher.refill_target);
        assert!(config.dispatcher.refill_target <= config.dispatcher.queue_capacity);
    }

    #[test]
    fn dispatcher_watermarks_fail_closed() {
        let raw = RawConfig {
            dispatcher: RawDispatcherSettings {
                low_watermark: 100,
                refill_target: 100,
                ..RawDispatcherSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(raw),
            Err(ConfigError::InvalidDispatcherConfig)
        ));
    }

    #[test]
    fn redacted_view_never_uses_secret_debug_or_display() {
        let raw = RawConfig {
            mongodb: RawMongoConfig {
                uri: Some(SecretReference::Literal(LiteralReference {
                    literal: "do-not-print-this".to_owned(),
                })),
                ..RawMongoConfig::default()
            },
            development: RawDevelopmentConfig {
                allow_literal_secrets: true,
            },
            ..RawConfig::default()
        };
        let config = AppConfig::try_from(raw).unwrap();
        let output = config.effective_redacted();
        assert!(!output.contains("do-not-print-this"));
        assert!(output.contains("<redacted:literal>"));
    }

    #[test]
    fn production_literal_secret_is_rejected() {
        let raw = RawConfig {
            mongodb: RawMongoConfig {
                uri: Some(SecretReference::Literal(LiteralReference {
                    literal: "secret".to_owned(),
                })),
                ..RawMongoConfig::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(raw),
            Err(ConfigError::LiteralSecretForbidden)
        ));
    }

    #[test]
    fn secret_value_debug_is_always_redacted() {
        let value = SecretValue("sensitive".into());
        assert_eq!(format!("{value:?}"), "<redacted>");
        assert_eq!(value.expose(), "sensitive");
    }

    #[test]
    fn config_path_is_not_silently_ignored() {
        let cli = Cli::parse_from([
            "faultkeep",
            "--config",
            "definitely-missing-config.toml",
            "--check-config",
        ]);
        assert!(load(&cli).is_err());
    }

    #[test]
    fn secret_file_line_ending_is_removed_without_general_trimming() {
        let path = temporary_path("secret");
        fs::write(&path, " value with spaces \r\n").unwrap();
        let reference = SecretReference::File(FileReference { file: path.clone() });
        let value = reference.resolve().unwrap();
        assert_eq!(value.expose(), " value with spaces ");
        fs::remove_file(path).unwrap();
    }

    fn temporary_path(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "faultkeep-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn config_file_unknown_fields_are_errors() {
        let path = temporary_path("unknown");
        fs::write(&path, "unknown_field = true\n").unwrap();
        let cli = Cli::parse_from([
            "faultkeep",
            "--config",
            path.to_str().unwrap(),
            "--check-config",
        ]);
        assert!(load(&cli).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn path_parameter_remains_a_normal_path() {
        assert_eq!(std::path::Path::new("a.toml").extension().unwrap(), "toml");
    }
}
