#![recursion_limit = "256"]

//! MongoDB project-identity adapter and initial empty-schema bootstrap.

mod api;
mod archive;
mod artifacts;
mod auth;
mod dashboards;
mod debug_files;
mod deletion;
mod event;
mod explore;
mod feedback;
mod finalizer;
mod issue;
mod maintenance;
mod monitors;
mod notifications;
mod releases;
mod sessions;
mod signals;

pub use api::MongoInvestigationStore;
pub use archive::MongoArchiveStore;
pub use artifacts::{ArtifactQuota, MongoArtifactStore};
pub use auth::MongoAuthStore;
pub use dashboards::MongoDashboardStore;
pub use debug_files::{DebugFileQuota, MongoDebugFileStore};
pub use deletion::{
    DATASET_REGISTRY, DELETION_PLAN_VERSION, DatasetOwnership, DatasetRegistration,
    FILESYSTEM_NAMESPACE_REGISTRY,
};
pub use event::{
    EventCodecConfig, EventCodecError, MongoEventStore, MongoPreparedEvent, decode_pending_event,
};
pub use explore::MongoExploreStore;
pub use feedback::MongoFeedbackStore;
pub use finalizer::{DecodedFinalizedEvent, MongoFinalizationStore, decode_finalized_event};
pub use issue::{IssueCodecConfig, IssueCodecError, MongoIssueStore, decode_issue};
pub use maintenance::MongoMaintenanceStore;
pub use monitors::{MongoMonitorStore, MonitorRetention};
pub use notifications::MongoNotificationStore;
pub use releases::MongoReleaseStore;
pub use sessions::{MongoSessionStore, SessionRetention};
pub use signals::{MongoSignalStore, SignalRetention};

use std::{collections::BTreeSet, sync::Arc, time::Instant};

use futures_util::TryStreamExt;
use metric_domain::{
    DisplayName, DsnKey, IpScrubPolicy, ItemCapabilities, OrganizationId, OrganizationIdentity,
    ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits, ProjectKeyIdentity,
    ProjectKeyLabel, ProjectKeyState, ProjectSnapshot, ScrubPolicy, SecretBytes, Slug, Timestamp,
    api::{ProjectKeyView, ProjectPolicyUpdate, ProjectView},
    inbound_filter::{
        InboundFilterField, InboundFilterOperation, InboundFilterPolicy, InboundFilterRule,
        InboundFilterSignal,
    },
};
use metric_ports::{PortFuture, ProjectStore, ProjectStoreError};
use mongodb::{
    Client, Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::{Error as MongoError, ErrorKind, WriteFailure},
    options::IndexOptions,
};
use thiserror::Error;

pub const SCHEMA_GENERATION: i32 = 17;
const SCHEMA_ID: &str = "metric.schema";
const SCHEMA_MODULES: [&str; 20] = [
    "project_identity_v1",
    "event_storage_v1",
    "issue_storage_v1",
    "finalization_storage_v1",
    "identity_authorization_v1",
    "project_deletion_v1",
    "native_debug_files_v1",
    "javascript_artifact_bundles_v1",
    "notification_outbox_webhooks_v1",
    "event_cold_archive_v1",
    "structured_logs_v1",
    "spans_virtual_traces_v1",
    "performance_insights_v1",
    "signal_cold_archive_v1",
    "signal_inbound_filters_v1",
    "releases_deploys_v1",
    "sessions_release_health_v1",
    "user_feedback_v1",
    "saved_queries_dashboards_v1",
    "cron_monitoring_v1",
];
const REQUIRED_COLLECTIONS: [&str; 36] = [
    "api_tokens",
    "alert_rules",
    "audit_log",
    "artifact_bundles",
    "artifact_uploads",
    "archive_manifests",
    "environments",
    "error_events",
    "feedback",
    "debug_files",
    "debug_uploads",
    "dashboards",
    "deploys",
    "issue_activities",
    "issues",
    "issue_stats_hourly",
    "logs",
    "monitor_runs",
    "monitors",
    "notification_deliveries",
    "notification_destinations",
    "organization_memberships",
    "organizations",
    "password_setup_tokens",
    "project_deletions",
    "project_keys",
    "projects",
    "releases",
    "saved_queries",
    "schema_meta",
    "session_stats_hourly",
    "sessions",
    "span_stats_hourly",
    "spans",
    "users",
    "web_sessions",
];

#[derive(Debug, Error)]
pub enum MongoBootstrapError {
    #[error("MongoDB operation failed")]
    Mongo(#[source] MongoError),
    #[error("configured database has no recognized schema marker")]
    MissingSchemaMarker,
    #[error("configured database schema is incomplete or incompatible")]
    IncompatibleSchema,
    #[error("configured database contains data but no Metric schema")]
    NonEmptyUnmanagedDatabase,
}

impl From<MongoError> for MongoBootstrapError {
    fn from(error: MongoError) -> Self {
        Self::Mongo(error)
    }
}

#[derive(Clone)]
pub struct MongoProjectStore {
    database: Database,
    scrub_hmac_key: Arc<SecretBytes>,
    max_keys_per_project: usize,
}

impl MongoProjectStore {
    pub async fn connect(
        uri: &str,
        database_name: &str,
        scrub_hmac_key: SecretBytes,
        max_keys_per_project: usize,
    ) -> Result<Self, MongoBootstrapError> {
        let client = Client::with_uri_str(uri).await?;
        let database = client.database(database_name);
        database.run_command(doc! { "ping": 1 }).await?;
        Ok(Self::from_database(
            database,
            scrub_hmac_key,
            max_keys_per_project,
        ))
    }

    #[must_use]
    pub fn from_database(
        database: Database,
        scrub_hmac_key: SecretBytes,
        max_keys_per_project: usize,
    ) -> Self {
        Self {
            database,
            scrub_hmac_key: Arc::new(scrub_hmac_key),
            max_keys_per_project,
        }
    }

    #[must_use]
    pub fn event_store(&self, codec: EventCodecConfig) -> MongoEventStore {
        MongoEventStore::from_database(self.database.clone(), codec)
    }

    #[must_use]
    pub fn issue_store(&self, codec: IssueCodecConfig) -> MongoIssueStore {
        MongoIssueStore::from_database(self.database.clone(), codec)
    }

    #[must_use]
    pub fn finalization_store(
        &self,
        event_codec: EventCodecConfig,
        issue_codec: IssueCodecConfig,
    ) -> MongoFinalizationStore {
        MongoFinalizationStore::from_database(self.database.clone(), event_codec, issue_codec)
    }

    #[must_use]
    pub fn auth_store(&self) -> MongoAuthStore {
        MongoAuthStore::from_database(self.database.clone())
    }

    #[must_use]
    pub fn investigation_store(
        &self,
        event_codec: EventCodecConfig,
        issue_codec: IssueCodecConfig,
    ) -> MongoInvestigationStore {
        MongoInvestigationStore::from_database(self.database.clone(), event_codec, issue_codec)
    }

