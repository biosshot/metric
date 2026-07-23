//! Authorized native API query and command boundary.

use std::sync::Arc;

use faultkeep_domain::{
    DsnKey, EventId, EventKey, ProjectId, ProjectKeyLabel, ProjectKeyState, Timestamp,
    api::{
        ActivityAnchor, ApiTokenView, EnvironmentAnchor, EventAnchor, EventView, IssueListQuery,
        IssueStatBucket, ProjectKeyView, ProjectPolicyUpdate, ProjectView, ReleaseAnchor,
    },
    auth::{Actor, AuditAction, AuthContext, Permission, RequestCorrelationId},
    grouping::IssueId,
    issue::{
        ActorKind, ActorRef, IssueCommand, IssueCommandAction, IssueCommandResult, IssueSnapshot,
        IssueStatus,
    },
};
use faultkeep_ports::{Clock, InvestigationStore, InvestigationStoreError};
use thiserror::Error;

use crate::{
    auth::{AuthError, IdentityService},
    issues::{IssueService, IssueServiceError},
    projects::{CreateProject, CreatedProject, ProjectService, ProjectServiceError},
    search::{
        CursorKind, SearchError, SearchResultPage, SearchService, cursor_digest, decode_cursor,
        encode_cursor,
    },
};

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 100;
const DAY_MILLIS: i64 = 86_400_000;
const MAX_RANGE_MILLIS: i64 = 30 * DAY_MILLIS;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NativeApiError {
    #[error("request is invalid")]
    InvalidRequest,
    #[error("cursor is invalid")]
    InvalidCursor,
    #[error("credential is invalid")]
    InvalidCredentials,
    #[error("request is forbidden")]
    Forbidden,
    #[error("target does not exist")]
    NotFound,
    #[error("target conflicts with existing state")]
    Conflict,
    #[error("request is rate limited")]
    RateLimited,
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error("service is temporarily unavailable")]
    Unavailable,
}

impl NativeApiError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidCursor => "invalid_cursor",
            Self::InvalidCredentials => "invalid_credentials",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::Search(error) => error.code(),
            Self::Unavailable => "temporarily_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct EventListRequest<'a> {
    pub issue_id: Option<IssueId>,
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

pub struct NativeApiService {
    identity: Arc<IdentityService>,
    projects: Arc<ProjectService>,
    issues: Arc<IssueService>,
    investigation: Arc<dyn InvestigationStore>,
    search: Arc<SearchService>,
    clock: Arc<dyn Clock>,
}

