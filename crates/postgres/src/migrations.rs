//! One dedicated connection owns SQL, bounded data steps and final publication.
use crate::{ConnectionPolicy, PostgresError};
use sqlx::{
    Connection, PgConnection, Postgres, Row, SqlSafeStr, Transaction,
    migrate::{Migrate, MigrateError, Migration, MigrationType, Migrator},
};
use std::{borrow::Cow, future::Future, pin::Pin, time::Duration};

const SCHEMA_GENERATION: i64 = 14;
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
        let migrator = registry();
        Self::ensure_registry(
            uri,
            connection_policy,
            migration_policy,
            &migrator,
            &[],
            SCHEMA_GENERATION,
        )
        .await
    }

    async fn ensure_registry(
        uri: &str,
        connection_policy: &ConnectionPolicy,
        policy: &MigrationPolicy,
        migrator: &Migrator,
        data_steps: &[Box<dyn DataMigration>],
        generation: i64,
    ) -> Result<SchemaStatus, PostgresError> {
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
        acquire_ownership(&mut connection, policy.lock_timeout).await?;
        let result = ensure_owned(
            &mut connection,
            policy.mode,
            migrator,
            data_steps,
            generation,
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
        ]),
        locking: false, // The dedicated connection already owns MIGRATION_LOCK.
        table_name: Cow::Borrowed(MIGRATION_TABLE),
        ..Migrator::DEFAULT
    }
}

async fn acquire_ownership(
    connection: &mut PgConnection,
    budget: Duration,
) -> Result<(), PostgresError> {
    tokio::time::timeout(budget, async {
        loop {
            let owned: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(MIGRATION_LOCK)
                .fetch_one(&mut *connection)
                .await?;
            if owned {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| PostgresError::MigrationBusy)?
}

async fn ensure_owned(
    connection: &mut PgConnection,
    mode: MigrationMode,
    migrator: &Migrator,
    data_steps: &[Box<dyn DataMigration>],
    generation: i64,
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
    if mode == MigrationMode::Auto {
        migrator
            .run(&mut *connection)
            .await
            .map_err(migration_error)?;
    }
    validate_data_registry(connection, data_steps).await?;
    for step in data_steps {
        run_data_step(connection, step.as_ref(), mode).await?;
    }
    if mode == MigrationMode::Auto {
        sqlx::query("UPDATE metric.schema_state SET generation=$1 WHERE singleton=true AND format_id='metric-postgres-v1'")
            .bind(generation).execute(&mut *connection).await?;
    }
    if read_state(connection).await? != generation {
        return Err(PostgresError::MigrationRequired);
    }
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
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => error.into(),
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