    #[must_use]
    pub fn release_store(&self, issue_codec: IssueCodecConfig) -> MongoReleaseStore {
        MongoReleaseStore::from_database(self.database.clone(), issue_codec)
    }

    #[must_use]
    pub fn maintenance_store(&self) -> MongoMaintenanceStore {
        MongoMaintenanceStore::from_database(self.database.clone())
    }

    #[must_use]
    pub fn notification_store(&self) -> MongoNotificationStore {
        MongoNotificationStore::from_database(self.database.clone())
    }

    #[must_use]
    pub fn archive_store(&self, codec: EventCodecConfig) -> MongoArchiveStore {
        MongoArchiveStore::from_database(self.database.clone(), codec)
    }

    #[must_use]
    pub fn debug_file_store(&self, quota: DebugFileQuota) -> MongoDebugFileStore {
        MongoDebugFileStore::from_database(self.database.clone(), quota)
    }

    #[must_use]
    pub fn artifact_store(&self, quota: ArtifactQuota) -> MongoArtifactStore {
        MongoArtifactStore::from_database(self.database.clone(), quota)
    }

    #[must_use]
    pub fn signal_store(&self) -> MongoSignalStore {
        MongoSignalStore::from_database(self.database.clone())
    }

    #[must_use]
    pub fn signal_store_with_retention(&self, retention: SignalRetention) -> MongoSignalStore {
        MongoSignalStore::with_retention(self.database.clone(), retention)
    }

    #[must_use]
    pub fn session_store(&self, retention: SessionRetention) -> MongoSessionStore {
        MongoSessionStore::from_database(self.database.clone(), retention)
    }

    #[must_use]
    pub fn feedback_store(&self) -> MongoFeedbackStore {
        MongoFeedbackStore::new(self.database.clone())
    }

    #[must_use]
    pub fn monitor_store(&self, retention: MonitorRetention) -> MongoMonitorStore {
        MongoMonitorStore::new(self.database.clone(), retention)
    }

    #[must_use]
    pub fn explore_store(&self) -> MongoExploreStore {
        MongoExploreStore::new(self.database.clone())
    }

    #[must_use]
    pub fn dashboard_store(&self) -> MongoDashboardStore {
        MongoDashboardStore::new(self.database.clone())
    }

    pub async fn bootstrap_or_validate(&self) -> Result<(), MongoBootstrapError> {
        let mut names = self.database.list_collection_names().await?;
        names.sort();
        if names.is_empty() {
            self.bootstrap_empty().await
        } else {
            self.validate_existing(&names).await
        }
    }

    async fn bootstrap_empty(&self) -> Result<(), MongoBootstrapError> {
        self.database
            .run_command(doc! { "create": "schema_meta" })
            .await?;
        self.database
            .collection::<Document>("schema_meta")
            .insert_one(doc! {
                "_id": SCHEMA_ID,
                "generation": SCHEMA_GENERATION,
                "state": "bootstrapping",
                "modules": SCHEMA_MODULES.to_vec(),
            })
            .await?;

        self.create_collections().await?;
        self.create_indexes().await?;
        self.database
            .collection::<Document>("schema_meta")
            .update_one(
                doc! { "_id": SCHEMA_ID, "state": "bootstrapping" },
                doc! { "$set": { "state": "complete" } },
            )
            .await?;
        self.validate_existing(&REQUIRED_COLLECTIONS.map(str::to_owned))
            .await
    }

    async fn create_collections(&self) -> Result<(), MongoBootstrapError> {
        self.create_validated_collection("organizations", organization_validator())
            .await?;
        self.create_validated_collection("projects", project_validator())
            .await?;
        self.create_validated_collection("project_keys", project_key_validator())
            .await?;
        self.create_validated_collection("project_deletions", deletion::deletion_validator())
            .await?;
        self.create_validated_collection("debug_uploads", debug_files::debug_upload_validator())
            .await?;
        self.create_validated_collection("debug_files", debug_files::debug_file_validator())
            .await?;
        self.create_validated_collection(
            "artifact_uploads",
            artifacts::artifact_upload_validator(),
        )
        .await?;
        self.create_validated_collection(
            "artifact_bundles",
            artifacts::artifact_bundle_validator(),
        )
        .await?;
        self.create_validated_collection(
            "archive_manifests",
            archive::archive_manifest_validator(),
        )
        .await?;
        self.create_validated_collection("error_events", event::event_validator())
            .await?;
        self.create_validated_collection("feedback", feedback::feedback_validator())
            .await?;
        self.create_validated_collection("monitors", monitors::monitor_validator())
            .await?;
        self.create_validated_collection("monitor_runs", monitors::monitor_run_validator())
            .await?;
        self.create_validated_collection("saved_queries", dashboards::saved_query_validator())
            .await?;
        self.create_validated_collection("dashboards", dashboards::dashboard_validator())
            .await?;
        self.create_validated_collection("logs", signals::log_validator())
            .await?;
        self.create_validated_collection("spans", signals::span_validator())
            .await?;
        self.create_validated_collection("span_stats_hourly", signals::span_stats_validator())
            .await?;
        self.create_validated_collection("sessions", sessions::session_validator())
            .await?;
        self.create_validated_collection(
            "session_stats_hourly",
            sessions::session_stats_validator(),
        )
        .await?;
        self.create_validated_collection("issues", issue::issue_validator())
            .await?;
        self.create_validated_collection("issue_activities", issue::issue_activity_validator())
            .await?;
        self.create_validated_collection("issue_stats_hourly", finalizer::hourly_validator())
            .await?;
        self.create_validated_collection(
            "notification_destinations",
            notifications::destination_validator(),
        )
        .await?;
        self.create_validated_collection("alert_rules", notifications::rule_validator())
            .await?;
        self.create_validated_collection(
            "notification_deliveries",
            notifications::delivery_validator(),
        )
        .await?;
        self.create_validated_collection("releases", finalizer::release_validator())
            .await?;
        self.create_validated_collection("deploys", releases::deploy_validator())
            .await?;
        self.create_validated_collection("environments", finalizer::environment_validator())
            .await?;
        auth::create_auth_collections(&self.database).await
    }

    async fn create_validated_collection(
        &self,
        name: &str,
        validator: Document,
    ) -> Result<(), MongoBootstrapError> {
        self.database
            .run_command(doc! {
                "create": name,
                "validator": validator,
                "validationLevel": "strict",
                "validationAction": "error",
            })
            .await?;
        Ok(())
    }

