//! Authorized native API query and command boundary.

use std::sync::Arc;

use metric_domain::{
    DsnKey, EventId, EventKey, ProjectId, ProjectKeyLabel, ProjectKeyState, Timestamp,
    api::{
        ActivityAnchor, ApiTokenView, EnvironmentAnchor, EventAnchor, EventView, IssueListQuery,
        IssueStatBucket, ProjectKeyView, ProjectPolicyUpdate, ProjectView, ReleaseAnchor,
    },
    auth::{Actor, AuditAction, AuthContext, Permission, RequestCorrelationId},
    blob::{BlobKey, BlobObjectId},
    deletion::{ProjectDeletionOperationId, ProjectDeletionStatus},
    grouping::IssueId,
    issue::{
        ActorKind, ActorRef, IssueCommand, IssueCommandAction, IssueCommandResult, IssueSnapshot,
        IssueStatus,
    },
    signals::{
        LogId, LogRecord, LogSeverity, PerformanceBucket, SignalCursor, SpanRecord, TraceId,
        TraceView,
    },
};
use metric_ports::{
    BlobReadSession, BlobStore, BlobStoreError, Clock, InvestigationStore, InvestigationStoreError,
    LogQuery, PerformanceQuery, SegmentQuery, SignalStore, SignalStoreError,
};
use thiserror::Error;

