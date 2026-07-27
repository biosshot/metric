use std::{
    error::Error,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use metric_domain::{
    ProjectId, SecretBytes, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
    sessions::{SessionId, SessionState, SessionUpdate},
};
use metric_mongo::{MongoProjectStore, SessionRetention};
use metric_ports::{DurableOutcome, SessionStore};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn session_upsert_health_and_stale_terminalization_are_durable() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = control.session_store(SessionRetention::default());
    let project = ProjectId::new(42)?;
    let session = SessionId::derive(project, [1; 16]);
    let release = ReleaseId::from_bytes([2; 16]);
    let environment = EnvironmentId::from_bytes([3; 16]);
    let update = |state, sequence, updated_at| SessionUpdate {
        id: session,
        project_id: project,
        release_id: release,
        environment_id: environment,
        started_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        updated_at: Timestamp::from_unix_millis(updated_at).unwrap(),
        state,
        sequence: Some(sequence),
        duration_ms: Some(1_000),
        user_digest: Some([4; 16]),
    };
    let outcomes = store
        .persist_sessions(vec![
            update(SessionState::Ok, 1, 1_700_000_000_100),
            update(SessionState::Exited, 3, 1_700_000_001_000),
            update(SessionState::Crashed, 2, 1_700_000_000_900),
            update(SessionState::Exited, 3, 1_700_000_001_000),
        ])
        .await?;
    assert_eq!(
        outcomes,
        vec![
            DurableOutcome::Accepted,
            DurableOutcome::Accepted,
            DurableOutcome::Accepted,
            DurableOutcome::Duplicate,
        ]
    );
    let stored = store.load_session(project, session).await?;
    assert_eq!(stored.state, SessionState::Crashed);
    assert_eq!(stored.sequence, Some(3));

    let health = store
        .release_health(
            project,
            release,
            Timestamp::from_unix_millis(1_699_999_000_000)?,
            Timestamp::from_unix_millis(1_700_001_000_000)?,
        )
        .await?;
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].sessions, 1);
    assert_eq!(health[0].crashed, 1);
    assert_eq!(health[0].approximate_users, 1);
    assert_eq!(health[0].approximate_crashed_users, 1);
    database
        .collection::<mongodb::bson::Document>("session_stats_hourly")
        .update_many(
            doc! { "p": project.get() },
            doc! { "$set": { "n": 999_i64 } },
        )
        .await?;
    assert_eq!(
        store
            .rebuild_session_stats(
                project,
                Timestamp::from_unix_millis(1_699_999_000_000)?,
                Timestamp::from_unix_millis(1_700_001_000_000)?,
            )
            .await?,
        1
    );
    assert_eq!(
        store
            .release_health(
                project,
                release,
                Timestamp::from_unix_millis(1_699_999_000_000)?,
                Timestamp::from_unix_millis(1_700_001_000_000)?,
            )
            .await?[0]
            .sessions,
        1
    );

    let active = SessionId::derive(project, [9; 16]);
    let mut active_update = update(SessionState::Ok, 1, 1_700_000_000_100);
    active_update.id = active;
    store.persist_sessions(vec![active_update]).await?;
    assert_eq!(
        store
            .terminalize_stale_sessions(
                Timestamp::from_unix_millis(1_700_000_010_100)?,
                Duration::from_secs(5),
            )
            .await?,
        1
    );
    assert_eq!(
        store.load_session(project, active).await?.state,
        SessionState::Abnormal
    );
    Ok(())
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI").unwrap_or_else(|_| {
        "mongodb://metric:metric-local-only@127.0.0.1:27018/?authSource=admin&serverSelectionTimeoutMS=2000&connectTimeoutMS=2000".to_owned()
    });
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(client.database(&format!(
        "metric_phase30_sessions_{}_{}",
        std::process::id(),
        nonce
    )))
}
