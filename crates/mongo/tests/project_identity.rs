use std::{
    error::Error,
    time::{Duration, Instant},
};

use metric_domain::{
    DisplayName, DsnKey, IpScrubPolicy, ItemCapabilities, OrganizationId, OrganizationIdentity,
    ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits, ProjectKeyIdentity,
    ProjectKeyLabel, ProjectKeyState, SecretBytes, Slug, Timestamp,
    api::ProjectPolicyUpdate,
    inbound_filter::{
        InboundFilterField, InboundFilterFields, InboundFilterOperation, InboundFilterPolicy,
        InboundFilterRule, InboundFilterSignal,
    },
};
use metric_mongo::{MongoBootstrapError, MongoProjectStore, SCHEMA_GENERATION};
use metric_ports::{ProjectStore, ProjectStoreError};
use mongodb::{Client, Database, bson::doc};

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn infrastructure_project_identity_schema_uniqueness_and_authorization() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "performance baseline requires MongoDB 8.0.12 from deploy/compose.dev.yml"]
async fn performance_project_identity_cold_mongodb_lookup() {
    let database = test_database().await.unwrap();
    let result = measure_cold_lookup(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn measure_cold_lookup(database: &Database) -> Result<(), Box<dyn Error>> {
    let store = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    store.bootstrap_or_validate().await?;
    store
        .insert_organization(organization(1, "perf-org"))
        .await?;
    store.insert_project(project(42, 1, "perf-project")).await?;
    let key = DsnKey::from_bytes([5; 16]);
    store.insert_project_key(project_key(key, 42)).await?;
    store.load_project(key).await?;

    let iterations = 1_000_u64;
    let mut samples = Vec::with_capacity(iterations as usize);
    let started = Instant::now();
    for _ in 0..iterations {
        let lookup_started = Instant::now();
        std::hint::black_box(store.load_project(key).await?);
        samples.push(lookup_started.elapsed());
    }
    let elapsed = started.elapsed();
    samples.sort_unstable();
    let percentile = |percent: usize| -> Duration { samples[(samples.len() - 1) * percent / 100] };
    let rps = iterations as f64 / elapsed.as_secs_f64();
    let average_micros = elapsed.as_micros() as f64 / iterations as f64;
    eprintln!(
        "MongoDB direct lookup: {rps:.0} lookups/s, {average_micros:.0} us average, p50={} us, p95={} us, p99={} us",
        percentile(50).as_micros(),
        percentile(95).as_micros(),
        percentile(99).as_micros()
    );
    assert_eq!(samples.len(), iterations as usize);
    Ok(())
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let store = MongoProjectStore::from_database(database.clone(), SecretBytes::new([7; 32]), 32);
    store.bootstrap_or_validate().await?;
    store.bootstrap_or_validate().await?;

    let marker = database
        .collection::<mongodb::bson::Document>("schema_meta")
        .find_one(doc! { "_id": "metric.schema" })
        .await?
        .unwrap();
    assert_eq!(marker.get_i32("generation")?, SCHEMA_GENERATION);
    assert_eq!(marker.get_str("state")?, "complete");

    let invalid = database
        .collection::<mongodb::bson::Document>("organizations")
        .insert_one(doc! { "_id": 1_i64, "slug": "invalid-without-required-fields" })
        .await;
    assert!(
        invalid.is_err(),
        "collection validator must reject incomplete data"
    );

    let inserted_organization = organization(1, "acme");
    store
        .insert_organization(inserted_organization.clone())
        .await?;
    assert_eq!(
        store.insert_organization(organization(1, "other")).await,
        Err(ProjectStoreError::IdentityCollision)
    );
    assert_eq!(
        store.insert_organization(organization(2, "acme")).await,
        Err(ProjectStoreError::OrganizationSlugExists)
    );

    let inserted_project = project(42, 1, "backend");
    store.insert_project(inserted_project.clone()).await?;
    assert_eq!(
        store.insert_project(project(43, 1, "backend")).await,
        Err(ProjectStoreError::ProjectSlugExists)
    );

    let key = DsnKey::from_bytes([3; 16]);
    store.insert_project_key(project_key(key, 42)).await?;
    assert_eq!(
        store.insert_project_key(project_key(key, 42)).await,
        Err(ProjectStoreError::KeyCollision)
    );
    assert_eq!(
        store.load_project(key).await?.project_id,
        ProjectId::new(42)?
    );

    store.set_key_state(key, ProjectKeyState::Disabled).await?;
    assert_eq!(
        store.load_project(key).await,
        Err(ProjectStoreError::NotFound)
    );
    store.set_key_state(key, ProjectKeyState::Active).await?;
    assert!(store.load_project(key).await.is_ok());

    let inbound_filters = InboundFilterPolicy::new(vec![InboundFilterRule {
        signal: InboundFilterSignal::Error,
        field: InboundFilterField::Message,
        operation: InboundFilterOperation::Contains,
        pattern: "healthcheck".into(),
    }])?;
    let (updated, _) = store
        .update_project_policy(
            ProjectId::new(42)?,
            ProjectPolicyUpdate {
                expected_revision: 1,
                ip_policy: IpScrubPolicy::Hmac,
                items: inserted_project.items,
                limits: inserted_project.limits,
                inbound_filters,
            },
        )
        .await?;
    assert_eq!(updated.policy_revision, 2);
    assert_eq!(updated.inbound_filters.rules().len(), 1);
    let resolved = store.load_project(key).await?;
    let mut fields = InboundFilterFields::empty(InboundFilterSignal::Error);
    fields.message = Some("periodic healthcheck failure");
    assert!(resolved.inbound_filters.matches(&fields).is_some());

    store
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::Disabled)
        .await?;
    assert_eq!(
        store.load_project(key).await,
        Err(ProjectStoreError::NotFound)
    );
    store
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::Active)
        .await?;
    assert!(store.load_project(key).await.is_ok());

    store
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::PendingDelete)
        .await?;
    assert_eq!(
        store.load_project(key).await,
        Err(ProjectStoreError::NotFound)
    );
    store
        .set_project_acceptance(ProjectId::new(42)?, ProjectAcceptanceState::Active)
        .await?;
    assert!(store.load_project(key).await.is_ok());

    database
        .run_command(doc! {
            "collMod": "organizations",
            "validator": {},
            "validationLevel": "strict",
            "validationAction": "error",
        })
        .await?;
    assert!(matches!(
        store.bootstrap_or_validate().await,
        Err(MongoBootstrapError::IncompatibleSchema)
    ));
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
    let name = format!(
        "metric_phase2_test_{}",
        mongodb::bson::oid::ObjectId::new().to_hex()
    );
    Ok(client.database(&name))
}