use crate::{
    auth::{AuthError, IdentityService},
    deletion::{ProjectDeletionError, ProjectDeletionService},
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

#[derive(Debug, Clone)]
pub struct LogListRequest<'a> {
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub severity: Option<LogSeverity>,
    pub message: Option<Box<str>>,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub trace_id: Option<TraceId>,
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct TransactionListRequest<'a> {
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct PerformanceListRequest {
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub environment: Option<Box<str>>,
    pub release: Option<Box<str>>,
    pub service: Option<Box<str>>,
    pub limit: Option<usize>,
}

pub struct NativeApiService {
    identity: Arc<IdentityService>,
    projects: Arc<ProjectService>,
    issues: Arc<IssueService>,
    investigation: Arc<dyn InvestigationStore>,
    search: Arc<SearchService>,
    clock: Arc<dyn Clock>,
    deletion: Option<Arc<ProjectDeletionService>>,
    blob_store: Option<Arc<dyn BlobStore>>,
    signal_store: Option<Arc<dyn SignalStore>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentView {
    pub attachment_id: BlobObjectId,
    pub blob_key: BlobKey,
    pub filename: Box<str>,
    pub content_type: Box<str>,
    pub attachment_type: Box<str>,
    pub size: u64,
    pub checksum: Box<str>,
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
            deletion: None,
            blob_store: None,
            signal_store: None,
        }
    }

    #[must_use]
    pub fn with_signal_store(mut self, signal_store: Arc<dyn SignalStore>) -> Self {
        self.signal_store = Some(signal_store);
        self
    }

    pub async fn list_logs(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        request: LogListRequest<'_>,
    ) -> Result<NativePage<LogRecord>, NativeApiError> {
        let LogListRequest {
            from,
            until,
            severity,
            message,
            environment,
            release,
            service,
            trace_id,
            cursor,
            limit,
        } = request;
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        let store = self.signal_store()?;
        let until = until.unwrap_or_else(|| self.clock.now());
        let from = from.unwrap_or_else(|| {
            Timestamp::from_unix_millis(until.unix_millis().saturating_sub(DAY_MILLIS))
                .expect("one-day subtraction remains in the timestamp range")
        });
        validate_time_range(from, until)?;
        let normalized = format!(
            "logs:{}:{}:{}:{}:{}:{}",
            severity.map_or("*", LogSeverity::as_str),
            message.as_deref().unwrap_or("*"),
            environment.as_deref().unwrap_or("*"),
            release.as_deref().unwrap_or("*"),
            service.as_deref().unwrap_or("*"),
            trace_id.map_or_else(|| "*".to_owned(), |value| value.to_string()),
        );
        let before = cursor
            .map(|value| decode_signal_cursor(value, project_id, &normalized, 6))
            .transpose()?;
        let page = store
            .list_logs(
                project_id,
                LogQuery {
                    from_ns: millis_to_ns(from)?,
                    until_ns: millis_to_ns(until)?,
                    severity,
                    message,
                    environment,
                    release,
                    service,
                    trace_id,
                    before,
                    limit: page_size(limit)?,
                },
            )
            .await
            .map_err(map_signal_error)?;
        Ok(NativePage {
            next_cursor: page
                .next
                .map(|cursor| encode_signal_cursor(cursor, project_id, &normalized, 6)),
            items: page.items,
        })
    }

    pub async fn log(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        log_id: LogId,
    ) -> Result<LogRecord, NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        self.signal_store()?
            .load_log(project_id, log_id)
            .await
            .map_err(map_signal_error)
    }

    pub async fn list_transactions(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        request: TransactionListRequest<'_>,
    ) -> Result<NativePage<SpanRecord>, NativeApiError> {
        let TransactionListRequest {
            from,
            until,
            environment,
            release,
            service,
            cursor,
            limit,
        } = request;
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        let until = until.unwrap_or_else(|| self.clock.now());
        let from = from.unwrap_or_else(|| {
            Timestamp::from_unix_millis(until.unix_millis().saturating_sub(DAY_MILLIS))
                .expect("one-day subtraction remains in the timestamp range")
        });
        validate_time_range(from, until)?;
        let normalized = format!(
            "transactions:{}:{}:{}",
            environment.as_deref().unwrap_or("*"),
            release.as_deref().unwrap_or("*"),
            service.as_deref().unwrap_or("*"),
        );
        let before = cursor
            .map(|value| decode_signal_cursor(value, project_id, &normalized, 7))
            .transpose()?;
        let page = self
            .signal_store()?
            .list_segments(
                project_id,
                SegmentQuery {
                    from_ns: millis_to_ns(from)?,
                    until_ns: millis_to_ns(until)?,
                    environment,
                    release,
                    service,
                    before,
                    limit: page_size(limit)?,
                },
            )
            .await
            .map_err(map_signal_error)?;
        Ok(NativePage {
            next_cursor: page
                .next
                .map(|cursor| encode_signal_cursor(cursor, project_id, &normalized, 7)),
            items: page.items,
        })
    }

    pub async fn trace(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        trace_id: TraceId,
    ) -> Result<TraceView, NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        self.signal_store()?
            .trace(vec![project_id], trace_id, 1_000, 250)
            .await
            .map_err(map_signal_error)
    }

    pub async fn performance(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        request: PerformanceListRequest,
    ) -> Result<Vec<PerformanceBucket>, NativeApiError> {
        let PerformanceListRequest {
            from,
            until,
            environment,
            release,
            service,
            limit,
        } = request;
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        let until = until.unwrap_or_else(|| self.clock.now());
        let from = from.unwrap_or_else(|| {
            Timestamp::from_unix_millis(until.unix_millis().saturating_sub(7 * DAY_MILLIS))
                .expect("seven-day subtraction remains in the timestamp range")
        });
        validate_time_range(from, until)?;
        self.signal_store()?
            .performance(
                project_id,
                PerformanceQuery {
                    from,
                    until,
                    environment,
                    release,
                    service,
                    limit: page_size(limit)?,
                },
            )
            .await
            .map_err(map_signal_error)
    }

    fn signal_store(&self) -> Result<&Arc<dyn SignalStore>, NativeApiError> {
        self.signal_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)
    }

    #[must_use]
    pub fn with_blob_store(mut self, blob_store: Arc<dyn BlobStore>) -> Self {
        self.blob_store = Some(blob_store);
        self
    }

    #[must_use]
    pub fn with_project_deletion(mut self, deletion: Arc<ProjectDeletionService>) -> Self {
        self.deletion = Some(deletion);
        self
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
        self.authorize_mutation(context, project_id, Permission::ProjectAdmin)
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
        self.authorize_mutation(context, project_id, Permission::ProjectAdmin)
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
        self.authorize_mutation(context, project_id, Permission::ProjectAdmin)
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
        self.authorize_mutation(context, project_id, Permission::IssueWrite)
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
    ) -> Result<NativePage<metric_domain::api::IssueActivityView>, NativeApiError> {
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

    pub async fn event_attachments(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        event_id: EventId,
    ) -> Result<Vec<AttachmentView>, NativeApiError> {
        let event = self.event(context, project_id, event_id).await?;
        let attachments = decode_attachments(event.payload.as_bytes())?;
        for attachment in &attachments {
            let (related_project, related_event, related_object) = attachment
                .blob_key
                .event_relation()
                .map_err(|_| NativeApiError::Unavailable)?;
            if related_project != project_id
                || related_event != event_id
                || related_object != attachment.attachment_id
            {
                return Err(NativeApiError::Unavailable);
            }
        }
        Ok(attachments)
    }

    pub async fn open_event_attachment(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        event_id: EventId,
        attachment_id: BlobObjectId,
    ) -> Result<(AttachmentView, Box<dyn BlobReadSession>), NativeApiError> {
        let attachment = self
            .event_attachments(context, project_id, event_id)
            .await?
            .into_iter()
            .find(|attachment| attachment.attachment_id == attachment_id)
            .ok_or(NativeApiError::NotFound)?;
        let store = self
            .blob_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?;
        let reader = store
            .open(&attachment.blob_key)
            .await
            .map_err(map_blob_error)?;
        Ok((attachment, reader))
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
    ) -> Result<NativePage<metric_domain::api::ReleaseView>, NativeApiError> {
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
    ) -> Result<NativePage<metric_domain::api::EnvironmentView>, NativeApiError> {
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

    pub async fn request_project_deletion(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        operation_id: ProjectDeletionOperationId,
        confirmation: &str,
        request_id: RequestCorrelationId,
    ) -> Result<ProjectDeletionStatus, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        let status = self
            .deletion
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .request(
                project_id,
                context.organization_id,
                context.user_id,
                operation_id,
                confirmation,
            )
            .await
            .map_err(map_deletion_error)?;
        self.identity
            .record_project_audit(
                context,
                request_id,
                AuditAction::ProjectDeletionRequested,
                "project_deletion",
                hex::encode(operation_id.as_bytes()),
            )
            .await
            .map_err(map_auth_error)?;
        Ok(status)
    }

    pub async fn cancel_project_deletion(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        operation_id: ProjectDeletionOperationId,
        request_id: RequestCorrelationId,
    ) -> Result<ProjectDeletionStatus, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        let status = self
            .deletion
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .cancel(project_id, operation_id)
            .await
            .map_err(map_deletion_error)?;
        self.identity
            .record_project_audit(
                context,
                request_id,
                AuditAction::ProjectDeletionCancelled,
                "project_deletion",
                hex::encode(operation_id.as_bytes()),
            )
            .await
            .map_err(map_auth_error)?;
        Ok(status)
    }

    pub async fn project_deletion_status(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
    ) -> Result<ProjectDeletionStatus, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        self.deletion
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .status(project_id)
            .await
            .map_err(map_deletion_error)
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

    async fn authorize_mutation(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        permission: Permission,
    ) -> Result<(), NativeApiError> {
        self.authorize(context, project_id, permission).await?;
        let project = self
            .projects
            .load_project_view(project_id)
            .await
            .map_err(map_project_error)?;
        if project.state == metric_domain::ProjectAcceptanceState::Active {
            Ok(())
        } else {
            Err(NativeApiError::Conflict)
        }
    }
}

