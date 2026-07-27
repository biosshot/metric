use std::{env, fmt, fs, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use clap::{Parser, ValueEnum};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use metric_domain::{BoundedDuration, ByteSize};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_ENV_FILE_BYTES: u64 = 256 * 1024;
const MAX_SHUTDOWN_GRACE_MILLIS: u64 = 5 * 60 * 1_000;
const MAX_PROJECT_CACHE_TTL_MILLIS: u64 = 10 * 60 * 1_000;
const MAX_AUTH_DURATION_MILLIS: u64 = 10 * 365 * 24 * 60 * 60 * 1_000;

pub type ShutdownGrace = BoundedDuration<MAX_SHUTDOWN_GRACE_MILLIS>;
pub type RequestTimeout = BoundedDuration<60_000>;
pub type ProjectCacheTtl = BoundedDuration<MAX_PROJECT_CACHE_TTL_MILLIS>;
pub type MongoBootstrapTimeout = BoundedDuration<60_000>;
pub type BatchWait = BoundedDuration<1_000>;
pub type DispatcherInterval = BoundedDuration<60_000>;
pub type SchedulerInterval = BoundedDuration<86_400_000>;
pub type ProcessorDuration = BoundedDuration<600_000>;
pub type BacklogAge = BoundedDuration<604_800_000>;
pub type AuthDuration = BoundedDuration<MAX_AUTH_DURATION_MILLIS>;
pub type ProjectDeletionDuration = BoundedDuration<MAX_AUTH_DURATION_MILLIS>;
type ConfiguredBytes = ByteSize<{ 1024 * 1024 * 1024 }>;
type ArtifactLogicalBytes = ByteSize<{ 4_u64 * 1024 * 1024 * 1024 }>;
type ArtifactQuotaBytes = ByteSize<{ 1024_u64 * 1024 * 1024 * 1024 * 1024 }>;

#[derive(Debug, Clone, Parser)]
#[command(name = "metric", version, about = "Metric all-in-one server")]
pub struct Cli {
    /// TOML configuration file.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Explicit dotenv file. Existing process variables take precedence.
    #[arg(long, value_name = "PATH")]
    pub env_file: Option<PathBuf>,
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
    pub blob: BlobConfig,
    pub archive: ArchiveSettings,
    pub native_crash: NativeCrashConfig,
    pub symbolicator: SymbolicatorSettings,
    pub artifacts: ArtifactSettings,
    pub incident_capsule: IncidentCapsuleSettings,
    pub notifications: NotificationSettings,
    pub dispatcher: DispatcherSettings,
    pub scheduler: SchedulerSettings,
    pub retention: RetentionSettings,
    pub project_deletion: ProjectDeletionSettings,
    pub processor: ProcessorSettings,
    pub auth: AuthSettings,
}

#[derive(Debug, Clone, Copy)]
pub struct ArtifactSettings {
    pub maximum_bundle_bytes: u64,
    pub maximum_logical_bytes: u64,
    pub maximum_entries: usize,
    pub maximum_entry_bytes: u64,
    pub maximum_concurrent_assemblies: usize,
    pub parse_timeout: RequestTimeout,
    pub orphan_grace: ProjectDeletionDuration,
    pub claim_lease: ProjectDeletionDuration,
    pub blob_operation_timeout: RequestTimeout,
    pub tombstone_retention: ProjectDeletionDuration,
    pub gc_interval: SchedulerInterval,
    pub gc_batch_size: usize,
    pub gc_max_concurrency: usize,
    pub maximum_bytes_per_organization: u64,
    pub maximum_bundles_per_organization: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct IncidentCapsuleSettings {
    pub max_events: usize,
    pub max_activities: usize,
    pub max_total_uncompressed_bytes: u64,
    pub max_entry_bytes: u64,
    pub generation_timeout: RequestTimeout,
    pub max_concurrency: usize,
    pub stream_chunk_bytes: usize,
    pub stream_buffer_chunks: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct NotificationSettings {
    pub queue_capacity: usize,
    pub worker_concurrency: usize,
    pub transition_batch_size: usize,
    pub due_scan_limit: usize,
    pub poll_interval: DispatcherInterval,
    pub max_attempts: u32,
    pub initial_delay: SchedulerInterval,
    pub max_delay: SchedulerInterval,
    pub timeout: RequestTimeout,
    pub attempt_lease: RequestTimeout,
    pub delivered_retention: AuthDuration,
    pub dead_retention: AuthDuration,
    pub maximum_response_bytes: usize,
    pub maximum_retry_after: SchedulerInterval,
    pub allow_http: bool,
    pub allow_private_networks: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct NativeCrashConfig {
    pub minidump: MinidumpSettings,
}

#[derive(Debug, Clone, Copy)]
pub struct MinidumpSettings {
    pub enabled: bool,
    pub max_bytes: u64,
    pub chunk_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct SymbolicatorSettings {
    pub endpoint: Option<url::Url>,
    pub callback_base_url: url::Url,
    pub request_timeout: RequestTimeout,
    pub maximum_concurrency: usize,
    pub circuit_failure_threshold: u32,
    pub circuit_cooldown: DispatcherInterval,
    pub maximum_response_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct BlobConfig {
    pub backend: BlobBackend,
    pub root: PathBuf,
    pub capacity_bytes: u64,
    pub reserve_bytes: u64,
    pub max_object_bytes: u64,
    pub s3: S3BlobSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlobBackend {
    Local,
    S3,
}

impl fmt::Display for BlobBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => formatter.write_str("local"),
            Self::S3 => formatter.write_str("s3"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct S3BlobSettings {
    pub endpoint: Option<url::Url>,
    pub region: String,
    pub bucket: String,
    pub access_key_id: Option<SecretReference>,
    pub secret_access_key: Option<SecretReference>,
    pub session_token: Option<SecretReference>,
    pub force_path_style: bool,
    pub part_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ArchiveSettings {
    pub enabled: bool,
    pub maximum_events: usize,
    pub target_uncompressed_bytes: usize,
    pub write_chunk_bytes: usize,
    pub poll_interval: SchedulerInterval,
    pub hot_copy_delay: SchedulerInterval,
    pub orphan_grace: ProjectDeletionDuration,
    pub cleanup_max_pages: usize,
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
    pub allow_insecure_cookies: bool,
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
    pub backlog: BacklogSettings,
    pub attachments: AttachmentSettings,
}

#[derive(Debug, Clone, Copy)]
pub struct AttachmentSettings {
    pub enabled: bool,
    pub max_count: usize,
    pub max_item_bytes: usize,
    pub max_total_bytes: usize,
    pub chunk_bytes: usize,
    pub orphan_grace: SchedulerInterval,
    pub cleanup_interval: SchedulerInterval,
    pub cleanup_batch_size: usize,
    pub cleanup_max_pages: usize,
}

impl Default for AttachmentSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_count: 10,
            max_item_bytes: 1024 * 1024,
            max_total_bytes: 5 * 1024 * 1024,
            chunk_bytes: 64 * 1024,
            orphan_grace: "24h".parse().expect("default orphan grace is valid"),
            cleanup_interval: "15m".parse().expect("default cleanup interval is valid"),
            cleanup_batch_size: 256,
            cleanup_max_pages: 16,
        }
    }
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
pub struct BacklogSettings {
    pub max_pending_events: Option<u64>,
    pub max_oldest_pending_age: BacklogAge,
}

impl Default for BacklogSettings {
    fn default() -> Self {
        Self {
            max_pending_events: None,
            max_oldest_pending_age: "1h".parse().expect("default backlog age is valid"),
        }
    }
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

#[derive(Debug, Clone, Copy)]
pub struct SchedulerSettings {
    pub poll_interval: SchedulerInterval,
    pub maintenance_interval: SchedulerInterval,
    pub reconciliation_interval: SchedulerInterval,
    pub backlog_interval: SchedulerInterval,
    pub task_timeout: SchedulerInterval,
    pub retry_base: SchedulerInterval,
    pub retry_max: SchedulerInterval,
    pub batch_size: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct RetentionSettings {
    pub events_days: u32,
    pub issue_stats_hourly_days: u32,
    pub logs_days: u32,
    pub spans_days: u32,
    pub span_stats_hourly_days: u32,
    pub sessions_days: u32,
    pub session_stats_hourly_days: u32,
    pub session_active_max_hours: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ProjectDeletionSettings {
    pub grace_period: ProjectDeletionDuration,
    pub delete_batch_documents: usize,
    pub completed_job_retention: ProjectDeletionDuration,
    pub slug_reservation: ProjectDeletionDuration,
    pub poll_interval: ProjectDeletionDuration,
    pub operation_timeout: ProjectDeletionDuration,
    pub drain_timeout: ProjectDeletionDuration,
    pub retry_base: ProjectDeletionDuration,
    pub retry_max: ProjectDeletionDuration,
}

impl RetentionSettings {
    #[must_use]
    pub const fn event_duration(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.events_days as u64 * 24 * 60 * 60)
    }

    #[must_use]
    pub const fn hourly_duration(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.issue_stats_hourly_days as u64 * 24 * 60 * 60)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessorSettings {
    pub max_concurrency: usize,
    pub max_attempts: u32,
    pub retry_base: ProcessorDuration,
    pub retry_max: ProcessorDuration,
    pub stage_timeout: ProcessorDuration,
    pub total_timeout: ProcessorDuration,
    pub state_timeout: ProcessorDuration,
}

#[derive(Debug, Clone, Copy)]
pub struct AuthSettings {
    pub identity_collision_retries: usize,
    pub store_timeout: AuthDuration,
    pub setup_token_timeout: AuthDuration,
    pub max_api_token_lifetime: AuthDuration,
    pub activity_touch_interval: AuthDuration,
    pub secure_cookie: bool,
    pub session_idle_timeout: AuthDuration,
    pub session_absolute_timeout: AuthDuration,
    pub password_memory_kib: u32,
    pub password_iterations: u32,
    pub password_parallelism: u32,
    pub password_max_concurrency: usize,
    pub login_max_attempts: u32,
    pub login_window: AuthDuration,
    pub login_capacity: usize,
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

impl Default for SchedulerSettings {
    fn default() -> Self {
        Self {
            poll_interval: "1s".parse().expect("default Scheduler poll is valid"),
            maintenance_interval: "1m".parse().expect("default maintenance interval is valid"),
            reconciliation_interval: "5m"
                .parse()
                .expect("default reconciliation interval is valid"),
            backlog_interval: "5s".parse().expect("default backlog interval is valid"),
            task_timeout: "10s".parse().expect("default task timeout is valid"),
            retry_base: "1s".parse().expect("default retry base is valid"),
            retry_max: "1m".parse().expect("default retry maximum is valid"),
            batch_size: 500,
        }
    }
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            events_days: 30,
            issue_stats_hourly_days: 400,
            logs_days: 30,
            spans_days: 30,
            span_stats_hourly_days: 90,
            sessions_days: 7,
            session_stats_hourly_days: 400,
            session_active_max_hours: 24,
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
    blob: RawBlobConfig,
    archive: RawArchiveSettings,
    native_crash: RawNativeCrashConfig,
    symbolicator: RawSymbolicatorSettings,
    artifacts: RawArtifactSettings,
    incident_capsule: RawIncidentCapsuleSettings,
    notifications: RawNotificationSettings,
    dispatcher: RawDispatcherSettings,
    scheduler: RawSchedulerSettings,
    retention: RawRetentionSettings,
    project_deletion: RawProjectDeletionSettings,
    processor: RawProcessorSettings,
    auth: RawAuthSettings,
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
            blob: RawBlobConfig::default(),
            archive: RawArchiveSettings::default(),
            native_crash: RawNativeCrashConfig::default(),
            symbolicator: RawSymbolicatorSettings::default(),
            artifacts: RawArtifactSettings::default(),
            incident_capsule: RawIncidentCapsuleSettings::default(),
            notifications: RawNotificationSettings::default(),
            dispatcher: RawDispatcherSettings::default(),
            scheduler: RawSchedulerSettings::default(),
            retention: RawRetentionSettings::default(),
            project_deletion: RawProjectDeletionSettings::default(),
            processor: RawProcessorSettings::default(),
            auth: RawAuthSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawNotificationSettings {
    queue: RawNotificationQueueSettings,
    retry: RawNotificationRetrySettings,
    retention: RawNotificationRetentionSettings,
    webhook: RawNotificationWebhookSettings,
    transition_batch_size: usize,
    due_scan_limit: usize,
    poll_interval: String,
}

impl Default for RawNotificationSettings {
    fn default() -> Self {
        Self {
            queue: RawNotificationQueueSettings::default(),
            retry: RawNotificationRetrySettings::default(),
            retention: RawNotificationRetentionSettings::default(),
            webhook: RawNotificationWebhookSettings::default(),
            transition_batch_size: 100,
            due_scan_limit: 100,
            poll_interval: "250ms".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawNotificationQueueSettings {
    capacity: usize,
    worker_concurrency: usize,
}

impl Default for RawNotificationQueueSettings {
    fn default() -> Self {
        Self {
            capacity: 1_000,
            worker_concurrency: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawNotificationRetrySettings {
    max_attempts: u32,
    initial_delay: String,
    max_delay: String,
    timeout: String,
    attempt_lease: String,
}

impl Default for RawNotificationRetrySettings {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            initial_delay: "5s".to_owned(),
            max_delay: "1h".to_owned(),
            timeout: "10s".to_owned(),
            attempt_lease: "30s".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawNotificationRetentionSettings {
    delivered_days: u32,
    dead_days: u32,
}

impl Default for RawNotificationRetentionSettings {
    fn default() -> Self {
        Self {
            delivered_days: 30,
            dead_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawNotificationWebhookSettings {
    maximum_response_bytes: String,
    maximum_retry_after: String,
    allow_http: bool,
    allow_private_networks: bool,
}

impl Default for RawNotificationWebhookSettings {
    fn default() -> Self {
        Self {
            maximum_response_bytes: "64 KiB".to_owned(),
            maximum_retry_after: "1h".to_owned(),
            allow_http: false,
            allow_private_networks: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawIncidentCapsuleSettings {
    max_events: usize,
    max_activities: usize,
    max_total_uncompressed_bytes: String,
    max_entry_bytes: String,
    generation_timeout: String,
    max_concurrency: usize,
    stream_chunk_bytes: String,
    stream_buffer_chunks: usize,
}

impl Default for RawIncidentCapsuleSettings {
    fn default() -> Self {
        Self {
            max_events: 10,
            max_activities: 100,
            max_total_uncompressed_bytes: "100 MiB".to_owned(),
            max_entry_bytes: "16 MiB".to_owned(),
            generation_timeout: "30s".to_owned(),
            max_concurrency: 4,
            stream_chunk_bytes: "64 KiB".to_owned(),
            stream_buffer_chunks: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawArtifactSettings {
    maximum_bundle_bytes: String,
    maximum_logical_bytes: String,
    maximum_entries: usize,
    maximum_entry_bytes: String,
    maximum_concurrent_assemblies: usize,
    parse_timeout: String,
    orphan_grace: String,
    claim_lease: String,
    blob_operation_timeout: String,
    tombstone_retention: String,
    gc_interval: String,
    gc_batch_size: usize,
    gc_max_concurrency: usize,
    maximum_bytes_per_organization: String,
    maximum_bundles_per_organization: u64,
}

impl Default for RawArtifactSettings {
    fn default() -> Self {
        Self {
            maximum_bundle_bytes: "64 MiB".to_owned(),
            maximum_logical_bytes: "512 MiB".to_owned(),
            maximum_entries: 10_000,
            maximum_entry_bytes: "16 MiB".to_owned(),
            maximum_concurrent_assemblies: 2,
            parse_timeout: "30s".to_owned(),
            orphan_grace: "24h".to_owned(),
            claim_lease: "5m".to_owned(),
            blob_operation_timeout: "30s".to_owned(),
            tombstone_retention: "24h".to_owned(),
            gc_interval: "15m".to_owned(),
            gc_batch_size: 100,
            gc_max_concurrency: 4,
            maximum_bytes_per_organization: "0 B".to_owned(),
            maximum_bundles_per_organization: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawNativeCrashConfig {
    minidump: RawMinidumpSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawSymbolicatorSettings {
    endpoint: Option<String>,
    callback_base_url: String,
    request_timeout: String,
    maximum_concurrency: usize,
    circuit_failure_threshold: u32,
    circuit_cooldown: String,
    maximum_response_bytes: String,
}

impl Default for RawSymbolicatorSettings {
    fn default() -> Self {
        Self {
            endpoint: None,
            callback_base_url: "http://127.0.0.1:3000/".to_owned(),
            request_timeout: "20s".to_owned(),
            maximum_concurrency: 8,
            circuit_failure_threshold: 5,
            circuit_cooldown: "30s".to_owned(),
            maximum_response_bytes: "4 MiB".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawMinidumpSettings {
    enabled: bool,
    max_bytes: String,
    chunk_bytes: String,
}

impl Default for RawMinidumpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bytes: "100 MiB".to_owned(),
            chunk_bytes: "64 KiB".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawBlobConfig {
    backend: BlobBackend,
    root: PathBuf,
    capacity: String,
    reserve: String,
    max_object_bytes: String,
    s3: RawS3BlobSettings,
}

impl Default for RawBlobConfig {
    fn default() -> Self {
        Self {
            backend: BlobBackend::Local,
            root: PathBuf::from("./metric-data/blobs"),
            capacity: "1 GiB".to_owned(),
            reserve: "128 MiB".to_owned(),
            max_object_bytes: "100 MiB".to_owned(),
            s3: RawS3BlobSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawS3BlobSettings {
    endpoint: Option<String>,
    region: String,
    bucket: String,
    access_key_id: Option<SecretReference>,
    secret_access_key: Option<SecretReference>,
    session_token: Option<SecretReference>,
    force_path_style: bool,
    part_bytes: String,
}

impl Default for RawS3BlobSettings {
    fn default() -> Self {
        Self {
            endpoint: None,
            region: "us-east-1".to_owned(),
            bucket: "metric".to_owned(),
            access_key_id: None,
            secret_access_key: None,
            session_token: None,
            force_path_style: true,
            part_bytes: "8 MiB".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawArchiveSettings {
    enabled: bool,
    maximum_events: usize,
    target_uncompressed_bytes: String,
    write_chunk_bytes: String,
    poll_interval: String,
    hot_copy_delay: String,
    orphan_grace: String,
    cleanup_max_pages: usize,
}

impl Default for RawArchiveSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            maximum_events: 500,
            target_uncompressed_bytes: "64 MiB".to_owned(),
            write_chunk_bytes: "256 KiB".to_owned(),
            poll_interval: "30s".to_owned(),
            hot_copy_delay: "0s".to_owned(),
            orphan_grace: "24h".to_owned(),
            cleanup_max_pages: 4,
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
            database: "metric".to_owned(),
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
    allow_insecure_cookies: bool,
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
    backlog: RawBacklogSettings,
    attachments: RawAttachmentSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawAttachmentSettings {
    enabled: bool,
    max_count: usize,
    max_item_bytes: String,
    max_total_bytes: String,
    chunk_bytes: String,
    orphan_grace: String,
    cleanup_interval: String,
    cleanup_batch_size: usize,
    cleanup_max_pages: usize,
}

impl Default for RawAttachmentSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_count: 10,
            max_item_bytes: "1 MiB".to_owned(),
            max_total_bytes: "5 MiB".to_owned(),
            chunk_bytes: "64 KiB".to_owned(),
            orphan_grace: "24h".to_owned(),
            cleanup_interval: "15m".to_owned(),
            cleanup_batch_size: 256,
            cleanup_max_pages: 16,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawProcessorSettings {
    max_concurrency: usize,
    max_attempts: u32,
    retry_base: String,
    retry_max: String,
    stage_timeout: String,
    total_timeout: String,
    state_timeout: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawAuthSettings {
    identity_collision_retries: usize,
    store_timeout: String,
    setup_token_timeout: String,
    max_api_token_lifetime: String,
    activity_touch_interval: String,
    secure_cookie: bool,
    session: RawAuthSessionSettings,
    password: RawAuthPasswordSettings,
    login: RawAuthLoginSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawAuthSessionSettings {
    idle_timeout: String,
    absolute_timeout: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawAuthPasswordSettings {
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    max_concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawAuthLoginSettings {
    max_attempts: u32,
    window: String,
    capacity: usize,
}

impl Default for RawAuthSettings {
    fn default() -> Self {
        Self {
            identity_collision_retries: 16,
            store_timeout: "5s".to_owned(),
            setup_token_timeout: "24h".to_owned(),
            max_api_token_lifetime: "365d".to_owned(),
            activity_touch_interval: "5m".to_owned(),
            secure_cookie: true,
            session: RawAuthSessionSettings::default(),
            password: RawAuthPasswordSettings::default(),
            login: RawAuthLoginSettings::default(),
        }
    }
}

impl Default for RawAuthSessionSettings {
    fn default() -> Self {
        Self {
            idle_timeout: "7d".to_owned(),
            absolute_timeout: "30d".to_owned(),
        }
    }
}

impl Default for RawAuthPasswordSettings {
    fn default() -> Self {
        Self {
            memory_kib: 19 * 1024,
            iterations: 2,
            parallelism: 1,
            max_concurrency: 2,
        }
    }
}

impl Default for RawAuthLoginSettings {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            window: "1m".to_owned(),
            capacity: 10_000,
        }
    }
}

impl Default for RawProcessorSettings {
    fn default() -> Self {
        Self {
            max_concurrency: 32,
            max_attempts: 5,
            retry_base: "1s".to_owned(),
            retry_max: "5m".to_owned(),
            stage_timeout: "15s".to_owned(),
            total_timeout: "1m".to_owned(),
            state_timeout: "5s".to_owned(),
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawSchedulerSettings {
    poll_interval: String,
    maintenance_interval: String,
    reconciliation_interval: String,
    backlog_interval: String,
    task_timeout: String,
    retry_base: String,
    retry_max: String,
    batch_size: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRetentionSettings {
    events_days: u32,
    issue_stats_hourly_days: u32,
    logs_days: u32,
    spans_days: u32,
    span_stats_hourly_days: u32,
    sessions_days: u32,
    session_stats_hourly_days: u32,
    session_active_max_hours: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawProjectDeletionSettings {
    grace_period: String,
    delete_batch_documents: usize,
    completed_job_retention: String,
    slug_reservation: String,
    poll_interval: String,
    operation_timeout: String,
    drain_timeout: String,
    retry_base: String,
    retry_max: String,
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

impl Default for RawSchedulerSettings {
    fn default() -> Self {
        Self {
            poll_interval: "1s".to_owned(),
            maintenance_interval: "1m".to_owned(),
            reconciliation_interval: "5m".to_owned(),
            backlog_interval: "5s".to_owned(),
            task_timeout: "10s".to_owned(),
            retry_base: "1s".to_owned(),
            retry_max: "1m".to_owned(),
            batch_size: 500,
        }
    }
}

impl Default for RawRetentionSettings {
    fn default() -> Self {
        Self {
            events_days: 30,
            issue_stats_hourly_days: 400,
            logs_days: 30,
            spans_days: 30,
            span_stats_hourly_days: 90,
            sessions_days: 7,
            session_stats_hourly_days: 400,
            session_active_max_hours: 24,
        }
    }
}

impl Default for RawProjectDeletionSettings {
    fn default() -> Self {
        Self {
            grace_period: "24h".to_owned(),
            delete_batch_documents: 5_000,
            completed_job_retention: "30d".to_owned(),
            slug_reservation: "30d".to_owned(),
            poll_interval: "1s".to_owned(),
            operation_timeout: "10s".to_owned(),
            drain_timeout: "10s".to_owned(),
            retry_base: "1s".to_owned(),
            retry_max: "1m".to_owned(),
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
            backlog: RawBacklogSettings::default(),
            attachments: RawAttachmentSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawBacklogSettings {
    max_pending_events: Option<u64>,
    max_oldest_pending_age: String,
}

impl Default for RawBacklogSettings {
    fn default() -> Self {
        Self {
            max_pending_events: None,
            max_oldest_pending_age: "1h".to_owned(),
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
    #[error("environment file does not exist or is not a regular file")]
    MissingEnvironmentFile,
    #[error("environment file exceeds the 256 KiB limit")]
    EnvironmentFileTooLarge,
    #[error("environment file could not be parsed")]
    InvalidEnvironmentFile,
    #[error("server.shutdown_grace is invalid or exceeds five minutes")]
    InvalidShutdownGrace,
    #[error("ingest configuration is invalid or outside supported bounds")]
    InvalidIngestConfig,
    #[error("blob configuration is invalid or outside supported bounds")]
    InvalidBlobConfig,
    #[error("cold archive configuration is invalid or outside supported bounds")]
    InvalidArchiveConfig,
    #[error("native crash configuration is invalid or outside supported bounds")]
    InvalidNativeCrashConfig,
    #[error("Symbolicator configuration is invalid or outside supported bounds")]
    InvalidSymbolicatorConfig,
    #[error("artifact bundle configuration is invalid or outside supported bounds")]
    InvalidArtifactConfig,
    #[error("Incident Capsule configuration is invalid or outside supported bounds")]
    InvalidIncidentCapsuleConfig,
    #[error("notification configuration is invalid or outside supported bounds")]
    InvalidNotificationConfig,
    #[error("MongoDB configuration is invalid or outside supported bounds")]
    InvalidMongoConfig,
    #[error("project identity configuration is invalid or outside supported bounds")]
    InvalidProjectConfig,
    #[error("dispatcher configuration is invalid or outside supported bounds")]
    InvalidDispatcherConfig,
    #[error("scheduler configuration is invalid or outside supported bounds")]
    InvalidSchedulerConfig,
    #[error("retention configuration is invalid or outside supported bounds")]
    InvalidRetentionConfig,
    #[error("project deletion configuration is invalid or outside supported bounds")]
    InvalidProjectDeletionConfig,
    #[error("processor configuration is invalid or outside supported bounds")]
    InvalidProcessorConfig,
    #[error("auth configuration is invalid or outside supported bounds")]
    InvalidAuthConfig,
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
    if let Some(path) = &cli.env_file {
        load_environment_file(path)?;
    }
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

fn load_environment_file(path: &std::path::Path) -> Result<(), ConfigError> {
    let metadata = fs::metadata(path).map_err(|_| ConfigError::MissingEnvironmentFile)?;
    if !metadata.is_file() {
        return Err(ConfigError::MissingEnvironmentFile);
    }
    if metadata.len() > MAX_ENV_FILE_BYTES {
        return Err(ConfigError::EnvironmentFileTooLarge);
    }
    dotenvy::from_path(path)
        .map(|_| ())
        .map_err(|_| ConfigError::InvalidEnvironmentFile)
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
        let capacity = ConfiguredBytes::from_str(&raw.blob.capacity)
            .map_err(|_| ConfigError::InvalidBlobConfig)?;
        let reserve = ConfiguredBytes::from_str(&raw.blob.reserve)
            .map_err(|_| ConfigError::InvalidBlobConfig)?;
        let max_object = ConfiguredBytes::from_str(&raw.blob.max_object_bytes)
            .map_err(|_| ConfigError::InvalidBlobConfig)?;
        for reference in [
            raw.blob.s3.access_key_id.as_ref(),
            raw.blob.s3.secret_access_key.as_ref(),
            raw.blob.s3.session_token.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            reference.validate(raw.development.allow_literal_secrets)?;
        }
        let s3_endpoint = raw
            .blob
            .s3
            .endpoint
            .as_deref()
            .map(url::Url::parse)
            .transpose()
            .map_err(|_| ConfigError::InvalidBlobConfig)?;
        let s3_part = ConfiguredBytes::from_str(&raw.blob.s3.part_bytes)
            .map_err(|_| ConfigError::InvalidBlobConfig)?;
        let archive_target = ConfiguredBytes::from_str(&raw.archive.target_uncompressed_bytes)
            .map_err(|_| ConfigError::InvalidArchiveConfig)?;
        let archive_chunk = ConfiguredBytes::from_str(&raw.archive.write_chunk_bytes)
            .map_err(|_| ConfigError::InvalidArchiveConfig)?;
        let archive_poll = SchedulerInterval::from_str(&raw.archive.poll_interval)
            .map_err(|_| ConfigError::InvalidArchiveConfig)?;
        let archive_hot_copy_delay = SchedulerInterval::from_str(&raw.archive.hot_copy_delay)
            .map_err(|_| ConfigError::InvalidArchiveConfig)?;
        let archive_orphan_grace = ProjectDeletionDuration::from_str(&raw.archive.orphan_grace)
            .map_err(|_| ConfigError::InvalidArchiveConfig)?;
        let minidump_max = ConfiguredBytes::from_str(&raw.native_crash.minidump.max_bytes)
            .map_err(|_| ConfigError::InvalidNativeCrashConfig)?;
        let minidump_chunk = ConfiguredBytes::from_str(&raw.native_crash.minidump.chunk_bytes)
            .map_err(|_| ConfigError::InvalidNativeCrashConfig)?;
        let symbolicator_endpoint = raw
            .symbolicator
            .endpoint
            .as_deref()
            .map(url::Url::parse)
            .transpose()
            .map_err(|_| ConfigError::InvalidSymbolicatorConfig)?;
        let callback_base_url = url::Url::parse(&raw.symbolicator.callback_base_url)
            .map_err(|_| ConfigError::InvalidSymbolicatorConfig)?;
        let symbolicator_timeout = RequestTimeout::from_str(&raw.symbolicator.request_timeout)
            .map_err(|_| ConfigError::InvalidSymbolicatorConfig)?;
        let symbolicator_cooldown =
            DispatcherInterval::from_str(&raw.symbolicator.circuit_cooldown)
                .map_err(|_| ConfigError::InvalidSymbolicatorConfig)?;
        let symbolicator_response =
            ConfiguredBytes::from_str(&raw.symbolicator.maximum_response_bytes)
                .map_err(|_| ConfigError::InvalidSymbolicatorConfig)?;
        let artifact_bundle_bytes =
            ArtifactLogicalBytes::from_str(&raw.artifacts.maximum_bundle_bytes)
                .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_logical_bytes =
            ArtifactLogicalBytes::from_str(&raw.artifacts.maximum_logical_bytes)
                .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_entry_bytes =
            ArtifactLogicalBytes::from_str(&raw.artifacts.maximum_entry_bytes)
                .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_quota_bytes =
            ArtifactQuotaBytes::from_str(&raw.artifacts.maximum_bytes_per_organization)
                .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_parse_timeout = RequestTimeout::from_str(&raw.artifacts.parse_timeout)
            .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_orphan_grace = ProjectDeletionDuration::from_str(&raw.artifacts.orphan_grace)
            .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_claim_lease = ProjectDeletionDuration::from_str(&raw.artifacts.claim_lease)
            .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_blob_timeout = RequestTimeout::from_str(&raw.artifacts.blob_operation_timeout)
            .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_tombstone =
            ProjectDeletionDuration::from_str(&raw.artifacts.tombstone_retention)
                .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let artifact_gc_interval = SchedulerInterval::from_str(&raw.artifacts.gc_interval)
            .map_err(|_| ConfigError::InvalidArtifactConfig)?;
        let capsule_total =
            ConfiguredBytes::from_str(&raw.incident_capsule.max_total_uncompressed_bytes)
                .map_err(|_| ConfigError::InvalidIncidentCapsuleConfig)?;
        let capsule_entry = ConfiguredBytes::from_str(&raw.incident_capsule.max_entry_bytes)
            .map_err(|_| ConfigError::InvalidIncidentCapsuleConfig)?;
        let capsule_timeout = RequestTimeout::from_str(&raw.incident_capsule.generation_timeout)
            .map_err(|_| ConfigError::InvalidIncidentCapsuleConfig)?;
        let capsule_chunk = ConfiguredBytes::from_str(&raw.incident_capsule.stream_chunk_bytes)
            .map_err(|_| ConfigError::InvalidIncidentCapsuleConfig)?;
        let notifications = NotificationSettings::try_from(raw.notifications)?;
        if raw.blob.root.as_os_str().is_empty()
            || capacity.get() == 0
            || reserve.get() >= capacity.get()
            || max_object.get() == 0
            || max_object.get() > capacity.get() - reserve.get()
        {
            return Err(ConfigError::InvalidBlobConfig);
        }
        let valid_s3_bucket = (3..=63).contains(&raw.blob.s3.bucket.len())
            && raw.blob.s3.bucket.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
            })
            && raw
                .blob
                .s3
                .bucket
                .as_bytes()
                .first()
                .zip(raw.blob.s3.bucket.as_bytes().last())
                .is_some_and(|(first, last)| {
                    first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
                });
        let valid_s3 = !raw.blob.s3.region.is_empty()
            && raw.blob.s3.region.len() <= 64
            && valid_s3_bucket
            && s3_endpoint
                .as_ref()
                .is_none_or(|endpoint| matches!(endpoint.scheme(), "http" | "https"))
            && (5 * 1024 * 1024..=64 * 1024 * 1024).contains(&s3_part.get())
            && (raw.blob.backend != BlobBackend::S3
                || (raw.blob.s3.access_key_id.is_some()
                    && raw.blob.s3.secret_access_key.is_some()));
        if !valid_s3 {
            return Err(ConfigError::InvalidBlobConfig);
        }
        let valid_archive = (1..=10_000).contains(&raw.archive.maximum_events)
            && (1024..=512 * 1024 * 1024).contains(&archive_target.get())
            && archive_target.get() <= max_object.get()
            && (4096..=1024 * 1024).contains(&archive_chunk.get())
            && !archive_poll.get().is_zero()
            && archive_hot_copy_delay.get() <= Duration::from_secs(24 * 60 * 60)
            && !archive_orphan_grace.get().is_zero()
            && (1..=1_024).contains(&raw.archive.cleanup_max_pages)
            && (!raw.archive.enabled || raw.mongodb.uri.is_some());
        if !valid_archive {
            return Err(ConfigError::InvalidArchiveConfig);
        }
        if minidump_max.get() == 0
            || minidump_max.get() > max_object.get()
            || !(4 * 1024..=1024 * 1024).contains(&minidump_chunk.get())
        {
            return Err(ConfigError::InvalidNativeCrashConfig);
        }
        let valid_symbolicator_urls = symbolicator_endpoint
            .iter()
            .chain(std::iter::once(&callback_base_url))
            .all(|url| matches!(url.scheme(), "http" | "https"));
        if !valid_symbolicator_urls
            || symbolicator_timeout.get().is_zero()
            || !(1..=1024).contains(&raw.symbolicator.maximum_concurrency)
            || raw.symbolicator.circuit_failure_threshold == 0
            || symbolicator_cooldown.get().is_zero()
            || !(1024..=16 * 1024 * 1024).contains(&symbolicator_response.get())
        {
            return Err(ConfigError::InvalidSymbolicatorConfig);
        }
        if artifact_bundle_bytes.get() == 0
            || artifact_bundle_bytes.get() > 512 * 1024 * 1024
            || artifact_logical_bytes.get() < artifact_bundle_bytes.get()
            || artifact_entry_bytes.get() == 0
            || artifact_entry_bytes.get() > artifact_logical_bytes.get()
            || !(1..=100_000).contains(&raw.artifacts.maximum_entries)
            || !(1..=64).contains(&raw.artifacts.maximum_concurrent_assemblies)
            || artifact_parse_timeout.get().is_zero()
            || artifact_orphan_grace.get().is_zero()
            || artifact_claim_lease.get() <= artifact_blob_timeout.get()
            || artifact_tombstone.get() <= artifact_claim_lease.get()
            || artifact_gc_interval.get().is_zero()
            || !(1..=100).contains(&raw.artifacts.gc_batch_size)
            || !(1..=4).contains(&raw.artifacts.gc_max_concurrency)
            || raw.artifacts.maximum_bundles_per_organization > 1_000_000_000
        {
            return Err(ConfigError::InvalidArtifactConfig);
        }
        if !(1..=10).contains(&raw.incident_capsule.max_events)
            || !(1..=100).contains(&raw.incident_capsule.max_activities)
            || capsule_total.get() == 0
            || capsule_total.get() > 100 * 1024 * 1024
            || capsule_entry.get() == 0
            || capsule_entry.get() > 16 * 1024 * 1024
            || capsule_entry.get() > capsule_total.get()
            || capsule_timeout.get().is_zero()
            || capsule_timeout.get() > std::time::Duration::from_secs(30)
            || !(1..=4).contains(&raw.incident_capsule.max_concurrency)
            || !(4 * 1024..=1024 * 1024).contains(&capsule_chunk.get())
            || !(1..=16).contains(&raw.incident_capsule.stream_buffer_chunks)
        {
            return Err(ConfigError::InvalidIncidentCapsuleConfig);
        }
        let dispatcher = DispatcherSettings::try_from(raw.dispatcher)?;
        let scheduler = SchedulerSettings::try_from(raw.scheduler)?;
        let retention = RetentionSettings::try_from(raw.retention)?;
        let project_deletion = ProjectDeletionSettings::try_from(raw.project_deletion)?;
        let processor = ProcessorSettings::try_from(raw.processor)?;
        let auth = AuthSettings::try_from(raw.auth)?;
        if !auth.secure_cookie && !raw.development.allow_insecure_cookies {
            return Err(ConfigError::InvalidAuthConfig);
        }
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
                allow_insecure_cookies: raw.development.allow_insecure_cookies,
            },
            ingest,
            blob: BlobConfig {
                backend: raw.blob.backend,
                root: raw.blob.root,
                capacity_bytes: capacity.get(),
                reserve_bytes: reserve.get(),
                max_object_bytes: max_object.get(),
                s3: S3BlobSettings {
                    endpoint: s3_endpoint,
                    region: raw.blob.s3.region,
                    bucket: raw.blob.s3.bucket,
                    access_key_id: raw.blob.s3.access_key_id,
                    secret_access_key: raw.blob.s3.secret_access_key,
                    session_token: raw.blob.s3.session_token,
                    force_path_style: raw.blob.s3.force_path_style,
                    part_bytes: usize::try_from(s3_part.get())
                        .map_err(|_| ConfigError::InvalidBlobConfig)?,
                },
            },
            archive: ArchiveSettings {
                enabled: raw.archive.enabled,
                maximum_events: raw.archive.maximum_events,
                target_uncompressed_bytes: usize::try_from(archive_target.get())
                    .map_err(|_| ConfigError::InvalidArchiveConfig)?,
                write_chunk_bytes: usize::try_from(archive_chunk.get())
                    .map_err(|_| ConfigError::InvalidArchiveConfig)?,
                poll_interval: archive_poll,
                hot_copy_delay: archive_hot_copy_delay,
                orphan_grace: archive_orphan_grace,
                cleanup_max_pages: raw.archive.cleanup_max_pages,
            },
            native_crash: NativeCrashConfig {
                minidump: MinidumpSettings {
                    enabled: raw.native_crash.minidump.enabled,
                    max_bytes: minidump_max.get(),
                    chunk_bytes: usize::try_from(minidump_chunk.get())
                        .map_err(|_| ConfigError::InvalidNativeCrashConfig)?,
                },
            },
            symbolicator: SymbolicatorSettings {
                endpoint: symbolicator_endpoint,
                callback_base_url,
                request_timeout: symbolicator_timeout,
                maximum_concurrency: raw.symbolicator.maximum_concurrency,
                circuit_failure_threshold: raw.symbolicator.circuit_failure_threshold,
                circuit_cooldown: symbolicator_cooldown,
                maximum_response_bytes: usize::try_from(symbolicator_response.get())
                    .map_err(|_| ConfigError::InvalidSymbolicatorConfig)?,
            },
            artifacts: ArtifactSettings {
                maximum_bundle_bytes: artifact_bundle_bytes.get(),
                maximum_logical_bytes: artifact_logical_bytes.get(),
                maximum_entries: raw.artifacts.maximum_entries,
                maximum_entry_bytes: artifact_entry_bytes.get(),
                maximum_concurrent_assemblies: raw.artifacts.maximum_concurrent_assemblies,
                parse_timeout: artifact_parse_timeout,
                orphan_grace: artifact_orphan_grace,
                claim_lease: artifact_claim_lease,
                blob_operation_timeout: artifact_blob_timeout,
                tombstone_retention: artifact_tombstone,
                gc_interval: artifact_gc_interval,
                gc_batch_size: raw.artifacts.gc_batch_size,
                gc_max_concurrency: raw.artifacts.gc_max_concurrency,
                maximum_bytes_per_organization: artifact_quota_bytes.get(),
                maximum_bundles_per_organization: raw.artifacts.maximum_bundles_per_organization,
            },
            incident_capsule: IncidentCapsuleSettings {
                max_events: raw.incident_capsule.max_events,
                max_activities: raw.incident_capsule.max_activities,
                max_total_uncompressed_bytes: capsule_total.get(),
                max_entry_bytes: capsule_entry.get(),
                generation_timeout: capsule_timeout,
                max_concurrency: raw.incident_capsule.max_concurrency,
                stream_chunk_bytes: usize::try_from(capsule_chunk.get())
                    .map_err(|_| ConfigError::InvalidIncidentCapsuleConfig)?,
                stream_buffer_chunks: raw.incident_capsule.stream_buffer_chunks,
            },
            notifications,
            dispatcher,
            scheduler,
            retention,
            project_deletion,
            processor,
            auth,
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
        let s3_access_key_id = self
            .blob
            .s3
            .access_key_id
            .as_ref()
            .map(SecretReference::resolve)
            .transpose()?;
        let s3_secret_access_key = self
            .blob
            .s3
            .secret_access_key
            .as_ref()
            .map(SecretReference::resolve)
            .transpose()?;
        let s3_session_token = self
            .blob
            .s3
            .session_token
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
                Ok::<_, ConfigError>(metric_domain::SecretBytes::new(bytes))
            })
            .transpose()?;
        Ok(ResolvedSecrets {
            mongodb_uri,
            scrub_hmac_key,
            s3_access_key_id,
            s3_secret_access_key,
            s3_session_token,
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
        let rendered = format!(
            "role = \"{}\"\n\n[server]\nhttp_address = \"{}\"\nshutdown_grace = \"{}\"\n\n[mongodb]\nuri = \"{}\"\ndatabase = \"{}\"\nbootstrap_timeout = \"{}\"\n\n[projects]\nscrub_hmac_key = \"{}\"\nidentity_collision_retries = {}\nmax_keys_per_project = {}\n\n[development]\nallow_literal_secrets = {}\nallow_insecure_cookies = {}\n\n[blob]\nbackend = \"{}\"\nroot = \"{}\"\ncapacity = {}\nreserve = {}\nmax_object_bytes = {}\n\n[native_crash.minidump]\nenabled = {}\nmax_bytes = {}\nchunk_bytes = {}\n\n[ingest]\nmax_compressed_request_bytes = {}\nmax_decompressed_request_bytes = {}\nmax_event_bytes = {}\nmax_envelope_items = {}\nmax_active_requests = {}\nmax_parsing_tasks = {}\nmax_waiting_for_storage = {}\nrequest_timeout = \"{}\"\nunsupported_backoff_seconds = {}\n\n[ingest.attachments]\nenabled = {}\nmax_count = {}\nmax_item_bytes = {}\nmax_total_bytes = {}\nchunk_bytes = {}\norphan_grace = \"{}\"\ncleanup_interval = \"{}\"\ncleanup_batch_size = {}\ncleanup_max_pages = {}\n\n[ingest.project_cache]\ncapacity = {}\nmax_inflight = {}\npositive_ttl = \"{}\"\nnegative_ttl = \"{}\"\n\n[ingest.batch]\nmax_wait = \"{}\"\nmax_documents = {}\nmax_bytes = {}\n\n[ingest.event_codec]\ncompression_level = {}\ncompression_min_savings = {}\n\n[ingest.backlog]\nmax_pending_events = {}\nmax_oldest_pending_age = \"{}\"\n\n[dispatcher]\nqueue_capacity = {}\nworker_concurrency = {}\nlow_watermark = {}\nrefill_target = {}\nrefill_batch_size = {}\npoll_interval = \"{}\"\nmetrics_interval = \"{}\"\nsource_timeout = \"{}\"\n\n[scheduler]\npoll_interval = \"{}\"\nmaintenance_interval = \"{}\"\nreconciliation_interval = \"{}\"\nbacklog_interval = \"{}\"\ntask_timeout = \"{}\"\nretry_base = \"{}\"\nretry_max = \"{}\"\nbatch_size = {}\n\n[retention]\nevents_days = {}\nissue_stats_hourly_days = {}\nlogs_days = {}\nspans_days = {}\nspan_stats_hourly_days = {}\nsessions_days = {}\nsession_stats_hourly_days = {}\nsession_active_max_hours = {}\n\n[project_deletion]\ngrace_period = \"{}\"\ndelete_batch_documents = {}\ncompleted_job_retention = \"{}\"\nslug_reservation = \"{}\"\npoll_interval = \"{}\"\noperation_timeout = \"{}\"\ndrain_timeout = \"{}\"\nretry_base = \"{}\"\nretry_max = \"{}\"\n\n[processor]\nmax_concurrency = {}\nmax_attempts = {}\nretry_base = \"{}\"\nretry_max = \"{}\"\nstage_timeout = \"{}\"\ntotal_timeout = \"{}\"\nstate_timeout = \"{}\"\n\n[auth]\nidentity_collision_retries = {}\nstore_timeout = \"{}\"\nsetup_token_timeout = \"{}\"\nmax_api_token_lifetime = \"{}\"\nactivity_touch_interval = \"{}\"\nsecure_cookie = {}\n\n[auth.session]\nidle_timeout = \"{}\"\nabsolute_timeout = \"{}\"\n\n[auth.password]\nmemory_kib = {}\niterations = {}\nparallelism = {}\nmax_concurrency = {}\n\n[auth.login]\nmax_attempts = {}\nwindow = \"{}\"\ncapacity = {}\n",
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
            self.development.allow_insecure_cookies,
            self.blob.backend,
            self.blob.root.display(),
            self.blob.capacity_bytes,
            self.blob.reserve_bytes,
            self.blob.max_object_bytes,
            self.native_crash.minidump.enabled,
            self.native_crash.minidump.max_bytes,
            self.native_crash.minidump.chunk_bytes,
            self.ingest.max_compressed_request_bytes,
            self.ingest.max_decompressed_request_bytes,
            self.ingest.max_event_bytes,
            self.ingest.max_envelope_items,
            self.ingest.max_active_requests,
            self.ingest.max_parsing_tasks,
            self.ingest.max_waiting_for_storage,
            humantime::format_duration(self.ingest.request_timeout.get()),
            self.ingest.unsupported_backoff_seconds,
            self.ingest.attachments.enabled,
            self.ingest.attachments.max_count,
            self.ingest.attachments.max_item_bytes,
            self.ingest.attachments.max_total_bytes,
            self.ingest.attachments.chunk_bytes,
            humantime::format_duration(self.ingest.attachments.orphan_grace.get()),
            humantime::format_duration(self.ingest.attachments.cleanup_interval.get()),
            self.ingest.attachments.cleanup_batch_size,
            self.ingest.attachments.cleanup_max_pages,
            self.ingest.project_cache.capacity,
            self.ingest.project_cache.max_inflight,
            humantime::format_duration(self.ingest.project_cache.positive_ttl.get()),
            humantime::format_duration(self.ingest.project_cache.negative_ttl.get()),
            humantime::format_duration(self.ingest.batch.max_wait.get()),
            self.ingest.batch.max_documents,
            self.ingest.batch.max_bytes,
            self.ingest.event_codec.compression_level,
            self.ingest.event_codec.compression_min_savings,
            self.ingest
                .backlog
                .max_pending_events
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            humantime::format_duration(self.ingest.backlog.max_oldest_pending_age.get()),
            self.dispatcher.queue_capacity,
            self.dispatcher.worker_concurrency,
            self.dispatcher.low_watermark,
            self.dispatcher.refill_target,
            self.dispatcher.refill_batch_size,
            humantime::format_duration(self.dispatcher.poll_interval.get()),
            humantime::format_duration(self.dispatcher.metrics_interval.get()),
            humantime::format_duration(self.dispatcher.source_timeout.get()),
            humantime::format_duration(self.scheduler.poll_interval.get()),
            humantime::format_duration(self.scheduler.maintenance_interval.get()),
            humantime::format_duration(self.scheduler.reconciliation_interval.get()),
            humantime::format_duration(self.scheduler.backlog_interval.get()),
            humantime::format_duration(self.scheduler.task_timeout.get()),
            humantime::format_duration(self.scheduler.retry_base.get()),
            humantime::format_duration(self.scheduler.retry_max.get()),
            self.scheduler.batch_size,
            self.retention.events_days,
            self.retention.issue_stats_hourly_days,
            self.retention.logs_days,
            self.retention.spans_days,
            self.retention.span_stats_hourly_days,
            self.retention.sessions_days,
            self.retention.session_stats_hourly_days,
            self.retention.session_active_max_hours,
            humantime::format_duration(self.project_deletion.grace_period.get()),
            self.project_deletion.delete_batch_documents,
            humantime::format_duration(self.project_deletion.completed_job_retention.get()),
            humantime::format_duration(self.project_deletion.slug_reservation.get()),
            humantime::format_duration(self.project_deletion.poll_interval.get()),
            humantime::format_duration(self.project_deletion.operation_timeout.get()),
            humantime::format_duration(self.project_deletion.drain_timeout.get()),
            humantime::format_duration(self.project_deletion.retry_base.get()),
            humantime::format_duration(self.project_deletion.retry_max.get()),
            self.processor.max_concurrency,
            self.processor.max_attempts,
            humantime::format_duration(self.processor.retry_base.get()),
            humantime::format_duration(self.processor.retry_max.get()),
            humantime::format_duration(self.processor.stage_timeout.get()),
            humantime::format_duration(self.processor.total_timeout.get()),
            humantime::format_duration(self.processor.state_timeout.get()),
            self.auth.identity_collision_retries,
            humantime::format_duration(self.auth.store_timeout.get()),
            humantime::format_duration(self.auth.setup_token_timeout.get()),
            humantime::format_duration(self.auth.max_api_token_lifetime.get()),
            humantime::format_duration(self.auth.activity_touch_interval.get()),
            self.auth.secure_cookie,
            humantime::format_duration(self.auth.session_idle_timeout.get()),
            humantime::format_duration(self.auth.session_absolute_timeout.get()),
            self.auth.password_memory_kib,
            self.auth.password_iterations,
            self.auth.password_parallelism,
            self.auth.password_max_concurrency,
            self.auth.login_max_attempts,
            humantime::format_duration(self.auth.login_window.get()),
            self.auth.login_capacity,
        );
        let endpoint = self
            .symbolicator
            .endpoint
            .as_ref()
            .map_or("<disabled>", url::Url::as_str);
        let rendered_extensions = format!(
            "{rendered}\n[symbolicator]\nendpoint = \"{endpoint}\"\ncallback_base_url = \"{}\"\nrequest_timeout = \"{}\"\nmaximum_concurrency = {}\ncircuit_failure_threshold = {}\ncircuit_cooldown = \"{}\"\nmaximum_response_bytes = {}\n\n[artifacts]\nmaximum_bundle_bytes = {}\nmaximum_logical_bytes = {}\nmaximum_entries = {}\nmaximum_entry_bytes = {}\nmaximum_concurrent_assemblies = {}\nparse_timeout = \"{}\"\norphan_grace = \"{}\"\nclaim_lease = \"{}\"\nblob_operation_timeout = \"{}\"\ntombstone_retention = \"{}\"\ngc_interval = \"{}\"\ngc_batch_size = {}\ngc_max_concurrency = {}\nmaximum_bytes_per_organization = {}\nmaximum_bundles_per_organization = {}\n\n[incident_capsule]\nmax_events = {}\nmax_activities = {}\nmax_total_uncompressed_bytes = {}\nmax_entry_bytes = {}\ngeneration_timeout = \"{}\"\nmax_concurrency = {}\nstream_chunk_bytes = {}\nstream_buffer_chunks = {}\n",
            self.symbolicator.callback_base_url,
            humantime::format_duration(self.symbolicator.request_timeout.get()),
            self.symbolicator.maximum_concurrency,
            self.symbolicator.circuit_failure_threshold,
            humantime::format_duration(self.symbolicator.circuit_cooldown.get()),
            self.symbolicator.maximum_response_bytes,
            self.artifacts.maximum_bundle_bytes,
            self.artifacts.maximum_logical_bytes,
            self.artifacts.maximum_entries,
            self.artifacts.maximum_entry_bytes,
            self.artifacts.maximum_concurrent_assemblies,
            humantime::format_duration(self.artifacts.parse_timeout.get()),
            humantime::format_duration(self.artifacts.orphan_grace.get()),
            humantime::format_duration(self.artifacts.claim_lease.get()),
            humantime::format_duration(self.artifacts.blob_operation_timeout.get()),
            humantime::format_duration(self.artifacts.tombstone_retention.get()),
            humantime::format_duration(self.artifacts.gc_interval.get()),
            self.artifacts.gc_batch_size,
            self.artifacts.gc_max_concurrency,
            self.artifacts.maximum_bytes_per_organization,
            self.artifacts.maximum_bundles_per_organization,
            self.incident_capsule.max_events,
            self.incident_capsule.max_activities,
            self.incident_capsule.max_total_uncompressed_bytes,
            self.incident_capsule.max_entry_bytes,
            humantime::format_duration(self.incident_capsule.generation_timeout.get()),
            self.incident_capsule.max_concurrency,
            self.incident_capsule.stream_chunk_bytes,
            self.incident_capsule.stream_buffer_chunks,
        );
        let rendered_notifications = format!(
            "{rendered_extensions}\n[notifications]\ntransition_batch_size = {}\ndue_scan_limit = {}\npoll_interval = \"{}\"\n\n[notifications.queue]\ncapacity = {}\nworker_concurrency = {}\n\n[notifications.retry]\nmax_attempts = {}\ninitial_delay = \"{}\"\nmax_delay = \"{}\"\ntimeout = \"{}\"\nattempt_lease = \"{}\"\n\n[notifications.retention]\ndelivered_days = {}\ndead_days = {}\n\n[notifications.webhook]\nmaximum_response_bytes = {}\nmaximum_retry_after = \"{}\"\nallow_http = {}\nallow_private_networks = {}\n",
            self.notifications.transition_batch_size,
            self.notifications.due_scan_limit,
            humantime::format_duration(self.notifications.poll_interval.get()),
            self.notifications.queue_capacity,
            self.notifications.worker_concurrency,
            self.notifications.max_attempts,
            humantime::format_duration(self.notifications.initial_delay.get()),
            humantime::format_duration(self.notifications.max_delay.get()),
            humantime::format_duration(self.notifications.timeout.get()),
            humantime::format_duration(self.notifications.attempt_lease.get()),
            self.notifications.delivered_retention.get().as_secs() / (24 * 60 * 60),
            self.notifications.dead_retention.get().as_secs() / (24 * 60 * 60),
            self.notifications.maximum_response_bytes,
            humantime::format_duration(self.notifications.maximum_retry_after.get()),
            self.notifications.allow_http,
            self.notifications.allow_private_networks,
        );
        let access_key = self
            .blob
            .s3
            .access_key_id
            .as_ref()
            .map_or("<not-configured>", SecretReference::redacted_origin);
        let secret_key = self
            .blob
            .s3
            .secret_access_key
            .as_ref()
            .map_or("<not-configured>", SecretReference::redacted_origin);
        let session_token = self
            .blob
            .s3
            .session_token
            .as_ref()
            .map_or("<not-configured>", SecretReference::redacted_origin);
        let endpoint = self
            .blob
            .s3
            .endpoint
            .as_ref()
            .map_or("<aws-default>", url::Url::as_str);
        format!(
            "{rendered_notifications}\n[blob.s3]\nendpoint = \"{endpoint}\"\nregion = \"{}\"\nbucket = \"{}\"\naccess_key_id = \"{access_key}\"\nsecret_access_key = \"{secret_key}\"\nsession_token = \"{session_token}\"\nforce_path_style = {}\npart_bytes = {}\n\n[archive]\nenabled = {}\nmaximum_events = {}\ntarget_uncompressed_bytes = {}\nwrite_chunk_bytes = {}\npoll_interval = \"{}\"\nhot_copy_delay = \"{}\"\norphan_grace = \"{}\"\ncleanup_max_pages = {}\n",
            self.blob.s3.region,
            self.blob.s3.bucket,
            self.blob.s3.force_path_style,
            self.blob.s3.part_bytes,
            self.archive.enabled,
            self.archive.maximum_events,
            self.archive.target_uncompressed_bytes,
            self.archive.write_chunk_bytes,
            humantime::format_duration(self.archive.poll_interval.get()),
            humantime::format_duration(self.archive.hot_copy_delay.get()),
            humantime::format_duration(self.archive.orphan_grace.get()),
            self.archive.cleanup_max_pages,
        )
    }

    #[must_use]
    pub fn has_literal_secret_warning(&self) -> bool {
        matches!(self.mongodb.uri, Some(SecretReference::Literal(_)))
            || matches!(
                self.projects.scrub_hmac_key,
                Some(SecretReference::Literal(_))
            )
            || matches!(
                self.blob.s3.access_key_id,
                Some(SecretReference::Literal(_))
            )
            || matches!(
                self.blob.s3.secret_access_key,
                Some(SecretReference::Literal(_))
            )
            || matches!(
                self.blob.s3.session_token,
                Some(SecretReference::Literal(_))
            )
    }
}

impl TryFrom<RawNotificationSettings> for NotificationSettings {
    type Error = ConfigError;

    fn try_from(raw: RawNotificationSettings) -> Result<Self, Self::Error> {
        let poll_interval = DispatcherInterval::from_str(&raw.poll_interval)
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let initial_delay = SchedulerInterval::from_str(&raw.retry.initial_delay)
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let max_delay = SchedulerInterval::from_str(&raw.retry.max_delay)
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let timeout = RequestTimeout::from_str(&raw.retry.timeout)
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let attempt_lease = RequestTimeout::from_str(&raw.retry.attempt_lease)
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let maximum_response = ConfiguredBytes::from_str(&raw.webhook.maximum_response_bytes)
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let maximum_retry_after = SchedulerInterval::from_str(&raw.webhook.maximum_retry_after)
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let days = |value: u32| Duration::from_secs(u64::from(value).saturating_mul(24 * 60 * 60));
        let delivered_retention = AuthDuration::new(days(raw.retention.delivered_days))
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let dead_retention = AuthDuration::new(days(raw.retention.dead_days))
            .map_err(|_| ConfigError::InvalidNotificationConfig)?;
        let valid = (1..=100_000).contains(&raw.queue.capacity)
            && (1..=1_024).contains(&raw.queue.worker_concurrency)
            && raw.queue.worker_concurrency <= raw.queue.capacity
            && (1..=10_000).contains(&raw.transition_batch_size)
            && (1..=1_000).contains(&raw.due_scan_limit)
            && !poll_interval.get().is_zero()
            && (1..=100).contains(&raw.retry.max_attempts)
            && !initial_delay.get().is_zero()
            && initial_delay <= max_delay
            && !timeout.get().is_zero()
            && attempt_lease > timeout
            && !delivered_retention.get().is_zero()
            && dead_retention >= delivered_retention
            && (1..=1024 * 1024).contains(&maximum_response.get())
            && !maximum_retry_after.get().is_zero();
        if !valid {
            return Err(ConfigError::InvalidNotificationConfig);
        }
        Ok(Self {
            queue_capacity: raw.queue.capacity,
            worker_concurrency: raw.queue.worker_concurrency,
            transition_batch_size: raw.transition_batch_size,
            due_scan_limit: raw.due_scan_limit,
            poll_interval,
            max_attempts: raw.retry.max_attempts,
            initial_delay,
            max_delay,
            timeout,
            attempt_lease,
            delivered_retention,
            dead_retention,
            maximum_response_bytes: usize::try_from(maximum_response.get())
                .map_err(|_| ConfigError::InvalidNotificationConfig)?,
            maximum_retry_after,
            allow_http: raw.webhook.allow_http,
            allow_private_networks: raw.webhook.allow_private_networks,
        })
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
        let attachment_item = ConfiguredBytes::from_str(&raw.attachments.max_item_bytes)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let attachment_total = ConfiguredBytes::from_str(&raw.attachments.max_total_bytes)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let attachment_chunk = ConfiguredBytes::from_str(&raw.attachments.chunk_bytes)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let orphan_grace = SchedulerInterval::from_str(&raw.attachments.orphan_grace)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let cleanup_interval = SchedulerInterval::from_str(&raw.attachments.cleanup_interval)
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
        let backlog_age = BacklogAge::from_str(&raw.backlog.max_oldest_pending_age)
            .map_err(|_| ConfigError::InvalidIngestConfig)?;
        let valid_backlog = raw.backlog.max_pending_events.is_none_or(|value| value > 0)
            && !backlog_age.get().is_zero();
        let valid_attachments = (1..=100).contains(&raw.attachments.max_count)
            && attachment_item.get() > 0
            && attachment_total.get() >= attachment_item.get()
            && attachment_total.get() <= decompressed.get()
            && (4 * 1024..=1024 * 1024).contains(&attachment_chunk.get())
            && !orphan_grace.get().is_zero()
            && !cleanup_interval.get().is_zero()
            && (1..=10_000).contains(&raw.attachments.cleanup_batch_size)
            && (1..=1024).contains(&raw.attachments.cleanup_max_pages);
        if !valid
            || !valid_cache
            || !valid_batch
            || !valid_codec
            || !valid_backlog
            || !valid_attachments
        {
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
            backlog: BacklogSettings {
                max_pending_events: raw.backlog.max_pending_events,
                max_oldest_pending_age: backlog_age,
            },
            attachments: AttachmentSettings {
                enabled: raw.attachments.enabled,
                max_count: raw.attachments.max_count,
                max_item_bytes: usize::try_from(attachment_item.get())
                    .map_err(|_| ConfigError::InvalidIngestConfig)?,
                max_total_bytes: usize::try_from(attachment_total.get())
                    .map_err(|_| ConfigError::InvalidIngestConfig)?,
                chunk_bytes: usize::try_from(attachment_chunk.get())
                    .map_err(|_| ConfigError::InvalidIngestConfig)?,
                orphan_grace,
                cleanup_interval,
                cleanup_batch_size: raw.attachments.cleanup_batch_size,
                cleanup_max_pages: raw.attachments.cleanup_max_pages,
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

impl TryFrom<RawSchedulerSettings> for SchedulerSettings {
    type Error = ConfigError;

    fn try_from(raw: RawSchedulerSettings) -> Result<Self, Self::Error> {
        let parse = |value: &str| {
            SchedulerInterval::from_str(value).map_err(|_| ConfigError::InvalidSchedulerConfig)
        };
        let poll_interval = parse(&raw.poll_interval)?;
        let maintenance_interval = parse(&raw.maintenance_interval)?;
        let reconciliation_interval = parse(&raw.reconciliation_interval)?;
        let backlog_interval = parse(&raw.backlog_interval)?;
        let task_timeout = parse(&raw.task_timeout)?;
        let retry_base = parse(&raw.retry_base)?;
        let retry_max = parse(&raw.retry_max)?;
        let valid = [
            poll_interval,
            maintenance_interval,
            reconciliation_interval,
            backlog_interval,
            task_timeout,
            retry_base,
            retry_max,
        ]
        .into_iter()
        .all(|duration| !duration.get().is_zero())
            && retry_base.get() <= retry_max.get()
            && (1..=10_000).contains(&raw.batch_size);
        if !valid {
            return Err(ConfigError::InvalidSchedulerConfig);
        }
        Ok(Self {
            poll_interval,
            maintenance_interval,
            reconciliation_interval,
            backlog_interval,
            task_timeout,
            retry_base,
            retry_max,
            batch_size: raw.batch_size,
        })
    }
}

impl TryFrom<RawRetentionSettings> for RetentionSettings {
    type Error = ConfigError;

    fn try_from(raw: RawRetentionSettings) -> Result<Self, Self::Error> {
        let valid = (1..=3_650).contains(&raw.events_days)
            && (1..=3_650).contains(&raw.issue_stats_hourly_days)
            && (1..=3_650).contains(&raw.logs_days)
            && (1..=3_650).contains(&raw.spans_days)
            && (1..=3_650).contains(&raw.span_stats_hourly_days)
            && (1..=3_650).contains(&raw.sessions_days)
            && (1..=3_650).contains(&raw.session_stats_hourly_days)
            && (1..=8_760).contains(&raw.session_active_max_hours);
        valid
            .then_some(Self {
                events_days: raw.events_days,
                issue_stats_hourly_days: raw.issue_stats_hourly_days,
                logs_days: raw.logs_days,
                spans_days: raw.spans_days,
                span_stats_hourly_days: raw.span_stats_hourly_days,
                sessions_days: raw.sessions_days,
                session_stats_hourly_days: raw.session_stats_hourly_days,
                session_active_max_hours: raw.session_active_max_hours,
            })
            .ok_or(ConfigError::InvalidRetentionConfig)
    }
}

impl TryFrom<RawProjectDeletionSettings> for ProjectDeletionSettings {
    type Error = ConfigError;

    fn try_from(raw: RawProjectDeletionSettings) -> Result<Self, Self::Error> {
        let parse = |value: &str| {
            ProjectDeletionDuration::from_str(value)
                .map_err(|_| ConfigError::InvalidProjectDeletionConfig)
        };
        let settings = Self {
            grace_period: parse(&raw.grace_period)?,
            delete_batch_documents: raw.delete_batch_documents,
            completed_job_retention: parse(&raw.completed_job_retention)?,
            slug_reservation: parse(&raw.slug_reservation)?,
            poll_interval: parse(&raw.poll_interval)?,
            operation_timeout: parse(&raw.operation_timeout)?,
            drain_timeout: parse(&raw.drain_timeout)?,
            retry_base: parse(&raw.retry_base)?,
            retry_max: parse(&raw.retry_max)?,
        };
        let valid = [
            settings.grace_period,
            settings.completed_job_retention,
            settings.slug_reservation,
            settings.poll_interval,
            settings.operation_timeout,
            settings.drain_timeout,
            settings.retry_base,
            settings.retry_max,
        ]
        .into_iter()
        .all(|duration| !duration.get().is_zero())
            && settings.retry_base.get() <= settings.retry_max.get()
            && (1..=10_000).contains(&settings.delete_batch_documents);
        valid
            .then_some(settings)
            .ok_or(ConfigError::InvalidProjectDeletionConfig)
    }
}

impl TryFrom<RawProcessorSettings> for ProcessorSettings {
    type Error = ConfigError;

    fn try_from(raw: RawProcessorSettings) -> Result<Self, Self::Error> {
        let retry_base = ProcessorDuration::from_str(&raw.retry_base)
            .map_err(|_| ConfigError::InvalidProcessorConfig)?;
        let retry_max = ProcessorDuration::from_str(&raw.retry_max)
            .map_err(|_| ConfigError::InvalidProcessorConfig)?;
        let stage_timeout = ProcessorDuration::from_str(&raw.stage_timeout)
            .map_err(|_| ConfigError::InvalidProcessorConfig)?;
        let total_timeout = ProcessorDuration::from_str(&raw.total_timeout)
            .map_err(|_| ConfigError::InvalidProcessorConfig)?;
        let state_timeout = ProcessorDuration::from_str(&raw.state_timeout)
            .map_err(|_| ConfigError::InvalidProcessorConfig)?;
        let valid = (1..=4_096).contains(&raw.max_concurrency)
            && (1..=100).contains(&raw.max_attempts)
            && !retry_base.get().is_zero()
            && retry_base.get() <= retry_max.get()
            && !stage_timeout.get().is_zero()
            && stage_timeout.get() <= total_timeout.get()
            && !state_timeout.get().is_zero()
            && state_timeout.get() <= total_timeout.get();
        if !valid {
            return Err(ConfigError::InvalidProcessorConfig);
        }
        Ok(Self {
            max_concurrency: raw.max_concurrency,
            max_attempts: raw.max_attempts,
            retry_base,
            retry_max,
            stage_timeout,
            total_timeout,
            state_timeout,
        })
    }
}

impl TryFrom<RawAuthSettings> for AuthSettings {
    type Error = ConfigError;

    fn try_from(raw: RawAuthSettings) -> Result<Self, Self::Error> {
        let parse =
            |value: &str| AuthDuration::from_str(value).map_err(|_| ConfigError::InvalidAuthConfig);
        let store_timeout = parse(&raw.store_timeout)?;
        let setup_token_timeout = parse(&raw.setup_token_timeout)?;
        let max_api_token_lifetime = parse(&raw.max_api_token_lifetime)?;
        let activity_touch_interval = parse(&raw.activity_touch_interval)?;
        let session_idle_timeout = parse(&raw.session.idle_timeout)?;
        let session_absolute_timeout = parse(&raw.session.absolute_timeout)?;
        let login_window = parse(&raw.login.window)?;
        let valid = (1..=1_024).contains(&raw.identity_collision_retries)
            && !store_timeout.get().is_zero()
            && !setup_token_timeout.get().is_zero()
            && !max_api_token_lifetime.get().is_zero()
            && !activity_touch_interval.get().is_zero()
            && activity_touch_interval.get() < session_idle_timeout.get()
            && session_idle_timeout.get() <= session_absolute_timeout.get()
            && (19 * 1024..=1024 * 1024).contains(&raw.password.memory_kib)
            && (2..=20).contains(&raw.password.iterations)
            && (1..=16).contains(&raw.password.parallelism)
            && (1..=64).contains(&raw.password.max_concurrency)
            && (1..=10_000).contains(&raw.login.max_attempts)
            && !login_window.get().is_zero()
            && (2..=1_000_000).contains(&raw.login.capacity);
        if !valid {
            return Err(ConfigError::InvalidAuthConfig);
        }
        Ok(Self {
            identity_collision_retries: raw.identity_collision_retries,
            store_timeout,
            setup_token_timeout,
            max_api_token_lifetime,
            activity_touch_interval,
            secure_cookie: raw.secure_cookie,
            session_idle_timeout,
            session_absolute_timeout,
            password_memory_kib: raw.password.memory_kib,
            password_iterations: raw.password.iterations,
            password_parallelism: raw.password.parallelism,
            password_max_concurrency: raw.password.max_concurrency,
            login_max_attempts: raw.login.max_attempts,
            login_window,
            login_capacity: raw.login.capacity,
        })
    }
}

pub struct ResolvedSecrets {
    pub mongodb_uri: Option<SecretValue>,
    pub scrub_hmac_key: Option<metric_domain::SecretBytes>,
    pub s3_access_key_id: Option<SecretValue>,
    pub s3_secret_access_key: Option<SecretValue>,
    pub s3_session_token: Option<SecretValue>,
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
        let cli = Cli::parse_from(["metric", "--check-config"]);
        let config = load(&cli).unwrap();
        assert_eq!(config.role, Role::All);
        assert_eq!(config.server.shutdown_grace.get().as_secs(), 10);
        assert!(config.dispatcher.low_watermark < config.dispatcher.refill_target);
        assert!(config.dispatcher.refill_target <= config.dispatcher.queue_capacity);
        assert_eq!(config.processor.max_attempts, 5);
        assert_eq!(config.scheduler.batch_size, 500);
        assert_eq!(config.retention.events_days, 30);
        assert_eq!(config.retention.issue_stats_hourly_days, 400);
        assert_eq!(config.auth.password_memory_kib, 19 * 1024);
        assert!(config.auth.secure_cookie);
        assert!(config.symbolicator.endpoint.is_none());
        assert_eq!(config.artifacts.maximum_bundle_bytes, 64 * 1024 * 1024);
        assert_eq!(config.artifacts.gc_max_concurrency, 4);
        assert_eq!(config.incident_capsule.max_events, 10);
        assert_eq!(config.notifications.queue_capacity, 1_000);
        assert_eq!(config.notifications.max_attempts, 8);
        assert!(!config.notifications.allow_private_networks);
        assert_eq!(config.blob.backend, BlobBackend::Local);
        assert!(!config.archive.enabled);
        assert_eq!(
            config.incident_capsule.max_total_uncompressed_bytes,
            100 * 1024 * 1024
        );
        assert_eq!(
            config.ingest.backlog.max_oldest_pending_age.get(),
            std::time::Duration::from_secs(60 * 60)
        );
    }

    #[test]
    fn artifact_archive_quota_and_gc_bounds_fail_closed() {
        for artifacts in [
            RawArtifactSettings {
                maximum_logical_bytes: "1 MiB".to_owned(),
                ..RawArtifactSettings::default()
            },
            RawArtifactSettings {
                claim_lease: "10s".to_owned(),
                blob_operation_timeout: "30s".to_owned(),
                ..RawArtifactSettings::default()
            },
            RawArtifactSettings {
                gc_max_concurrency: 5,
                ..RawArtifactSettings::default()
            },
        ] {
            assert!(matches!(
                AppConfig::try_from(RawConfig {
                    artifacts,
                    ..RawConfig::default()
                }),
                Err(ConfigError::InvalidArtifactConfig)
            ));
        }
    }

    #[test]
    fn incident_capsule_limits_fail_closed() {
        for incident_capsule in [
            RawIncidentCapsuleSettings {
                max_events: 11,
                ..RawIncidentCapsuleSettings::default()
            },
            RawIncidentCapsuleSettings {
                max_entry_bytes: "17 MiB".to_owned(),
                ..RawIncidentCapsuleSettings::default()
            },
            RawIncidentCapsuleSettings {
                generation_timeout: "31s".to_owned(),
                ..RawIncidentCapsuleSettings::default()
            },
            RawIncidentCapsuleSettings {
                stream_buffer_chunks: 17,
                ..RawIncidentCapsuleSettings::default()
            },
        ] {
            assert!(matches!(
                AppConfig::try_from(RawConfig {
                    incident_capsule,
                    ..RawConfig::default()
                }),
                Err(ConfigError::InvalidIncidentCapsuleConfig)
            ));
        }
    }

    #[test]
    fn notification_limits_fail_closed() {
        for notifications in [
            RawNotificationSettings {
                queue: RawNotificationQueueSettings {
                    capacity: 4,
                    worker_concurrency: 5,
                },
                ..RawNotificationSettings::default()
            },
            RawNotificationSettings {
                retry: RawNotificationRetrySettings {
                    timeout: "30s".to_owned(),
                    attempt_lease: "10s".to_owned(),
                    ..RawNotificationRetrySettings::default()
                },
                ..RawNotificationSettings::default()
            },
            RawNotificationSettings {
                retention: RawNotificationRetentionSettings {
                    delivered_days: 90,
                    dead_days: 30,
                },
                ..RawNotificationSettings::default()
            },
        ] {
            assert!(matches!(
                AppConfig::try_from(RawConfig {
                    notifications,
                    ..RawConfig::default()
                }),
                Err(ConfigError::InvalidNotificationConfig)
            ));
        }
    }

    #[test]
    fn symbolicator_transport_bounds_fail_closed() {
        for symbolicator in [
            RawSymbolicatorSettings {
                endpoint: Some("file:///tmp/symbolicator".to_owned()),
                ..RawSymbolicatorSettings::default()
            },
            RawSymbolicatorSettings {
                maximum_concurrency: 0,
                ..RawSymbolicatorSettings::default()
            },
            RawSymbolicatorSettings {
                maximum_response_bytes: "32 MiB".to_owned(),
                ..RawSymbolicatorSettings::default()
            },
        ] {
            let raw = RawConfig {
                symbolicator,
                ..RawConfig::default()
            };
            assert!(matches!(
                AppConfig::try_from(raw),
                Err(ConfigError::InvalidSymbolicatorConfig)
            ));
        }
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
    fn processor_and_backlog_bounds_fail_closed() {
        let raw = RawConfig {
            processor: RawProcessorSettings {
                max_attempts: 0,
                ..RawProcessorSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(raw),
            Err(ConfigError::InvalidProcessorConfig)
        ));
        let raw = RawConfig {
            ingest: RawIngestConfig {
                backlog: RawBacklogSettings {
                    max_pending_events: Some(0),
                    ..RawBacklogSettings::default()
                },
                ..RawIngestConfig::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(raw),
            Err(ConfigError::InvalidIngestConfig)
        ));
    }

    #[test]
    fn scheduler_and_retention_bounds_fail_closed() {
        let scheduler = RawConfig {
            scheduler: RawSchedulerSettings {
                retry_base: "2m".to_owned(),
                retry_max: "1m".to_owned(),
                ..RawSchedulerSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(scheduler),
            Err(ConfigError::InvalidSchedulerConfig)
        ));
        let retention = RawConfig {
            retention: RawRetentionSettings {
                events_days: 0,
                ..RawRetentionSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(retention),
            Err(ConfigError::InvalidRetentionConfig)
        ));
        let deletion = RawConfig {
            project_deletion: RawProjectDeletionSettings {
                delete_batch_documents: 0,
                ..RawProjectDeletionSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(deletion),
            Err(ConfigError::InvalidProjectDeletionConfig)
        ));
    }

    #[test]
    fn auth_cost_session_and_cookie_bounds_fail_closed() {
        let weak = RawConfig {
            auth: RawAuthSettings {
                password: RawAuthPasswordSettings {
                    memory_kib: 1024,
                    ..RawAuthPasswordSettings::default()
                },
                ..RawAuthSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(weak),
            Err(ConfigError::InvalidAuthConfig)
        ));
        let inverted = RawConfig {
            auth: RawAuthSettings {
                session: RawAuthSessionSettings {
                    idle_timeout: "31d".to_owned(),
                    absolute_timeout: "30d".to_owned(),
                },
                ..RawAuthSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(inverted),
            Err(ConfigError::InvalidAuthConfig)
        ));
        let insecure = RawConfig {
            auth: RawAuthSettings {
                secure_cookie: false,
                ..RawAuthSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(insecure),
            Err(ConfigError::InvalidAuthConfig)
        ));
        let local = RawConfig {
            development: RawDevelopmentConfig {
                allow_insecure_cookies: true,
                ..RawDevelopmentConfig::default()
            },
            auth: RawAuthSettings {
                secure_cookie: false,
                ..RawAuthSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(!AppConfig::try_from(local).unwrap().auth.secure_cookie);
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
                ..RawDevelopmentConfig::default()
            },
            ..RawConfig::default()
        };
        let config = AppConfig::try_from(raw).unwrap();
        let output = config.effective_redacted();
        assert!(!output.contains("do-not-print-this"));
        assert!(output.contains("<redacted:literal>"));
    }

    #[test]
    fn s3_credentials_are_required_and_redacted() {
        let missing_credentials = RawConfig {
            blob: RawBlobConfig {
                backend: BlobBackend::S3,
                ..RawBlobConfig::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(missing_credentials),
            Err(ConfigError::InvalidBlobConfig)
        ));

        let raw = RawConfig {
            blob: RawBlobConfig {
                backend: BlobBackend::S3,
                s3: RawS3BlobSettings {
                    access_key_id: Some(SecretReference::Literal(LiteralReference {
                        literal: "phase21-access-key".to_owned(),
                    })),
                    secret_access_key: Some(SecretReference::Literal(LiteralReference {
                        literal: "phase21-secret-key".to_owned(),
                    })),
                    ..RawS3BlobSettings::default()
                },
                ..RawBlobConfig::default()
            },
            development: RawDevelopmentConfig {
                allow_literal_secrets: true,
                ..RawDevelopmentConfig::default()
            },
            ..RawConfig::default()
        };
        let config = AppConfig::try_from(raw).unwrap();
        let output = config.effective_redacted();
        assert!(output.contains("backend = \"s3\""));
        assert!(!output.contains("phase21-access-key"));
        assert!(!output.contains("phase21-secret-key"));
        assert!(output.contains("access_key_id = \"<redacted:literal>\""));
        assert!(output.contains("secret_access_key = \"<redacted:literal>\""));
    }

    #[test]
    fn archive_requires_mongodb_and_stays_disabled_by_default() {
        let raw = RawConfig {
            archive: RawArchiveSettings {
                enabled: true,
                ..RawArchiveSettings::default()
            },
            ..RawConfig::default()
        };
        assert!(matches!(
            AppConfig::try_from(raw),
            Err(ConfigError::InvalidArchiveConfig)
        ));
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
            "metric",
            "--config",
            "definitely-missing-config.toml",
            "--check-config",
        ]);
        assert!(load(&cli).is_err());
    }

    #[test]
    fn explicit_env_file_is_loaded_and_process_environment_wins() {
        let path = temporary_path("environment");
        let name = format!("METRIC_ENV_FILE_TEST_{}", uuid::Uuid::new_v4().simple());
        let existing_path = env::var("PATH").expect("test process has PATH");
        fs::write(
            &path,
            format!("{name}='loaded value'\nPATH=must-not-override-process\n"),
        )
        .unwrap();
        let cli = Cli::parse_from([
            "metric",
            "--env-file",
            path.to_str().unwrap(),
            "--check-config",
        ]);

        load(&cli).unwrap();

        assert_eq!(env::var(name).unwrap(), "loaded value");
        assert_eq!(env::var("PATH").unwrap(), existing_path);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn env_file_is_explicit_bounded_and_fail_closed() {
        let missing = Cli::parse_from([
            "metric",
            "--env-file",
            "definitely-missing.env",
            "--check-config",
        ]);
        assert!(matches!(
            load(&missing),
            Err(ConfigError::MissingEnvironmentFile)
        ));

        let invalid_path = temporary_path("invalid-environment");
        fs::write(&invalid_path, "BROKEN='unterminated\n").unwrap();
        let invalid = Cli::parse_from([
            "metric",
            "--env-file",
            invalid_path.to_str().unwrap(),
            "--check-config",
        ]);
        assert!(matches!(
            load(&invalid),
            Err(ConfigError::InvalidEnvironmentFile)
        ));
        fs::remove_file(invalid_path).unwrap();

        let oversized_path = temporary_path("oversized-environment");
        fs::write(
            &oversized_path,
            vec![b'x'; usize::try_from(MAX_ENV_FILE_BYTES).unwrap() + 1],
        )
        .unwrap();
        let oversized = Cli::parse_from([
            "metric",
            "--env-file",
            oversized_path.to_str().unwrap(),
            "--check-config",
        ]);
        assert!(matches!(
            load(&oversized),
            Err(ConfigError::EnvironmentFileTooLarge)
        ));
        fs::remove_file(oversized_path).unwrap();
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
            "metric-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn config_file_unknown_fields_are_errors() {
        let path = temporary_path("unknown");
        fs::write(&path, "unknown_field = true\n").unwrap();
        let cli = Cli::parse_from([
            "metric",
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