impl NativeApiService {
    #[must_use]
    pub fn new(
        identity: Arc<IdentityService>,
        projects: Arc<ProjectService>,
        issues: Arc<IssueService>,
        investigation: Arc<dyn InvestigationStore>,
        search: Arc<SearchService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            identity,
            projects,
            issues,
            investigation,
            search,
            clock,
        }
    }

    pub async fn list_projects(
        &self,
        context: &AuthContext,
    ) -> Result<Vec<ProjectView>, NativeApiError> {
        require(context, Permission::ProjectRead)?;
        self.projects
            .list_projects(context.organization_id, MAX_PAGE)
            .await
            .map_err(map_project_error)
    }

    pub async fn create_project(
        &self,
        context: &AuthContext,
        mut command: CreateProject,
        request_id: RequestCorrelationId,
    ) -> Result<CreatedProject, NativeApiError> {
        require(context, Permission::OrganizationAdmin)?;
        command.organization_id = context.organization_id;
        let created = self
            .projects
            .create_project(command)
            .await
            .map_err(map_project_error)?;
        self.identity
            .record_project_audit(
                context,
                request_id,
                AuditAction::ProjectCreated,
                "project",
                created.project_id.get().to_string(),
            )
            .await
            .map_err(map_auth_error)?;
        Ok(created)
    }

    pub async fn project(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
    ) -> Result<ProjectView, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        self.projects
            .load_project_view(project_id)
            .await
            .map_err(map_project_error)
    }

    pub async fn project_keys(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
    ) -> Result<Vec<ProjectKeyView>, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        self.projects
            .list_project_keys(project_id)
            .await
            .map_err(map_project_error)
    }

    pub async fn create_project_key(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        label: ProjectKeyLabel,
        request_id: RequestCorrelationId,
    ) -> Result<DsnKey, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        let key = self
            .projects
            .create_project_key(project_id, label)
            .await
            .map_err(map_project_error)?;
        self.identity
            .record_project_audit(
                context,
                request_id,
                AuditAction::ProjectKeyCreated,
                "project_key",
                key.to_string(),
            )
            .await
            .map_err(map_auth_error)?;
        Ok(key)
    }

    pub async fn disable_project_key(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        key: DsnKey,
        request_id: RequestCorrelationId,
    ) -> Result<(), NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        self.projects
            .set_project_key_state(project_id, key, ProjectKeyState::Disabled)
            .await
            .map_err(map_project_error)?;
        self.identity
            .record_project_audit(
                context,
                request_id,
                AuditAction::ProjectKeyDisabled,
                "project_key",
                key.to_string(),
            )
            .await
            .map_err(map_auth_error)
    }

    pub async fn update_project_policy(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        update: ProjectPolicyUpdate,
        request_id: RequestCorrelationId,
    ) -> Result<ProjectView, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        let project = self
            .projects
            .update_project_policy(project_id, update)
            .await
            .map_err(map_project_error)?;
        self.identity
            .record_project_audit(
                context,
                request_id,
                AuditAction::ProjectPolicyChanged,
                "project",
                project_id.get().to_string(),
            )
            .await
            .map_err(map_auth_error)?;
        Ok(project)
    }

    pub async fn list_issues(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        status: Option<IssueStatus>,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<NativePage<IssueSnapshot>, NativeApiError> {
        self.authorize(context, project_id, Permission::IssueRead)
            .await?;
        let limit = page_size(limit)?;
        let normalized = format!("issues:status={}", status_name(status));
        let digest = cursor_digest(project_id, &normalized, CursorKind::Issue);
        let before = cursor
            .map(|value| decode_issue_anchor(value, digest))
            .transpose()?;
        let page = self
            .investigation
            .list_issues(
                project_id,
                IssueListQuery {
                    status,
                    before,
                    limit,
                },
            )
            .await
            .map_err(map_store_error)?;
        Ok(NativePage {
            next_cursor: page.next.map(|anchor| {
                encode_cursor(
                    CursorKind::Issue,
                    anchor.last_seen,
                    &anchor.issue_id.as_bytes(),
                    digest,
                )
            }),
            items: page.items,
        })
    }

    pub async fn issue(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        issue_id: IssueId,
    ) -> Result<IssueSnapshot, NativeApiError> {
        self.authorize(context, project_id, Permission::IssueRead)
            .await?;
        self.issues
            .load(project_id, issue_id)
            .await
            .map_err(map_issue_error)
    }

    pub async fn issue_command(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        issue_id: IssueId,
        idempotency_key: [u8; 16],
        action: IssueCommandAction,
    ) -> Result<IssueCommandResult, NativeApiError> {
        self.authorize(context, project_id, Permission::IssueWrite)
            .await?;
        if let IssueCommandAction::Assign(Some(assignee)) = action {
            self.identity
                .validate_issue_assignee(context, assignee)
                .await
                .map_err(map_auth_error)?;
        }
        self.issues
            .apply_command(IssueCommand {
                project_id,
                issue_id,
                idempotency_key,
                actor: actor_ref(context),
                at: self.clock.now(),
                action,
            })
            .await
            .map_err(map_issue_error)
    }

    pub async fn issue_statistics(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        issue_id: IssueId,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        limit: Option<usize>,
    ) -> Result<Vec<IssueStatBucket>, NativeApiError> {
        self.authorize(context, project_id, Permission::IssueRead)
            .await?;
        let (from, until) = time_range(self.clock.now(), from, until)?;
        self.investigation
            .issue_statistics(project_id, issue_id, from, until, page_size(limit)?)
            .await
            .map_err(map_store_error)
    }

    pub async fn issue_activity(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        issue_id: IssueId,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<NativePage<faultkeep_domain::api::IssueActivityView>, NativeApiError> {
        self.authorize(context, project_id, Permission::IssueRead)
            .await?;
        let digest = cursor_digest(
            project_id,
            &format!("activity:{issue_id}"),
            CursorKind::Activity,
        );
        let before = cursor
            .map(|value| decode_activity_anchor(value, digest))
            .transpose()?;
        let page = self
            .investigation
            .issue_activity(project_id, issue_id, before, page_size(limit)?)
            .await
            .map_err(map_store_error)?;
        Ok(NativePage {
            next_cursor: page.next.map(|anchor| {
                encode_cursor(
                    CursorKind::Activity,
                    anchor.at,
                    &anchor.id.as_bytes(),
                    digest,
                )
            }),
            items: page.items,
        })
    }

    pub async fn list_events(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        request: EventListRequest<'_>,
    ) -> Result<NativePage<EventView>, NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        let (from, until) = time_range(self.clock.now(), request.from, request.until)?;
        let normalized = format!(
            "events:issue={}:from={}:until={}",
            request
                .issue_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "all".to_owned()),
            from.unix_millis(),
            until.unix_millis()
        );
        let digest = cursor_digest(project_id, &normalized, CursorKind::Event);
        let before = request
            .cursor
            .map(|value| decode_event_anchor(value, digest))
            .transpose()?;
        let page = self
            .investigation
            .list_events(
                project_id,
                request.issue_id,
                from,
                until,
                before,
                page_size(request.limit)?,
            )
            .await
            .map_err(map_store_error)?;
        let next_cursor = page.next.map(|anchor| {
            encode_cursor(
                CursorKind::Event,
                anchor.occurred_at,
                &anchor.event_key.as_bytes(),
                digest,
            )
        });
        Ok(NativePage {
            items: page.items,
            next_cursor,
        })
    }

    pub async fn event(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        event_id: EventId,
    ) -> Result<EventView, NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        self.investigation
            .load_event(project_id, EventKey::new(project_id, event_id))
            .await
            .map_err(map_store_error)
    }

    pub async fn search(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        query: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<SearchResultPage, NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        self.search
            .search(project_id, query, cursor, limit)
            .await
            .map_err(NativeApiError::Search)
    }

    pub async fn releases(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<NativePage<faultkeep_domain::api::ReleaseView>, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        let digest = cursor_digest(project_id, "releases:newest", CursorKind::Release);
        let before = cursor
            .map(|value| decode_release_anchor(value, digest))
            .transpose()?;
        let page = self
            .investigation
            .list_releases(
                context.organization_id,
                project_id,
                before,
                page_size(limit)?,
            )
            .await
            .map_err(map_store_error)?;
        Ok(NativePage {
            next_cursor: page.next.map(|anchor| {
                encode_cursor(
                    CursorKind::Release,
                    anchor.last_seen,
                    &anchor.id.as_bytes(),
                    digest,
                )
            }),
            items: page.items,
        })
    }

    pub async fn environments(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<NativePage<faultkeep_domain::api::EnvironmentView>, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        let digest = cursor_digest(project_id, "environments:newest", CursorKind::Environment);
        let before = cursor
            .map(|value| decode_environment_anchor(value, digest))
            .transpose()?;
        let page = self
            .investigation
            .list_environments(project_id, before, page_size(limit)?)
            .await
            .map_err(map_store_error)?;
        Ok(NativePage {
            next_cursor: page.next.map(|anchor| {
                encode_cursor(
                    CursorKind::Environment,
                    anchor.last_seen,
                    &anchor.id.as_bytes(),
                    digest,
                )
            }),
            items: page.items,
        })
    }

    pub async fn api_tokens(
        &self,
        context: &AuthContext,
    ) -> Result<Vec<ApiTokenView>, NativeApiError> {
        self.identity
            .list_api_tokens(context, MAX_PAGE)
            .await
            .map_err(map_auth_error)
    }

    async fn authorize(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        permission: Permission,
    ) -> Result<(), NativeApiError> {
        self.identity
            .authorize_project(context, project_id, permission)
            .await
            .map_err(map_auth_error)
    }
}

