//! Explicit Release and Deploy control-plane orchestration.

use std::sync::Arc;

use metric_domain::{
    OrganizationId, ProjectId, Timestamp,
    auth::{AuthContext, Permission},
    finalization::{ReleaseId, derive_release_id},
    releases::{
        CreateDeploy, CreateRelease, DeployId, DeployRecord, FinalizeRelease, ReleaseIssueSummary,
        ReleaseRecord, RepositoryReference, derive_deploy_id, validate_version,
    },
};
use metric_ports::{Clock, ReleaseIssueKind, ReleaseStore, ReleaseStoreError};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReleaseError {
    #[error("release request is invalid")]
    InvalidRequest,
    #[error("release request is forbidden")]
    Forbidden,
    #[error("release target does not exist")]
    NotFound,
    #[error("release state conflicts with this request")]
    Conflict,
    #[error("release service is temporarily unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateDeployRequest {
    pub operation_id: [u8; 16],
    pub environment: Box<str>,
    pub name: Option<Box<str>>,
    pub url: Option<Box<str>>,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
}

pub struct ReleaseService {
    store: Arc<dyn ReleaseStore>,
    clock: Arc<dyn Clock>,
}

impl ReleaseService {
    #[must_use]
    pub fn new(store: Arc<dyn ReleaseStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    pub async fn resolve_cli_projects(
        &self,
        context: &AuthContext,
        organization_slug: &str,
        project_slugs: Vec<Box<str>>,
    ) -> Result<(OrganizationId, Vec<ProjectId>), ReleaseError> {
        require(context, Permission::ReleaseWrite)?;
        let (organization, projects) = self
            .store
            .resolve_projects(organization_slug.into(), project_slugs)
            .await
            .map_err(map_store)?;
        if organization != context.organization_id {
            return Err(ReleaseError::NotFound);
        }
        Ok((organization, projects))
    }

    pub async fn create(
        &self,
        context: &AuthContext,
        project_ids: Vec<ProjectId>,
        version: Box<str>,
        url: Option<Box<str>>,
        reference: Option<Box<str>>,
        repositories: Vec<RepositoryReference>,
    ) -> Result<ReleaseRecord, ReleaseError> {
        require(context, Permission::ReleaseWrite)?;
        self.store
            .create_release(CreateRelease {
                organization_id: context.organization_id,
                project_ids,
                version,
                url,
                reference,
                repositories,
                created_at: self.clock.now(),
            })
            .await
            .map_err(map_store)
    }

    pub async fn finalize(
        &self,
        context: &AuthContext,
        release_id: ReleaseId,
        released_at: Option<Timestamp>,
    ) -> Result<ReleaseRecord, ReleaseError> {
        require(context, Permission::ReleaseWrite)?;
        self.store
            .finalize_release(FinalizeRelease {
                organization_id: context.organization_id,
                release_id,
                released_at: released_at.unwrap_or_else(|| self.clock.now()),
            })
            .await
            .map_err(map_store)
    }

    pub async fn load(
        &self,
        context: &AuthContext,
        release_id: ReleaseId,
    ) -> Result<ReleaseRecord, ReleaseError> {
        require(context, Permission::ReleaseRead)?;
        self.store
            .load_release(context.organization_id, release_id)
            .await
            .map_err(map_store)
    }

    pub async fn load_version(
        &self,
        context: &AuthContext,
        version: &str,
    ) -> Result<ReleaseRecord, ReleaseError> {
        validate_version(version).map_err(|_| ReleaseError::InvalidRequest)?;
        self.load(context, derive_release_id(context.organization_id, version))
            .await
    }

    pub async fn create_deploy(
        &self,
        context: &AuthContext,
        release_id: ReleaseId,
        project_ids: Vec<ProjectId>,
        request: CreateDeployRequest,
    ) -> Result<DeployRecord, ReleaseError> {
        require(context, Permission::ReleaseWrite)?;
        let now = self.clock.now();
        self.store
            .create_deploy(CreateDeploy {
                deploy_id: derive_deploy_id(
                    context.organization_id,
                    release_id,
                    request.operation_id,
                ),
                organization_id: context.organization_id,
                release_id,
                project_ids,
                environment: request.environment,
                name: request.name,
                url: request.url,
                started_at: request.started_at.unwrap_or(now),
                finished_at: request.finished_at,
                created_at: now,
            })
            .await
            .map_err(map_store)
    }

    pub async fn finish_deploy(
        &self,
        context: &AuthContext,
        deploy_id: DeployId,
        finished_at: Option<Timestamp>,
    ) -> Result<DeployRecord, ReleaseError> {
        require(context, Permission::ReleaseWrite)?;
        self.store
            .finish_deploy(
                context.organization_id,
                deploy_id,
                finished_at.unwrap_or_else(|| self.clock.now()),
            )
            .await
            .map_err(map_store)
    }

    pub async fn deploys(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release_id: ReleaseId,
        limit: usize,
    ) -> Result<Vec<DeployRecord>, ReleaseError> {
        require(context, Permission::ReleaseRead)?;
        self.store
            .list_deploys(context.organization_id, project_id, release_id, None, limit)
            .await
            .map_err(map_store)
    }

    pub async fn issues(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release: Box<str>,
        kind: ReleaseIssueKind,
        limit: usize,
    ) -> Result<Vec<ReleaseIssueSummary>, ReleaseError> {
        require(context, Permission::ReleaseRead)?;
        self.store
            .list_release_issues(project_id, release, kind, None, limit)
            .await
            .map_err(map_store)
    }
}

fn require(context: &AuthContext, permission: Permission) -> Result<(), ReleaseError> {
    if context.permissions.contains(permission) {
        Ok(())
    } else {
        Err(ReleaseError::Forbidden)
    }
}

fn map_store(error: ReleaseStoreError) -> ReleaseError {
    match error {
        ReleaseStoreError::NotFound => ReleaseError::NotFound,
        ReleaseStoreError::Conflict => ReleaseError::Conflict,
        ReleaseStoreError::InvalidData => ReleaseError::InvalidRequest,
        ReleaseStoreError::Unavailable => ReleaseError::Unavailable,
    }
}
