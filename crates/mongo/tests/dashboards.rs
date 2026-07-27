use std::error::Error;

use metric_domain::{
    ProjectId, SecretBytes, Timestamp,
    auth::UserId,
    dashboards::{
        Dashboard, DashboardId, DashboardRefreshInterval, DashboardWidget, DashboardWidgetId,
        SavedQuery, SavedQueryId, WidgetShape,
    },
    explore::{ExploreDataset, ExploreQuery},
};
use metric_mongo::MongoProjectStore;
use metric_ports::{DashboardStore, DashboardStoreError};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires a real MongoDB configured by METRIC_TEST_MONGODB_URI"]
async fn saved_query_and_dashboard_crud_is_project_scoped_and_revisioned() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let control = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    control.bootstrap_or_validate().await?;
    let store = control.dashboard_store();
    let project = ProjectId::new(42)?;
    let other = ProjectId::new(43)?;
    let now = Timestamp::from_unix_millis(1_700_000_000_000)?;
    let saved = SavedQuery {
        id: SavedQueryId::from_bytes([1; 16]),
        project_id: project,
        name: "Recent logs".into(),
        query: ExploreQuery {
            dataset: ExploreDataset::Logs,
            from: Timestamp::from_unix_millis(now.unix_millis() - 3_600_000)?,
            until: now,
            predicates: Vec::new(),
            aggregates: Vec::new(),
            group_by: Vec::new(),
            interval: None,
            cursor: None,
            limit: 50,
        },
        revision: 1,
        created_by: UserId::new(9)?,
        updated_by: UserId::new(9)?,
        created_at: now,
        updated_at: now,
    };
    store.insert_saved_query(saved.clone()).await?;
    assert_eq!(
        store.list_saved_queries(project, 10).await?,
        vec![saved.clone()]
    );
    assert!(store.list_saved_queries(other, 10).await?.is_empty());

    let dashboard = Dashboard {
        id: DashboardId::from_bytes([2; 16]),
        project_id: project,
        name: "Operations".into(),
        widgets: vec![DashboardWidget {
            id: DashboardWidgetId::from_bytes([3; 16]),
            title: "Recent logs".into(),
            saved_query_id: saved.id,
            shape: WidgetShape::Table,
        }],
        refresh_interval: DashboardRefreshInterval::Manual,
        revision: 1,
        created_by: UserId::new(9)?,
        updated_by: UserId::new(9)?,
        created_at: now,
        updated_at: now,
    };
    store.insert_dashboard(dashboard.clone()).await?;
    assert_eq!(
        store.load_dashboard(project, dashboard.id).await?,
        dashboard
    );

    let mut updated = saved.clone();
    updated.name = "All recent logs".into();
    updated.revision = 2;
    store.replace_saved_query(updated.clone(), 1).await?;
    assert_eq!(
        store.replace_saved_query(updated, 1).await,
        Err(DashboardStoreError::Conflict)
    );

    // Deleting a referenced query deliberately leaves a visible partial widget failure.
    store.delete_saved_query(project, saved.id).await?;
    assert_eq!(
        store.load_saved_query(project, saved.id).await,
        Err(DashboardStoreError::NotFound)
    );
    assert_eq!(
        store.load_dashboard(project, dashboard.id).await?,
        dashboard
    );

    let marker = database
        .collection::<mongodb::bson::Document>("schema_meta")
        .find_one(doc! { "_id": "metric.schema" })
        .await?
        .unwrap();
    assert_eq!(marker.get_i32("generation")?, 14);
    Ok(())
}

async fn test_database() -> Result<Database, Box<dyn Error>> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_owned());
    let client = Client::with_uri_str(uri).await?;
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await?;
    Ok(client.database(&format!(
        "metric_phase33_dashboards_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    )))
}