fn require(context: &AuthContext, permission: Permission) -> Result<(), NativeApiError> {
    if context.permissions.contains(permission) {
        Ok(())
    } else {
        Err(NativeApiError::Forbidden)
    }
}

fn actor_ref(context: &AuthContext) -> ActorRef {
    let (kind, value) = match context.actor {
        Actor::WebSession | Actor::Bootstrap => (ActorKind::User, context.user_id.get()),
        Actor::PersonalApiToken => (ActorKind::ApiCredential, context.credential_id.get()),
    };
    let mut id = [0_u8; 16];
    id[8..].copy_from_slice(&value.to_be_bytes());
    ActorRef::new(kind, id)
}

fn page_size(value: Option<usize>) -> Result<usize, NativeApiError> {
    let value = value.unwrap_or(DEFAULT_PAGE);
    if (1..=MAX_PAGE).contains(&value) {
        Ok(value)
    } else {
        Err(NativeApiError::InvalidRequest)
    }
}

fn time_range(
    now: Timestamp,
    from: Option<Timestamp>,
    until: Option<Timestamp>,
) -> Result<(Timestamp, Timestamp), NativeApiError> {
    let until = until.unwrap_or_else(|| {
        Timestamp::from_unix_millis(now.unix_millis().saturating_add(1)).unwrap_or(now)
    });
    let from = from.unwrap_or_else(|| {
        Timestamp::from_unix_millis(now.unix_millis().saturating_sub(DAY_MILLIS)).unwrap_or(now)
    });
    if from >= until || until.unix_millis().saturating_sub(from.unix_millis()) > MAX_RANGE_MILLIS {
        return Err(NativeApiError::InvalidRequest);
    }
    Ok((from, until))
}

fn status_name(status: Option<IssueStatus>) -> &'static str {
    match status {
        None => "all",
        Some(IssueStatus::Open) => "open",
        Some(IssueStatus::Resolved) => "resolved",
        Some(IssueStatus::Ignored) => "ignored",
    }
}

