use std::{
    collections::{BTreeMap, BTreeSet},
    future::{Future, IntoFuture},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::TryStreamExt;
use mongodb::{
    Collection, Database,
    bson::{Bson, DateTime, Document, doc, oid::ObjectId},
    error::{Error as MongoError, ErrorKind},
    options::ReturnDocument,
};
use thiserror::Error;
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};

mod dsn_maps;
mod telegram_configuration;
pub(crate) use dsn_maps::DSN_MAPS_MIGRATION;

pub(crate) use telegram_configuration::TELEGRAM_CONFIGURATION_MIGRATION;

const SCHEMA_ID: &str = "metric.schema";
const MAX_CHECKPOINT_BYTES: usize = 4 * 1024;
const MAX_WARNING_CODES: usize = 16;
const MAX_WARNING_CODE_BYTES: usize = 64;
const MAX_MIGRATION_NAME_BYTES: usize = 96;
const MAX_DOCUMENT_BATCH_COUNT: u32 = 1_000;
const MAX_DOCUMENT_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_DOCUMENT_CURSOR_BATCH_COUNT: u32 = 16;

pub type MigrationFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaMigrationProgress {
    Inspecting,
    Waiting {
        from_generation: i32,
        to_generation: i32,
        completed_steps: u32,
        total_steps: u32,
        processed_records: u64,
        warnings: u64,
    },
    Running {
        from_generation: i32,
        to_generation: i32,
        completed_steps: u32,
        total_steps: u32,
        processed_records: u64,
        warnings: u64,
    },
    Complete {
        generation: i32,
    },
}

pub trait SchemaMigrationReporter: Send + Sync {
    fn report(&self, progress: SchemaMigrationProgress);
}