    async fn create_indexes(&self) -> Result<(), MongoBootstrapError> {
        self.database
            .collection::<Document>("organizations")
            .create_index(index(doc! { "slug": 1 }, "organization_slug_unique", true))
            .await?;
        self.database
            .collection::<Document>("projects")
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "organization_id": 1, "slug": 1 })
                    .options(
                        IndexOptions::builder()
                            .name("project_organization_slug_unique".to_owned())
                            .unique(true)
                            .partial_filter_expression(doc! {
                                "state": { "$in": ["active", "disabled", "pending_delete", "purging"] }
                            })
                            .build(),
                    )
                    .build(),
            )
            .await?;
        self.database
            .collection::<Document>("project_keys")
            .create_index(index(
                doc! { "project_id": 1, "status": 1, "created_at": -1 },
                "project_key_administration",
                false,
            ))
            .await?;
        for model in deletion::deletion_indexes() {
            self.database
                .collection::<Document>("project_deletions")
                .create_index(model)
                .await?;
        }
        debug_files::create_debug_file_indexes(&self.database).await?;
        artifacts::create_artifact_indexes(&self.database).await?;
        for model in archive::archive_indexes() {
            self.database
                .collection::<Document>("archive_manifests")
                .create_index(model)
                .await?;
        }
        event::create_event_indexes(&self.database).await?;
        signals::create_signal_indexes(&self.database).await?;
        sessions::create_session_indexes(&self.database).await?;
        for model in feedback::feedback_indexes() {
            self.database
                .collection::<Document>("feedback")
                .create_index(model)
                .await?;
        }
        for model in monitors::monitor_indexes() {
            self.database
                .collection::<Document>("monitors")
                .create_index(model)
                .await?;
        }
        for model in monitors::monitor_run_indexes() {
            self.database
                .collection::<Document>("monitor_runs")
                .create_index(model)
                .await?;
        }
        dashboards::create_dashboard_indexes(&self.database).await?;
        issue::create_issue_indexes(&self.database).await?;
        finalizer::create_finalization_indexes(&self.database).await?;
        releases::create_deploy_indexes(&self.database).await?;
        auth::create_auth_indexes(&self.database).await?;
        for collection in [
            "notification_destinations",
            "alert_rules",
            "notification_deliveries",
        ] {
            for model in notifications::notification_indexes(collection) {
                self.database
                    .collection::<Document>(collection)
                    .create_index(model)
                    .await?;
            }
        }
        Ok(())
    }

    async fn validate_existing(&self, names: &[String]) -> Result<(), MongoBootstrapError> {
        let actual = names.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let required = REQUIRED_COLLECTIONS.into_iter().collect::<BTreeSet<_>>();
        if !actual.contains("schema_meta") {
            return if actual.is_empty() {
                Err(MongoBootstrapError::MissingSchemaMarker)
            } else {
                Err(MongoBootstrapError::NonEmptyUnmanagedDatabase)
            };
        }
        if actual != required {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        let marker = self
            .database
            .collection::<Document>("schema_meta")
            .find_one(doc! { "_id": SCHEMA_ID })
            .await?
            .ok_or(MongoBootstrapError::MissingSchemaMarker)?;
        let compatible = marker.get_i32("generation") == Ok(SCHEMA_GENERATION)
            && marker.get_str("state") == Ok("complete")
            && marker.get_array("modules").is_ok_and(|modules| {
                modules.as_slice() == SCHEMA_MODULES.map(|module| Bson::String(module.to_owned()))
            });
        if !compatible {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        self.validate_index_names().await?;
        if !event::validate_event_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !issue::validate_issue_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !finalizer::validate_finalization_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !releases::validate_deploy_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !auth::validate_auth_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !deletion::validate_deletion_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !debug_files::validate_debug_file_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !artifacts::validate_artifact_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !archive::validate_archive_indexes(&self.database).await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        if !self.validate_project_slug_index().await? {
            return Err(MongoBootstrapError::IncompatibleSchema);
        }
        self.validate_collection_options().await
    }

    async fn validate_project_slug_index(&self) -> Result<bool, MongoBootstrapError> {
        let mut indexes = self
            .database
            .collection::<Document>("projects")
            .list_indexes()
            .await?;
        while let Some(model) = indexes.try_next().await? {
            let Some(options) = model.options else {
                continue;
            };
            if options.name.as_deref() == Some("project_organization_slug_unique") {
                return Ok(model.keys == doc! { "organization_id": 1, "slug": 1 }
                    && options.unique == Some(true)
                    && options.partial_filter_expression
                        == Some(doc! {
                            "state": { "$in": ["active", "disabled", "pending_delete", "purging"] }
                        }));
            }
        }
        Ok(false)
    }

    async fn validate_index_names(&self) -> Result<(), MongoBootstrapError> {
        for (collection, expected) in [
            (
                "organizations",
                BTreeSet::from(["_id_", "organization_slug_unique"]),
            ),
            (
                "projects",
                BTreeSet::from(["_id_", "project_organization_slug_unique"]),
            ),
            (
                "project_keys",
                BTreeSet::from(["_id_", "project_key_administration"]),
            ),
            ("project_deletions", deletion::deletion_index_names()),
            ("error_events", event::event_index_names()),
            ("feedback", feedback::feedback_index_names()),
            ("monitors", monitors::monitor_index_names()),
            ("monitor_runs", monitors::monitor_run_index_names()),
            (
                "saved_queries",
                BTreeSet::from([
                    "_id_",
                    "saved_queries_project_name_unique",
                    "saved_queries_project_updated",
                ]),
            ),
            (
                "dashboards",
                BTreeSet::from([
                    "_id_",
                    "dashboards_project_name_unique",
                    "dashboards_project_updated",
                ]),
            ),
            ("logs", signals::signal_index_names("logs")),
            ("spans", signals::signal_index_names("spans")),
            (
                "span_stats_hourly",
                signals::signal_index_names("span_stats_hourly"),
            ),
            ("issues", issue::issue_index_names()),
            ("issue_activities", issue::issue_activity_index_names()),
            (
                "issue_stats_hourly",
                finalizer::finalization_index_names("issue_stats_hourly"),
            ),
            ("releases", finalizer::finalization_index_names("releases")),
            ("deploys", releases::deploy_index_names()),
            (
                "environments",
                finalizer::finalization_index_names("environments"),
            ),
            ("users", auth::auth_index_names("users")),
            (
                "organization_memberships",
                auth::auth_index_names("organization_memberships"),
            ),
            ("web_sessions", auth::auth_index_names("web_sessions")),
            ("api_tokens", auth::auth_index_names("api_tokens")),
            (
                "password_setup_tokens",
                auth::auth_index_names("password_setup_tokens"),
            ),
            ("audit_log", auth::auth_index_names("audit_log")),
            (
                "notification_destinations",
                notifications::notification_index_names("notification_destinations"),
            ),
            (
                "alert_rules",
                notifications::notification_index_names("alert_rules"),
            ),
            (
                "notification_deliveries",
                notifications::notification_index_names("notification_deliveries"),
            ),
        ] {
            let names = self
                .database
                .collection::<Document>(collection)
                .list_index_names()
                .await?;
            let actual = names.iter().map(String::as_str).collect::<BTreeSet<_>>();
            if actual != expected {
                return Err(MongoBootstrapError::IncompatibleSchema);
            }
        }
        Ok(())
    }

    async fn validate_collection_options(&self) -> Result<(), MongoBootstrapError> {
        for (name, expected_validator) in [
            ("organizations", organization_validator()),
            ("projects", project_validator()),
            ("project_keys", project_key_validator()),
            ("project_deletions", deletion::deletion_validator()),
            ("debug_uploads", debug_files::debug_upload_validator()),
            ("debug_files", debug_files::debug_file_validator()),
            ("artifact_uploads", artifacts::artifact_upload_validator()),
            ("artifact_bundles", artifacts::artifact_bundle_validator()),
            ("error_events", event::event_validator()),
            ("feedback", feedback::feedback_validator()),
            ("monitors", monitors::monitor_validator()),
            ("monitor_runs", monitors::monitor_run_validator()),
            ("saved_queries", dashboards::saved_query_validator()),
            ("dashboards", dashboards::dashboard_validator()),
            ("logs", signals::log_validator()),
            ("spans", signals::span_validator()),
            ("span_stats_hourly", signals::span_stats_validator()),
            ("issues", issue::issue_validator()),
            ("issue_activities", issue::issue_activity_validator()),
            ("issue_stats_hourly", finalizer::hourly_validator()),
            ("releases", finalizer::release_validator()),
            ("deploys", releases::deploy_validator()),
            ("environments", finalizer::environment_validator()),
            ("users", auth::user_validator()),
            ("organization_memberships", auth::membership_validator()),
            ("web_sessions", auth::session_validator()),
            ("api_tokens", auth::api_token_validator()),
            ("password_setup_tokens", auth::setup_token_validator()),
            ("audit_log", auth::audit_validator()),
            (
                "notification_destinations",
                notifications::destination_validator(),
            ),
            ("alert_rules", notifications::rule_validator()),
            (
                "notification_deliveries",
                notifications::delivery_validator(),
            ),
        ] {
            let response = self
                .database
                .run_command(doc! {
                    "listCollections": 1,
                    "filter": { "name": name },
                    "nameOnly": false,
                })
                .await?;
            let first_batch = response
                .get_document("cursor")
                .and_then(|cursor| cursor.get_array("firstBatch"))
                .map_err(|_| MongoBootstrapError::IncompatibleSchema)?;
            let options = first_batch
                .first()
                .and_then(Bson::as_document)
                .and_then(|collection| collection.get_document("options").ok())
                .ok_or(MongoBootstrapError::IncompatibleSchema)?;
            let compatible = options.get_document("validator") == Ok(&expected_validator)
                && options.get_str("validationLevel") == Ok("strict")
                && options.get_str("validationAction") == Ok("error");
            if !compatible {
                return Err(MongoBootstrapError::IncompatibleSchema);
            }
        }
        Ok(())
    }

    async fn insert_organization_inner(
        &self,
        organization: OrganizationIdentity,
    ) -> Result<(), ProjectStoreError> {
        let document = doc! {
            "_id": i64::try_from(organization.id.get()).map_err(|_| ProjectStoreError::InvalidData)?,
            "slug": organization.slug.as_str(),
            "display_name": organization.display_name.as_str(),
            "created_at": date(organization.created_at),
        };
        self.database
            .collection::<Document>("organizations")
            .insert_one(document)
            .await
            .map(|_| ())
            .map_err(|error| classify_insert_error(&error, "organization_slug_unique"))
    }

    async fn insert_project_inner(
        &self,
        project: ProjectIdentity,
    ) -> Result<(), ProjectStoreError> {
        let organization_id = i64::try_from(project.organization_id.get())
            .map_err(|_| ProjectStoreError::InvalidData)?;
        let organization_exists = self
            .database
            .collection::<Document>("organizations")
            .find_one(doc! { "_id": organization_id })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .is_some();
        if !organization_exists {
            return Err(ProjectStoreError::NotFound);
        }
        let mut limits = doc! {
            "max_event_bytes": i32::try_from(project.limits.max_event_bytes.get())
                .map_err(|_| ProjectStoreError::InvalidData)?,
        };
        if let Some(rate) = project.limits.max_events_per_second {
            limits.insert("max_events_per_second", i64::from(rate.get()));
        }
        if let Some(burst) = project.limits.burst {
            limits.insert("burst", i64::from(burst.get()));
        }
        let document = doc! {
            "_id": project.id.get(),
            "organization_id": organization_id,
            "slug": project.slug.as_str(),
            "display_name": project.display_name.as_str(),
            "state": project_state_name(project.state),
            "policy": {
                "revision": i64::try_from(project.policy_revision).map_err(|_| ProjectStoreError::InvalidData)?,
                "ip": ip_policy_name(project.ip_policy),
                "inbound_filters": Bson::Array(Vec::new()),
            },
            "items": {
                "error": project.items.error,
                "client_report": project.items.client_report,
                "log": project.items.log,
                "transaction": project.items.transaction,
                "span": project.items.span,
                "feedback": project.items.feedback,
                "check_in": project.items.check_in,
            },
            "limits": limits,
            "grouping_revision": i64::try_from(project.grouping_revision).map_err(|_| ProjectStoreError::InvalidData)?,
            "created_at": date(project.created_at),
        };
        self.database
            .collection::<Document>("projects")
            .insert_one(document)
            .await
            .map(|_| ())
            .map_err(|error| classify_insert_error(&error, "project_organization_slug_unique"))
    }

    async fn insert_project_key_inner(
        &self,
        key: ProjectKeyIdentity,
    ) -> Result<(), ProjectStoreError> {
        let project_exists = self
            .database
            .collection::<Document>("projects")
            .find_one(doc! { "_id": key.project_id.get() })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .is_some();
        if !project_exists {
            return Err(ProjectStoreError::NotFound);
        }
        let existing = self
            .database
            .collection::<Document>("project_keys")
            .count_documents(doc! { "project_id": key.project_id.get() })
            .limit(u64::try_from(self.max_keys_per_project).unwrap_or(u64::MAX))
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        if existing >= u64::try_from(self.max_keys_per_project).unwrap_or(u64::MAX) {
            return Err(ProjectStoreError::TooManyKeys);
        }
        self.database
            .collection::<Document>("project_keys")
            .insert_one(doc! {
                "_id": key_binary(key.key),
                "project_id": key.project_id.get(),
                "status": key_state_name(key.state),
                "label": key.label.as_str(),
                "created_at": date(key.created_at),
            })
            .await
            .map(|_| ())
            .map_err(|error| {
                if duplicate_write(&error).is_some() {
                    ProjectStoreError::KeyCollision
                } else {
                    ProjectStoreError::Unavailable
                }
            })
    }

    async fn load_project_inner(&self, key: DsnKey) -> Result<ProjectSnapshot, ProjectStoreError> {
        let key_document = self
            .database
            .collection::<Document>("project_keys")
            .find_one(doc! { "_id": key_binary(key) })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .ok_or(ProjectStoreError::NotFound)?;
        let key_state = parse_key_state(
            key_document
                .get_str("status")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )?;
        if key_state != ProjectKeyState::Active {
            return Err(ProjectStoreError::NotFound);
        }
        let project_id = ProjectId::new(
            key_document
                .get_i32("project_id")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?;
        let project = self
            .database
            .collection::<Document>("projects")
            .find_one(doc! { "_id": project_id.get() })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .ok_or(ProjectStoreError::NotFound)?;
        decode_snapshot(project_id, key_state, &project, &self.scrub_hmac_key)
    }

    async fn set_key_state_inner(
        &self,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> Result<ProjectId, ProjectStoreError> {
        let collection = self.database.collection::<Document>("project_keys");
        let document = collection
            .find_one(doc! { "_id": key_binary(key) })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .ok_or(ProjectStoreError::NotFound)?;
        let project_id = ProjectId::new(
            document
                .get_i32("project_id")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?;
        collection
            .update_one(
                doc! { "_id": key_binary(key) },
                doc! { "$set": { "status": key_state_name(state) } },
            )
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        Ok(project_id)
    }

    async fn set_project_key_state_inner(
        &self,
        project_id: ProjectId,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> Result<(), ProjectStoreError> {
        let result = self
            .database
            .collection::<Document>("project_keys")
            .update_one(
                doc! { "_id": key_binary(key), "project_id": project_id.get() },
                doc! { "$set": { "status": key_state_name(state) } },
            )
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        if result.matched_count == 0 {
            return Err(ProjectStoreError::NotFound);
        }
        Ok(())
    }

    async fn set_project_acceptance_inner(
        &self,
        project_id: ProjectId,
        state: ProjectAcceptanceState,
    ) -> Result<Vec<DsnKey>, ProjectStoreError> {
        if matches!(
            state,
            ProjectAcceptanceState::Purging | ProjectAcceptanceState::Deleted
        ) {
            return Err(ProjectStoreError::InvalidData);
        }
        let projects = self.database.collection::<Document>("projects");
        if projects
            .find_one(doc! { "_id": project_id.get() })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .is_none()
        {
            return Err(ProjectStoreError::NotFound);
        }
        let keys_collection = self.database.collection::<Document>("project_keys");
        let mut cursor = keys_collection
            .find(doc! { "project_id": project_id.get() })
            .projection(doc! { "_id": 1 })
            .limit(i64::try_from(self.max_keys_per_project.saturating_add(1)).unwrap_or(i64::MAX))
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        let mut keys = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
        {
            let binary = document
                .get_binary_generic("_id")
                .map_err(|_| ProjectStoreError::InvalidData)?;
            keys.push(dsn_key_from_slice(binary)?);
        }
        if keys.len() > self.max_keys_per_project {
            return Err(ProjectStoreError::TooManyKeys);
        }

        if state == ProjectAcceptanceState::Active {
            keys_collection
                .update_many(
                    doc! { "project_id": project_id.get(), "status": "suspended_by_deletion" },
                    doc! { "$set": { "status": "active" } },
                )
                .await
                .map_err(|_| ProjectStoreError::Unavailable)?;
        }
        projects
            .update_one(
                doc! { "_id": project_id.get() },
                doc! { "$set": { "state": project_state_name(state) } },
            )
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        if state == ProjectAcceptanceState::PendingDelete {
            keys_collection
                .update_many(
                    doc! { "project_id": project_id.get(), "status": "active" },
                    doc! { "$set": { "status": "suspended_by_deletion" } },
                )
                .await
                .map_err(|_| ProjectStoreError::Unavailable)?;
        }
        Ok(keys)
    }

    async fn list_projects_inner(
        &self,
        organization_id: metric_domain::OrganizationId,
        limit: usize,
    ) -> Result<Vec<ProjectView>, ProjectStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(ProjectStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("projects")
            .find(doc! {
                "organization_id": i64::try_from(organization_id.get())
                    .map_err(|_| ProjectStoreError::InvalidData)?,
                "state": { "$ne": "deleted" },
            })
            .sort(doc! { "slug": 1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(100))
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        let mut projects = Vec::with_capacity(limit);
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
        {
            projects.push(decode_project_view(&document)?);
        }
        Ok(projects)
    }

    async fn load_project_by_id_inner(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectView, ProjectStoreError> {
        let document = self
            .database
            .collection::<Document>("projects")
            .find_one(doc! { "_id": project_id.get(), "state": { "$ne": "deleted" } })
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
            .ok_or(ProjectStoreError::NotFound)?;
        decode_project_view(&document)
    }

    async fn list_project_keys_inner(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<ProjectKeyView>, ProjectStoreError> {
        let mut cursor = self
            .database
            .collection::<Document>("project_keys")
            .find(doc! { "project_id": project_id.get() })
            .sort(doc! { "created_at": 1, "_id": 1 })
            .limit(i64::try_from(self.max_keys_per_project.saturating_add(1)).unwrap_or(i64::MAX))
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        let mut keys = Vec::new();
        while let Some(document) = cursor
            .try_next()
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?
        {
            keys.push(decode_project_key_view(&document)?);
        }
        if keys.len() > self.max_keys_per_project {
            return Err(ProjectStoreError::TooManyKeys);
        }
        Ok(keys)
    }

    async fn update_project_policy_inner(
        &self,
        project_id: ProjectId,
        update: ProjectPolicyUpdate,
    ) -> Result<(ProjectView, Vec<DsnKey>), ProjectStoreError> {
        if update.expected_revision == 0 {
            return Err(ProjectStoreError::InvalidData);
        }
        let mut limits = doc! {
            "max_event_bytes": i32::try_from(update.limits.max_event_bytes.get())
                .map_err(|_| ProjectStoreError::InvalidData)?,
        };
        if let Some(value) = update.limits.max_events_per_second {
            limits.insert("max_events_per_second", i64::from(value.get()));
        }
        if let Some(value) = update.limits.burst {
            limits.insert("burst", i64::from(value.get()));
        }
        let projects = self.database.collection::<Document>("projects");
        let document = projects
            .find_one_and_update(
                doc! {
                    "_id": project_id.get(),
                    "policy.revision": i64::try_from(update.expected_revision)
                        .map_err(|_| ProjectStoreError::InvalidData)?,
                },
                doc! { "$set": {
                    "policy": {
                        "revision": i64::try_from(update.expected_revision.saturating_add(1))
                            .map_err(|_| ProjectStoreError::InvalidData)?,
                        "ip": ip_policy_name(update.ip_policy),
                        "inbound_filters": encode_inbound_filter_policy(&update.inbound_filters),
                    },
                    "items": {
                        "error": update.items.error,
                        "client_report": update.items.client_report,
                        "log": update.items.log,
                        "transaction": update.items.transaction,
                        "span": update.items.span,
                        "feedback": update.items.feedback,
                        "check_in": update.items.check_in,
                    },
                    "limits": limits,
                }},
            )
            .return_document(mongodb::options::ReturnDocument::After)
            .await
            .map_err(|_| ProjectStoreError::Unavailable)?;
        let document = match document {
            Some(document) => document,
            None => {
                let exists = projects
                    .find_one(doc! { "_id": project_id.get() })
                    .projection(doc! { "_id": 1 })
                    .await
                    .map_err(|_| ProjectStoreError::Unavailable)?
                    .is_some();
                return Err(if exists {
                    ProjectStoreError::RevisionConflict
                } else {
                    ProjectStoreError::NotFound
                });
            }
        };
        let keys = self
            .list_project_keys_inner(project_id)
            .await?
            .into_iter()
            .map(|key| key.key)
            .collect();
        Ok((decode_project_view(&document)?, keys))
    }
}

impl ProjectStore for MongoProjectStore {
    fn insert_organization(
        &self,
        organization: OrganizationIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(self.insert_organization_inner(organization))
    }

    fn insert_project(
        &self,
        project: ProjectIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(self.insert_project_inner(project))
    }

    fn insert_project_key(
        &self,
        key: ProjectKeyIdentity,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(self.insert_project_key_inner(key))
    }

    fn load_project(
        &self,
        key: DsnKey,
    ) -> PortFuture<'_, Result<ProjectSnapshot, ProjectStoreError>> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.load_project_inner(key).await;
            let outcome = match result {
                Ok(_) => "ok",
                Err(ProjectStoreError::NotFound) => "not_found",
                Err(_) => "error",
            };
            metrics::histogram!(
                "metric_mongodb_operation_duration_seconds",
                "operation" => "project_lookup",
                "outcome" => outcome
            )
            .record(started.elapsed().as_secs_f64());
            if outcome == "error" {
                metrics::counter!(
                    "metric_mongodb_operation_errors_total",
                    "operation" => "project_lookup"
                )
                .increment(1);
            }
            result
        })
    }

    fn set_key_state(
        &self,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> PortFuture<'_, Result<ProjectId, ProjectStoreError>> {
        Box::pin(self.set_key_state_inner(key, state))
    }

    fn set_project_key_state(
        &self,
        project_id: ProjectId,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
        Box::pin(self.set_project_key_state_inner(project_id, key, state))
    }

    fn set_project_acceptance(
        &self,
        project_id: ProjectId,
        state: ProjectAcceptanceState,
    ) -> PortFuture<'_, Result<Vec<DsnKey>, ProjectStoreError>> {
        Box::pin(self.set_project_acceptance_inner(project_id, state))
    }

    fn list_projects(
        &self,
        organization_id: metric_domain::OrganizationId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ProjectView>, ProjectStoreError>> {
        Box::pin(self.list_projects_inner(organization_id, limit))
    }

    fn load_project_by_id(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<ProjectView, ProjectStoreError>> {
        Box::pin(self.load_project_by_id_inner(project_id))
    }

    fn list_project_keys(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<Vec<ProjectKeyView>, ProjectStoreError>> {
        Box::pin(self.list_project_keys_inner(project_id))
    }

    fn update_project_policy(
        &self,
        project_id: ProjectId,
        update: ProjectPolicyUpdate,
    ) -> PortFuture<'_, Result<(ProjectView, Vec<DsnKey>), ProjectStoreError>> {
        Box::pin(self.update_project_policy_inner(project_id, update))
    }
}

fn index(keys: Document, name: &str, unique: bool) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .unique(unique)
                .build(),
        )
        .build()
}

fn classify_insert_error(error: &MongoError, slug_index: &str) -> ProjectStoreError {
    match duplicate_write(error) {
        Some(message) if message.contains(slug_index) => match slug_index {
            "organization_slug_unique" => ProjectStoreError::OrganizationSlugExists,
            _ => ProjectStoreError::ProjectSlugExists,
        },
        Some(_) => ProjectStoreError::IdentityCollision,
        None => ProjectStoreError::Unavailable,
    }
}

fn duplicate_write(error: &MongoError) -> Option<&str> {
    match error.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(write)) if write.code == 11000 => {
            Some(&write.message)
        }
        _ => None,
    }
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn key_binary(key: DsnKey) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: key.as_bytes().to_vec(),
    }
}

fn dsn_key_from_slice(bytes: &[u8]) -> Result<DsnKey, ProjectStoreError> {
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| ProjectStoreError::InvalidData)?;
    Ok(DsnKey::from_bytes(bytes))
}