fn decode_issue_anchor(
    value: &str,
    digest: [u8; 16],
) -> Result<faultkeep_domain::api::IssueAnchor, NativeApiError> {
    let (last_seen, id) =
        decode_cursor(value, CursorKind::Issue, 16, digest).map_err(map_cursor_error)?;
    Ok(faultkeep_domain::api::IssueAnchor {
        last_seen,
        issue_id: IssueId::from_bytes(id.try_into().map_err(|_| NativeApiError::InvalidCursor)?),
    })
}

fn decode_event_anchor(value: &str, digest: [u8; 16]) -> Result<EventAnchor, NativeApiError> {
    let (occurred_at, id) =
        decode_cursor(value, CursorKind::Event, 20, digest).map_err(map_cursor_error)?;
    Ok(EventAnchor {
        occurred_at,
        event_key: EventKey::from_bytes(id.try_into().map_err(|_| NativeApiError::InvalidCursor)?)
            .map_err(|_| NativeApiError::InvalidCursor)?,
    })
}

fn decode_activity_anchor(value: &str, digest: [u8; 16]) -> Result<ActivityAnchor, NativeApiError> {
    let (at, id) =
        decode_cursor(value, CursorKind::Activity, 16, digest).map_err(map_cursor_error)?;
    Ok(ActivityAnchor {
        at,
        id: faultkeep_domain::issue::IssueActivityId::from_bytes(
            id.try_into().map_err(|_| NativeApiError::InvalidCursor)?,
        ),
    })
}

fn decode_release_anchor(value: &str, digest: [u8; 16]) -> Result<ReleaseAnchor, NativeApiError> {
    let (last_seen, id) =
        decode_cursor(value, CursorKind::Release, 16, digest).map_err(map_cursor_error)?;
    Ok(ReleaseAnchor {
        last_seen,
        id: faultkeep_domain::finalization::ReleaseId::from_bytes(
            id.try_into().map_err(|_| NativeApiError::InvalidCursor)?,
        ),
    })
}

fn decode_environment_anchor(
    value: &str,
    digest: [u8; 16],
) -> Result<EnvironmentAnchor, NativeApiError> {
    let (last_seen, id) =
        decode_cursor(value, CursorKind::Environment, 16, digest).map_err(map_cursor_error)?;
    Ok(EnvironmentAnchor {
        last_seen,
        id: faultkeep_domain::finalization::EnvironmentId::from_bytes(
            id.try_into().map_err(|_| NativeApiError::InvalidCursor)?,
        ),
    })
}

fn map_cursor_error(_: SearchError) -> NativeApiError {
    NativeApiError::InvalidCursor
}

fn map_store_error(error: InvestigationStoreError) -> NativeApiError {
    match error {
        InvestigationStoreError::NotFound => NativeApiError::NotFound,
        InvestigationStoreError::InvalidData | InvestigationStoreError::Unavailable => {
            NativeApiError::Unavailable
        }
    }
}

fn map_project_error(error: ProjectServiceError) -> NativeApiError {
    match error {
        ProjectServiceError::AlreadyExists => NativeApiError::Conflict,
        ProjectServiceError::NotFound => NativeApiError::NotFound,
        ProjectServiceError::InvalidConfiguration
        | ProjectServiceError::CollisionExhausted
        | ProjectServiceError::RandomUnavailable
        | ProjectServiceError::Unavailable => NativeApiError::Unavailable,
        ProjectServiceError::InvalidStateTransition => NativeApiError::InvalidRequest,
    }
}

fn map_issue_error(error: IssueServiceError) -> NativeApiError {
    match error {
        IssueServiceError::NotFound => NativeApiError::NotFound,
        IssueServiceError::InvalidGroupingIdentity | IssueServiceError::InvalidSummary => {
            NativeApiError::InvalidRequest
        }
        IssueServiceError::IdentityCollision => NativeApiError::Conflict,
        IssueServiceError::InvalidData | IssueServiceError::Unavailable => {
            NativeApiError::Unavailable
        }
    }
}

fn map_auth_error(error: AuthError) -> NativeApiError {
    match error {
        AuthError::Forbidden => NativeApiError::Forbidden,
        AuthError::InvalidCredentials | AuthError::InvalidCredential => {
            NativeApiError::InvalidCredentials
        }
        AuthError::RateLimited => NativeApiError::RateLimited,
        AuthError::AlreadyExists | AuthError::FinalOwner => NativeApiError::Conflict,
        AuthError::NotFound => NativeApiError::NotFound,
        AuthError::InvalidPassword | AuthError::InvalidTokenPolicy => {
            NativeApiError::InvalidRequest
        }
        _ => NativeApiError::Unavailable,
    }
}