fn decode_attachments(payload: &[u8]) -> Result<Vec<AttachmentView>, NativeApiError> {
    let event: serde_json::Value =
        serde_json::from_slice(payload).map_err(|_| NativeApiError::Unavailable)?;
    let values = event
        .get("attachments")
        .map(|values| values.as_array().ok_or(NativeApiError::Unavailable))
        .transpose()?
        .cloned()
        .unwrap_or_default();
    if values.len() > 100 {
        return Err(NativeApiError::Unavailable);
    }
    let mut attachments = values
        .iter()
        .map(|value| -> Result<AttachmentView, NativeApiError> {
            let object = value.as_object().ok_or(NativeApiError::Unavailable)?;
            let text = |name| {
                object
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .ok_or(NativeApiError::Unavailable)
            };
            let size = object
                .get("size")
                .and_then(serde_json::Value::as_u64)
                .ok_or(NativeApiError::Unavailable)?;
            Ok(AttachmentView {
                attachment_id: BlobObjectId::parse(text("attachment_id")?)
                    .map_err(|_| NativeApiError::Unavailable)?,
                blob_key: BlobKey::new(text("blob_key")?.to_owned())
                    .map_err(|_| NativeApiError::Unavailable)?,
                filename: text("filename")?.into(),
                content_type: text("content_type")?.into(),
                attachment_type: text("attachment_type")?.into(),
                size,
                checksum: text("checksum")?.into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(native) = event.get("native_crash") {
        let object = native.as_object().ok_or(NativeApiError::Unavailable)?;
        let text = |name| {
            object
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or(NativeApiError::Unavailable)
        };
        attachments.push(AttachmentView {
            attachment_id: BlobObjectId::parse(text("object_id")?)
                .map_err(|_| NativeApiError::Unavailable)?,
            blob_key: BlobKey::new(text("blob_key")?.to_owned())
                .map_err(|_| NativeApiError::Unavailable)?,
            filename: "minidump.dmp".into(),
            content_type: "application/octet-stream".into(),
            attachment_type: "event.minidump".into(),
            size: object
                .get("size")
                .and_then(serde_json::Value::as_u64)
                .ok_or(NativeApiError::Unavailable)?,
            checksum: text("checksum")?.into(),
        });
    }
    Ok(attachments)
}

fn map_blob_error(error: BlobStoreError) -> NativeApiError {
    match error {
        BlobStoreError::NotFound => NativeApiError::Unavailable,
        BlobStoreError::TooLarge
        | BlobStoreError::Capacity
        | BlobStoreError::Corrupt
        | BlobStoreError::Invalid
        | BlobStoreError::Unavailable => NativeApiError::Unavailable,
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

fn validate_time_range(from: Timestamp, until: Timestamp) -> Result<(), NativeApiError> {
    if from >= until || until.unix_millis().saturating_sub(from.unix_millis()) > MAX_RANGE_MILLIS {
        Err(NativeApiError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn millis_to_ns(value: Timestamp) -> Result<i64, NativeApiError> {
    value
        .unix_millis()
        .checked_mul(1_000_000)
        .ok_or(NativeApiError::InvalidRequest)
}

fn signal_cursor_digest(project_id: ProjectId, normalized: &str, kind: u8) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric/signal-api-cursor/v1");
    hasher.update(&[kind]);
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(normalized.as_bytes());
    hasher.finalize().as_bytes()[..16]
        .try_into()
        .expect("BLAKE3 digest prefix")
}

fn encode_signal_cursor(
    cursor: SignalCursor,
    project_id: ProjectId,
    normalized: &str,
    kind: u8,
) -> String {
    let mut bytes = Vec::with_capacity(42);
    bytes.extend_from_slice(&[1, kind]);
    bytes.extend_from_slice(&cursor.time_ns.to_be_bytes());
    bytes.extend_from_slice(&cursor.id);
    bytes.extend_from_slice(&signal_cursor_digest(project_id, normalized, kind));
    hex::encode(bytes)
}

fn decode_signal_cursor(
    value: &str,
    project_id: ProjectId,
    normalized: &str,
    kind: u8,
) -> Result<SignalCursor, NativeApiError> {
    let bytes = hex::decode(value).map_err(|_| NativeApiError::InvalidCursor)?;
    if bytes.len() != 42
        || bytes[..2] != [1, kind]
        || bytes[26..] != signal_cursor_digest(project_id, normalized, kind)
    {
        return Err(NativeApiError::InvalidCursor);
    }
    Ok(SignalCursor {
        time_ns: i64::from_be_bytes(
            bytes[2..10]
                .try_into()
                .map_err(|_| NativeApiError::InvalidCursor)?,
        ),
        id: bytes[10..26]
            .try_into()
            .map_err(|_| NativeApiError::InvalidCursor)?,
    })
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
) -> Result<metric_domain::api::IssueAnchor, NativeApiError> {
    let (last_seen, id) =
        decode_cursor(value, CursorKind::Issue, 16, digest).map_err(map_cursor_error)?;
    Ok(metric_domain::api::IssueAnchor {
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
        id: metric_domain::issue::IssueActivityId::from_bytes(
            id.try_into().map_err(|_| NativeApiError::InvalidCursor)?,
        ),
    })
}

fn decode_release_anchor(value: &str, digest: [u8; 16]) -> Result<ReleaseAnchor, NativeApiError> {
    let (last_seen, id) =
        decode_cursor(value, CursorKind::Release, 16, digest).map_err(map_cursor_error)?;
    Ok(ReleaseAnchor {
        last_seen,
        id: metric_domain::finalization::ReleaseId::from_bytes(
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
        id: metric_domain::finalization::EnvironmentId::from_bytes(
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

fn map_signal_error(error: SignalStoreError) -> NativeApiError {
    match error {
        SignalStoreError::NotFound => NativeApiError::NotFound,
        SignalStoreError::Conflict => NativeApiError::Conflict,
        SignalStoreError::InvalidData
        | SignalStoreError::Capacity
        | SignalStoreError::Unavailable => NativeApiError::Unavailable,
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

fn map_deletion_error(error: ProjectDeletionError) -> NativeApiError {
    match error {
        ProjectDeletionError::ConfirmationMismatch | ProjectDeletionError::InvalidConfiguration => {
            NativeApiError::InvalidRequest
        }
        ProjectDeletionError::Conflict | ProjectDeletionError::NotCancellable => {
            NativeApiError::Conflict
        }
        ProjectDeletionError::NotFound => NativeApiError::NotFound,
        ProjectDeletionError::Unavailable => NativeApiError::Unavailable,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_and_minidump_metadata_decode_without_blob_bytes() {
        let payload = br#"{
            "attachments":[{
                "attachment_id":"02020202020202020202020202020202",
                "blob_key":"projects/7/events/01010101010101010101010101010101/02020202020202020202020202020202",
                "filename":"context.json",
                "content_type":"application/json",
                "attachment_type":"event.attachment",
                "size":12,
                "checksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }],
            "native_crash":{
                "object_id":"03030303030303030303030303030303",
                "blob_key":"projects/7/events/01010101010101010101010101010101/03030303030303030303030303030303",
                "size":44,
                "checksum":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            }
        }"#;
        let decoded = decode_attachments(payload).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].filename.as_ref(), "context.json");
        assert_eq!(decoded[1].attachment_type.as_ref(), "event.minidump");
    }
}