fn decode_snapshot(
    project_id: ProjectId,
    key_state: ProjectKeyState,
    project: &Document,
    hmac_key: &SecretBytes,
) -> Result<ProjectSnapshot, ProjectStoreError> {
    let state = parse_project_state(
        project
            .get_str("state")
            .map_err(|_| ProjectStoreError::InvalidData)?,
    )?;
    if state != ProjectAcceptanceState::Active {
        return Err(ProjectStoreError::NotFound);
    }
    let policy = project
        .get_document("policy")
        .map_err(|_| ProjectStoreError::InvalidData)?;
    let items = project
        .get_document("items")
        .map_err(|_| ProjectStoreError::InvalidData)?;
    let limits = project
        .get_document("limits")
        .map_err(|_| ProjectStoreError::InvalidData)?;
    let max_event_bytes = u32::try_from(
        limits
            .get_i32("max_event_bytes")
            .map_err(|_| ProjectStoreError::InvalidData)?,
    )
    .ok()
    .and_then(std::num::NonZeroU32::new)
    .ok_or(ProjectStoreError::InvalidData)?;
    let max_events_per_second = optional_positive_u32(limits, "max_events_per_second")?;
    let burst = optional_positive_u32(limits, "burst")?;
    Ok(ProjectSnapshot {
        project_id,
        organization_id: OrganizationId::new(
            project
                .get_i64("organization_id")
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                .or_else(|| {
                    project
                        .get_i32("organization_id")
                        .ok()
                        .and_then(|value| u64::try_from(value).ok())
                })
                .ok_or(ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?,
        state,
        key_state,
        scrub_policy: ScrubPolicy {
            revision: positive_i64(policy, "revision")?,
            ip_policy: parse_ip_policy(
                policy
                    .get_str("ip")
                    .map_err(|_| ProjectStoreError::InvalidData)?,
            )?,
            hmac_key: hmac_key.clone(),
        },
        items: ItemCapabilities {
            error: items
                .get_bool("error")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            client_report: items
                .get_bool("client_report")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            log: items
                .get_bool("log")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            transaction: items
                .get_bool("transaction")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            span: items
                .get_bool("span")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            feedback: items
                .get_bool("feedback")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            check_in: items
                .get_bool("check_in")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        },
        limits: ProjectIngestLimits {
            max_event_bytes,
            max_events_per_second,
            burst,
        },
        inbound_filters: Arc::new(
            decode_inbound_filter_policy(policy)?
                .compile()
                .map_err(|_| ProjectStoreError::InvalidData)?,
        ),
        grouping_revision: positive_i64(project, "grouping_revision")?,
    })
}

pub(crate) fn decode_project_view(document: &Document) -> Result<ProjectView, ProjectStoreError> {
    let project_id = ProjectId::new(
        document
            .get_i32("_id")
            .map_err(|_| ProjectStoreError::InvalidData)?,
    )
    .map_err(|_| ProjectStoreError::InvalidData)?;
    let organization_id = OrganizationId::new(
        u64::try_from(
            document
                .get_i64("organization_id")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?,
    )
    .map_err(|_| ProjectStoreError::InvalidData)?;
    let policy = document
        .get_document("policy")
        .map_err(|_| ProjectStoreError::InvalidData)?;
    let items = document
        .get_document("items")
        .map_err(|_| ProjectStoreError::InvalidData)?;
    let limits = document
        .get_document("limits")
        .map_err(|_| ProjectStoreError::InvalidData)?;
    Ok(ProjectView {
        id: project_id,
        organization_id,
        slug: Slug::new(
            document
                .get_str("slug")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?,
        display_name: DisplayName::new(
            document
                .get_str("display_name")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?,
        state: parse_project_state(
            document
                .get_str("state")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )?,
        policy_revision: positive_i64(policy, "revision")?,
        ip_policy: parse_ip_policy(
            policy
                .get_str("ip")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )?,
        items: ItemCapabilities {
            error: items
                .get_bool("error")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            client_report: items
                .get_bool("client_report")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            log: items
                .get_bool("log")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            transaction: items
                .get_bool("transaction")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            span: items
                .get_bool("span")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            feedback: items
                .get_bool("feedback")
                .map_err(|_| ProjectStoreError::InvalidData)?,
            check_in: items
                .get_bool("check_in")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        },
        limits: ProjectIngestLimits {
            max_event_bytes: u32::try_from(
                limits
                    .get_i32("max_event_bytes")
                    .map_err(|_| ProjectStoreError::InvalidData)?,
            )
            .ok()
            .and_then(std::num::NonZeroU32::new)
            .ok_or(ProjectStoreError::InvalidData)?,
            max_events_per_second: optional_positive_u32(limits, "max_events_per_second")?,
            burst: optional_positive_u32(limits, "burst")?,
        },
        inbound_filters: decode_inbound_filter_policy(policy)?,
        grouping_revision: positive_i64(document, "grouping_revision")?,
        created_at: timestamp(document, "created_at")?,
    })
}

fn decode_project_key_view(document: &Document) -> Result<ProjectKeyView, ProjectStoreError> {
    Ok(ProjectKeyView {
        key: dsn_key_from_slice(
            document
                .get_binary_generic("_id")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )?,
        project_id: ProjectId::new(
            document
                .get_i32("project_id")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?,
        state: parse_key_state(
            document
                .get_str("status")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )?,
        label: ProjectKeyLabel::new(
            document
                .get_str("label")
                .map_err(|_| ProjectStoreError::InvalidData)?,
        )
        .map_err(|_| ProjectStoreError::InvalidData)?,
        created_at: timestamp(document, "created_at")?,
    })
}

fn timestamp(document: &Document, field: &str) -> Result<Timestamp, ProjectStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| ProjectStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| ProjectStoreError::InvalidData)
}

fn optional_positive_u32(
    document: &Document,
    field: &str,
) -> Result<Option<std::num::NonZeroU32>, ProjectStoreError> {
    match document.get(field) {
        None => Ok(None),
        Some(Bson::Int32(value)) => u32::try_from(*value)
            .ok()
            .and_then(std::num::NonZeroU32::new)
            .map(Some)
            .ok_or(ProjectStoreError::InvalidData),
        Some(Bson::Int64(value)) => u32::try_from(*value)
            .ok()
            .and_then(std::num::NonZeroU32::new)
            .map(Some)
            .ok_or(ProjectStoreError::InvalidData),
        Some(_) => Err(ProjectStoreError::InvalidData),
    }
}

fn positive_i64(document: &Document, field: &str) -> Result<u64, ProjectStoreError> {
    let value = document
        .get_i64(field)
        .map_err(|_| ProjectStoreError::InvalidData)?;
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ProjectStoreError::InvalidData)
}

fn project_state_name(state: ProjectAcceptanceState) -> &'static str {
    match state {
        ProjectAcceptanceState::Active => "active",
        ProjectAcceptanceState::Disabled => "disabled",
        ProjectAcceptanceState::PendingDelete => "pending_delete",
        ProjectAcceptanceState::Purging => "purging",
        ProjectAcceptanceState::Deleted => "deleted",
    }
}

fn parse_project_state(value: &str) -> Result<ProjectAcceptanceState, ProjectStoreError> {
    match value {
        "active" => Ok(ProjectAcceptanceState::Active),
        "disabled" => Ok(ProjectAcceptanceState::Disabled),
        "pending_delete" => Ok(ProjectAcceptanceState::PendingDelete),
        "purging" => Ok(ProjectAcceptanceState::Purging),
        "deleted" => Ok(ProjectAcceptanceState::Deleted),
        _ => Err(ProjectStoreError::InvalidData),
    }
}

fn key_state_name(state: ProjectKeyState) -> &'static str {
    match state {
        ProjectKeyState::Active => "active",
        ProjectKeyState::Disabled => "disabled",
        ProjectKeyState::SuspendedByDeletion => "suspended_by_deletion",
    }
}

fn parse_key_state(value: &str) -> Result<ProjectKeyState, ProjectStoreError> {
    match value {
        "active" => Ok(ProjectKeyState::Active),
        "disabled" => Ok(ProjectKeyState::Disabled),
        "suspended_by_deletion" => Ok(ProjectKeyState::SuspendedByDeletion),
        _ => Err(ProjectStoreError::InvalidData),
    }
}

fn ip_policy_name(policy: IpScrubPolicy) -> &'static str {
    match policy {
        IpScrubPolicy::Hmac => "hmac",
        IpScrubPolicy::Keep => "keep",
        IpScrubPolicy::Remove => "remove",
        IpScrubPolicy::Truncate => "truncate",
    }
}