impl<F> SchemaMigrationReporter for F
where
    F: Fn(SchemaMigrationProgress) + Send + Sync,
{
    fn report(&self, progress: SchemaMigrationProgress) {
        self(progress);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordErrorPolicy {
    Strict,
    Tolerate { maximum_warnings: u64 },
    Rebuildable { maximum_warnings: u64 },
}

impl RecordErrorPolicy {
    fn permits(self, warnings: u64) -> bool {
        match self {
            Self::Strict => warnings == 0,
            Self::Tolerate { maximum_warnings } | Self::Rebuildable { maximum_warnings } => {
                warnings <= maximum_warnings
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MigrationCheckpoint {
    pub cursor: Option<Bson>,
    pub processed_records: u64,
    pub step_warnings: u64,
    pub warning_counts: BTreeMap<String, u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationDocumentLimits {
    pub maximum_documents: u32,
    pub maximum_bytes: usize,
}

impl MigrationDocumentLimits {
    pub fn new(maximum_documents: u32, maximum_bytes: usize) -> Result<Self, MongoMigrationError> {
        if maximum_documents == 0
            || maximum_documents > MAX_DOCUMENT_BATCH_COUNT
            || maximum_bytes == 0
            || maximum_bytes > MAX_DOCUMENT_BATCH_BYTES
        {
            return Err(MongoMigrationError::InvalidRegistry {
                code: "invalid_document_batch_limits",
            });
        }
        Ok(Self {
            maximum_documents,
            maximum_bytes,
        })
    }
}

#[derive(Debug)]
pub struct BoundedDocumentPage {
    pub documents: Vec<Document>,
    pub next_cursor: Option<Bson>,
    pub encoded_bytes: usize,
    pub exhausted: bool,
}

pub async fn load_bounded_document_page(
    collection: &Collection<Document>,
    filter: Document,
    after: Option<&Bson>,
    mut projection: Document,
    limits: MigrationDocumentLimits,
) -> Result<BoundedDocumentPage, MigrationAttemptError> {
    projection.insert("_id", 1_i32);
    let filter = after.map_or(filter.clone(), |cursor| {
        doc! { "$and": [filter, { "_id": { "$gt": cursor.clone() } }] }
    });
    let mut cursor = collection
        .find(filter)
        .projection(projection)
        .sort(doc! { "_id": 1 })
        .limit(i64::from(limits.maximum_documents))
        // Keep driver prefetch small as well as bounding the page retained by the
        // migration. This avoids one network round trip per small document without
        // allowing a large count-based cursor batch to dominate process memory.
        .batch_size(
            limits
                .maximum_documents
                .min(MAX_DOCUMENT_CURSOR_BATCH_COUNT),
        )
        .await
        .map_err(MigrationAttemptError::from_mongo)?;
    let capacity = usize::try_from(limits.maximum_documents).unwrap_or(1_000);
    let mut documents = Vec::with_capacity(capacity);
    let mut encoded_bytes = 0_usize;
    let mut next_cursor = None;
    let mut exhausted = true;
    while let Some(document) = cursor
        .try_next()
        .await
        .map_err(MigrationAttemptError::from_mongo)?
    {
        let size = mongodb::bson::to_vec(&document)
            .map_err(|_| MigrationAttemptError::blocked("migration_document_cannot_be_encoded"))?
            .len();
        if size > limits.maximum_bytes {
            return Err(MigrationAttemptError::blocked(
                "migration_document_exceeds_batch_byte_limit",
            ));
        }
        if !documents.is_empty()
            && encoded_bytes
                .checked_add(size)
                .is_none_or(|total| total > limits.maximum_bytes)
        {
            exhausted = false;
            break;
        }
        encoded_bytes = encoded_bytes.checked_add(size).ok_or_else(|| {
            MigrationAttemptError::blocked("migration_document_batch_size_overflow")
        })?;
        next_cursor = Some(
            document
                .get("_id")
                .cloned()
                .ok_or_else(|| MigrationAttemptError::blocked("migration_document_id_missing"))?,
        );
        documents.push(document);
    }
    if documents.len() == capacity {
        exhausted = false;
    }
    Ok(BoundedDocumentPage {
        documents,
        next_cursor,
        encoded_bytes,
        exhausted,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct MigrationBatch {
    pub next_cursor: Option<Bson>,
    pub processed_records: u64,
    pub warning_counts: BTreeMap<&'static str, u64>,
    pub step_complete: bool,
}

impl MigrationBatch {
    #[must_use]
    pub fn complete(processed_records: u64) -> Self {
        Self {
            next_cursor: None,
            processed_records,
            warning_counts: BTreeMap::new(),
            step_complete: true,
        }
    }

    #[must_use]
    pub fn more(next_cursor: Bson, processed_records: u64) -> Self {
        Self {
            next_cursor: Some(next_cursor),
            processed_records,
            warning_counts: BTreeMap::new(),
            step_complete: false,
        }
    }

    #[must_use]
    pub fn with_warning(mut self, code: &'static str, count: u64) -> Self {
        self.warning_counts.insert(code, count);
        self
    }
}

#[derive(Debug, Error)]
pub enum MigrationAttemptError {
    #[error("transient MongoDB migration operation failed")]
    Retryable(#[source] MongoError),
    #[error("database migration is blocked ({code})")]
    Blocked {
        code: &'static str,
        #[source]
        source: Option<MongoError>,
    },
}

impl MigrationAttemptError {
    #[must_use]
    pub fn blocked(code: &'static str) -> Self {
        Self::Blocked { code, source: None }
    }

    #[must_use]
    pub fn from_mongo(error: MongoError) -> Self {
        if is_retryable_mongo_error(&error) {
            Self::Retryable(error)
        } else {
            Self::Blocked {
                code: "migration_database_operation_rejected",
                source: Some(error),
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum MongoMigrationError {
    #[error("database schema generation {found} is newer than supported generation {supported}")]
    DatabaseNewer { found: i32, supported: i32 },
    #[error("database migration chain is incomplete between generations {from} and {target}")]
    MissingTransition { from: i32, target: i32 },
    #[error("database migration registry is invalid ({code})")]
    InvalidRegistry { code: &'static str },
    #[error("database migration state is invalid ({code})")]
    InvalidState { code: &'static str },
    #[error("database migration is blocked ({code})")]
    Blocked {
        code: &'static str,
        #[source]
        source: Option<MongoError>,
    },
}

impl From<MigrationAttemptError> for MongoMigrationError {
    fn from(error: MigrationAttemptError) -> Self {
        match error {
            MigrationAttemptError::Retryable(source) => Self::Blocked {
                code: "migration_retry_state_escaped",
                source: Some(source),
            },
            MigrationAttemptError::Blocked { code, source } => Self::Blocked { code, source },
        }
    }
}

pub trait MongoMigration: Send + Sync {
    fn source_generation(&self) -> i32;
    fn target_generation(&self) -> i32;
    fn name(&self) -> &'static str;
    fn step_count(&self) -> u32;
    fn step_name(&self, step: u32) -> &'static str;
    fn record_error_policy(&self, step: u32) -> RecordErrorPolicy;

    fn preflight<'a>(
        &'a self,
        database: &'a Database,
    ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>>;

    fn run_batch<'a>(
        &'a self,
        database: &'a Database,
        step: u32,
        checkpoint: &'a MigrationCheckpoint,
    ) -> MigrationFuture<'a, Result<MigrationBatch, MigrationAttemptError>>;

    fn verify<'a>(
        &'a self,
        database: &'a Database,
    ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>>;
}

#[derive(Clone, Copy)]
pub struct MigrationRunnerConfig {
    pub retry_initial: Duration,
    pub retry_maximum: Duration,
    pub lease_duration: Duration,
    pub lease_renew_interval: Duration,
    pub lease_wait_interval: Duration,
}

impl Default for MigrationRunnerConfig {
    fn default() -> Self {
        Self {
            retry_initial: Duration::from_millis(250),
            retry_maximum: Duration::from_secs(30),
            lease_duration: Duration::from_secs(60),
            lease_renew_interval: Duration::from_secs(10),
            lease_wait_interval: Duration::from_secs(1),
        }
    }
}

pub struct MigrationRegistry<'a> {
    target_generation: i32,
    transitions: &'a [&'a dyn MongoMigration],
}

impl<'a> MigrationRegistry<'a> {
    #[must_use]
    pub const fn new(target_generation: i32, transitions: &'a [&'a dyn MongoMigration]) -> Self {
        Self {
            target_generation,
            transitions,
        }
    }

    fn plan(&self, current: i32) -> Result<Vec<&'a dyn MongoMigration>, MongoMigrationError> {
        if self.target_generation < 0 || current < 0 {
            return Err(MongoMigrationError::InvalidRegistry {
                code: "negative_schema_generation",
            });
        }
        if current > self.target_generation {
            return Err(MongoMigrationError::DatabaseNewer {
                found: current,
                supported: self.target_generation,
            });
        }
        let mut sources = BTreeSet::new();
        let mut previous_source = None;
        for transition in self.transitions {
            if transition.source_generation() < 0
                || transition.source_generation().checked_add(1)
                    != Some(transition.target_generation())
                || transition.step_count() == 0
                || i32::try_from(transition.step_count()).is_err()
                || !valid_static_name(transition.name())
            {
                return Err(MongoMigrationError::InvalidRegistry {
                    code: "invalid_transition_descriptor",
                });
            }
            if !sources.insert(transition.source_generation()) {
                return Err(MongoMigrationError::InvalidRegistry {
                    code: "duplicate_transition_source",
                });
            }
            if previous_source.is_some_and(|source| transition.source_generation() < source) {
                return Err(MongoMigrationError::InvalidRegistry {
                    code: "unordered_transitions",
                });
            }
            previous_source = Some(transition.source_generation());
            for step in 0..transition.step_count() {
                if !valid_static_name(transition.step_name(step)) {
                    return Err(MongoMigrationError::InvalidRegistry {
                        code: "empty_step_name",
                    });
                }
            }
        }

        let mut generation = current;
        let mut plan = Vec::new();
        while generation < self.target_generation {
            let Some(transition) = self
                .transitions
                .iter()
                .copied()
                .find(|migration| migration.source_generation() == generation)
            else {
                return Err(MongoMigrationError::MissingTransition {
                    from: generation,
                    target: self.target_generation,
                });
            };
            generation = transition.target_generation();
            plan.push(transition);
        }
        Ok(plan)
    }
}

fn valid_static_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MIGRATION_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub async fn migrate_to_target(
    database: &Database,
    registry: MigrationRegistry<'_>,
    reporter: &dyn SchemaMigrationReporter,
) -> Result<(), MongoMigrationError> {
    migrate_to_target_with_config(
        database,
        registry,
        reporter,
        MigrationRunnerConfig::default(),
    )
    .await
}

async fn migrate_to_target_with_config(
    database: &Database,
    registry: MigrationRegistry<'_>,
    reporter: &dyn SchemaMigrationReporter,
    config: MigrationRunnerConfig,
) -> Result<(), MongoMigrationError> {
    reporter.report(SchemaMigrationProgress::Inspecting);
    let marker = read_marker(database, config).await?;
    let current = marker_generation(&marker)?;
    let state = marker
        .get_str("state")
        .map_err(|_| MongoMigrationError::InvalidState {
            code: "missing_schema_state",
        })?;
    if !matches!(state, "complete" | "migrating") {
        return Err(MongoMigrationError::InvalidState {
            code: "unsupported_schema_state",
        });
    }
    if state == "migrating" && current == registry.target_generation {
        return Err(MongoMigrationError::InvalidState {
            code: "unsupported_in_progress_transition",
        });
    }
    let plan = registry.plan(current)?;
    let total_steps = plan.iter().try_fold(0_u32, |total, transition| {
        total
            .checked_add(transition.step_count())
            .ok_or(MongoMigrationError::InvalidRegistry {
                code: "too_many_migration_steps",
            })
    })?;
    let mut completed_before = 0_u32;

    for transition in plan {
        run_transition(
            database,
            transition,
            completed_before,
            total_steps,
            reporter,
            config,
        )
        .await?;
        completed_before = completed_before
            .checked_add(transition.step_count())
            .ok_or(MongoMigrationError::InvalidRegistry {
                code: "too_many_migration_steps",
            })?;
    }

    reporter.report(SchemaMigrationProgress::Complete {
        generation: registry.target_generation,
    });
    Ok(())
}

async fn run_transition(
    database: &Database,
    migration: &dyn MongoMigration,
    completed_before: u32,
    total_steps: u32,
    reporter: &dyn SchemaMigrationReporter,
    config: MigrationRunnerConfig,
) -> Result<(), MongoMigrationError> {
    let context = TransitionContext {
        completed_before,
        total_steps,
        reporter,
        config,
    };
    loop {
        let lease = acquire_lease(
            database,
            migration,
            completed_before,
            total_steps,
            reporter,
            config,
        )
        .await?;
        let Some(lease) = lease else {
            return Ok(());
        };
        let heartbeat = LeaseHeartbeat::start(database.clone(), &lease, config);
        let result = run_owned_transition(database, migration, &lease, &heartbeat, &context).await;
        heartbeat.stop().await;
        match result {
            Ok(()) => return Ok(()),
            Err(OwnedTransitionError::LeaseLost) => {
                sleep(config.lease_wait_interval).await;
            }
            Err(OwnedTransitionError::Migration(error)) => return Err(error),
        }
    }
}

struct TransitionContext<'a> {
    completed_before: u32,
    total_steps: u32,
    reporter: &'a dyn SchemaMigrationReporter,
    config: MigrationRunnerConfig,
}

async fn run_owned_transition(
    database: &Database,
    migration: &dyn MongoMigration,
    lease: &MigrationLease,
    heartbeat: &LeaseHeartbeat,
    context: &TransitionContext<'_>,
) -> Result<(), OwnedTransitionError> {
    retry_preflight(migration, database, heartbeat, context.config).await?;
    let marker = read_marker(database, context.config).await?;
    let mut checkpoint = parse_checkpoint(&marker, migration)?;
    let mut step = marker_step(&marker)?;

    while step < migration.step_count() {
        if heartbeat.is_lost() {
            return Err(OwnedTransitionError::LeaseLost);
        }
        report_running(
            context.reporter,
            migration,
            context.completed_before.saturating_add(step),
            context.total_steps,
            &checkpoint,
        );
        let batch = retry_batch(
            migration,
            database,
            step,
            &checkpoint,
            heartbeat,
            context.config,
        )
        .await?;
        validate_batch(&batch)?;
        merge_batch(&mut checkpoint, &batch, migration.record_error_policy(step))?;
        let next_step = if batch.step_complete {
            step.saturating_add(1)
        } else {
            step
        };
        persist_checkpoint(
            database,
            migration,
            lease,
            next_step,
            &checkpoint,
            batch.step_complete,
            context.config,
        )
        .await?;
        step = next_step;
        if batch.step_complete {
            checkpoint.cursor = None;
            checkpoint.step_warnings = 0;
        }
    }

    retry_verify(migration, database, heartbeat, context.config).await?;
    if heartbeat.is_lost() {
        return Err(OwnedTransitionError::LeaseLost);
    }
    let collection = database.collection::<Document>("schema_meta");
    let result = metadata_operation(
        || {
            collection.update_one(
                doc! {
                    "_id": SCHEMA_ID,
                    "generation": migration.source_generation(),
                    "state": "migrating",
                    "migration.from": migration.source_generation(),
                    "migration.to": migration.target_generation(),
                    "migration.name": migration.name(),
                    "migration.owner": &lease.owner,
                },
                doc! {
                    "$set": {
                        "generation": migration.target_generation(),
                        "state": "complete",
                    },
                    "$unset": { "migration": "" },
                },
            )
        },
        context.config,
    )
    .await?;
    if result.modified_count != 1 {
        return Err(OwnedTransitionError::LeaseLost);
    }
    Ok(())
}

fn report_running(
    reporter: &dyn SchemaMigrationReporter,
    migration: &dyn MongoMigration,
    completed_steps: u32,
    total_steps: u32,
    checkpoint: &MigrationCheckpoint,
) {
    reporter.report(SchemaMigrationProgress::Running {
        from_generation: migration.source_generation(),
        to_generation: migration.target_generation(),
        completed_steps,
        total_steps,
        processed_records: checkpoint.processed_records,
        warnings: warning_total(&checkpoint.warning_counts),
    });
}

async fn acquire_lease(
    database: &Database,
    migration: &dyn MongoMigration,
    completed_before: u32,
    total_steps: u32,
    reporter: &dyn SchemaMigrationReporter,
    config: MigrationRunnerConfig,
) -> Result<Option<MigrationLease>, MongoMigrationError> {
    let owner = ObjectId::new().to_hex();
    loop {
        let now = DateTime::now();
        let lease_until = add_duration(now, config.lease_duration)?;
        let initial = migration_document(migration, &owner, lease_until);
        let collection = database.collection::<Document>("schema_meta");
        let acquired = metadata_operation(
            || {
                collection
                    .find_one_and_update(
                        doc! {
                            "_id": SCHEMA_ID,
                            "generation": migration.source_generation(),
                            "state": "complete",
                        },
                        doc! {
                            "$set": {
                                "state": "migrating",
                                "migration": initial.clone(),
                            }
                        },
                    )
                    .return_document(ReturnDocument::After)
            },
            config,
        )
        .await?;
        if acquired.is_some() {
            return Ok(Some(MigrationLease {
                owner,
                from_generation: migration.source_generation(),
                to_generation: migration.target_generation(),
                name: migration.name(),
            }));
        }

        let marker = read_marker(database, config).await?;
        let generation = marker_generation(&marker)?;
        let state = marker
            .get_str("state")
            .map_err(|_| MongoMigrationError::InvalidState {
                code: "missing_schema_state",
            })?;
        if state == "complete" && generation >= migration.target_generation() {
            return Ok(None);
        }
        if state != "migrating" || generation != migration.source_generation() {
            return Err(MongoMigrationError::InvalidState {
                code: "unexpected_transition_state",
            });
        }
        validate_in_progress(&marker, migration)?;
        let migration_state =
            marker
                .get_document("migration")
                .map_err(|_| MongoMigrationError::InvalidState {
                    code: "missing_migration_state",
                })?;
        if migration_state.get_str("owner") == Ok(owner.as_str()) {
            return Ok(Some(MigrationLease {
                owner,
                from_generation: migration.source_generation(),
                to_generation: migration.target_generation(),
                name: migration.name(),
            }));
        }
        let existing_until = migration_state.get_datetime("lease_until").map_err(|_| {
            MongoMigrationError::InvalidState {
                code: "missing_migration_lease",
            }
        })?;
        if existing_until.timestamp_millis() <= now.timestamp_millis() {
            let claimed = metadata_operation(
                || {
                    collection
                        .find_one_and_update(
                            doc! {
                                "_id": SCHEMA_ID,
                                "generation": migration.source_generation(),
                                "state": "migrating",
                                "migration.from": migration.source_generation(),
                                "migration.to": migration.target_generation(),
                                "migration.name": migration.name(),
                                "migration.lease_until": { "$lte": now },
                            },
                            doc! {
                                "$set": {
                                    "migration.owner": &owner,
                                    "migration.lease_until": lease_until,
                                }
                            },
                        )
                        .return_document(ReturnDocument::After)
                },
                config,
            )
            .await?;
            if claimed.is_some() {
                return Ok(Some(MigrationLease {
                    owner,
                    from_generation: migration.source_generation(),
                    to_generation: migration.target_generation(),
                    name: migration.name(),
                }));
            }
            continue;
        }
        let checkpoint = parse_checkpoint(&marker, migration)?;
        let step = marker_step(&marker)?;
        reporter.report(SchemaMigrationProgress::Waiting {
            from_generation: migration.source_generation(),
            to_generation: migration.target_generation(),
            completed_steps: completed_before.saturating_add(step),
            total_steps,
            processed_records: checkpoint.processed_records,
            warnings: warning_total(&checkpoint.warning_counts),
        });
        sleep(config.lease_wait_interval).await;
    }
}

fn migration_document(
    migration: &dyn MongoMigration,
    owner: &str,
    lease_until: DateTime,
) -> Document {
    doc! {
        "from": migration.source_generation(),
        "to": migration.target_generation(),
        "name": migration.name(),
        "step": 0_i32,
        "total_steps": i64::from(migration.step_count()),
        "processed": 0_i64,
        "step_warnings": 0_i64,
        "warning_counts": {},
        "owner": owner,
        "lease_until": lease_until,
    }
}

fn marker_generation(marker: &Document) -> Result<i32, MongoMigrationError> {
    marker
        .get_i32("generation")
        .map_err(|_| MongoMigrationError::InvalidState {
            code: "missing_schema_generation",
        })
}

fn marker_step(marker: &Document) -> Result<u32, MongoMigrationError> {
    let step = marker
        .get_document("migration")
        .and_then(|migration| migration.get_i32("step"))
        .map_err(|_| MongoMigrationError::InvalidState {
            code: "invalid_migration_step",
        })?;
    u32::try_from(step).map_err(|_| MongoMigrationError::InvalidState {
        code: "invalid_migration_step",
    })
}

fn validate_in_progress(
    marker: &Document,
    migration: &dyn MongoMigration,
) -> Result<(), MongoMigrationError> {
    let state =
        marker
            .get_document("migration")
            .map_err(|_| MongoMigrationError::InvalidState {
                code: "missing_migration_state",
            })?;
    let valid = state.get_i32("from") == Ok(migration.source_generation())
        && state.get_i32("to") == Ok(migration.target_generation())
        && state.get_str("name") == Ok(migration.name())
        && state.get_i64("total_steps") == Ok(i64::from(migration.step_count()));
    if valid {
        Ok(())
    } else {
        Err(MongoMigrationError::InvalidState {
            code: "unknown_in_progress_transition",
        })
    }
}

fn parse_checkpoint(
    marker: &Document,
    migration: &dyn MongoMigration,
) -> Result<MigrationCheckpoint, MongoMigrationError> {
    validate_in_progress(marker, migration)?;
    let state =
        marker
            .get_document("migration")
            .map_err(|_| MongoMigrationError::InvalidState {
                code: "missing_migration_state",
            })?;
    let processed = state
        .get_i64("processed")
        .map_err(|_| MongoMigrationError::InvalidState {
            code: "invalid_processed_count",
        })?;
    let processed_records =
        u64::try_from(processed).map_err(|_| MongoMigrationError::InvalidState {
            code: "invalid_processed_count",
        })?;
    let step_warnings = state
        .get_i64("step_warnings")
        .ok()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(MongoMigrationError::InvalidState {
            code: "invalid_step_warning_count",
        })?;
    let warning_document =
        state
            .get_document("warning_counts")
            .map_err(|_| MongoMigrationError::InvalidState {
                code: "invalid_warning_counts",
            })?;
    if warning_document.len() > MAX_WARNING_CODES {
        return Err(MongoMigrationError::InvalidState {
            code: "too_many_warning_codes",
        });
    }
    let mut warning_counts = BTreeMap::new();
    for (code, value) in warning_document {
        validate_warning_code(code)?;
        let count = value
            .as_i64()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(MongoMigrationError::InvalidState {
                code: "invalid_warning_count",
            })?;
        warning_counts.insert(code.clone(), count);
    }
    let cursor = state.get("cursor").cloned();
    validate_checkpoint_cursor(cursor.as_ref())?;
    Ok(MigrationCheckpoint {
        cursor,
        processed_records,
        step_warnings,
        warning_counts,
    })
}

fn validate_batch(batch: &MigrationBatch) -> Result<(), MongoMigrationError> {
    if !batch.step_complete && batch.next_cursor.is_none() {
        return Err(MongoMigrationError::Blocked {
            code: "migration_batch_did_not_advance",
            source: None,
        });
    }
    if batch.warning_counts.len() > MAX_WARNING_CODES {
        return Err(MongoMigrationError::Blocked {
            code: "too_many_warning_codes",
            source: None,
        });
    }
    for code in batch.warning_counts.keys() {
        validate_warning_code(code)?;
    }
    validate_checkpoint_cursor(batch.next_cursor.as_ref())
}

fn validate_checkpoint_cursor(cursor: Option<&Bson>) -> Result<(), MongoMigrationError> {
    let Some(cursor) = cursor else {
        return Ok(());
    };
    let encoded = mongodb::bson::to_vec(&doc! { "cursor": cursor.clone() }).map_err(|_| {
        MongoMigrationError::Blocked {
            code: "migration_cursor_cannot_be_encoded",
            source: None,
        }
    })?;
    if encoded.len() > MAX_CHECKPOINT_BYTES {
        return Err(MongoMigrationError::Blocked {
            code: "migration_cursor_too_large",
            source: None,
        });
    }
    Ok(())
}

fn validate_warning_code(code: &str) -> Result<(), MongoMigrationError> {
    let valid = !code.is_empty()
        && code.len() <= MAX_WARNING_CODE_BYTES
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(MongoMigrationError::Blocked {
            code: "invalid_migration_warning_code",
            source: None,
        })
    }
}

fn merge_batch(
    checkpoint: &mut MigrationCheckpoint,
    batch: &MigrationBatch,
    policy: RecordErrorPolicy,
) -> Result<(), MongoMigrationError> {
    if !batch.step_complete
        && checkpoint.cursor.is_some()
        && batch.next_cursor.as_ref() == checkpoint.cursor.as_ref()
    {
        return Err(MongoMigrationError::Blocked {
            code: "migration_batch_cursor_did_not_advance",
            source: None,
        });
    }
    checkpoint.processed_records = checkpoint
        .processed_records
        .checked_add(batch.processed_records)
        .ok_or(MongoMigrationError::Blocked {
            code: "migration_processed_count_overflow",
            source: None,
        })?;
    for (code, count) in &batch.warning_counts {
        let value = checkpoint
            .warning_counts
            .entry((*code).to_owned())
            .or_default();
        *value = value
            .checked_add(*count)
            .ok_or(MongoMigrationError::Blocked {
                code: "migration_warning_count_overflow",
                source: None,
            })?;
    }
    if checkpoint.warning_counts.len() > MAX_WARNING_CODES {
        return Err(MongoMigrationError::Blocked {
            code: "too_many_warning_codes",
            source: None,
        });
    }
    checkpoint.step_warnings = checkpoint
        .step_warnings
        .checked_add(
            batch
                .warning_counts
                .values()
                .copied()
                .fold(0_u64, u64::saturating_add),
        )
        .ok_or(MongoMigrationError::Blocked {
            code: "migration_warning_count_overflow",
            source: None,
        })?;
    if !policy.permits(checkpoint.step_warnings) {
        return Err(MongoMigrationError::Blocked {
            code: "migration_warning_budget_exhausted",
            source: None,
        });
    }
    checkpoint.cursor = batch.next_cursor.clone();
    Ok(())
}

fn warning_total(warnings: &BTreeMap<String, u64>) -> u64 {
    warnings.values().copied().fold(0_u64, u64::saturating_add)
}

async fn persist_checkpoint(
    database: &Database,
    migration: &dyn MongoMigration,
    lease: &MigrationLease,
    step: u32,
    checkpoint: &MigrationCheckpoint,
    step_complete: bool,
    config: MigrationRunnerConfig,
) -> Result<(), OwnedTransitionError> {
    let processed =
        i64::try_from(checkpoint.processed_records).map_err(|_| MongoMigrationError::Blocked {
            code: "migration_processed_count_overflow",
            source: None,
        })?;
    let warning_counts = warning_document(&checkpoint.warning_counts)?;
    let step_warnings = if step_complete {
        0_i64
    } else {
        i64::try_from(checkpoint.step_warnings).map_err(|_| MongoMigrationError::Blocked {
            code: "migration_warning_count_overflow",
            source: None,
        })?
    };
    let mut set = doc! {
        "migration.step": i32::try_from(step).map_err(|_| MongoMigrationError::Blocked {
            code: "too_many_migration_steps",
            source: None,
        })?,
        "migration.processed": processed,
        "migration.step_warnings": step_warnings,
        "migration.warning_counts": warning_counts,
    };
    let mut unset = Document::new();
    if step_complete {
        unset.insert("migration.cursor", "");
    } else if let Some(cursor) = &checkpoint.cursor {
        set.insert("migration.cursor", cursor.clone());
    }
    let mut update = doc! { "$set": set };
    if !unset.is_empty() {
        update.insert("$unset", unset);
    }
    let collection = database.collection::<Document>("schema_meta");
    let result = metadata_operation(
        || {
            collection.update_one(
                doc! {
                    "_id": SCHEMA_ID,
                    "generation": migration.source_generation(),
                    "state": "migrating",
                    "migration.from": migration.source_generation(),
                    "migration.to": migration.target_generation(),
                    "migration.name": migration.name(),
                    "migration.owner": &lease.owner,
                },
                update.clone(),
            )
        },
        config,
    )
    .await?;
    if result.matched_count == 1 {
        Ok(())
    } else {
        Err(OwnedTransitionError::LeaseLost)
    }
}

fn warning_document(warnings: &BTreeMap<String, u64>) -> Result<Document, MongoMigrationError> {
    let mut document = Document::new();
    for (code, count) in warnings {
        validate_warning_code(code)?;
        document.insert(
            code,
            i64::try_from(*count).map_err(|_| MongoMigrationError::Blocked {
                code: "migration_warning_count_overflow",
                source: None,
            })?,
        );
    }
    Ok(document)
}

async fn read_marker(
    database: &Database,
    config: MigrationRunnerConfig,
) -> Result<Document, MongoMigrationError> {
    let collection = database.collection::<Document>("schema_meta");
    metadata_operation(|| collection.find_one(doc! { "_id": SCHEMA_ID }), config)
        .await?
        .ok_or(MongoMigrationError::InvalidState {
            code: "missing_schema_marker",
        })
}

async fn metadata_operation<T, F, Fut>(
    mut operation: F,
    config: MigrationRunnerConfig,
) -> Result<T, MongoMigrationError>
where
    F: FnMut() -> Fut,
    Fut: IntoFuture<Output = Result<T, MongoError>>,
{
    let mut delay = config.retry_initial;
    loop {
        match operation().into_future().await {
            Ok(value) => return Ok(value),
            Err(error) if is_retryable_mongo_error(&error) => {
                sleep(delay).await;
                delay = delay.saturating_mul(2).min(config.retry_maximum);
            }
            Err(source) => {
                return Err(MongoMigrationError::Blocked {
                    code: "migration_database_operation_rejected",
                    source: Some(source),
                });
            }
        }
    }
}

async fn retry_preflight(
    migration: &dyn MongoMigration,
    database: &Database,
    heartbeat: &LeaseHeartbeat,
    config: MigrationRunnerConfig,
) -> Result<(), OwnedTransitionError> {
    let mut delay = config.retry_initial;
    loop {
        if heartbeat.is_lost() {
            return Err(OwnedTransitionError::LeaseLost);
        }
        match migration.preflight(database).await {
            Ok(value) => return Ok(value),
            Err(MigrationAttemptError::Retryable(_)) => {
                sleep(delay).await;
                delay = delay.saturating_mul(2).min(config.retry_maximum);
            }
            Err(error @ MigrationAttemptError::Blocked { .. }) => {
                return Err(OwnedTransitionError::Migration(error.into()));
            }
        }
    }
}

async fn retry_batch(
    migration: &dyn MongoMigration,
    database: &Database,
    step: u32,
    checkpoint: &MigrationCheckpoint,
    heartbeat: &LeaseHeartbeat,
    config: MigrationRunnerConfig,
) -> Result<MigrationBatch, OwnedTransitionError> {
    let mut delay = config.retry_initial;
    loop {
        if heartbeat.is_lost() {
            return Err(OwnedTransitionError::LeaseLost);
        }
        match migration.run_batch(database, step, checkpoint).await {
            Ok(value) => return Ok(value),
            Err(MigrationAttemptError::Retryable(_)) => {
                sleep(delay).await;
                delay = delay.saturating_mul(2).min(config.retry_maximum);
            }
            Err(error @ MigrationAttemptError::Blocked { .. }) => {
                return Err(OwnedTransitionError::Migration(error.into()));
            }
        }
    }
}

async fn retry_verify(
    migration: &dyn MongoMigration,
    database: &Database,
    heartbeat: &LeaseHeartbeat,
    config: MigrationRunnerConfig,
) -> Result<(), OwnedTransitionError> {
    let mut delay = config.retry_initial;
    loop {
        if heartbeat.is_lost() {
            return Err(OwnedTransitionError::LeaseLost);
        }
        match migration.verify(database).await {
            Ok(value) => return Ok(value),
            Err(MigrationAttemptError::Retryable(_)) => {
                sleep(delay).await;
                delay = delay.saturating_mul(2).min(config.retry_maximum);
            }
            Err(error @ MigrationAttemptError::Blocked { .. }) => {
                return Err(OwnedTransitionError::Migration(error.into()));
            }
        }
    }
}

fn is_retryable_mongo_error(error: &MongoError) -> bool {
    if error.contains_label("RetryableWriteError")
        || error.contains_label("TransientTransactionError")
    {
        return true;
    }
    match error.kind.as_ref() {
        ErrorKind::Io(_)
        | ErrorKind::ConnectionPoolCleared { .. }
        | ErrorKind::ServerSelection { .. }
        | ErrorKind::DnsResolve { .. } => true,
        ErrorKind::Command(command) => matches!(
            command.code,
            6 | 7 | 89 | 91 | 189 | 262 | 9001 | 11600 | 11602 | 13435 | 13436
        ),
        _ => false,
    }
}

fn add_duration(datetime: DateTime, duration: Duration) -> Result<DateTime, MongoMigrationError> {
    let millis =
        i64::try_from(duration.as_millis()).map_err(|_| MongoMigrationError::InvalidRegistry {
            code: "migration_lease_duration_too_large",
        })?;
    let timestamp = datetime.timestamp_millis().checked_add(millis).ok_or(
        MongoMigrationError::InvalidRegistry {
            code: "migration_lease_timestamp_overflow",
        },
    )?;
    Ok(DateTime::from_millis(timestamp))
}

struct MigrationLease {
    owner: String,
    from_generation: i32,
    to_generation: i32,
    name: &'static str,
}

struct LeaseHeartbeat {
    lost: Arc<AtomicBool>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl LeaseHeartbeat {
    fn start(database: Database, lease: &MigrationLease, config: MigrationRunnerConfig) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let task_lost = Arc::clone(&lost);
        let (stop, mut stop_receiver) = watch::channel(false);
        let owner = lease.owner.clone();
        let from_generation = lease.from_generation;
        let to_generation = lease.to_generation;
        let name = lease.name;
        let task = tokio::spawn(async move {
            let mut last_renewal = Instant::now();
            loop {
                tokio::select! {
                    changed = stop_receiver.changed() => {
                        if changed.is_err() || *stop_receiver.borrow() {
                            return;
                        }
                    }
                    () = sleep(config.lease_renew_interval) => {
                        let lease_until = match add_duration(DateTime::now(), config.lease_duration) {
                            Ok(value) => value,
                            Err(_) => {
                                task_lost.store(true, Ordering::Release);
                                return;
                            }
                        };
                        let result = database
                            .collection::<Document>("schema_meta")
                            .update_one(
                                doc! {
                                    "_id": SCHEMA_ID,
                                    "generation": from_generation,
                                    "state": "migrating",
                                    "migration.from": from_generation,
                                    "migration.to": to_generation,
                                    "migration.name": name,
                                    "migration.owner": &owner,
                                },
                                doc! { "$set": { "migration.lease_until": lease_until } },
                            )
                            .await;
                        match result {
                            Ok(result) if result.matched_count == 1 => last_renewal = Instant::now(),
                            Ok(_) => {
                                task_lost.store(true, Ordering::Release);
                                return;
                            }
                            Err(error) if is_retryable_mongo_error(&error)
                                && last_renewal.elapsed() < config.lease_duration => {}
                            Err(_) => {
                                task_lost.store(true, Ordering::Release);
                                return;
                            }
                        }
                    }
                }
            }
        });
        Self { lost, stop, task }
    }

    fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = timeout(Duration::from_secs(1), self.task).await;
    }
}

enum OwnedTransitionError {
    LeaseLost,
    Migration(MongoMigrationError),
}

impl From<MongoMigrationError> for OwnedTransitionError {
    fn from(error: MongoMigrationError) -> Self {
        Self::Migration(error)
    }
}

impl From<MigrationAttemptError> for OwnedTransitionError {
    fn from(error: MigrationAttemptError) -> Self {
        Self::Migration(error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use mongodb::{Client, IndexModel, bson::doc, options::IndexOptions};

    struct Descriptor {
        from: i32,
        to: i32,
        steps: u32,
    }

    impl MongoMigration for Descriptor {
        fn source_generation(&self) -> i32 {
            self.from
        }

        fn target_generation(&self) -> i32 {
            self.to
        }

        fn name(&self) -> &'static str {
            "fixture"
        }

        fn step_count(&self) -> u32 {
            self.steps
        }

        fn step_name(&self, _step: u32) -> &'static str {
            "fixture_step"
        }

        fn record_error_policy(&self, _step: u32) -> RecordErrorPolicy {
            RecordErrorPolicy::Strict
        }

        fn preflight<'a>(
            &'a self,
            _database: &'a Database,
        ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
            Box::pin(async { Ok(()) })
        }

        fn run_batch<'a>(
            &'a self,
            _database: &'a Database,
            _step: u32,
            _checkpoint: &'a MigrationCheckpoint,
        ) -> MigrationFuture<'a, Result<MigrationBatch, MigrationAttemptError>> {
            Box::pin(async { Ok(MigrationBatch::complete(0)) })
        }

        fn verify<'a>(
            &'a self,
            _database: &'a Database,
        ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn registry_builds_only_a_complete_adjacent_chain() {
        let first = Descriptor {
            from: 1,
            to: 2,
            steps: 2,
        };
        let second = Descriptor {
            from: 2,
            to: 3,
            steps: 1,
        };
        let migrations: [&dyn MongoMigration; 2] = [&first, &second];
        let plan = MigrationRegistry::new(3, &migrations).plan(1).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].source_generation(), 1);
        assert_eq!(plan[1].target_generation(), 3);

        let missing: [&dyn MongoMigration; 1] = [&first];
        assert!(matches!(
            MigrationRegistry::new(3, &missing).plan(1),
            Err(MongoMigrationError::MissingTransition { from: 2, target: 3 })
        ));
    }

    #[test]
    fn registry_rejects_duplicate_and_non_adjacent_transitions() {
        let first = Descriptor {
            from: 1,
            to: 2,
            steps: 1,
        };
        let duplicate = Descriptor {
            from: 1,
            to: 2,
            steps: 1,
        };
        let migrations: [&dyn MongoMigration; 2] = [&first, &duplicate];
        assert!(matches!(
            MigrationRegistry::new(2, &migrations).plan(1),
            Err(MongoMigrationError::InvalidRegistry {
                code: "duplicate_transition_source"
            })
        ));

        let invalid = Descriptor {
            from: 1,
            to: 3,
            steps: 1,
        };
        let migrations: [&dyn MongoMigration; 1] = [&invalid];
        assert!(matches!(
            MigrationRegistry::new(3, &migrations).plan(1),
            Err(MongoMigrationError::InvalidRegistry {
                code: "invalid_transition_descriptor"
            })
        ));

        let second = Descriptor {
            from: 2,
            to: 3,
            steps: 1,
        };
        let migrations: [&dyn MongoMigration; 2] = [&second, &first];
        assert!(matches!(
            MigrationRegistry::new(3, &migrations).plan(1),
            Err(MongoMigrationError::InvalidRegistry {
                code: "unordered_transitions"
            })
        ));
    }

    #[test]
    fn warning_policy_is_explicit_and_checkpoint_is_bounded() {
        let mut checkpoint = MigrationCheckpoint::default();
        let batch = MigrationBatch::complete(10).with_warning("malformed_bucket", 2);
        assert!(
            merge_batch(
                &mut checkpoint,
                &batch,
                RecordErrorPolicy::Tolerate {
                    maximum_warnings: 2
                }
            )
            .is_ok()
        );
        assert_eq!(checkpoint.processed_records, 10);
        assert_eq!(checkpoint.warning_counts["malformed_bucket"], 2);

        let strict = MigrationBatch::complete(1).with_warning("malformed_bucket", 1);
        assert!(matches!(
            merge_batch(
                &mut MigrationCheckpoint::default(),
                &strict,
                RecordErrorPolicy::Strict
            ),
            Err(MongoMigrationError::Blocked {
                code: "migration_warning_budget_exhausted",
                ..
            })
        ));
        assert!(matches!(
            validate_checkpoint_cursor(Some(&Bson::String("x".repeat(MAX_CHECKPOINT_BYTES)))),
            Err(MongoMigrationError::Blocked {
                code: "migration_cursor_too_large",
                ..
            })
        ));

        let mut stalled = MigrationCheckpoint {
            cursor: Some(Bson::Int32(4)),
            ..MigrationCheckpoint::default()
        };
        assert!(matches!(
            merge_batch(
                &mut stalled,
                &MigrationBatch::more(Bson::Int32(4), 0),
                RecordErrorPolicy::Strict,
            ),
            Err(MongoMigrationError::Blocked {
                code: "migration_batch_cursor_did_not_advance",
                ..
            })
        ));

        let mut too_many_codes = MigrationCheckpoint::default();
        for index in 0..MAX_WARNING_CODES {
            too_many_codes
                .warning_counts
                .insert(format!("warning_{index}"), 1);
        }
        assert!(matches!(
            merge_batch(
                &mut too_many_codes,
                &MigrationBatch::complete(0).with_warning("one_more_warning", 1),
                RecordErrorPolicy::Tolerate {
                    maximum_warnings: u64::MAX,
                },
            ),
            Err(MongoMigrationError::Blocked {
                code: "too_many_warning_codes",
                ..
            })
        ));
    }

    struct FixtureMigration {
        fail_after_first_effect: AtomicBool,
        fail_after_first_checkpoint: AtomicBool,
    }

    impl FixtureMigration {
        fn new(fail_after_first_effect: bool, fail_after_first_checkpoint: bool) -> Self {
            Self {
                fail_after_first_effect: AtomicBool::new(fail_after_first_effect),
                fail_after_first_checkpoint: AtomicBool::new(fail_after_first_checkpoint),
            }
        }
    }

    impl MongoMigration for FixtureMigration {
        fn source_generation(&self) -> i32 {
            1
        }

        fn target_generation(&self) -> i32 {
            2
        }

        fn name(&self) -> &'static str {
            "bounded_fixture"
        }

        fn step_count(&self) -> u32 {
            2
        }

        fn step_name(&self, step: u32) -> &'static str {
            match step {
                0 => "rewrite_fixture_documents",
                1 => "create_fixture_index",
                _ => "invalid",
            }
        }

        fn record_error_policy(&self, _step: u32) -> RecordErrorPolicy {
            RecordErrorPolicy::Strict
        }

        fn preflight<'a>(
            &'a self,
            database: &'a Database,
        ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
            Box::pin(async move {
                let names = database
                    .list_collection_names()
                    .await
                    .map_err(MigrationAttemptError::from_mongo)?;
                if names.iter().any(|name| name == "migration_fixture") {
                    Ok(())
                } else {
                    Err(MigrationAttemptError::blocked("fixture_collection_missing"))
                }
            })
        }

        fn run_batch<'a>(
            &'a self,
            database: &'a Database,
            step: u32,
            checkpoint: &'a MigrationCheckpoint,
        ) -> MigrationFuture<'a, Result<MigrationBatch, MigrationAttemptError>> {
            Box::pin(async move {
                let collection = database.collection::<Document>("migration_fixture");
                match step {
                    0 => {
                        if checkpoint.cursor.is_some()
                            && self
                                .fail_after_first_checkpoint
                                .swap(false, Ordering::AcqRel)
                        {
                            return Err(MigrationAttemptError::blocked(
                                "injected_after_checkpoint_failure",
                            ));
                        }
                        let last_id = checkpoint
                            .cursor
                            .as_ref()
                            .and_then(Bson::as_i32)
                            .unwrap_or(-1);
                        let after = (last_id >= 0).then_some(Bson::Int32(last_id));
                        let page = load_bounded_document_page(
                            &collection,
                            doc! { "legacy": true },
                            after.as_ref(),
                            doc! { "_id": 1 },
                            MigrationDocumentLimits::new(2, 4 * 1024).map_err(|_| {
                                MigrationAttemptError::blocked("fixture_limits_invalid")
                            })?,
                        )
                        .await?;
                        let ids = page
                            .documents
                            .iter()
                            .map(|document| {
                                document.get_i32("_id").map_err(|_| {
                                    MigrationAttemptError::blocked("fixture_id_invalid")
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        if ids.is_empty() {
                            return Ok(MigrationBatch::complete(0));
                        }
                        for id in &ids {
                            collection
                                .update_one(
                                    doc! { "_id": id, "legacy": true },
                                    doc! {
                                        "$set": { "migrated": true },
                                        "$unset": { "legacy": "" },
                                    },
                                )
                                .await
                                .map_err(MigrationAttemptError::from_mongo)?;
                        }
                        if self.fail_after_first_effect.swap(false, Ordering::AcqRel) {
                            return Err(MigrationAttemptError::blocked(
                                "injected_after_effect_failure",
                            ));
                        }
                        let processed = u64::try_from(ids.len()).expect("two records fit u64");
                        if page.exhausted {
                            Ok(MigrationBatch::complete(processed))
                        } else {
                            Ok(MigrationBatch::more(
                                page.next_cursor
                                    .expect("a nonempty bounded page has a cursor"),
                                processed,
                            ))
                        }
                    }
                    1 => {
                        collection
                            .create_index(
                                IndexModel::builder()
                                    .keys(doc! { "migrated": 1 })
                                    .options(
                                        IndexOptions::builder()
                                            .name("fixture_migrated".to_owned())
                                            .build(),
                                    )
                                    .build(),
                            )
                            .await
                            .map_err(MigrationAttemptError::from_mongo)?;
                        Ok(MigrationBatch::complete(0))
                    }
                    _ => Err(MigrationAttemptError::blocked("fixture_step_invalid")),
                }
            })
        }

        fn verify<'a>(
            &'a self,
            database: &'a Database,
        ) -> MigrationFuture<'a, Result<(), MigrationAttemptError>> {
            Box::pin(async move {
                let collection = database.collection::<Document>("migration_fixture");
                let remaining = collection
                    .count_documents(doc! { "legacy": true })
                    .await
                    .map_err(MigrationAttemptError::from_mongo)?;
                let indexes = collection
                    .list_index_names()
                    .await
                    .map_err(MigrationAttemptError::from_mongo)?;
                if remaining == 0 && indexes.iter().any(|name| name == "fixture_migrated") {
                    Ok(())
                } else {
                    Err(MigrationAttemptError::blocked(
                        "fixture_target_verification_failed",
                    ))
                }
            })
        }
    }

    #[tokio::test]
    #[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
    async fn mongodb_runner_resumes_around_checkpoint_and_takes_expired_lease() {
        let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
            "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
        });
        let client = Client::with_uri_str(uri).await.unwrap();
        let database = client.database(&format!(
            "metric_migration_test_{}",
            ObjectId::new().to_hex()
        ));
        database
            .collection::<Document>("schema_meta")
            .insert_one(doc! {
                "_id": SCHEMA_ID,
                "generation": 1_i32,
                "state": "complete",
            })
            .await
            .unwrap();
        database
            .collection::<Document>("migration_fixture")
            .insert_many((0..5).map(|id| doc! { "_id": id, "legacy": true }))
            .await
            .unwrap();

        let migration = FixtureMigration::new(true, true);
        let transitions: [&dyn MongoMigration; 1] = [&migration];
        let reports = Mutex::new(Vec::new());
        let reporter = |progress| reports.lock().unwrap().push(progress);
        let config = MigrationRunnerConfig {
            retry_initial: Duration::from_millis(5),
            retry_maximum: Duration::from_millis(20),
            lease_duration: Duration::from_secs(2),
            lease_renew_interval: Duration::from_millis(100),
            lease_wait_interval: Duration::from_millis(5),
        };
        let first = migrate_to_target_with_config(
            &database,
            MigrationRegistry::new(2, &transitions),
            &reporter,
            config,
        )
        .await;
        assert!(matches!(
            first,
            Err(MongoMigrationError::Blocked {
                code: "injected_after_effect_failure",
                ..
            })
        ));
        let partially_changed = database
            .collection::<Document>("migration_fixture")
            .count_documents(doc! { "migrated": true })
            .await
            .unwrap();
        assert_eq!(partially_changed, 2);
        database
            .collection::<Document>("schema_meta")
            .update_one(
                doc! { "_id": SCHEMA_ID },
                doc! { "$set": { "migration.lease_until": DateTime::from_millis(0) } },
            )
            .await
            .unwrap();

        let second = migrate_to_target_with_config(
            &database,
            MigrationRegistry::new(2, &transitions),
            &reporter,
            config,
        )
        .await;
        assert!(matches!(
            second,
            Err(MongoMigrationError::Blocked {
                code: "injected_after_checkpoint_failure",
                ..
            })
        ));
        let checkpointed = database
            .collection::<Document>("schema_meta")
            .find_one(doc! { "_id": SCHEMA_ID })
            .await
            .unwrap()
            .unwrap();
        let state = checkpointed.get_document("migration").unwrap();
        assert_eq!(state.get_i32("step"), Ok(0));
        assert_eq!(state.get_i64("processed"), Ok(2));
        assert_eq!(state.get_i32("cursor"), Ok(3));
        assert_eq!(
            database
                .collection::<Document>("migration_fixture")
                .count_documents(doc! { "migrated": true })
                .await
                .unwrap(),
            4
        );
        database
            .collection::<Document>("schema_meta")
            .update_one(
                doc! { "_id": SCHEMA_ID },
                doc! { "$set": { "migration.lease_until": DateTime::from_millis(0) } },
            )
            .await
            .unwrap();

        migrate_to_target_with_config(
            &database,
            MigrationRegistry::new(2, &transitions),
            &reporter,
            config,
        )
        .await
        .unwrap();
        let marker = database
            .collection::<Document>("schema_meta")
            .find_one(doc! { "_id": SCHEMA_ID })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(marker.get_i32("generation"), Ok(2));
        assert_eq!(marker.get_str("state"), Ok("complete"));
        assert!(!marker.contains_key("migration"));
        assert_eq!(
            database
                .collection::<Document>("migration_fixture")
                .count_documents(doc! { "migrated": true })
                .await
                .unwrap(),
            5
        );
        assert!(
            reports
                .lock()
                .unwrap()
                .iter()
                .any(|progress| matches!(progress, SchemaMigrationProgress::Running { .. }))
        );
        database.drop().await.unwrap();
    }
}
