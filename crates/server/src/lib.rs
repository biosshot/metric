//! Configuration and composition root for the single `all` role.

pub mod config;
pub mod debug_http;
pub mod http;
pub mod ingest_http;
pub mod native_http;
pub mod web_http;
pub mod webhook;

use std::{io, process::ExitCode};

use config::{BlobBackend, Cli, ConfigError};
use faultkeep_application::{
    archive::{ArchiveConfig, ArchiveError, ArchiveService, ArchiveTask, start_archive_worker},
    artifacts::{
        ArtifactCleanupTask, ArtifactConfig, ArtifactError, ArtifactService, start_artifact_cleanup,
    },
    auth::{AuthConfig, AuthError, IdentityService, LoginRateLimitConfig, PasswordConfig},
    blob_cleanup::{
        BlobCleanupConfig, BlobCleanupError, BlobCleanupService, BlobCleanupTask,
        start_blob_cleanup_worker,
    },
    debug_files::{
        DebugFileCleanupTask, DebugFileConfig, DebugFileError, DebugFileService,
        start_debug_file_cleanup,
    },
    deletion::{
        ProjectDeletionConfig, ProjectDeletionError, ProjectDeletionService, ProjectDeletionTask,
        ProjectFencedEventSink, ProjectWorkRegistry, start_project_deletion_worker,
    },
    dispatcher::{
        BacklogGuardedEventSink, Dispatcher, DispatcherConfig, DispatcherStartError, DispatcherTask,
    },
    finalizer::{Finalizer, FinalizerConfig, FinalizerError},
    incident_capsule::{
        IncidentCapsuleAccess, IncidentCapsuleConfig, IncidentCapsuleError, IncidentCapsuleService,
    },
    log_writer::{LogWriter, LogWriterConfig, LogWriterStartError, LogWriterTask},
    native_api::NativeApiService,
    normalizer::{Normalizer, NormalizerConfigError, NormalizerLimits},
    notifications::{
        NotificationConfig, NotificationDispatcher, NotificationError, NotificationTask,
    },
    observability::{Metric, Metrics, Outcome},
    processor::{
        FinalizerBatchConfig, FinalizerBatchTask, FinalizerBatcher, GrouperStage,
        IssuePreparerStage, Processor, ProcessorConfig, ProcessorConfigError,
    },
    projects::{ProjectCacheConfig, ProjectService, ProjectServiceError},
    scheduler::{Scheduler, SchedulerConfig, SchedulerStartError, SchedulerTask},
    search::{SearchConfig, SearchService},
    shutdown::ShutdownRoot,
    span_writer::{SpanWriter, SpanWriterConfig, SpanWriterStartError, SpanWriterTask},
    symbolication::{BaselineSymbolicationService, SymbolicationConfig, SymbolicationService},
    writer::{MongoWriter, MongoWriterConfig, MongoWriterStartError, MongoWriterTask},
};
use faultkeep_blob::{LocalBlobConfig, LocalBlobStore, S3BlobConfig, S3BlobStore};
use faultkeep_domain::Timestamp;
use faultkeep_mongo::{EventCodecConfig, IssueCodecConfig, MongoBootstrapError, MongoProjectStore};
use faultkeep_ports::{
    BlobReferenceStore, BlobStore, BlobStoreError, Clock, EventBacklog, EventSink, EventSinkError,
    LogSink, OutcomeSink, PortFuture, ProjectResolveError, ProjectResolver, RandomError,
    RandomSource, SignalStore, SpanSink,
};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("HTTP server failed: {0}")]
    Http(#[from] io::Error),
    #[error("structured tracing could not be initialized")]
    Tracing,
    #[error(transparent)]
    Mongo(#[from] MongoBootstrapError),
    #[error(transparent)]
    Projects(#[from] ProjectServiceError),
    #[error("MongoDB schema bootstrap/check exceeded its deadline")]
    MongoBootstrapTimeout,
    #[error(transparent)]
    Writer(#[from] MongoWriterStartError),
    #[error(transparent)]
    LogWriter(#[from] LogWriterStartError),
    #[error(transparent)]
    SpanWriter(#[from] SpanWriterStartError),
    #[error(transparent)]
    Dispatcher(#[from] DispatcherStartError),
    #[error(transparent)]
    Scheduler(#[from] SchedulerStartError),
    #[error(transparent)]
    IncidentCapsule(#[from] IncidentCapsuleError),
    #[error(transparent)]
    Notifications(#[from] NotificationError),
    #[error(transparent)]
    Normalizer(#[from] NormalizerConfigError),
    #[error(transparent)]
    Finalizer(#[from] FinalizerError),
    #[error(transparent)]
    Processor(#[from] ProcessorConfigError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    ProjectDeletion(#[from] ProjectDeletionError),
    #[error(transparent)]
    Blob(#[from] BlobStoreError),
    #[error(transparent)]
    BlobCleanup(#[from] BlobCleanupError),
    #[error(transparent)]
    DebugFiles(#[from] DebugFileError),
    #[error(transparent)]
    Artifacts(#[from] ArtifactError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("external Symbolicator configuration is invalid")]
    Symbolicator,
}

struct RuntimeModules {
    project_resolver: std::sync::Arc<dyn ProjectResolver>,
    event_sink: std::sync::Arc<dyn EventSink>,
    log_sink: Option<std::sync::Arc<dyn LogSink>>,
    span_sink: Option<std::sync::Arc<dyn SpanSink>>,
    writer_task: Option<MongoWriterTask>,
    log_writer_task: Option<LogWriterTask>,
    span_writer_task: Option<SpanWriterTask>,
    dispatcher_task: Option<DispatcherTask>,
    scheduler_task: Option<SchedulerTask>,
    archive_task: Option<ArchiveTask>,
    finalizer_batcher: Option<std::sync::Arc<FinalizerBatcher>>,
    finalizer_batch_task: Option<FinalizerBatchTask>,
    identity_service: Option<std::sync::Arc<IdentityService>>,
    native_api_service: Option<std::sync::Arc<NativeApiService>>,
    incident_capsule_service: Option<std::sync::Arc<IncidentCapsuleService>>,
    notification_task: Option<NotificationTask>,
    project_deletion_task: Option<ProjectDeletionTask>,
    blob_cleanup_task: Option<BlobCleanupTask>,
    debug_file_service: Option<std::sync::Arc<DebugFileService>>,
    artifact_service: Option<std::sync::Arc<ArtifactService>>,
    private_source_signer: Option<faultkeep_symbolication::PrivateSourceSigner>,
    debug_file_cleanup_task: Option<DebugFileCleanupTask>,
    artifact_cleanup_task: Option<ArtifactCleanupTask>,
}

pub async fn execute(cli: Cli) -> Result<ExitCode, ServerError> {
    let config = config::load(&cli)?;
    let mut secrets = config.validate_secrets()?;
    if config.has_literal_secret_warning() {
        eprintln!("warning: a literal secret is enabled for local development");
    }
    if cli.check_config {
        println!("configuration is valid");
        return Ok(ExitCode::SUCCESS);
    }
    if cli.print_effective_config {
        print!("{}", config.effective_redacted());
        return Ok(ExitCode::SUCCESS);
    }
    init_tracing()?;
    let metrics = Metrics;
    let shutdown = ShutdownRoot::new();
    let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(SystemClock);
    let random: std::sync::Arc<dyn RandomSource> = std::sync::Arc::new(SystemRandom);
    let blob_store: std::sync::Arc<dyn BlobStore> = match config.blob.backend {
        BlobBackend::Local => std::sync::Arc::new(
            LocalBlobStore::new(
                &config.blob.root,
                LocalBlobConfig {
                    capacity_bytes: config.blob.capacity_bytes,
                    reserve_bytes: config.blob.reserve_bytes,
                    max_object_bytes: config.blob.max_object_bytes,
                },
            )
            .await?,
        ),
        BlobBackend::S3 => std::sync::Arc::new(S3BlobStore::new(S3BlobConfig {
            endpoint: config
                .blob
                .s3
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.as_str().into()),
            region: config.blob.s3.region.clone().into(),
            bucket: config.blob.s3.bucket.clone().into(),
            access_key_id: secrets
                .s3_access_key_id
                .take()
                .expect("validated S3 configuration has an access key")
                .expose()
                .into(),
            secret_access_key: secrets
                .s3_secret_access_key
                .take()
                .expect("validated S3 configuration has a secret key")
                .expose()
                .into(),
            session_token: secrets
                .s3_session_token
                .take()
                .map(|value| value.expose().into()),
            force_path_style: config.blob.s3.force_path_style,
            part_bytes: config.blob.s3.part_bytes,
            max_object_bytes: config.blob.max_object_bytes,
        })?),
    };
    let private_source_signer = secrets
        .scrub_hmac_key
        .as_ref()
        .map(|key| {
            let mut derivation = Sha256::new();
            derivation.update(b"faultkeep/private-symbol-source-key/v1");
            derivation.update(key.expose());
            faultkeep_symbolication::PrivateSourceSigner::new(derivation.finalize().to_vec(), None)
                .map_err(|_| ServerError::Symbolicator)
        })
        .transpose()?;
    let RuntimeModules {
        project_resolver,
        event_sink,
        log_sink,
        span_sink,
        writer_task,
        log_writer_task,
        span_writer_task,
        dispatcher_task,
        scheduler_task,
        archive_task,
        finalizer_batcher,
        finalizer_batch_task,
        identity_service,
        native_api_service,
        incident_capsule_service,
        notification_task,
        project_deletion_task,
        blob_cleanup_task,
        debug_file_service,
        artifact_service,
        private_source_signer: runtime_private_source_signer,
        debug_file_cleanup_task,
        artifact_cleanup_task,
    } = if let Some(uri) = secrets.mongodb_uri.take() {
        let hmac_key = secrets
            .scrub_hmac_key
            .take()
            .expect("validated MongoDB configuration has a scrub HMAC key");
        let webhook_secret_box = webhook::WebhookSecretBox::new(&hmac_key);
        let setup = async {
            let store = MongoProjectStore::connect(
                uri.expose(),
                &config.mongodb.database,
                hmac_key,
                config.projects.max_keys_per_project,
            )
            .await?;
            store.bootstrap_or_validate().await?;
            Ok::<_, MongoBootstrapError>(store)
        };
        let store = timeout(config.mongodb.bootstrap_timeout.get(), setup)
            .await
            .map_err(|_| ServerError::MongoBootstrapTimeout)??;
        let project_service = std::sync::Arc::new(ProjectService::new(
            std::sync::Arc::new(store.clone()),
            std::sync::Arc::clone(&clock),
            std::sync::Arc::clone(&random),
            config.projects.identity_collision_retries,
            ProjectCacheConfig {
                capacity: config.ingest.project_cache.capacity,
                max_inflight: config.ingest.project_cache.max_inflight,
                positive_ttl: config.ingest.project_cache.positive_ttl.get(),
                negative_ttl: config.ingest.project_cache.negative_ttl.get(),
            },
        )?);
        let notification_store: std::sync::Arc<dyn faultkeep_ports::NotificationStore> =
            std::sync::Arc::new(store.notification_store());
        let notification_adapter: std::sync::Arc<dyn faultkeep_ports::WebhookDeliveryAdapter> =
            std::sync::Arc::new(
                webhook::ReqwestWebhookAdapter::new(
                    webhook_secret_box,
                    webhook::WebhookAdapterConfig {
                        timeout: config.notifications.timeout.get(),
                        max_response_bytes: config.notifications.maximum_response_bytes,
                        max_retry_after: config.notifications.maximum_retry_after.get(),
                        allow_http: config.notifications.allow_http,
                        allow_private_networks: config.notifications.allow_private_networks,
                    },
                )
                .map_err(|_| NotificationError::InvalidConfiguration)?,
            );
        let notification_dispatcher = std::sync::Arc::new(NotificationDispatcher::new(
            notification_store,
            notification_adapter,
            std::sync::Arc::clone(&clock),
            NotificationConfig {
                queue_capacity: config.notifications.queue_capacity,
                worker_concurrency: config.notifications.worker_concurrency,
                transition_batch_size: config.notifications.transition_batch_size,
                due_scan_limit: config.notifications.due_scan_limit,
                poll_interval: config.notifications.poll_interval.get(),
                attempt_timeout: config.notifications.timeout.get(),
                attempt_lease: config.notifications.attempt_lease.get(),
                max_attempts: config.notifications.max_attempts,
                initial_delay: config.notifications.initial_delay.get(),
                max_delay: config.notifications.max_delay.get(),
                delivered_retention: config.notifications.delivered_retention.get(),
                dead_retention: config.notifications.dead_retention.get(),
            },
        )?);
        let notification_signal: std::sync::Arc<
            dyn faultkeep_application::notifications::NotificationSignal,
        > = notification_dispatcher.clone();
        let notification_task = notification_dispatcher.start(shutdown.signal())?;
        let project_resolver: std::sync::Arc<dyn ProjectResolver> = project_service.clone();
        let project_work = std::sync::Arc::new(ProjectWorkRegistry::default());
        let deletion_config = ProjectDeletionConfig {
            grace_period: config.project_deletion.grace_period.get(),
            delete_batch_documents: config.project_deletion.delete_batch_documents,
            completed_job_retention: config.project_deletion.completed_job_retention.get(),
            slug_reservation: config.project_deletion.slug_reservation.get(),
            poll_interval: config.project_deletion.poll_interval.get(),
            operation_timeout: config.project_deletion.operation_timeout.get(),
            drain_timeout: config.project_deletion.drain_timeout.get(),
            retry_base: config.project_deletion.retry_base.get(),
            retry_max: config.project_deletion.retry_max.get(),
        };
        let deletion_store: std::sync::Arc<dyn faultkeep_ports::ProjectDeletionStore> =
            std::sync::Arc::new(store.clone());
        let deletion_service = ProjectDeletionService::new(
            std::sync::Arc::clone(&deletion_store),
            std::sync::Arc::clone(&project_service),
            std::sync::Arc::clone(&clock),
            std::sync::Arc::clone(&project_work),
            deletion_config,
        )?;
        let identity_service = std::sync::Arc::new(IdentityService::new(
            std::sync::Arc::new(store.auth_store()),
            std::sync::Arc::clone(&clock),
            std::sync::Arc::clone(&random),
            AuthConfig {
                identity_collision_retries: config.auth.identity_collision_retries,
                session_idle_timeout: config.auth.session_idle_timeout.get(),
                session_absolute_timeout: config.auth.session_absolute_timeout.get(),
                activity_touch_interval: config.auth.activity_touch_interval.get(),
                setup_token_timeout: config.auth.setup_token_timeout.get(),
                max_api_token_lifetime: config.auth.max_api_token_lifetime.get(),
                store_timeout: config.auth.store_timeout.get(),
                password: PasswordConfig {
                    memory_kib: config.auth.password_memory_kib,
                    iterations: config.auth.password_iterations,
                    parallelism: config.auth.password_parallelism,
                    max_concurrency: config.auth.password_max_concurrency,
                },
                login_rate_limit: LoginRateLimitConfig {
                    max_attempts: config.auth.login_max_attempts,
                    window: config.auth.login_window.get(),
                    capacity: config.auth.login_capacity,
                },
            },
        )?);
        if let Some(token) =
            startup_bootstrap_token(identity_service.ensure_bootstrap_token().await)?
        {
            eprintln!(
                "FAULTKEEP_BOOTSTRAP_TOKEN={} (shown once; store it securely)",
                token.encode_hex()
            );
        }
        let event_codec = EventCodecConfig {
            compression_level: config.ingest.event_codec.compression_level,
            compression_min_savings: config.ingest.event_codec.compression_min_savings,
            max_decoded_body_bytes: config.ingest.max_event_bytes,
            max_encoded_document_bytes: config.ingest.max_event_bytes.saturating_add(64 * 1024),
        };
        let archive_task = if config.archive.enabled {
            let service = ArchiveService::new(
                std::sync::Arc::new(store.archive_store(event_codec)),
                std::sync::Arc::clone(&blob_store),
                std::sync::Arc::clone(&clock),
                ArchiveConfig {
                    maximum_events: config.archive.maximum_events,
                    target_uncompressed_bytes: config.archive.target_uncompressed_bytes,
                    write_chunk_bytes: config.archive.write_chunk_bytes,
                    poll_interval: config.archive.poll_interval.get(),
                    hot_copy_delay: config.archive.hot_copy_delay.get(),
                    orphan_grace: config.archive.orphan_grace.get(),
                    cleanup_max_pages: config.archive.cleanup_max_pages,
                },
            )?;
            Some(start_archive_worker(service, shutdown.signal()))
        } else {
            None
        };
        let issue_codec = IssueCodecConfig::default();
        let issue_service = std::sync::Arc::new(faultkeep_application::issues::IssueService::new(
            std::sync::Arc::new(store.issue_store(issue_codec)),
        ));
        let investigation: std::sync::Arc<dyn faultkeep_ports::InvestigationStore> =
            std::sync::Arc::new(store.investigation_store(event_codec, issue_codec));
        let search = std::sync::Arc::new(
            SearchService::new(
                std::sync::Arc::clone(&investigation),
                std::sync::Arc::clone(&clock),
                SearchConfig::default(),
            )
            .map_err(|_| ServerError::Auth(AuthError::InvalidConfiguration))?,
        );
        let capsule_access: std::sync::Arc<dyn IncidentCapsuleAccess> = identity_service.clone();
        let incident_capsule_service = std::sync::Arc::new(IncidentCapsuleService::new(
            capsule_access,
            std::sync::Arc::clone(&issue_service),
            std::sync::Arc::clone(&investigation),
            std::sync::Arc::clone(&clock),
            IncidentCapsuleConfig {
                max_events: config.incident_capsule.max_events,
                max_activities: config.incident_capsule.max_activities,
                max_total_uncompressed_bytes: config.incident_capsule.max_total_uncompressed_bytes,
                max_entry_bytes: config.incident_capsule.max_entry_bytes,
                generation_timeout: config.incident_capsule.generation_timeout.get(),
                max_concurrency: config.incident_capsule.max_concurrency,
                stream_chunk_bytes: config.incident_capsule.stream_chunk_bytes,
                stream_buffer_chunks: config.incident_capsule.stream_buffer_chunks,
            },
            shutdown.signal(),
        )?);
        let signal_store: std::sync::Arc<dyn SignalStore> = std::sync::Arc::new(
            store.signal_store_with_retention(faultkeep_mongo::SignalRetention {
                logs_days: config.retention.logs_days,
                spans_days: config.retention.spans_days,
                span_stats_hourly_days: config.retention.span_stats_hourly_days,
            }),
        );
        let (log_writer, log_writer_task) = LogWriter::start(
            std::sync::Arc::clone(&signal_store),
            LogWriterConfig {
                channel_capacity: config.ingest.max_waiting_for_storage,
                max_wait: config.ingest.batch.max_wait.get(),
                max_documents: config.ingest.batch.max_documents,
                max_bytes: config.ingest.batch.max_bytes,
                operation_timeout: config.ingest.request_timeout.get(),
                shutdown_drain: config.server.shutdown_grace.get(),
            },
            shutdown.signal(),
        )?;
        let log_sink: std::sync::Arc<dyn LogSink> = log_writer;
        let (span_writer, span_writer_task) = SpanWriter::start(
            std::sync::Arc::clone(&signal_store),
            SpanWriterConfig {
                channel_capacity: config.ingest.max_waiting_for_storage,
                max_wait: config.ingest.batch.max_wait.get(),
                max_documents: config.ingest.batch.max_documents,
                max_bytes: config.ingest.batch.max_bytes,
                operation_timeout: config.ingest.request_timeout.get(),
                shutdown_drain: config.server.shutdown_grace.get(),
            },
            shutdown.signal(),
        )?;
        let span_sink: std::sync::Arc<dyn SpanSink> = span_writer;
        let native_api_service = std::sync::Arc::new(
            NativeApiService::new(
                std::sync::Arc::clone(&identity_service),
                project_service,
                issue_service,
                std::sync::Arc::clone(&investigation),
                search,
                std::sync::Arc::clone(&clock),
            )
            .with_project_deletion(deletion_service)
            .with_blob_store(std::sync::Arc::clone(&blob_store))
            .with_signal_store(std::sync::Arc::clone(&signal_store)),
        );
        let debug_metadata: std::sync::Arc<dyn faultkeep_ports::DebugFileStore> =
            std::sync::Arc::new(store.debug_file_store(faultkeep_mongo::DebugFileQuota::default()));
        let debug_file_service = std::sync::Arc::new(DebugFileService::new(
            debug_metadata,
            std::sync::Arc::clone(&blob_store),
            std::sync::Arc::clone(&clock),
            DebugFileConfig {
                max_file_bytes: config
                    .blob
                    .max_object_bytes
                    .min(faultkeep_application::debug_files::SENTRY_CLI_MAX_FILE_BYTES),
                ..DebugFileConfig::default()
            },
        )?);
        debug_file_service.recover(100).await?;
        let _ = debug_file_service.cleanup_once().await;
        let debug_file_cleanup_task = start_debug_file_cleanup(
            std::sync::Arc::clone(&debug_file_service),
            config.ingest.attachments.cleanup_interval.get(),
            shutdown.signal(),
        )?;
        let artifact_metadata: std::sync::Arc<dyn faultkeep_ports::ArtifactStore> =
            std::sync::Arc::new(store.artifact_store(faultkeep_mongo::ArtifactQuota {
                maximum_bytes_per_organization: config.artifacts.maximum_bytes_per_organization,
                maximum_bundles_per_organization: config.artifacts.maximum_bundles_per_organization,
            }));
        let artifact_service = std::sync::Arc::new(ArtifactService::new(
            artifact_metadata,
            std::sync::Arc::clone(&blob_store),
            std::sync::Arc::clone(&clock),
            std::sync::Arc::clone(&random),
            ArtifactConfig {
                maximum_bundle_bytes: config
                    .blob
                    .max_object_bytes
                    .min(config.artifacts.maximum_bundle_bytes),
                maximum_logical_bytes: config.artifacts.maximum_logical_bytes,
                maximum_entries: config.artifacts.maximum_entries,
                maximum_entry_bytes: config.artifacts.maximum_entry_bytes,
                maximum_concurrent_assemblies: config.artifacts.maximum_concurrent_assemblies,
                parse_timeout: config.artifacts.parse_timeout.get(),
                orphan_grace: config.artifacts.orphan_grace.get(),
                claim_lease: config.artifacts.claim_lease.get(),
                blob_operation_timeout: config.artifacts.blob_operation_timeout.get(),
                tombstone_retention: config.artifacts.tombstone_retention.get(),
                gc_batch_size: config.artifacts.gc_batch_size,
                gc_max_concurrency: config.artifacts.gc_max_concurrency,
                ..ArtifactConfig::default()
            },
        )?);
        artifact_service.recover(100).await?;
        let _ = artifact_service.gc_once().await;
        let artifact_cleanup_task = start_artifact_cleanup(
            std::sync::Arc::clone(&artifact_service),
            config.artifacts.gc_interval.get(),
            shutdown.signal(),
        )?;
        let event_store = std::sync::Arc::new(store.event_store(event_codec));
        let blob_references: std::sync::Arc<dyn BlobReferenceStore> = event_store.clone();
        let blob_cleanup_task = start_blob_cleanup_worker(
            std::sync::Arc::new(BlobCleanupService::new(
                std::sync::Arc::clone(&blob_store),
                blob_references,
                std::sync::Arc::clone(&clock),
                BlobCleanupConfig {
                    orphan_grace: config.ingest.attachments.orphan_grace.get(),
                    interval: config.ingest.attachments.cleanup_interval.get(),
                    batch_size: config.ingest.attachments.cleanup_batch_size,
                    max_pages_per_run: config.ingest.attachments.cleanup_max_pages,
                },
            )?),
            shutdown.signal(),
        );
        let backlog: std::sync::Arc<dyn EventBacklog> = event_store.clone();
        let finalizer = std::sync::Arc::new(
            Finalizer::new(
                std::sync::Arc::new(store.finalization_store(event_codec, issue_codec)),
                FinalizerConfig {
                    event_retention: config.retention.event_duration(),
                    hourly_retention: config.retention.hourly_duration(),
                    archive_events: config.archive.enabled,
                    ..FinalizerConfig::default()
                },
            )?
            .with_notification_signal(notification_signal),
        );
        let (finalizer_batcher, finalizer_batch_task) = FinalizerBatcher::start(
            finalizer,
            FinalizerBatchConfig {
                shutdown_drain: config.server.shutdown_grace.get(),
                ..FinalizerBatchConfig::default()
            },
        )?;
        let symbolicator: std::sync::Arc<dyn faultkeep_application::processor::SymbolicationStage> =
            if let Some(endpoint) = config.symbolicator.endpoint.clone() {
                let signer = private_source_signer
                    .clone()
                    .ok_or(ServerError::Symbolicator)?;
                let backend = faultkeep_symbolication::ExternalSymbolicator::new(
                    faultkeep_symbolication::ExternalSymbolicatorConfig {
                        endpoint,
                        callback_base_url: config.symbolicator.callback_base_url.clone(),
                        request_timeout: config.symbolicator.request_timeout.get(),
                        maximum_concurrency: config.symbolicator.maximum_concurrency,
                        circuit_failure_threshold: config.symbolicator.circuit_failure_threshold,
                        circuit_cooldown: config.symbolicator.circuit_cooldown.get(),
                        maximum_response_bytes: config.symbolicator.maximum_response_bytes,
                    },
                    signer,
                )
                .map_err(|_| ServerError::Symbolicator)?;
                std::sync::Arc::new(
                    SymbolicationService::new(
                        std::sync::Arc::new(backend),
                        SymbolicationConfig::default(),
                    )
                    .map_err(|_| ServerError::Symbolicator)?,
                )
            } else {
                std::sync::Arc::new(BaselineSymbolicationService)
            };
        let processor = std::sync::Arc::new(Processor::new(
            event_store.clone(),
            event_store.clone(),
            std::sync::Arc::new(Normalizer::new(NormalizerLimits::default())?),
            symbolicator,
            std::sync::Arc::new(GrouperStage),
            std::sync::Arc::new(IssuePreparerStage),
            finalizer_batcher.clone(),
            std::sync::Arc::clone(&clock),
            ProcessorConfig {
                max_concurrency: config.processor.max_concurrency,
                max_attempts: config.processor.max_attempts,
                retry_base: config.processor.retry_base.get(),
                retry_max: config.processor.retry_max.get(),
                stage_timeout: config.processor.stage_timeout.get(),
                total_timeout: config.processor.total_timeout.get(),
                state_timeout: config.processor.state_timeout.get(),
            },
        )?);
        let (dispatcher, dispatcher_task) = Dispatcher::start(
            backlog,
            processor,
            std::sync::Arc::clone(&clock),
            DispatcherConfig {
                queue_capacity: config.dispatcher.queue_capacity,
                worker_concurrency: config.dispatcher.worker_concurrency,
                low_watermark: config.dispatcher.low_watermark,
                refill_target: config.dispatcher.refill_target,
                refill_batch_size: config.dispatcher.refill_batch_size,
                poll_interval: config.dispatcher.poll_interval.get(),
                metrics_interval: config.dispatcher.metrics_interval.get(),
                source_timeout: config.dispatcher.source_timeout.get(),
                shutdown_drain: config.server.shutdown_grace.get(),
                max_pending_events: config.ingest.backlog.max_pending_events,
                max_oldest_pending_age: Some(config.ingest.backlog.max_oldest_pending_age.get()),
            },
            shutdown.signal(),
        )
        .await?;
        let backlog_guard = dispatcher.backlog_guard();
        let (writer, writer_task) = MongoWriter::start(
            event_store,
            dispatcher,
            MongoWriterConfig {
                channel_capacity: config.ingest.max_waiting_for_storage,
                max_wait: config.ingest.batch.max_wait.get(),
                max_documents: config.ingest.batch.max_documents,
                max_bytes: config.ingest.batch.max_bytes,
                operation_timeout: config.ingest.request_timeout.get(),
                shutdown_drain: config.server.shutdown_grace.get(),
            },
            shutdown.signal(),
        )?;
        let writer: std::sync::Arc<dyn EventSink> = writer;
        let event_sink: std::sync::Arc<dyn EventSink> =
            std::sync::Arc::new(BacklogGuardedEventSink::new(writer, backlog_guard));
        let event_sink: std::sync::Arc<dyn EventSink> =
            std::sync::Arc::new(ProjectFencedEventSink::new(event_sink, project_work));
        let project_deletion_task = start_project_deletion_worker(
            deletion_store,
            std::sync::Arc::clone(&clock),
            deletion_config,
            shutdown.signal(),
        )?;
        let (_scheduler, scheduler_task) = Scheduler::start(
            std::sync::Arc::new(store.maintenance_store()),
            std::sync::Arc::clone(&clock),
            SchedulerConfig {
                poll_interval: config.scheduler.poll_interval.get(),
                maintenance_interval: config.scheduler.maintenance_interval.get(),
                reconciliation_interval: config.scheduler.reconciliation_interval.get(),
                backlog_interval: config.scheduler.backlog_interval.get(),
                task_timeout: config.scheduler.task_timeout.get(),
                retry_base: config.scheduler.retry_base.get(),
                retry_max: config.scheduler.retry_max.get(),
                batch_size: config.scheduler.batch_size,
                event_retention: config.retention.event_duration(),
                hourly_retention: config.retention.hourly_duration(),
                archive_events: config.archive.enabled,
            },
            shutdown.signal(),
        )
        .await?;
        RuntimeModules {
            project_resolver,
            event_sink,
            log_sink: Some(log_sink),
            span_sink: Some(span_sink),
            writer_task: Some(writer_task),
            log_writer_task: Some(log_writer_task),
            span_writer_task: Some(span_writer_task),
            dispatcher_task: Some(dispatcher_task),
            scheduler_task: Some(scheduler_task),
            archive_task,
            finalizer_batcher: Some(finalizer_batcher),
            finalizer_batch_task: Some(finalizer_batch_task),
            identity_service: Some(identity_service),
            native_api_service: Some(native_api_service),
            incident_capsule_service: Some(incident_capsule_service),
            notification_task: Some(notification_task),
            project_deletion_task: Some(project_deletion_task),
            blob_cleanup_task: Some(blob_cleanup_task),
            debug_file_service: Some(debug_file_service),
            artifact_service: Some(artifact_service),
            private_source_signer,
            debug_file_cleanup_task: Some(debug_file_cleanup_task),
            artifact_cleanup_task: Some(artifact_cleanup_task),
        }
    } else {
        RuntimeModules {
            project_resolver: std::sync::Arc::new(UnavailableProjectResolver),
            event_sink: std::sync::Arc::new(UnavailableEventSink),
            log_sink: None,
            span_sink: None,
            writer_task: None,
            log_writer_task: None,
            span_writer_task: None,
            dispatcher_task: None,
            scheduler_task: None,
            archive_task: None,
            finalizer_batcher: None,
            finalizer_batch_task: None,
            identity_service: None,
            native_api_service: None,
            incident_capsule_service: None,
            notification_task: None,
            project_deletion_task: None,
            blob_cleanup_task: None,
            debug_file_service: None,
            artifact_service: None,
            private_source_signer: None,
            debug_file_cleanup_task: None,
            artifact_cleanup_task: None,
        }
    };
    let mut ingest_service = faultkeep_application::ingest::IngestService::new(
        project_resolver,
        event_sink,
        std::sync::Arc::new(NoopOutcomeSink),
        clock,
        random,
        config.ingest.max_waiting_for_storage,
        shutdown.signal(),
    )
    .with_blob_store(
        blob_store,
        faultkeep_application::ingest::AttachmentIngestConfig {
            enabled: config.ingest.attachments.enabled,
            chunk_bytes: config.ingest.attachments.chunk_bytes,
        },
    )
    .with_minidumps(faultkeep_application::ingest::MinidumpIngestConfig {
        enabled: config.native_crash.minidump.enabled,
        max_bytes: config.native_crash.minidump.max_bytes,
        chunk_bytes: config.native_crash.minidump.chunk_bytes,
        retained_header_bytes: 64 * 1024,
    });
    if let Some(log_sink) = log_sink {
        ingest_service = ingest_service.with_log_sink(log_sink);
    }
    if let Some(span_sink) = span_sink {
        ingest_service = ingest_service.with_span_sink(span_sink);
    }
    let ingest = std::sync::Arc::new(ingest_service);
    let required_ready = writer_task.is_some()
        && log_writer_task.is_some()
        && span_writer_task.is_some()
        && dispatcher_task.is_some()
        && scheduler_task.is_some()
        && notification_task.is_some()
        && (!config.archive.enabled || archive_task.is_some());
    let application_routes = ingest_http::router(ingest, config.ingest.clone(), shutdown.signal())
        .merge(native_http::router(
            identity_service.clone(),
            native_api_service,
            config.auth.secure_cookie,
            required_ready,
            native_http::NativeHttpModules {
                retention: required_ready.then_some(native_http::RetentionCapability {
                    events_days: config.retention.events_days,
                    issue_stats_hourly_days: config.retention.issue_stats_hourly_days,
                    logs_days: config.retention.logs_days,
                    spans_days: config.retention.spans_days,
                    span_stats_hourly_days: config.retention.span_stats_hourly_days,
                }),
                project_deletion: required_ready.then_some(
                    native_http::ProjectDeletionCapability {
                        grace_period_seconds: config.project_deletion.grace_period.get().as_secs(),
                        delete_batch_documents: config.project_deletion.delete_batch_documents,
                        slug_reservation_seconds: config
                            .project_deletion
                            .slug_reservation
                            .get()
                            .as_secs(),
                    },
                ),
                debug_files: required_ready.then_some(native_http::DebugFileCapability {
                    external_symbolicator: config.symbolicator.endpoint.is_some(),
                    artifact_bundles: true,
                }),
                incident_capsule: incident_capsule_service,
                notifications: notification_task.is_some(),
            },
        ))
        .merge(debug_http::router(
            identity_service.clone(),
            debug_file_service,
            artifact_service,
            runtime_private_source_signer,
        ))
        .merge(web_http::router());
    let app = http::router_with_readiness(
        shutdown.signal(),
        metrics,
        application_routes,
        required_ready,
    );
    let listener = TcpListener::bind(config.server.http_address).await?;
    info!(
        operation = "runtime.ready",
        role = %config.role,
        address = %config.server.http_address,
        "HTTP listener ready"
    );

    let server = http::run(
        listener,
        shutdown.signal(),
        config.server.shutdown_grace.get(),
        app,
    );
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result?,
        () = wait_for_os_shutdown() => {
            warn!(operation = "runtime.shutdown", "shutdown signal received");
            shutdown.begin();
            server.await?;
        }
    }
    shutdown.begin();
    if let Some(task) = scheduler_task {
        task.wait().await;
    }
    if let Some(task) = archive_task {
        task.wait().await;
    }
    if let Some(task) = notification_task {
        task.wait().await;
    }
    if let Some(task) = project_deletion_task {
        task.wait().await;
    }
    if let Some(task) = blob_cleanup_task {
        task.wait().await;
    }
    if let Some(task) = debug_file_cleanup_task {
        task.wait().await;
    }
    if let Some(task) = artifact_cleanup_task {
        task.wait().await;
    }
    if let Some(task) = writer_task {
        task.wait().await;
    }
    if let Some(task) = log_writer_task {
        task.wait().await;
    }
    if let Some(task) = span_writer_task {
        task.wait().await;
    }
    if let Some(task) = dispatcher_task {
        task.wait().await;
    }
    if let Some(batcher) = finalizer_batcher {
        batcher.close();
    }
    if let Some(task) = finalizer_batch_task {
        task.wait().await;
    }
    metrics.increment(Metric::Shutdowns, Outcome::Ok);
    info!(operation = "runtime.stopped", "graceful shutdown complete");
    Ok(ExitCode::SUCCESS)
}

struct UnavailableProjectResolver;

impl ProjectResolver for UnavailableProjectResolver {
    fn resolve(
        &self,
        _key: faultkeep_domain::DsnKey,
    ) -> PortFuture<'_, Result<faultkeep_domain::ProjectSnapshot, ProjectResolveError>> {
        Box::pin(async { Err(ProjectResolveError::Unavailable) })
    }
}

struct UnavailableEventSink;

impl EventSink for UnavailableEventSink {
    fn persist(
        &self,
        _event: faultkeep_domain::AcceptedEvent,
    ) -> PortFuture<'_, Result<faultkeep_ports::DurableOutcome, EventSinkError>> {
        Box::pin(async { Err(EventSinkError::Unavailable) })
    }
}

struct NoopOutcomeSink;

impl OutcomeSink for NoopOutcomeSink {
    fn record(&self, _outcome: faultkeep_ports::IngestOutcome) {}
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Timestamp::from_unix_millis(i64::try_from(millis).unwrap_or(i64::MAX))
            .expect("current system time is in the supported range")
    }
}

struct SystemRandom;

impl RandomSource for SystemRandom {
    fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
        getrandom::fill(output).map_err(|_| RandomError)
    }
}

fn startup_bootstrap_token(
    result: Result<Option<faultkeep_domain::auth::PlainSecret>, AuthError>,
) -> Result<Option<faultkeep_domain::auth::PlainSecret>, AuthError> {
    match result {
        Err(AuthError::BootstrapClosed) => Ok(None),
        result => result,
    }
}

fn init_tracing() -> Result<(), ServerError> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init()
        .map_err(|_| ServerError::Tracing)
}

#[cfg(unix)]
async fn wait_for_os_shutdown() {
    use tokio::signal::unix::{SignalKind, signal};

    let terminate = signal(SignalKind::terminate());
    match terminate {
        Ok(mut terminate) => {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if result.is_err() {
                        error!(operation = "runtime.signal", error_code = "signal_handler_failed", "signal handler failed");
                    }
                }
                _ = terminate.recv() => {}
            }
        }
        Err(_) => {
            error!(
                operation = "runtime.signal",
                error_code = "signal_handler_failed",
                "SIGTERM handler failed"
            );
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
async fn wait_for_os_shutdown() {
    if tokio::signal::ctrl_c().await.is_err() {
        error!(
            operation = "runtime.signal",
            error_code = "signal_handler_failed",
            "signal handler failed"
        );
    }
}

#[cfg(test)]
mod production_fence_tests {
    use super::*;

    #[test]
    fn completed_bootstrap_is_a_valid_startup_state() {
        assert_eq!(
            startup_bootstrap_token(Err(AuthError::BootstrapClosed)),
            Ok(None)
        );
        assert_eq!(
            startup_bootstrap_token(Err(AuthError::Unavailable)),
            Err(AuthError::Unavailable)
        );
    }

    #[tokio::test]
    async fn production_composition_has_no_fake_project_or_durable_success() {
        let key = faultkeep_domain::DsnKey::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(
            UnavailableProjectResolver.resolve(key).await,
            Err(ProjectResolveError::Unavailable)
        );
    }
}