fn parse_ip_policy(value: &str) -> Result<IpScrubPolicy, ProjectStoreError> {
    match value {
        "hmac" => Ok(IpScrubPolicy::Hmac),
        "keep" => Ok(IpScrubPolicy::Keep),
        "remove" => Ok(IpScrubPolicy::Remove),
        "truncate" => Ok(IpScrubPolicy::Truncate),
        _ => Err(ProjectStoreError::InvalidData),
    }
}

fn encode_inbound_filter_policy(policy: &InboundFilterPolicy) -> Bson {
    Bson::Array(
        policy
            .rules()
            .iter()
            .map(|rule| {
                Bson::Document(doc! {
                    "signal": rule.signal.as_str(),
                    "field": rule.field.as_str(),
                    "operation": rule.operation.as_str(),
                    "pattern": rule.pattern.as_ref(),
                })
            })
            .collect(),
    )
}

fn decode_inbound_filter_policy(
    policy: &Document,
) -> Result<InboundFilterPolicy, ProjectStoreError> {
    let Some(filters) = policy.get("inbound_filters") else {
        return Ok(InboundFilterPolicy::default());
    };
    let filters = filters.as_array().ok_or(ProjectStoreError::InvalidData)?;
    let mut rules = Vec::with_capacity(filters.len());
    for filter in filters {
        let filter = filter.as_document().ok_or(ProjectStoreError::InvalidData)?;
        let signal = match filter
            .get_str("signal")
            .map_err(|_| ProjectStoreError::InvalidData)?
        {
            "error" => InboundFilterSignal::Error,
            "log" => InboundFilterSignal::Log,
            "transaction" => InboundFilterSignal::Transaction,
            "span" => InboundFilterSignal::Span,
            _ => return Err(ProjectStoreError::InvalidData),
        };
        let field = match filter
            .get_str("field")
            .map_err(|_| ProjectStoreError::InvalidData)?
        {
            "release" => InboundFilterField::Release,
            "environment" => InboundFilterField::Environment,
            "service" => InboundFilterField::Service,
            "message" => InboundFilterField::Message,
            "exception_type" => InboundFilterField::ExceptionType,
            "logger" => InboundFilterField::Logger,
            "request_host" => InboundFilterField::RequestHost,
            "request_path" => InboundFilterField::RequestPath,
            "severity" => InboundFilterField::Severity,
            "name" => InboundFilterField::Name,
            "operation" => InboundFilterField::Operation,
            "status" => InboundFilterField::Status,
            "duration" => InboundFilterField::Duration,
            _ => return Err(ProjectStoreError::InvalidData),
        };
        let operation = match filter
            .get_str("operation")
            .map_err(|_| ProjectStoreError::InvalidData)?
        {
            "exact" => InboundFilterOperation::Exact,
            "prefix" => InboundFilterOperation::Prefix,
            "suffix" => InboundFilterOperation::Suffix,
            "contains" => InboundFilterOperation::Contains,
            "glob" => InboundFilterOperation::Glob,
            _ => return Err(ProjectStoreError::InvalidData),
        };
        rules.push(InboundFilterRule {
            signal,
            field,
            operation,
            pattern: filter
                .get_str("pattern")
                .map_err(|_| ProjectStoreError::InvalidData)?
                .into(),
        });
    }
    InboundFilterPolicy::new(rules).map_err(|_| ProjectStoreError::InvalidData)
}