fn organization(id: u64, slug: &str) -> OrganizationIdentity {
    OrganizationIdentity {
        id: OrganizationId::new(id).unwrap(),
        slug: Slug::new(slug).unwrap(),
        display_name: DisplayName::new("Acme").unwrap(),
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}

fn project(id: i32, organization_id: u64, slug: &str) -> ProjectIdentity {
    ProjectIdentity {
        id: ProjectId::new(id).unwrap(),
        organization_id: OrganizationId::new(organization_id).unwrap(),
        slug: Slug::new(slug).unwrap(),
        display_name: DisplayName::new("Backend").unwrap(),
        state: ProjectAcceptanceState::Active,
        policy_revision: 1,
        ip_policy: IpScrubPolicy::Hmac,
        items: ItemCapabilities {
            error: true,
            client_report: true,
            log: true,
            transaction: true,
            span: true,
            feedback: true,
        },
        limits: ProjectIngestLimits::default(),
        grouping_revision: 1,
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}

fn project_key(key: DsnKey, project_id: i32) -> ProjectKeyIdentity {
    ProjectKeyIdentity {
        key,
        project_id: ProjectId::new(project_id).unwrap(),
        state: ProjectKeyState::Active,
        label: ProjectKeyLabel::new("default").unwrap(),
        created_at: Timestamp::from_unix_millis(1_000).unwrap(),
    }
}
