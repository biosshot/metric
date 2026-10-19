//! One dedicated connection owns SQL, bounded data steps and final publication.
use crate::{ConnectionPolicy, PostgresError};
use sqlx::{
    Connection, PgConnection, Postgres, Row, SqlSafeStr, Transaction,
    migrate::{Migrate, MigrateError, Migration, MigrationType, Migrator},
};
use std::{borrow::Cow, future::Future, pin::Pin, time::Duration};

const SCHEMA_GENERATION: i64 = 22;
// Fixed application lock namespace, independent of table existence or Mongo generation.
const MIGRATION_LOCK: i64 = 0x4d45_5452_4943_5047;
// This is application-owned static SQL, never configurable or derived from input.
const MIGRATION_TABLE: &str = "metric._sqlx_migrations";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MigrationMode {
    #[default]
    Auto,
    Check,
}

#[derive(Debug, Clone)]
pub struct MigrationPolicy {
    pub mode: MigrationMode,
    pub lock_timeout: Duration,
    pub statement_timeout: Duration,
}

impl Default for MigrationPolicy {
    fn default() -> Self {
        Self {
            mode: MigrationMode::Auto,
            lock_timeout: Duration::from_secs(30),
            statement_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaStatus {
    pub generation: i64,
}

/// Bounded, driver-free progress. Contains no URI, SQL, row or checkpoint values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationPhase {
    Inspecting,
    Waiting,
    Running,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationProgress {
    pub phase: MigrationPhase,
    pub completed_steps: u32,
    pub total_steps: u32,
}

type Reporter<'a> = &'a (dyn Fn(MigrationProgress) + Send + Sync);

type StepFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PostgresError>> + Send + 'a>>;

/// Internal adapter extension point: future Rust migrations use bounded batches.
/// Never modify an installed step's identifier, generation or checksum.
trait DataMigration: Send + Sync {
    fn id(&self) -> &'static str;
    fn generation(&self) -> i64;
    fn checksum(&self) -> &'static [u8];
    fn preflight<'a>(&'a self, connection: &'a mut PgConnection) -> StepFuture<'a, ()>;
    /// Mutations and returned checkpoint are committed together by the manager.
    fn run_batch<'a>(
        &'a self,
        transaction: &'a mut Transaction<'_, Postgres>,
        checkpoint: Option<&'a str>,
    ) -> StepFuture<'a, DataBatch>;
    fn verify<'a>(&'a self, connection: &'a mut PgConnection) -> StepFuture<'a, ()>;
}

struct DataBatch {
    checkpoint: Option<String>,
    complete: bool,
}

/// Native dedicated connection; dropping the future closes it and releases ownership.
pub struct PostgresSchemaManager;

impl PostgresSchemaManager {
    pub async fn ensure(
        uri: &str,
        connection_policy: &ConnectionPolicy,
        migration_policy: &MigrationPolicy,
    ) -> Result<SchemaStatus, PostgresError> {
        Self::ensure_with_reporter(uri, connection_policy, migration_policy, &|_| {}).await
    }

    pub async fn ensure_with_reporter(
        uri: &str,
        connection_policy: &ConnectionPolicy,
        migration_policy: &MigrationPolicy,
        reporter: Reporter<'_>,
    ) -> Result<SchemaStatus, PostgresError> {
        let migrator = registry();
        Self::ensure_registry_reported(
            uri,
            connection_policy,
            migration_policy,
            &migrator,
            &[],
            SCHEMA_GENERATION,
            reporter,
        )
        .await
    }

    #[cfg(test)]
    async fn ensure_registry(
        uri: &str,
        connection_policy: &ConnectionPolicy,
        policy: &MigrationPolicy,
        migrator: &Migrator,
        data_steps: &[Box<dyn DataMigration>],
        generation: i64,
    ) -> Result<SchemaStatus, PostgresError> {
        Self::ensure_registry_reported(
            uri,
            connection_policy,
            policy,
            migrator,
            data_steps,
            generation,
            &|_| {},
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn ensure_registry_reported(
        uri: &str,
        connection_policy: &ConnectionPolicy,
        policy: &MigrationPolicy,
        migrator: &Migrator,
        data_steps: &[Box<dyn DataMigration>],
        generation: i64,
        reporter: Reporter<'_>,
    ) -> Result<SchemaStatus, PostgresError> {
        let progress = MigrationProgress {
            phase: MigrationPhase::Inspecting,
            completed_steps: 0,
            total_steps: u32::try_from(migrator.iter().count() + data_steps.len())
                .map_err(|_| PostgresError::InvalidPolicy)?,
        };
        reporter(progress);
        if policy.lock_timeout < Duration::from_millis(1)
            || policy.lock_timeout > Duration::from_secs(300)
            || policy.statement_timeout < Duration::from_millis(1)
            || policy.statement_timeout > Duration::from_secs(1800)
        {
            return Err(PostgresError::InvalidPolicy);
        }
        let options = connection_policy.connect_options(uri)?;
        let mut connection = tokio::time::timeout(
            connection_policy.bootstrap_timeout,
            PgConnection::connect_with(&options),
        )
        .await
        .map_err(|_| PostgresError::Timeout)??;
        // Parameter values are bound even for server settings. Native defaults stay intact.
        sqlx::query("SELECT set_config('statement_timeout', $1, false), set_config('lock_timeout', $2, false), set_config('search_path', 'metric', false)")
            .bind(policy.statement_timeout.as_millis().to_string())
            .bind(policy.lock_timeout.as_millis().to_string())
            .execute(&mut connection).await?;
        acquire_ownership_reported(&mut connection, policy.lock_timeout, reporter, progress)
            .await?;
        let result = ensure_owned(
            &mut connection,
            policy.mode,
            migrator,
            data_steps,
            generation,
            reporter,
            progress,
        )
        .await;
        // Close instead of returning a session advisory lock to a pool. Cancellation also
        // drops this non-pooled connection; no background unlocked migration may continue.
        let _ = connection.close().await;
        result
    }
}

fn registry() -> Migrator {
    Migrator {
        migrations: Cow::Owned(vec![
            Migration::new(
                1,
                Cow::Borrowed("schema infrastructure"),
                MigrationType::Simple,
                include_str!("../migrations/0001_schema.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                2,
                Cow::Borrowed("project identity"),
                MigrationType::Simple,
                include_str!("../migrations/0002_identity.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                3,
                Cow::Borrowed("authentication and audit"),
                MigrationType::Simple,
                include_str!("../migrations/0003_auth.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                4,
                Cow::Borrowed("durable Events and scoped blob references"),
                MigrationType::Simple,
                include_str!("../migrations/0004_events.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                5,
                Cow::Borrowed("Issue state, activity and notification transitions"),
                MigrationType::Simple,
                include_str!("../migrations/0005_issues.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                6,
                Cow::Borrowed("transactional finalization, hourly statistics and catalogs"),
                MigrationType::Simple,
                include_str!("../migrations/0006_finalization.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                7,
                Cow::Borrowed("explicit Releases, Deploys and cursor indexes"),
                MigrationType::Simple,
                include_str!("../migrations/0007_releases.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                8,
                Cow::Borrowed("bounded investigation catalog timeline"),
                MigrationType::Simple,
                include_str!("../migrations/0008_investigation.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                9,
                Cow::Borrowed("scoped revisioned Dashboard and Saved Query configuration"),
                MigrationType::Simple,
                include_str!("../migrations/0009_dashboards.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                10,
                Cow::Borrowed("transactional Session lifecycle and whole-hour health repair"),
                MigrationType::Simple,
                include_str!("../migrations/0010_sessions.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                11,
                Cow::Borrowed("scoped Replay manifests and immutable segment references"),
                MigrationType::Simple,
                include_str!("../migrations/0011_replays.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                12,
                Cow::Borrowed("bounded transactional application Metric aggregates"),
                MigrationType::Simple,
                include_str!("../migrations/0012_metrics.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                13,
                Cow::Borrowed("scoped Feedback workflow and atomic attachment references"),
                MigrationType::Simple,
                include_str!("../migrations/0013_feedback.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                14,
                Cow::Borrowed("bounded Signals and repeatable whole-hour Span repair"),
                MigrationType::Simple,
                include_str!("../migrations/0014_signals.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                15,
                Cow::Borrowed("scoped Explore Metric timeline"),
                MigrationType::Simple,
                include_str!("../migrations/0015_explore.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                16,
                Cow::Borrowed("transactional Cron and Uptime monitor lifecycle"),
                MigrationType::Simple,
                include_str!("../migrations/0016_monitors.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                17,
                Cow::Borrowed("scoped notification outbox and native lease ownership"),
                MigrationType::Simple,
                include_str!("../migrations/0017_notifications.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                18,
                Cow::Borrowed("bounded archive manifests and scoped hot-copy commitment"),
                MigrationType::Simple,
                include_str!("../migrations/0018_archive.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                19,
                Cow::Borrowed("transactional debug metadata and project quotas"),
                MigrationType::Simple,
                include_str!("../migrations/0019_debug_files.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                20,
                Cow::Borrowed("organization artifacts and generation-fenced garbage collection"),
                MigrationType::Simple,
                include_str!("../migrations/0020_artifacts.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                21,
                Cow::Borrowed("durable bounded project deletion"),
                MigrationType::Simple,
                include_str!("../migrations/0021_project_deletion.sql").into_sql_str(),
                false,
            ),
            Migration::new(
                22,
                Cow::Borrowed("native source bundle debug files"),
                MigrationType::Simple,
                include_str!("../migrations/0022_native_source_bundles.sql").into_sql_str(),
                false,
            ),
        ]),
        locking: false, // The dedicated connection already owns MIGRATION_LOCK.
        table_name: Cow::Borrowed(MIGRATION_TABLE),
        ..Migrator::DEFAULT
    }
}

#[cfg(test)]
async fn acquire_ownership(
    connection: &mut PgConnection,
    budget: Duration,
) -> Result<(), PostgresError> {
    acquire_ownership_reported(
        connection,
        budget,
        &|_| {},
        MigrationProgress {
            phase: MigrationPhase::Inspecting,
            completed_steps: 0,
            total_steps: 0,
        },
    )
    .await
}

async fn acquire_ownership_reported(
    connection: &mut PgConnection,
    budget: Duration,
    reporter: Reporter<'_>,
    progress: MigrationProgress,
) -> Result<(), PostgresError> {
    tokio::time::timeout(budget, async {
        let mut reported_waiting = false;
        loop {
            let owned: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(MIGRATION_LOCK)
                .fetch_one(&mut *connection)
                .await?;
            if owned {
                return Ok(());
            }
            if !reported_waiting {
                reporter(MigrationProgress {
                    phase: MigrationPhase::Waiting,
                    ..progress
                });
                reported_waiting = true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| PostgresError::MigrationBusy)?
}

#[allow(clippy::too_many_arguments)]
async fn ensure_owned(
    connection: &mut PgConnection,
    mode: MigrationMode,
    migrator: &Migrator,
    data_steps: &[Box<dyn DataMigration>],
    generation: i64,
    reporter: Reporter<'_>,
    mut progress: MigrationProgress,
) -> Result<SchemaStatus, PostgresError> {
    let schema_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'metric')")
            .fetch_one(&mut *connection)
            .await?;
    let ledger_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('metric._sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut *connection)
            .await?;
    if schema_exists && !ledger_exists {
        // An unrelated existing schema must not be adopted or overwritten.
        let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='metric'")
            .fetch_one(&mut *connection).await?;
        if tables != 0 {
            return Err(PostgresError::UnknownSchema);
        }
    }
    if !ledger_exists {
        if mode == MigrationMode::Check {
            return Err(PostgresError::MigrationRequired);
        }
        sqlx::query("CREATE SCHEMA IF NOT EXISTS metric")
            .execute(&mut *connection)
            .await?;
        connection
            .ensure_migrations_table(MIGRATION_TABLE)
            .await
            .map_err(migration_error)?;
    }
    validate_sql_registry(connection, migrator, mode).await?;
    let state_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('metric.schema_state') IS NOT NULL")
            .fetch_one(&mut *connection)
            .await?;
    if state_exists {
        let state = read_state(connection).await?;
        if state > generation {
            return Err(PostgresError::NewerSchema);
        }
        if mode == MigrationMode::Check && state != generation {
            return Err(PostgresError::MigrationRequired);
        }
    } else if mode == MigrationMode::Check {
        return Err(PostgresError::MigrationRequired);
    }
    let applied = connection
        .list_applied_migrations(MIGRATION_TABLE)
        .await
        .map_err(migration_error)?;
    progress.phase = MigrationPhase::Running;
    progress.completed_steps =
        u32::try_from(applied.len()).map_err(|_| PostgresError::InvalidData)?;
    reporter(progress);
    if mode == MigrationMode::Auto {
        // Use SQLx's native transactional apply/ledger primitive under the same
        // dedicated ownership. Registry/checksum/dirty validation happened above.
        for migration in migrator.iter() {
            if migration.migration_type.is_down_migration()
                || applied
                    .iter()
                    .any(|installed| installed.version == migration.version)
            {
                continue;
            }
            connection
                .apply(MIGRATION_TABLE, migration)
                .await
                .map_err(migration_error)?;
            progress.completed_steps += 1;
            reporter(progress);
        }
    }
    validate_data_registry(connection, data_steps).await?;
    for step in data_steps {
        run_data_step(connection, step.as_ref(), mode).await?;
        progress.completed_steps += 1;
        reporter(progress);
    }
    if mode == MigrationMode::Auto {
        sqlx::query("UPDATE metric.schema_state SET generation=$1 WHERE singleton=true AND format_id='metric-postgres-v1'")
            .bind(generation).execute(&mut *connection).await?;
    }
    if read_state(connection).await? != generation {
        return Err(PostgresError::MigrationRequired);
    }
    reporter(MigrationProgress {
        phase: MigrationPhase::Complete,
        ..progress
    });
    Ok(SchemaStatus { generation })
}

async fn read_state(connection: &mut PgConnection) -> Result<i64, PostgresError> {
    let rows =
        sqlx::query("SELECT format_id, generation FROM metric.schema_state WHERE singleton=true")
            .fetch_all(connection)
            .await?;
    if rows.len() != 1 || rows[0].try_get::<String, _>("format_id")? != "metric-postgres-v1" {
        return Err(PostgresError::UnknownSchema);
    }
    Ok(rows[0].try_get("generation")?)
}

async fn validate_sql_registry(
    connection: &mut PgConnection,
    migrator: &Migrator,
    mode: MigrationMode,
) -> Result<(), PostgresError> {
    if connection
        .dirty_version(MIGRATION_TABLE)
        .await
        .map_err(migration_error)?
        .is_some()
    {
        return Err(PostgresError::MigrationMismatch);
    }
    let applied = connection
        .list_applied_migrations(MIGRATION_TABLE)
        .await
        .map_err(migration_error)?;
    for installed in &applied {
        let Some(expected) = migrator
            .iter()
            .find(|candidate| candidate.version == installed.version)
        else {
            return Err(PostgresError::NewerSchema);
        };
        if installed.checksum != expected.checksum {
            return Err(PostgresError::MigrationMismatch);
        }
    }
    if mode == MigrationMode::Check && applied.len() != migrator.iter().count() {
        return Err(PostgresError::MigrationRequired);
    }
    Ok(())
}

async fn validate_data_registry(
    connection: &mut PgConnection,
    steps: &[Box<dyn DataMigration>],
) -> Result<(), PostgresError> {
    let rows = sqlx::query("SELECT step_id, generation, checksum FROM metric.data_migrations")
        .fetch_all(connection)
        .await?;
    for row in rows {
        let id: String = row.try_get("step_id")?;
        let Some(step) = steps.iter().find(|step| step.id() == id) else {
            return Err(PostgresError::MigrationMismatch);
        };
        if row.try_get::<i64, _>("generation")? != step.generation()
            || row.try_get::<Vec<u8>, _>("checksum")? != step.checksum()
        {
            return Err(PostgresError::MigrationMismatch);
        }
    }
    Ok(())
}

async fn run_data_step(
    connection: &mut PgConnection,
    step: &dyn DataMigration,
    mode: MigrationMode,
) -> Result<(), PostgresError> {
    let installed =
        sqlx::query("SELECT checkpoint, complete FROM metric.data_migrations WHERE step_id=$1")
            .bind(step.id())
            .fetch_optional(&mut *connection)
            .await?;
    if installed
        .as_ref()
        .is_some_and(|row| row.get::<bool, _>("complete"))
    {
        return step.verify(connection).await;
    }
    if mode == MigrationMode::Check {
        return Err(PostgresError::MigrationRequired);
    }
    step.preflight(connection).await?;
    let mut checkpoint: Option<String> = installed
        .map(|row| row.try_get("checkpoint"))
        .transpose()?
        .flatten();
    loop {
        let mut transaction = connection.begin().await?;
        let batch = step
            .run_batch(&mut transaction, checkpoint.as_deref())
            .await?;
        if batch
            .checkpoint
            .as_ref()
            .is_some_and(|value| value.len() > 4096)
            || (!batch.complete && batch.checkpoint == checkpoint)
        {
            return Err(PostgresError::InvalidData);
        }
        sqlx::query("INSERT INTO metric.data_migrations(step_id,generation,checksum,checkpoint,complete) VALUES ($1,$2,$3,$4,false) ON CONFLICT (step_id) DO UPDATE SET checkpoint=EXCLUDED.checkpoint")
            .bind(step.id()).bind(step.generation()).bind(step.checksum()).bind(&batch.checkpoint)
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        checkpoint = batch.checkpoint;
        if batch.complete {
            break;
        }
    }
    step.verify(connection).await?;
    sqlx::query("UPDATE metric.data_migrations SET complete=true WHERE step_id=$1")
        .bind(step.id())
        .execute(connection)
        .await?;
    Ok(())
}

fn migration_error(error: MigrateError) -> PostgresError {
    match error {
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => {
            if let sqlx::Error::Database(database) = &error
                && let Some(code) = database.code()
            {
                if code == "42501" {
                    return PostgresError::PermissionDenied;
                }
                if code.starts_with("42") {
                    return PostgresError::MigrationMismatch;
                }
                if code.starts_with("22") {
                    return PostgresError::InvalidData;
                }
            }
            error.into()
        }
        MigrateError::VersionMissing(_) => PostgresError::NewerSchema,
        _ => PostgresError::MigrationMismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    async fn bootstrap(fixture: &Fixture) {
        PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn reported_sql_progress_is_monotonic_and_complete_only_after_verification() {
        let fixture = Fixture::new().await;
        let reports = std::sync::Mutex::new(Vec::new());
        let reporter = |p| reports.lock().unwrap().push(p);
        PostgresSchemaManager::ensure_with_reporter(
            &fixture.uri,
            &fixture.policy,
            &MigrationPolicy::default(),
            &reporter,
        )
        .await
        .unwrap();
        let values = reports.lock().unwrap().clone();
        assert_eq!(values.first().unwrap().phase, MigrationPhase::Inspecting);
        assert_eq!(values.last().unwrap().phase, MigrationPhase::Complete);
        assert_eq!(
            values.last().unwrap().completed_steps,
            SCHEMA_GENERATION as u32
        );
        assert!(values.iter().all(
            |p| p.total_steps == SCHEMA_GENERATION as u32 && p.completed_steps <= p.total_steps
        ));
        assert!(
            values
                .windows(2)
                .all(|p| p[0].completed_steps <= p[1].completed_steps)
        );
        reports.lock().unwrap().clear();
        let bad = upgraded_registry("CREATE TABLE metric.report_rollback(id integer); SELECT 1/0;");
        assert!(
            PostgresSchemaManager::ensure_registry_reported(
                &fixture.uri,
                &fixture.policy,
                &MigrationPolicy::default(),
                &bad,
                &[],
                SCHEMA_GENERATION + 1,
                &reporter
            )
            .await
            .is_err()
        );
        assert!(
            !reports
                .lock()
                .unwrap()
                .iter()
                .any(|p| p.phase == MigrationPhase::Complete)
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn reported_ownership_wait_is_cancellable_without_schema_mutation() {
        let fixture = Fixture::new().await;
        let mut owner = fixture.connection().await;
        acquire_ownership(&mut owner, Duration::from_secs(1))
            .await
            .unwrap();
        let waiting = AtomicBool::new(false);
        let reporter = |p: MigrationProgress| {
            if p.phase == MigrationPhase::Waiting {
                waiting.store(true, Ordering::SeqCst);
            }
        };
        assert!(
            tokio::time::timeout(
                Duration::from_millis(300),
                PostgresSchemaManager::ensure_with_reporter(
                    &fixture.uri,
                    &fixture.policy,
                    &MigrationPolicy::default(),
                    &reporter
                )
            )
            .await
            .is_err()
        );
        assert!(waiting.load(Ordering::SeqCst));
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('metric._sqlx_migrations') IS NOT NULL")
                .fetch_one(&mut owner)
                .await
                .unwrap();
        assert!(!exists);
        owner.close().await.unwrap();
        bootstrap(&fixture).await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn source_bundle_upgrade_from_twenty_one_preserves_existing_debug_files() {
        let fixture = Fixture::new().await;
        let mut legacy = registry();
        legacy.migrations.to_mut().retain(|step| step.version <= 21);
        PostgresSchemaManager::ensure_registry(
            &fixture.uri,
            &fixture.policy,
            &MigrationPolicy::default(),
            &legacy,
            &[],
            21,
        )
        .await
        .unwrap();
        let db = crate::PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        crate::test_support::seed_projects(db.clone(), 32).await;
        sqlx::query("INSERT INTO metric.debug_files(project_id,file_id,debug_id,file_type,checksum,sha1,size,name,uploaded_at) VALUES(1,$1,$2,3,$3,$4,12,'existing.pdb',1800000000000)")
            .bind([1_u8;16].as_slice()).bind([2_u8;16].as_slice()).bind([3_u8;32].as_slice()).bind([4_u8;20].as_slice()).execute(&db.pool).await.unwrap();
        let before: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(f) FROM metric.debug_files f")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(
            sqlx::query("UPDATE metric.debug_files SET file_type=6")
                .execute(&db.pool)
                .await
                .is_err()
        );
        bootstrap(&fixture).await;
        bootstrap(&fixture).await;
        let after: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(f) FROM metric.debug_files f")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(before, after);
        sqlx::query("UPDATE metric.debug_files SET file_type=6")
            .execute(&db.pool)
            .await
            .unwrap();
        assert!(
            sqlx::query("UPDATE metric.debug_files SET file_type=7")
                .execute(&db.pool)
                .await
                .is_err()
        );
        db.pool.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn deletion_upgrade_from_twenty_preserves_existing_artifact_and_project() {
        let fixture = Fixture::new().await;
        let mut legacy = registry();
        legacy.migrations.to_mut().retain(|step| step.version <= 20);
        PostgresSchemaManager::ensure_registry(
            &fixture.uri,
            &fixture.policy,
            &MigrationPolicy::default(),
            &legacy,
            &[],
            20,
        )
        .await
        .unwrap();
        let db = crate::PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        crate::test_support::seed_projects(db.clone(), 32).await;
        sqlx::query("INSERT INTO metric.artifact_bundles(bundle_id,organization_id,sha1,bundle_debug_id,debug_tokens,checksum,size,uploaded_at) VALUES($1,1,$2,$3,'{}',$4,3,1800000000000)")
            .bind([1_u8;16].as_slice()).bind([1_u8;20].as_slice()).bind([9_u8;16].as_slice()).bind([1_u8;32].as_slice()).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO metric.artifact_bundle_bindings(organization_id,bundle_id,project_id) VALUES(1,$1,1)").bind([1_u8;16].as_slice()).execute(&db.pool).await.unwrap();
        let project: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(p) FROM metric.projects p WHERE id=1")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let bundle: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(b) FROM metric.artifact_bundles b")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        bootstrap(&fixture).await;
        bootstrap(&fixture).await;
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT to_jsonb(p)-'deleted_at'-'deletion_operation'-'slug_reserved_until' FROM metric.projects p WHERE id=1").fetch_one(&db.pool).await.unwrap(),project);
        assert_eq!(
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT to_jsonb(b)-'deletion_operation' FROM metric.artifact_bundles b"
            )
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            bundle
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric.project_deletions")
                .fetch_one(&db.pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM metric.artifact_bundle_bindings WHERE project_id=1"
            )
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            1
        );
        db.pool.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn artifact_upgrade_from_nineteen_preserves_debug_metadata_and_counters() {
        use metric_domain::{
            OrganizationId, ProjectId, Timestamp,
            debug_files::{DebugFile, DebugFileId, DebugFileType, DebugId, DebugUpload},
        };
        use metric_ports::DebugFileStore;
        let fixture = Fixture::new().await;
        let mut legacy = registry();
        legacy.migrations.to_mut().retain(|step| step.version <= 19);
        PostgresSchemaManager::ensure_registry(
            &fixture.uri,
            &fixture.policy,
            &MigrationPolicy::default(),
            &legacy,
            &[],
            19,
        )
        .await
        .unwrap();
        let db = crate::PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        crate::test_support::seed_projects(db.clone(), 32).await;
        let store =
            crate::PostgresDebugFileStore::new(db.clone(), crate::DebugFileQuota::default())
                .unwrap();
        let pid = ProjectId::new(1).unwrap();
        let time = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let upload = DebugUpload {
            id: [1; 16],
            project_id: pid,
            organization_id: OrganizationId::new(1).unwrap(),
            sha1: [1; 20],
            name: "upgrade.so".into(),
            debug_id: Some(DebugId::Uuid {
                uuid: [9; 16],
                appendix: 0,
            }),
            code_id: None,
            chunks: vec![[1; 20]],
            created_at: time,
            updated_at: time,
        };
        let file = DebugFile {
            id: DebugFileId::derive(pid, [9; 32]),
            project_id: pid,
            debug_id: upload.debug_id.clone(),
            code_id: None,
            file_type: DebugFileType::Elf,
            checksum: [9; 32],
            sha1: upload.sha1,
            size: 3,
            name: upload.name.clone(),
            uploaded_at: time,
        };
        store.upsert_upload(upload.clone()).await.unwrap();
        store
            .publish_debug_file(upload.id, file.clone())
            .await
            .unwrap();
        let project: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(p) FROM metric.projects p WHERE id=1")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        let organization: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(o) FROM metric.organizations o WHERE id=1")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        bootstrap(&fixture).await;
        bootstrap(&fixture).await;
        assert_eq!(store.load_debug_file(pid, file.id).await.unwrap(), file);
        assert_eq!(
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT to_jsonb(p)-'deleted_at'-'deletion_operation'-'slug_reserved_until' FROM metric.projects p WHERE id=1"
            )
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            project
        );
        assert_eq!(sqlx::query_scalar::<_,serde_json::Value>("SELECT to_jsonb(o)-'artifact_bytes'-'artifact_count' FROM metric.organizations o WHERE id=1").fetch_one(&db.pool).await.unwrap(),organization);
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT artifact_bytes,artifact_count FROM metric.organizations WHERE id=1"
            )
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            (0, 0)
        );
        db.pool.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn debug_metadata_upgrade_from_eighteen_preserves_project_and_archive() {
        let fixture = Fixture::new().await;
        let mut legacy = registry();
        legacy.migrations.to_mut().retain(|step| step.version <= 18);
        PostgresSchemaManager::ensure_registry(
            &fixture.uri,
            &fixture.policy,
            &MigrationPolicy::default(),
            &legacy,
            &[],
            18,
        )
        .await
        .unwrap();
        let database = crate::PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        crate::test_support::seed_projects(database.clone(), 32).await;
        let before: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(p) FROM metric.projects p WHERE id=1")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        bootstrap(&fixture).await;
        bootstrap(&fixture).await;
        let after: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(p)-'debug_file_bytes'-'debug_file_count'-'deleted_at'-'deletion_operation'-'slug_reserved_until' FROM metric.projects p WHERE id=1").fetch_one(&database.pool).await.unwrap();
        assert_eq!(before, after);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric._sqlx_migrations")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(count, SCHEMA_GENERATION);
        let quotas: (i64, i64) = sqlx::query_as(
            "SELECT debug_file_bytes,debug_file_count FROM metric.projects WHERE id=1",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(quotas, (0, 0));
        let archive: bool =
            sqlx::query_scalar("SELECT to_regclass('metric.archive_manifests') IS NOT NULL")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert!(archive);
        database.pool.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn archive_upgrade_from_seventeen_preserves_held_sources() {
        let fixture = Fixture::new().await;
        let mut legacy = registry();
        legacy.migrations.to_mut().retain(|step| step.version <= 17);
        PostgresSchemaManager::ensure_registry(
            &fixture.uri,
            &fixture.policy,
            &MigrationPolicy::default(),
            &legacy,
            &[],
            17,
        )
        .await
        .unwrap();
        let database = crate::PostgresDatabase::connect(&fixture.uri, &fixture.policy)
            .await
            .unwrap();
        crate::test_support::seed_projects(database.clone(), 32).await;
        let time = metric_domain::Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let store = crate::PostgresMetricStore::new(
            database.clone(),
            crate::MetricRetention {
                days: 1,
                max_series_per_project: 16,
                archive: true,
            },
        )
        .unwrap();
        let mut batch = metric_domain::metrics::MetricDeltaBatch::default();
        batch.push(metric_domain::metrics::MetricDelta {
            series: metric_domain::metrics::MetricSeries {
                project_id: metric_domain::ProjectId::new(1).unwrap(),
                name: "archive-upgrade".into(),
                kind: metric_domain::metrics::MetricKind::Counter,
                unit: "none".into(),
                tags: Default::default(),
            },
            bucket_start: metric_domain::Timestamp::from_unix_millis(
                time.unix_millis() - 172_800_000,
            )
            .unwrap(),
            bucket_width_seconds: 60,
            received_at: time,
            trace_id: None,
            aggregate: metric_domain::metrics::MetricAggregate::from_measurement(
                metric_domain::metrics::MetricKind::Counter,
                2.0,
            ),
        });
        metric_ports::MetricStore::persist_metrics(&store, batch)
            .await
            .unwrap();
        let before: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(b) FROM metric.metric_buckets b")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric._sqlx_migrations")
                .fetch_one(&database.pool)
                .await
                .unwrap(),
            17
        );
        bootstrap(&fixture).await;
        bootstrap(&fixture).await;
        assert_eq!(
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT to_jsonb(b) FROM metric.metric_buckets b"
            )
            .fetch_one(&database.pool)
            .await
            .unwrap(),
            before
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM metric._sqlx_migrations")
                .fetch_one(&database.pool)
                .await
                .unwrap(),
            SCHEMA_GENERATION
        );
        let archive =
            crate::PostgresArchiveStore::new(database.clone(), crate::EventCodecConfig::default())
                .unwrap();
        let claimed = metric_ports::ArchiveStore::claim(
            &archive,
            metric_ports::ArchiveClaimRequest {
                kind: metric_domain::archive::ArchiveKind::Metric,
                now: time,
                maximum_events: 10,
                target_uncompressed_bytes: 1024 * 1024,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(claimed.records.len(), 1);
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT archive_due IS NOT NULL AND expire_at IS NULL FROM metric.metric_buckets"
            )
            .fetch_one(&database.pool)
            .await
            .unwrap()
        );
        database.pool.close().await;
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn empty_bootstrap_current_and_check_mode() {
        let fixture = Fixture::new().await;
        let check = MigrationPolicy {
            mode: MigrationMode::Check,
            ..Default::default()
        };
        assert_eq!(
            PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &check).await,
            Err(PostgresError::MigrationRequired)
        );
        bootstrap(&fixture).await;
        bootstrap(&fixture).await;
        assert_eq!(
            PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &check)
                .await
                .unwrap()
                .generation,
            SCHEMA_GENERATION
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn checksum_and_newer_schema_are_rejected_without_writes() {
        let fixture = Fixture::new().await;
        bootstrap(&fixture).await;
        let mut connection = fixture.connection().await;
        sqlx::query("UPDATE metric._sqlx_migrations SET checksum=$1 WHERE version=1")
            .bind(vec![1_u8])
            .execute(&mut connection)
            .await
            .unwrap();
        assert_eq!(
            PostgresSchemaManager::ensure(
                &fixture.uri,
                &fixture.policy,
                &MigrationPolicy::default()
            )
            .await,
            Err(PostgresError::MigrationMismatch)
        );
        sqlx::query("UPDATE metric._sqlx_migrations SET checksum=$1 WHERE version=1")
            .bind(registry().migrations[0].checksum.as_ref())
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("UPDATE metric.schema_state SET generation=999")
            .execute(&mut connection)
            .await
            .unwrap();
        assert_eq!(
            PostgresSchemaManager::ensure(
                &fixture.uri,
                &fixture.policy,
                &MigrationPolicy::default()
            )
            .await,
            Err(PostgresError::NewerSchema)
        );
        assert_eq!(read_state(&mut connection).await.unwrap(), 999);
        connection.close().await.unwrap();
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn unknown_schema_is_not_adopted() {
        let fixture = Fixture::new().await;
        let mut connection = fixture.connection().await;
        sqlx::raw_sql("CREATE SCHEMA metric; CREATE TABLE metric.unrelated (id integer)")
            .execute(&mut connection)
            .await
            .unwrap();
        assert_eq!(
            PostgresSchemaManager::ensure(
                &fixture.uri,
                &fixture.policy,
                &MigrationPolicy::default()
            )
            .await,
            Err(PostgresError::UnknownSchema)
        );
        let ledger: bool =
            sqlx::query_scalar("SELECT to_regclass('metric._sqlx_migrations') IS NOT NULL")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert!(!ledger);
        connection.close().await.unwrap();
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn competing_starters_and_bounded_ownership() {
        let fixture = Fixture::new().await;
        let policy = MigrationPolicy::default();
        let (one, two) = tokio::join!(
            PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &policy),
            PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &policy)
        );
        assert_eq!(one.unwrap().generation, SCHEMA_GENERATION);
        assert_eq!(two.unwrap().generation, SCHEMA_GENERATION);
        let mut owner = fixture.connection().await;
        acquire_ownership(&mut owner, Duration::from_secs(1))
            .await
            .unwrap();
        let bounded = MigrationPolicy {
            lock_timeout: Duration::from_millis(200),
            ..Default::default()
        };
        assert_eq!(
            PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &bounded).await,
            Err(PostgresError::MigrationBusy)
        );
        owner.close().await.unwrap();
        bootstrap(&fixture).await;
        fixture.cleanup().await;
    }

    fn upgraded_registry(sql: &'static str) -> Migrator {
        let mut migrator = registry();
        migrator.migrations.to_mut().push(Migration::new(
            SCHEMA_GENERATION + 1,
            Cow::Borrowed("synthetic future upgrade"),
            MigrationType::Simple,
            sql.into_sql_str(),
            false,
        ));
        migrator
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn failed_ddl_rolls_back_and_retains_published_generation() {
        let fixture = Fixture::new().await;
        bootstrap(&fixture).await;
        let bad = upgraded_registry("CREATE TABLE metric.rollback_probe(id integer); SELECT 1/0;");
        assert!(
            PostgresSchemaManager::ensure_registry(
                &fixture.uri,
                &fixture.policy,
                &MigrationPolicy::default(),
                &bad,
                &[],
                SCHEMA_GENERATION + 1
            )
            .await
            .is_err()
        );
        let mut connection = fixture.connection().await;
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('metric.rollback_probe') IS NOT NULL")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert!(!exists);
        assert_eq!(
            read_state(&mut connection).await.unwrap(),
            SCHEMA_GENERATION
        );
        connection.close().await.unwrap();
        bootstrap(&fixture).await;
        fixture.cleanup().await;
    }

    struct SyntheticStep {
        fail_second: Arc<AtomicBool>,
    }

    impl DataMigration for SyntheticStep {
        fn id(&self) -> &'static str {
            "synthetic-bounded-upgrade"
        }
        fn generation(&self) -> i64 {
            SCHEMA_GENERATION + 1
        }
        fn checksum(&self) -> &'static [u8] {
            b"immutable-test-step-v1"
        }
        fn preflight<'a>(&'a self, connection: &'a mut PgConnection) -> StepFuture<'a, ()> {
            Box::pin(async move {
                let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.migration_probe")
                    .fetch_one(connection)
                    .await?;
                if count == 5 {
                    Ok(())
                } else {
                    Err(PostgresError::InvalidData)
                }
            })
        }
        fn run_batch<'a>(
            &'a self,
            transaction: &'a mut Transaction<'_, Postgres>,
            checkpoint: Option<&'a str>,
        ) -> StepFuture<'a, DataBatch> {
            Box::pin(async move {
                let previous = checkpoint
                    .map(str::parse::<i32>)
                    .transpose()
                    .map_err(|_| PostgresError::InvalidData)?
                    .unwrap_or(0);
                let current = previous + 1;
                sqlx::query(
                    "UPDATE metric.migration_probe SET applications=applications+1 WHERE id=$1",
                )
                .bind(current)
                .execute(&mut **transaction)
                .await?;
                if current == 2 && self.fail_second.swap(false, Ordering::SeqCst) {
                    return Err(PostgresError::Unavailable);
                }
                Ok(DataBatch {
                    checkpoint: Some(current.to_string()),
                    complete: current == 5,
                })
            })
        }
        fn verify<'a>(&'a self, connection: &'a mut PgConnection) -> StepFuture<'a, ()> {
            Box::pin(async move {
                let correct: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM metric.migration_probe WHERE applications=1",
                )
                .fetch_one(connection)
                .await?;
                if correct == 5 {
                    Ok(())
                } else {
                    Err(PostgresError::InvalidData)
                }
            })
        }
    }

    #[tokio::test]
    #[ignore = "requires Docker PostgreSQL"]
    async fn future_rust_upgrade_resumes_checkpoint_without_double_application() {
        let fixture = Fixture::new().await;
        bootstrap(&fixture).await;
        let sql = upgraded_registry(
            "CREATE TABLE metric.migration_probe(id integer PRIMARY KEY, applications integer NOT NULL DEFAULT 0); INSERT INTO metric.migration_probe(id) SELECT generate_series(1,5);",
        );
        let steps: Vec<Box<dyn DataMigration>> = vec![Box::new(SyntheticStep {
            fail_second: Arc::new(AtomicBool::new(true)),
        })];
        let policy = MigrationPolicy::default();
        assert_eq!(
            PostgresSchemaManager::ensure_registry(
                &fixture.uri,
                &fixture.policy,
                &policy,
                &sql,
                &steps,
                SCHEMA_GENERATION + 1
            )
            .await,
            Err(PostgresError::Unavailable)
        );
        let mut connection = fixture.connection().await;
        assert_eq!(
            read_state(&mut connection).await.unwrap(),
            SCHEMA_GENERATION
        );
        let checkpoint: String = sqlx::query_scalar("SELECT checkpoint FROM metric.data_migrations WHERE step_id='synthetic-bounded-upgrade'").fetch_one(&mut connection).await.unwrap();
        assert_eq!(checkpoint, "1");
        let second: i32 =
            sqlx::query_scalar("SELECT applications FROM metric.migration_probe WHERE id=2")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(second, 0);
        let check = MigrationPolicy {
            mode: MigrationMode::Check,
            ..Default::default()
        };
        assert_eq!(
            PostgresSchemaManager::ensure_registry(
                &fixture.uri,
                &fixture.policy,
                &check,
                &sql,
                &steps,
                SCHEMA_GENERATION + 1
            )
            .await,
            Err(PostgresError::MigrationRequired)
        );
        assert_eq!(
            PostgresSchemaManager::ensure_registry(
                &fixture.uri,
                &fixture.policy,
                &policy,
                &sql,
                &steps,
                SCHEMA_GENERATION + 1
            )
            .await
            .unwrap()
            .generation,
            SCHEMA_GENERATION + 1
        );
        assert_eq!(
            PostgresSchemaManager::ensure_registry(
                &fixture.uri,
                &fixture.policy,
                &check,
                &sql,
                &steps,
                SCHEMA_GENERATION + 1
            )
            .await
            .unwrap()
            .generation,
            SCHEMA_GENERATION + 1
        );
        connection.close().await.unwrap();
        fixture.cleanup().await;
    }
}