fn organization_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "slug", "display_name", "created_at"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "long", "minimum": 1 },
            "slug": { "bsonType": "string", "minLength": 1, "maxLength": 63 },
            "display_name": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
            "created_at": { "bsonType": "date" },
            "ab": { "bsonType": "long", "minimum": 0 },
            "ac": { "bsonType": "long", "minimum": 0 },
            "auth_lock": {
                "bsonType": "object",
                "required": ["operation_id", "expires_at"],
                "additionalProperties": false,
                "properties": {
                    "operation_id": { "bsonType": "long", "minimum": 1 },
                    "expires_at": { "bsonType": "date" },
                },
            },
        }
    }}
}

fn project_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "organization_id", "state", "created_at"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "int", "minimum": 1 },
            "organization_id": { "bsonType": "long", "minimum": 1 },
            "slug": { "bsonType": "string", "minLength": 1, "maxLength": 63 },
            "display_name": { "bsonType": "string", "minLength": 1, "maxLength": 128 },
            "state": { "enum": ["active", "disabled", "pending_delete", "purging", "deleted"] },
            "policy": {
                "bsonType": "object",
                "required": ["revision", "ip"],
                "additionalProperties": false,
                "properties": {
                    "revision": { "bsonType": "long", "minimum": 1 },
                    "ip": { "enum": ["hmac", "keep", "remove", "truncate"] },
                    "inbound_filters": {
                        "bsonType": "array",
                        "maxItems": 32,
                        "items": {
                            "bsonType": "object",
                            "required": ["signal", "field", "operation", "pattern"],
                            "additionalProperties": false,
                            "properties": {
                                "signal": { "enum": ["error", "log", "transaction", "span"] },
                                "field": { "enum": [
                                    "release", "environment", "service", "message",
                                    "exception_type", "logger", "request_host", "request_path",
                                    "severity", "name", "operation", "status", "duration"
                                ] },
                                "operation": { "enum": ["exact", "prefix", "suffix", "contains", "glob"] },
                                "pattern": { "bsonType": "string", "minLength": 1, "maxLength": 256 },
                            },
                        },
                    },
                },
            },
                    "items": {
                        "bsonType": "object",
                        "required": ["error", "client_report", "log", "transaction", "span", "feedback", "check_in"],
                        "additionalProperties": false,
                        "properties": {
                            "error": { "bsonType": "bool" },
                            "client_report": { "bsonType": "bool" },
                            "log": { "bsonType": "bool" },
                            "transaction": { "bsonType": "bool" },
                            "span": { "bsonType": "bool" },
                            "feedback": { "bsonType": "bool" },
                            "check_in": { "bsonType": "bool" },
                },
            },
            "limits": {
                "bsonType": "object",
                "required": ["max_event_bytes"],
                "additionalProperties": false,
                "properties": {
                    "max_event_bytes": { "bsonType": "int", "minimum": 1 },
                    "max_events_per_second": { "bsonType": ["int", "long"], "minimum": 1 },
                    "burst": { "bsonType": ["int", "long"], "minimum": 1 },
                },
            },
            "grouping_revision": { "bsonType": "long", "minimum": 1 },
            "dr": { "bsonType": "long", "minimum": 1 },
            "ar": { "bsonType": "long", "minimum": 1 },
            "db": { "bsonType": "long", "minimum": 0 },
            "dc": { "bsonType": "long", "minimum": 0 },
            "catalog_usage": {
                "bsonType": "object",
                "additionalProperties": false,
                "properties": {
                    "rd": { "bsonType": "date" },
                    "rc": { "bsonType": "int", "minimum": 1 },
                    "ec": { "bsonType": "int", "minimum": 1 },
                },
            },
            "created_at": { "bsonType": "date" },
            "deleted_at": { "bsonType": "date" },
            "deletion_operation_id": { "bsonType": "binData" },
            "slug_reserved_until": { "bsonType": "date" },
        },
        "oneOf": [
            {
                "required": ["slug", "display_name", "policy", "items", "limits", "grouping_revision"],
                "properties": {
                    "state": { "enum": ["active", "disabled", "pending_delete", "purging"] }
                }
            },
            {
                "required": ["deleted_at", "deletion_operation_id", "slug_reserved_until"],
                "properties": {
                    "state": { "enum": ["deleted"] }
                }
            }
        ]
    }}
}

fn project_key_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "project_id", "status", "label", "created_at"],
        "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" },
            "project_id": { "bsonType": "int", "minimum": 1 },
            "status": { "enum": ["active", "disabled", "suspended_by_deletion"] },
            "label": { "bsonType": "string", "minLength": 1, "maxLength": 64 },
            "created_at": { "bsonType": "date" },
            "disabled_at": { "bsonType": "date" },
            "last_used_at": { "bsonType": "date" },
        }
    }}
}
