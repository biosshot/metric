use futures_util::TryStreamExt;
use metric_domain::{
    EventId, OrganizationId, ProjectId, Timestamp,
    finalization::ReleaseId,
    grouping::IssueId,
    issue::{IssueRelease, IssueTitle},
    releases::{
        CreateDeploy, CreateRelease, DeployId, DeployRecord, FinalizeRelease, ReleaseIssueSummary,
        ReleaseRecord, RepositoryReference,
    },
};
use metric_ports::{PortFuture, ReleaseIssueKind, ReleaseStore, ReleaseStoreError};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    error::{Error as MongoError, ErrorKind, WriteFailure},
    options::{IndexOptions, ReturnDocument},
};

use crate::IssueCodecConfig;

const DUPLICATE_KEY_CODE: i32 = 11000;

#[derive(Clone)]
pub struct MongoReleaseStore {
    database: Database,
    issue_codec: IssueCodecConfig,
}

impl MongoReleaseStore {
    #[must_use]
    pub const fn from_database(database: Database, issue_codec: IssueCodecConfig) -> Self {
        Self {
            database,
            issue_codec,
        }
    }

    async fn resolve_projects_inner(
        &self,
        organization_slug: Box<str>,
        project_slugs: Vec<Box<str>>,
    ) -> Result<(OrganizationId, Vec<ProjectId>), ReleaseStoreError> {
        if project_slugs.is_empty() || project_slugs.len() > 256 {
            return Err(ReleaseStoreError::InvalidData);
        }
        let organization = self
            .database
            .collection::<Document>("organizations")
            .find_one(doc! { "slug": organization_slug.as_ref() })
            .projection(doc! { "_id": 1 })
            .await
            .map_err(unavailable)?
            .ok_or(ReleaseStoreError::NotFound)?;
        let organization_id = organization_id(
            organization
                .get_i64("_id")
                .map_err(|_| ReleaseStoreError::InvalidData)?,
        )?;
        let requested = project_slugs
            .iter()
            .map(|value| value.as_ref())
            .collect::<std::collections::BTreeSet<_>>();
        if requested.len() != project_slugs.len() {
            return Err(ReleaseStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("projects")
            .find(doc! {
                "organization_id": i64::try_from(organization_id.get()).map_err(|_| ReleaseStoreError::InvalidData)?,
                "slug": { "$in": project_slugs.iter().map(|value| value.as_ref()).collect::<Vec<_>>() },
                "state": { "$in": ["active", "disabled"] },
            })
            .projection(doc! { "_id": 1, "slug": 1 })
            .await
            .map_err(unavailable)?;
        let mut projects = Vec::with_capacity(project_slugs.len());
        let mut found = std::collections::BTreeSet::new();
        while let Some(project) = cursor.try_next().await.map_err(unavailable)? {
            let slug = project
                .get_str("slug")
                .map_err(|_| ReleaseStoreError::InvalidData)?;
            found.insert(slug.to_owned());
            projects.push(
                ProjectId::new(
                    project
                        .get_i32("_id")
                        .map_err(|_| ReleaseStoreError::InvalidData)?,
                )
                .map_err(|_| ReleaseStoreError::InvalidData)?,
            );
        }
        if found.len() != requested.len() {
            return Err(ReleaseStoreError::NotFound);
        }
        projects.sort_unstable_by_key(|project| project.get());
        Ok((organization_id, projects))
    }

    async fn create_release_inner(
        &self,
        command: CreateRelease,
    ) -> Result<ReleaseRecord, ReleaseStoreError> {
        let release_id = command
            .validate()
            .map_err(|_| ReleaseStoreError::InvalidData)?;
        let projects = command
            .project_ids
            .iter()
            .map(|project| project.get())
            .collect::<Vec<_>>();
        let mut set = doc! {
            "organization_id": { "$ifNull": ["$organization_id", i64::try_from(command.organization_id.get()).map_err(|_| ReleaseStoreError::InvalidData)?] },
            "version": { "$ifNull": ["$version", command.version.as_ref()] },
            "status": { "$ifNull": ["$status", "open"] },
            "project_ids": { "$setUnion": [{ "$ifNull": ["$project_ids", []] }, projects] },
            "created_at": { "$ifNull": ["$created_at", date(command.created_at)] },
            "activity_at": { "$max": [{ "$ifNull": ["$activity_at", date(command.created_at)] }, date(command.created_at)] },
            "source": { "$ifNull": ["$source", "api"] },
            "explicit": true,
        };
        if let Some(value) = command.url.as_ref() {
            set.insert("url", value.as_ref());
        }
        if let Some(value) = command.reference.as_ref() {
            set.insert("ref", value.as_ref());
        }
        if !command.repositories.is_empty() {
            set.insert("repositories", repositories_document(&command.repositories));
        }
        let result = self
            .database
            .collection::<Document>("releases")
            .find_one_and_update(
                doc! {
                    "_id": binary(release_id.as_bytes()),
                    "organization_id": i64::try_from(command.organization_id.get()).map_err(|_| ReleaseStoreError::InvalidData)?,
                    "version": command.version.as_ref(),
                },
                vec![doc! { "$set": set }],
            )
            .upsert(true)
            .return_document(ReturnDocument::After)
            .await;
        match result {
            Ok(Some(document)) => decode_release(&document),
            Ok(None) => Err(ReleaseStoreError::Unavailable),
            Err(error) if duplicate_write(&error) => Err(ReleaseStoreError::Conflict),
            Err(_) => Err(ReleaseStoreError::Unavailable),
        }
    }

    async fn finalize_release_inner(
        &self,
        command: FinalizeRelease,
    ) -> Result<ReleaseRecord, ReleaseStoreError> {
        let organization = i64::try_from(command.organization_id.get())
            .map_err(|_| ReleaseStoreError::InvalidData)?;
        let document = self
            .database
            .collection::<Document>("releases")
            .find_one_and_update(
                doc! {
                    "_id": binary(command.release_id.as_bytes()),
                    "organization_id": organization,
                },
                vec![doc! { "$set": {
                    "released_at": { "$ifNull": ["$released_at", date(command.released_at)] },
                    "activity_at": { "$max": [{ "$ifNull": ["$activity_at", date(command.released_at)] }, date(command.released_at)] },
                    "explicit": true,
                } }],
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(unavailable)?
            .ok_or(ReleaseStoreError::NotFound)?;
        decode_release(&document)
    }

    async fn load_release_inner(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
    ) -> Result<ReleaseRecord, ReleaseStoreError> {
        let document = self
            .database
            .collection::<Document>("releases")
            .find_one(doc! {
                "_id": binary(release_id.as_bytes()),
                "organization_id": i64::try_from(organization_id.get()).map_err(|_| ReleaseStoreError::InvalidData)?,
            })
            .await
            .map_err(unavailable)?
            .ok_or(ReleaseStoreError::NotFound)?;
        decode_release(&document)
    }

    async fn create_deploy_inner(
        &self,
        command: CreateDeploy,
    ) -> Result<DeployRecord, ReleaseStoreError> {
        command
            .validate()
            .map_err(|_| ReleaseStoreError::InvalidData)?;
        self.load_release_inner(command.organization_id, command.release_id)
            .await?;
        let document = deploy_document(&command)?;
        let collection = self.database.collection::<Document>("deploys");
        match collection.insert_one(document).await {
            Ok(_) => Ok(deploy_record(&command)),
            Err(error) if duplicate_write(&error) => {
                let existing = collection
                    .find_one(doc! { "_id": binary(command.deploy_id.as_bytes()) })
                    .await
                    .map_err(unavailable)?
                    .ok_or(ReleaseStoreError::Conflict)?;
                let decoded = decode_deploy(&existing)?;
                if decoded == deploy_record(&command) {
                    Ok(decoded)
                } else {
                    Err(ReleaseStoreError::Conflict)
                }
            }
            Err(_) => Err(ReleaseStoreError::Unavailable),
        }
    }

    async fn finish_deploy_inner(
        &self,
        organization_id: OrganizationId,
        deploy_id: DeployId,
        finished_at: Timestamp,
    ) -> Result<DeployRecord, ReleaseStoreError> {
        let organization =
            i64::try_from(organization_id.get()).map_err(|_| ReleaseStoreError::InvalidData)?;
        let collection = self.database.collection::<Document>("deploys");
        let existing = collection
            .find_one(doc! {
                "_id": binary(deploy_id.as_bytes()),
                "organization_id": organization,
            })
            .await
            .map_err(unavailable)?
            .ok_or(ReleaseStoreError::NotFound)?;
        let current = decode_deploy(&existing)?;
        if finished_at < current.started_at {
            return Err(ReleaseStoreError::InvalidData);
        }
        if let Some(stored) = current.finished_at {
            return if stored == finished_at {
                Ok(current)
            } else {
                Err(ReleaseStoreError::Conflict)
            };
        }
        let updated = collection
            .find_one_and_update(
                doc! {
                    "_id": binary(deploy_id.as_bytes()),
                    "organization_id": organization,
                    "finished_at": { "$exists": false },
                },
                doc! { "$set": { "finished_at": date(finished_at) } },
            )
            .return_document(ReturnDocument::After)
            .await
            .map_err(unavailable)?
            .ok_or(ReleaseStoreError::Conflict)?;
        decode_deploy(&updated)
    }

    async fn list_deploys_inner(
        &self,
        organization_id: OrganizationId,
        project_id: ProjectId,
        release_id: ReleaseId,
        before: Option<(Timestamp, DeployId)>,
        limit: usize,
    ) -> Result<Vec<DeployRecord>, ReleaseStoreError> {
        if limit == 0 || limit > 100 {
            return Err(ReleaseStoreError::InvalidData);
        }
        let mut filter = doc! {
            "organization_id": i64::try_from(organization_id.get()).map_err(|_| ReleaseStoreError::InvalidData)?,
            "project_ids": project_id.get(),
            "release_id": binary(release_id.as_bytes()),
        };
        if let Some((started, id)) = before {
            filter.insert(
                "$or",
                vec![
                    doc! { "started_at": { "$lt": date(started) } },
                    doc! { "started_at": date(started), "_id": { "$lt": binary(id.as_bytes()) } },
                ],
            );
        }
        let mut cursor = self
            .database
            .collection::<Document>("deploys")
            .find(filter)
            .sort(doc! { "started_at": -1, "_id": -1 })
            .limit(i64::try_from(limit).map_err(|_| ReleaseStoreError::InvalidData)?)
            .await
            .map_err(unavailable)?;
        let mut values = Vec::with_capacity(limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            values.push(decode_deploy(&document)?);
        }
        Ok(values)
    }

    async fn list_release_issues_inner(
        &self,
        project_id: ProjectId,
        release: Box<str>,
        kind: ReleaseIssueKind,
        before: Option<(Timestamp, IssueId)>,
        limit: usize,
    ) -> Result<Vec<ReleaseIssueSummary>, ReleaseStoreError> {
        if limit == 0 || limit > 100 {
            return Err(ReleaseStoreError::InvalidData);
        }
        let (release_field, time_field) = match kind {
            ReleaseIssueKind::New => ("fr", "f"),
            ReleaseIssueKind::Regressed => ("d.r", "d.t"),
        };
        let mut filter = doc! { "p": project_id.get(), release_field: release.as_ref() };
        if let Some((at, id)) = before {
            filter.insert(
                "$or",
                vec![
                    doc! { time_field: { "$lt": date(at) } },
                    doc! { time_field: date(at), "_id": { "$lt": binary(id.as_bytes()) } },
                ],
            );
        }
        let mut cursor = self
            .database
            .collection::<Document>("issues")
            .find(filter)
            .projection(doc! { "_id": 1, "t": 1, "f": 1, "l": 1, "fr": 1, "lr": 1, "m": 1 })
            .sort(doc! { time_field: -1, "_id": -1 })
            .limit(i64::try_from(limit).map_err(|_| ReleaseStoreError::InvalidData)?)
            .await
            .map_err(unavailable)?;
        let mut values = Vec::with_capacity(limit);
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            values.push(decode_issue_summary(&document, self.issue_codec)?);
        }
        Ok(values)
    }
}

impl ReleaseStore for MongoReleaseStore {
    fn resolve_projects(
        &self,
        organization_slug: Box<str>,
        project_slugs: Vec<Box<str>>,
    ) -> PortFuture<'_, Result<(OrganizationId, Vec<ProjectId>), ReleaseStoreError>> {
        Box::pin(self.resolve_projects_inner(organization_slug, project_slugs))
    }

    fn create_release(
        &self,
        command: CreateRelease,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
        Box::pin(self.create_release_inner(command))
    }

    fn finalize_release(
        &self,
        command: FinalizeRelease,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
        Box::pin(self.finalize_release_inner(command))
    }

    fn load_release(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
    ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
        Box::pin(self.load_release_inner(organization_id, release_id))
    }

    fn create_deploy(
        &self,
        command: CreateDeploy,
    ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>> {
        Box::pin(self.create_deploy_inner(command))
    }

    fn finish_deploy(
        &self,
        organization_id: OrganizationId,
        deploy_id: DeployId,
        finished_at: Timestamp,
    ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>> {
        Box::pin(self.finish_deploy_inner(organization_id, deploy_id, finished_at))
    }

    fn list_deploys(
        &self,
        organization_id: OrganizationId,
        project_id: ProjectId,
        release_id: ReleaseId,
        before: Option<(Timestamp, DeployId)>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DeployRecord>, ReleaseStoreError>> {
        Box::pin(self.list_deploys_inner(organization_id, project_id, release_id, before, limit))
    }

    fn list_release_issues(
        &self,
        project_id: ProjectId,
        release: Box<str>,
        kind: ReleaseIssueKind,
        before: Option<(Timestamp, IssueId)>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ReleaseIssueSummary>, ReleaseStoreError>> {
        Box::pin(self.list_release_issues_inner(project_id, release, kind, before, limit))
    }
}

fn decode_release(document: &Document) -> Result<ReleaseRecord, ReleaseStoreError> {
    let project_ids = document
        .get_array("project_ids")
        .map_err(|_| ReleaseStoreError::InvalidData)?
        .iter()
        .map(|value| match value {
            Bson::Int32(value) => {
                ProjectId::new(*value).map_err(|_| ReleaseStoreError::InvalidData)
            }
            _ => Err(ReleaseStoreError::InvalidData),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let repositories = match document.get("repositories") {
        None => Vec::new(),
        Some(Bson::Array(values)) => values
            .iter()
            .map(|value| match value {
                Bson::Document(value) => Ok(RepositoryReference {
                    repository: value
                        .get_str("repository")
                        .map_err(|_| ReleaseStoreError::InvalidData)?
                        .into(),
                    commit_from: optional_string(value, "commit_from")?,
                    commit_to: optional_string(value, "commit_to")?,
                }),
                _ => Err(ReleaseStoreError::InvalidData),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ReleaseStoreError::InvalidData),
    };
    Ok(ReleaseRecord {
        id: ReleaseId::from_bytes(fixed_binary::<16>(document, "_id")?),
        organization_id: organization_id(
            document
                .get_i64("organization_id")
                .map_err(|_| ReleaseStoreError::InvalidData)?,
        )?,
        version: document
            .get_str("version")
            .map_err(|_| ReleaseStoreError::InvalidData)?
            .into(),
        project_ids,
        created_at: timestamp(document, "created_at")?,
        activity_at: timestamp(document, "activity_at")?,
        released_at: optional_timestamp(document, "released_at")?,
        first_seen: optional_timestamp(document, "first_seen")?,
        last_seen: optional_timestamp(document, "last_seen")?,
        first_event_id: optional_fixed_binary::<20>(document, "first_event_id")?
            .map(|bytes| EventId::from_bytes(bytes[4..].try_into().expect("20-byte key tail"))),
        latest_event_id: optional_fixed_binary::<20>(document, "latest_event_id")?
            .map(|bytes| EventId::from_bytes(bytes[4..].try_into().expect("20-byte key tail"))),
        url: optional_string(document, "url")?,
        reference: optional_string(document, "ref")?,
        repositories,
        explicit: document.get_bool("explicit").unwrap_or(false),
    })
}

fn deploy_document(command: &CreateDeploy) -> Result<Document, ReleaseStoreError> {
    let mut document = doc! {
        "_id": binary(command.deploy_id.as_bytes()),
        "organization_id": i64::try_from(command.organization_id.get()).map_err(|_| ReleaseStoreError::InvalidData)?,
        "release_id": binary(command.release_id.as_bytes()),
        "project_ids": command.project_ids.iter().map(|project| project.get()).collect::<Vec<_>>(),
        "environment": command.environment.as_ref(),
        "started_at": date(command.started_at),
        "created_at": date(command.created_at),
    };
    if let Some(value) = command.name.as_ref() {
        document.insert("name", value.as_ref());
    }
    if let Some(value) = command.url.as_ref() {
        document.insert("url", value.as_ref());
    }
    if let Some(value) = command.finished_at {
        document.insert("finished_at", date(value));
    }
    Ok(document)
}

fn deploy_record(command: &CreateDeploy) -> DeployRecord {
    DeployRecord {
        id: command.deploy_id,
        release_id: command.release_id,
        project_ids: command.project_ids.clone(),
        environment: command.environment.clone(),
        name: command.name.clone(),
        url: command.url.clone(),
        started_at: command.started_at,
        finished_at: command.finished_at,
        created_at: command.created_at,
    }
}

fn decode_deploy(document: &Document) -> Result<DeployRecord, ReleaseStoreError> {
    Ok(DeployRecord {
        id: DeployId::from_bytes(fixed_binary::<16>(document, "_id")?),
        release_id: ReleaseId::from_bytes(fixed_binary::<16>(document, "release_id")?),
        project_ids: document
            .get_array("project_ids")
            .map_err(|_| ReleaseStoreError::InvalidData)?
            .iter()
            .map(|value| match value {
                Bson::Int32(value) => {
                    ProjectId::new(*value).map_err(|_| ReleaseStoreError::InvalidData)
                }
                _ => Err(ReleaseStoreError::InvalidData),
            })
            .collect::<Result<Vec<_>, _>>()?,
        environment: document
            .get_str("environment")
            .map_err(|_| ReleaseStoreError::InvalidData)?
            .into(),
        name: optional_string(document, "name")?,
        url: optional_string(document, "url")?,
        started_at: timestamp(document, "started_at")?,
        finished_at: optional_timestamp(document, "finished_at")?,
        created_at: timestamp(document, "created_at")?,
    })
}

fn decode_issue_summary(
    document: &Document,
    _codec: IssueCodecConfig,
) -> Result<ReleaseIssueSummary, ReleaseStoreError> {
    Ok(ReleaseIssueSummary {
        issue_id: IssueId::from_bytes(fixed_binary::<16>(document, "_id")?),
        title: IssueTitle::new(
            document
                .get_str("t")
                .map_err(|_| ReleaseStoreError::InvalidData)?,
        )
        .map_err(|_| ReleaseStoreError::InvalidData)?,
        first_seen: timestamp(document, "f")?,
        last_seen: timestamp(document, "l")?,
        first_release: optional_string(document, "fr")?
            .map(IssueRelease::new)
            .transpose()
            .map_err(|_| ReleaseStoreError::InvalidData)?,
        last_release: decode_last_release(document)?,
    })
}

fn decode_last_release(document: &Document) -> Result<Option<IssueRelease>, ReleaseStoreError> {
    if document.get_bool("m") == Ok(true) {
        return Ok(None);
    }
    optional_string(document, "lr")?
        .or(optional_string(document, "fr")?)
        .map(IssueRelease::new)
        .transpose()
        .map_err(|_| ReleaseStoreError::InvalidData)
}

fn repositories_document(repositories: &[RepositoryReference]) -> Bson {
    Bson::Array(
        repositories
            .iter()
            .map(|reference| {
                let mut document = doc! { "repository": reference.repository.as_ref() };
                if let Some(value) = reference.commit_from.as_ref() {
                    document.insert("commit_from", value.as_ref());
                }
                if let Some(value) = reference.commit_to.as_ref() {
                    document.insert("commit_to", value.as_ref());
                }
                Bson::Document(document)
            })
            .collect(),
    )
}

pub(crate) fn deploy_validator() -> Document {
    doc! { "$and": [
        { "$jsonSchema": {
            "bsonType": "object",
            "required": ["_id", "organization_id", "release_id", "project_ids", "environment", "started_at", "created_at"],
            "additionalProperties": false,
            "properties": {
                "_id": { "bsonType": "binData" },
                "organization_id": { "bsonType": "long", "minimum": 1 },
                "release_id": { "bsonType": "binData" },
                "project_ids": { "bsonType": "array", "minItems": 1, "maxItems": 256, "items": { "bsonType": "int", "minimum": 1 } },
                "environment": { "bsonType": "string", "minLength": 1 },
                "name": { "bsonType": "string", "minLength": 1 },
                "url": { "bsonType": "string", "minLength": 1 },
                "started_at": { "bsonType": "date" },
                "finished_at": { "bsonType": "date" },
                "created_at": { "bsonType": "date" },
            },
        } },
        { "$expr": { "$and": [
            { "$eq": [{ "$binarySize": "$_id" }, 16] },
            { "$eq": [{ "$binarySize": "$release_id" }, 16] },
            { "$lte": [{ "$strLenBytes": "$environment" }, 64] },
            { "$cond": [{ "$ne": [{ "$type": "$name" }, "missing"] }, { "$lte": [{ "$strLenBytes": "$name" }, 200] }, true] },
            { "$cond": [{ "$ne": [{ "$type": "$url" }, "missing"] }, { "$lte": [{ "$strLenBytes": "$url" }, 2048] }, true] },
            { "$cond": [{ "$ne": [{ "$type": "$finished_at" }, "missing"] }, { "$gte": ["$finished_at", "$started_at"] }, true] },
        ] } },
    ] }
}

pub(crate) fn deploy_index_names() -> std::collections::BTreeSet<&'static str> {
    std::collections::BTreeSet::from(["_id_", "deploy_project_release_timeline"])
}

pub(crate) async fn create_deploy_indexes(database: &Database) -> Result<(), MongoError> {
    database
        .collection::<Document>("deploys")
        .create_index(deploy_index())
        .await?;
    Ok(())
}

pub(crate) async fn validate_deploy_indexes(database: &Database) -> Result<bool, MongoError> {
    let mut cursor = database
        .collection::<Document>("deploys")
        .list_indexes()
        .await?;
    let mut found = false;
    while let Some(index) = cursor.try_next().await? {
        if index
            .options
            .as_ref()
            .and_then(|value| value.name.as_deref())
            == Some("deploy_project_release_timeline")
        {
            found = index.keys == deploy_index().keys;
        }
    }
    Ok(found)
}

fn deploy_index() -> IndexModel {
    IndexModel::builder()
        .keys(doc! {
            "organization_id": 1,
            "project_ids": 1,
            "release_id": 1,
            "started_at": -1,
            "_id": -1,
        })
        .options(
            IndexOptions::builder()
                .name("deploy_project_release_timeline".to_owned())
                .build(),
        )
        .build()
}

fn optional_string(document: &Document, name: &str) -> Result<Option<Box<str>>, ReleaseStoreError> {
    match document.get(name) {
        None => Ok(None),
        Some(Bson::String(value)) => Ok(Some(value.clone().into_boxed_str())),
        Some(_) => Err(ReleaseStoreError::InvalidData),
    }
}

fn timestamp(document: &Document, name: &str) -> Result<Timestamp, ReleaseStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(name)
            .map_err(|_| ReleaseStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| ReleaseStoreError::InvalidData)
}

fn optional_timestamp(
    document: &Document,
    name: &str,
) -> Result<Option<Timestamp>, ReleaseStoreError> {
    match document.get(name) {
        None => Ok(None),
        Some(Bson::DateTime(value)) => Timestamp::from_unix_millis(value.timestamp_millis())
            .map(Some)
            .map_err(|_| ReleaseStoreError::InvalidData),
        Some(_) => Err(ReleaseStoreError::InvalidData),
    }
}

fn fixed_binary<const N: usize>(
    document: &Document,
    name: &str,
) -> Result<[u8; N], ReleaseStoreError> {
    document
        .get_binary_generic(name)
        .map_err(|_| ReleaseStoreError::InvalidData)?
        .as_slice()
        .try_into()
        .map_err(|_| ReleaseStoreError::InvalidData)
}

fn optional_fixed_binary<const N: usize>(
    document: &Document,
    name: &str,
) -> Result<Option<[u8; N]>, ReleaseStoreError> {
    match document.get(name) {
        None => Ok(None),
        Some(Bson::Binary(value)) if value.subtype == BinarySubtype::Generic => value
            .bytes
            .as_slice()
            .try_into()
            .map(Some)
            .map_err(|_| ReleaseStoreError::InvalidData),
        Some(_) => Err(ReleaseStoreError::InvalidData),
    }
}

fn organization_id(value: i64) -> Result<OrganizationId, ReleaseStoreError> {
    OrganizationId::new(u64::try_from(value).map_err(|_| ReleaseStoreError::InvalidData)?)
        .map_err(|_| ReleaseStoreError::InvalidData)
}

fn date(value: Timestamp) -> DateTime {
    DateTime::from_millis(value.unix_millis())
}

fn binary<const N: usize>(bytes: [u8; N]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}

fn unavailable(_: MongoError) -> ReleaseStoreError {
    ReleaseStoreError::Unavailable
}

fn duplicate_write(error: &MongoError) -> bool {
    matches!(
        error.kind.as_ref(),
        ErrorKind::Write(WriteFailure::WriteError(write)) if write.code == DUPLICATE_KEY_CODE
    ) || matches!(
        error.kind.as_ref(),
        ErrorKind::Command(command) if command.code == DUPLICATE_KEY_CODE
    )
}
