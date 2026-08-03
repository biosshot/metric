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
    dashboards::{
        Dashboard, DashboardId, DashboardRefresh, DashboardVariables, SavedQuery, SavedQueryId,
    },
    deletion::{ProjectDeletionOperationId, ProjectDeletionStatus},
    explore::{
        ExploreAggregate, ExploreAggregateKind, ExploreCursor, ExploreDataset, ExploreField,
        ExploreInterval, ExploreQuery, ExploreResult, ExploreValue,
    },
    feedback::{FeedbackAnchor, FeedbackRecord, FeedbackStatus},
    finalization::derive_environment_id,
    grouping::IssueId,
    issue::{
        ActorKind, ActorRef, IssueCommand, IssueCommandAction, IssueCommandResult,
        IssueSearchQuery, IssueSnapshot, IssueStatus,
    },
    monitors::{
        MonitorConfig, MonitorDefinition, MonitorId, MonitorPage, MonitorRun, MonitorRunAnchor,
        MonitorRunId, MonitorSchedule, UptimeMonitorConfig,
    },
    releases::{DeployRecord, ReleaseIssueSummary, ReleaseRecord, RepositoryReference},
    replays::{ReplayPage, ReplayRecord, ReplaySegment},
    signals::{
        LogId, LogRecord, LogSeverity, PerformanceBucket, SignalCursor, SpanRecord, TraceId,
        TraceView,
    },
};
use metric_ports::{
    BlobReadSession, BlobStore, BlobStoreError, Clock, FeedbackQuery, FeedbackStore,
    FeedbackStoreError, InvestigationStore, InvestigationStoreError, LogQuery, MonitorStore,
    PerformanceQuery, ReplayQuery, ReplayStore, SegmentQuery, SignalStore, SignalStoreError,
};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    auth::{AuthError, IdentityService},
    dashboards::{DashboardError, DashboardInput, DashboardService, SavedQueryInput},
    deletion::{ProjectDeletionError, ProjectDeletionService},
    explore::{ExploreError, ExploreService},
    issues::{IssueService, IssueServiceError},
    projects::{CreateProject, CreatedProject, ProjectService, ProjectServiceError},
    query::{
        DEFAULT_QUERY_ROWS, MAX_QUERY_ROWS, MAX_QUERY_VALUES, ParsedQuery, QueryError,
        QueryExpression, QueryField, QueryOperator, QueryPredicate, QuerySource,
        matches_expression,
    },
    releases::{CreateDeployRequest, ReleaseError, ReleaseService},
    search::{
        CursorKind, SearchError, SearchResultPage, SearchService, cursor_digest, decode_cursor,
        encode_cursor,
    },
};

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 100;
const MAX_MONITORS_PAGE: usize = 100_000;
const DAY_MILLIS: i64 = 86_400_000;
const MAX_RANGE_MILLIS: i64 = 30 * DAY_MILLIS;
// Logs and spans persist timestamps as signed nanoseconds, so the shared upper
// bound must remain representable after the Mongo adapter converts milliseconds.
const ALL_TIME_UNTIL_MILLIS: i64 = i64::MAX / 1_000_000;

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
    #[error("project is disabled")]
    ProjectDisabled,
    #[error("project deletion is pending")]
    ProjectDeletionPending,
    #[error("project purge is in progress")]
    ProjectPurging,
    #[error("project is deleted")]
    ProjectDeleted,
    #[error("request is rate limited")]
    RateLimited,
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    Explore(#[from] ExploreError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error(transparent)]
    Dashboard(#[from] DashboardError),
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
            Self::ProjectDisabled => "project_disabled",
            Self::ProjectDeletionPending => "project_deletion_pending",
            Self::ProjectPurging => "project_purging",
            Self::ProjectDeleted => "project_deleted",
            Self::RateLimited => "rate_limited",
            Self::Search(error) => error.code(),
            Self::Explore(error) => error.code(),
            Self::Query(error) => error.code(),
            Self::Dashboard(error) => error.code(),
            Self::Unavailable => "temporarily_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactCorrelations {
    pub replay_ids: Vec<EventId>,
    pub feedback_ids: Vec<EventId>,
}

#[derive(Debug, Clone, Copy)]
pub struct EventListRequest<'a> {
    pub issue_id: Option<IssueId>,
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct MonitorRunListRequest<'a> {
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct IssueListRequest<'a> {
    pub status: Option<IssueStatus>,
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

#[derive(Debug, Clone, Copy)]
pub struct FeedbackListRequest<'a> {
    pub status: Option<FeedbackStatus>,
    pub replay_id: Option<EventId>,
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnifiedQueryResultSpec {
    Records,
    Number {
        aggregates: Vec<ExploreAggregate>,
        group_by: Vec<ExploreField>,
    },
    Timeseries {
        aggregates: Vec<ExploreAggregate>,
        group_by: Vec<ExploreField>,
        interval: ExploreInterval,
    },
    Values {
        field: QueryField,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedQueryShape {
    Records,
    Number,
    Timeseries,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnifiedQueryRequest<'a> {
    pub source: QuerySource,
    pub text: &'a str,
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub result: UnifiedQueryResultSpec,
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnifiedQueryResult {
    Issues {
        page: NativePage<IssueSnapshot>,
        normalized: Box<str>,
        cost: u32,
    },
    Errors {
        page: SearchResultPage,
        normalized: Box<str>,
        cost: u32,
    },
    Rows {
        source: QuerySource,
        shape: UnifiedQueryShape,
        result: ExploreResult,
        normalized: Box<str>,
        cost: u32,
    },
    Replays {
        page: ReplayPage,
        next_cursor: Option<String>,
        normalized: Box<str>,
        cost: u32,
    },
    Feedback {
        page: NativePage<FeedbackRecord>,
        normalized: Box<str>,
        cost: u32,
    },
    Releases {
        page: NativePage<metric_domain::api::ReleaseView>,
        normalized: Box<str>,
        cost: u32,
    },
    Values {
        source: QuerySource,
        field: QueryField,
        items: Vec<Box<str>>,
        normalized: Box<str>,
        cost: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInput {
    pub slug: Box<str>,
    pub name: Box<str>,
    pub environment: Box<str>,
    pub enabled: bool,
    pub schedule_type: Box<str>,
    pub schedule: Box<str>,
    pub checkin_margin_seconds: u32,
    pub max_runtime_seconds: u32,
    pub uptime: Option<UptimeMonitorConfig>,
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
    session_store: Option<Arc<dyn metric_ports::SessionStore>>,
    releases: Option<Arc<ReleaseService>>,
    feedback_store: Option<Arc<dyn FeedbackStore>>,
    explore: Option<Arc<ExploreService>>,
    dashboards: Option<Arc<DashboardService>>,
    monitor_store: Option<Arc<dyn MonitorStore>>,
    replay_store: Option<Arc<dyn ReplayStore>>,
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
            session_store: None,
            releases: None,
            feedback_store: None,
            explore: None,
            dashboards: None,
            monitor_store: None,
            replay_store: None,
        }
    }

    #[must_use]
    pub fn with_signal_store(mut self, signal_store: Arc<dyn SignalStore>) -> Self {
        self.signal_store = Some(signal_store);
        self
    }

    #[must_use]
    pub fn with_session_store(
        mut self,
        session_store: Arc<dyn metric_ports::SessionStore>,
    ) -> Self {
        self.session_store = Some(session_store);
        self
    }

    #[must_use]
    pub fn with_release_service(mut self, releases: Arc<ReleaseService>) -> Self {
        self.releases = Some(releases);
        self
    }

    #[must_use]
    pub fn with_feedback_store(mut self, feedback_store: Arc<dyn FeedbackStore>) -> Self {
        self.feedback_store = Some(feedback_store);
        self
    }

    #[must_use]
    pub fn with_monitor_store(mut self, monitor_store: Arc<dyn MonitorStore>) -> Self {
        self.monitor_store = Some(monitor_store);
        self
    }

    #[must_use]
    pub fn with_replay_store(mut self, replay_store: Arc<dyn ReplayStore>) -> Self {
        self.replay_store = Some(replay_store);
        self
    }

    pub async fn unified_query(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        request: UnifiedQueryRequest<'_>,
    ) -> Result<UnifiedQueryResult, NativeApiError> {
        let parsed = ParsedQuery::parse(request.source, request.text)?;
        let limit = request.limit.unwrap_or(
            if matches!(&request.result, UnifiedQueryResultSpec::Values { .. }) {
                MAX_QUERY_VALUES
            } else {
                DEFAULT_QUERY_ROWS
            },
        );
        if !(1..=MAX_QUERY_ROWS).contains(&limit) {
            return Err(QueryError::LimitExceeded.into());
        }
        match &request.result {
            UnifiedQueryResultSpec::Values { .. }
                if limit > MAX_QUERY_VALUES
                    || !request.text.trim().is_empty()
                    || request.from.is_some()
                    || request.until.is_some()
                    || request.cursor.is_some() =>
            {
                return Err(QueryError::LimitExceeded.into());
            }
            UnifiedQueryResultSpec::Number { .. } | UnifiedQueryResultSpec::Timeseries { .. }
                if request.cursor.is_some() =>
            {
                return Err(QueryError::InvalidCursor.into());
            }
            _ => {}
        }
        let cost = query_cost(&parsed, limit)?;
        match request.result.clone() {
            UnifiedQueryResultSpec::Records => match request.source {
                QuerySource::Issues => {
                    self.authorize(context, project_id, Permission::IssueRead)
                        .await?;
                    let page = self
                        .query_issues(
                            project_id,
                            &parsed,
                            request.from,
                            request.until,
                            request.cursor,
                            limit,
                        )
                        .await?;
                    Ok(UnifiedQueryResult::Issues {
                        page,
                        normalized: parsed.normalized,
                        cost,
                    })
                }
                QuerySource::Errors => {
                    self.authorize(context, project_id, Permission::EventRead)
                        .await?;
                    let page = self
                        .search
                        .search(
                            project_id,
                            &parsed,
                            request.from,
                            request.until,
                            request.cursor,
                            Some(limit),
                        )
                        .await?;
                    Ok(UnifiedQueryResult::Errors {
                        page,
                        normalized: parsed.normalized,
                        cost,
                    })
                }
                QuerySource::Logs | QuerySource::Traces | QuerySource::Metrics => {
                    self.query_explore_records(context, project_id, parsed, request, limit)
                        .await
                }
                QuerySource::Replays => {
                    self.authorize(context, project_id, Permission::ProjectRead)
                        .await?;
                    let (page, next_cursor) = self
                        .query_replays(
                            project_id,
                            &parsed,
                            request.from,
                            request.until,
                            request.cursor,
                            limit,
                        )
                        .await?;
                    Ok(UnifiedQueryResult::Replays {
                        page,
                        next_cursor,
                        normalized: parsed.normalized,
                        cost,
                    })
                }
                QuerySource::Feedback => {
                    self.authorize(context, project_id, Permission::ProjectRead)
                        .await?;
                    let page = self
                        .query_feedback(
                            project_id,
                            &parsed,
                            request.from,
                            request.until,
                            request.cursor,
                            limit,
                        )
                        .await?;
                    Ok(UnifiedQueryResult::Feedback {
                        page,
                        normalized: parsed.normalized,
                        cost,
                    })
                }
                QuerySource::Releases => {
                    self.authorize(context, project_id, Permission::ReleaseRead)
                        .await?;
                    let page = self
                        .query_releases(
                            context.organization_id,
                            project_id,
                            &parsed,
                            request.from,
                            request.until,
                            request.cursor,
                            limit,
                        )
                        .await?;
                    Ok(UnifiedQueryResult::Releases {
                        page,
                        normalized: parsed.normalized,
                        cost,
                    })
                }
            },
            UnifiedQueryResultSpec::Number {
                aggregates,
                group_by,
            } => {
                self.query_explore_shape(
                    context, project_id, parsed, request, aggregates, group_by, None, limit,
                )
                .await
            }
            UnifiedQueryResultSpec::Timeseries {
                aggregates,
                group_by,
                interval,
            } => {
                self.query_explore_shape(
                    context,
                    project_id,
                    parsed,
                    request,
                    aggregates,
                    group_by,
                    Some(interval),
                    limit,
                )
                .await
            }
            UnifiedQueryResultSpec::Values { field } => {
                self.query_values(context, project_id, parsed, field, limit)
                    .await
            }
        }
    }

    async fn query_explore_records(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        mut parsed: ParsedQuery,
        request: UnifiedQueryRequest<'_>,
        limit: usize,
    ) -> Result<UnifiedQueryResult, NativeApiError> {
        if parsed.source == QuerySource::Traces {
            let segment = QueryExpression::Predicate(QueryPredicate {
                field: QueryField::IsSegment,
                operator: QueryOperator::Equal,
                value: "true".into(),
            });
            parsed.expression = Some(match parsed.expression.take() {
                Some(value) => QueryExpression::And(vec![value, segment]),
                None => segment,
            });
        }
        self.query_explore_shape(
            context,
            project_id,
            parsed,
            request,
            Vec::new(),
            Vec::new(),
            None,
            limit,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn query_explore_shape(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        parsed: ParsedQuery,
        request: UnifiedQueryRequest<'_>,
        aggregates: Vec<ExploreAggregate>,
        group_by: Vec<ExploreField>,
        interval: Option<ExploreInterval>,
        limit: usize,
    ) -> Result<UnifiedQueryResult, NativeApiError> {
        let dataset = query_dataset(parsed.source)?;
        let (from, until) =
            unified_time_range(self.clock.now(), request.from, request.until, parsed.source)?;
        let expression = parsed.explore_expression()?;
        let query = ExploreQuery {
            dataset,
            from,
            until,
            predicates: Vec::new(),
            expression,
            aggregates,
            group_by,
            interval,
            cursor: None,
            limit,
        };
        let shape = if query.aggregates.is_empty() {
            UnifiedQueryShape::Records
        } else if query.interval.is_some() {
            UnifiedQueryShape::Timeseries
        } else {
            UnifiedQueryShape::Number
        };
        let (result, normalized, cost) = self
            .explore(context, project_id, query, request.cursor)
            .await
            .map_err(map_unified_explore_error)?;
        Ok(UnifiedQueryResult::Rows {
            source: parsed.source,
            shape,
            result,
            normalized,
            cost,
        })
    }

    async fn query_issues(
        &self,
        project_id: ProjectId,
        parsed: &ParsedQuery,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<NativePage<IssueSnapshot>, NativeApiError> {
        validate_optional_time_range(from, until)?;
        if let Some(predicate) = positive_predicate(parsed.expression.as_ref(), QueryField::IssueId)
        {
            let issue_id = IssueId::from_bytes(hex_16_text(&predicate.value)?);
            let issue = self
                .issues
                .load(project_id, issue_id)
                .await
                .map_err(map_issue_error)?;
            let items = (in_time_range(issue.last_seen, from, until)
                && issue_matches(parsed.expression.as_ref(), &issue))
            .then_some(issue)
            .into_iter()
            .collect();
            return Ok(NativePage {
                items,
                next_cursor: None,
            });
        }
        if let Some(predicate) = positive_predicate(parsed.expression.as_ref(), QueryField::Title) {
            if cursor.is_some() {
                return Err(QueryError::InvalidCursor.into());
            }
            let candidates = self
                .issues
                .search_titles(
                    project_id,
                    IssueSearchQuery::new(predicate.value.clone(), limit.min(100))
                        .map_err(|_| QueryError::LimitExceeded)?,
                )
                .await
                .map_err(map_issue_error)?;
            let mut items = Vec::with_capacity(candidates.len());
            for candidate in candidates {
                let issue = self
                    .issues
                    .load(project_id, candidate.issue_id)
                    .await
                    .map_err(map_issue_error)?;
                if in_time_range(issue.last_seen, from, until)
                    && issue_matches(parsed.expression.as_ref(), &issue)
                {
                    items.push(issue);
                }
            }
            return Ok(NativePage {
                items,
                next_cursor: None,
            });
        }
        let normalized = format!(
            "{}|from:{}|until:{}",
            parsed.normalized,
            from.map_or(i64::MIN, Timestamp::unix_millis),
            until.map_or(i64::MAX, Timestamp::unix_millis)
        );
        let digest = cursor_digest(project_id, &normalized, CursorKind::Issue);
        let before = cursor
            .map(|value| decode_issue_anchor(value, digest))
            .transpose()?;
        let status = positive_predicate(parsed.expression.as_ref(), QueryField::Status)
            .map(|value| parse_issue_status(&value.value))
            .transpose()?;
        let page = self
            .investigation
            .list_issues(
                project_id,
                IssueListQuery {
                    status,
                    from,
                    until,
                    before,
                    limit: limit.min(100),
                },
            )
            .await
            .map_err(map_store_error)?;
        let next_cursor = page.next.map(|anchor| {
            encode_cursor(
                CursorKind::Issue,
                anchor.last_seen,
                &anchor.issue_id.as_bytes(),
                digest,
            )
        });
        Ok(NativePage {
            items: page
                .items
                .into_iter()
                .filter(|issue| issue_matches(parsed.expression.as_ref(), issue))
                .collect(),
            next_cursor,
        })
    }

    async fn query_replays(
        &self,
        project_id: ProjectId,
        parsed: &ParsedQuery,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<(ReplayPage, Option<String>), NativeApiError> {
        validate_optional_time_range(from, until)?;
        let normalized = format!(
            "{}|from:{}|until:{}",
            parsed.normalized,
            from.map_or(i64::MIN, Timestamp::unix_millis),
            until.map_or(i64::MAX, Timestamp::unix_millis)
        );
        let digest = cursor_digest(project_id, &normalized, CursorKind::Replay);
        let before = cursor
            .map(|value| decode_replay_anchor(value, digest))
            .transpose()?;
        let error_id = positive_predicate(parsed.expression.as_ref(), QueryField::EventId)
            .map(|value| EventId::parse(&value.value).map_err(|_| QueryError::Syntax))
            .transpose()?;
        let trace_id = positive_predicate(parsed.expression.as_ref(), QueryField::TraceId)
            .map(|value| TraceId::parse(&value.value).map_err(|_| QueryError::Syntax))
            .transpose()?;
        let mut page = self
            .replay_store()?
            .list_replays(
                project_id,
                ReplayQuery {
                    from,
                    until,
                    error_id,
                    trace_id,
                    before,
                    limit: limit.min(100),
                },
            )
            .await
            .map_err(map_signal_error)?;
        let next_cursor = page.next.map(|anchor| {
            encode_cursor(
                CursorKind::Replay,
                anchor.received_at,
                &anchor.replay_id.as_bytes(),
                digest,
            )
        });
        page.items
            .retain(|replay| replay_matches(parsed.expression.as_ref(), replay));
        Ok((page, next_cursor))
    }

    async fn query_feedback(
        &self,
        project_id: ProjectId,
        parsed: &ParsedQuery,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<NativePage<FeedbackRecord>, NativeApiError> {
        validate_optional_time_range(from, until)?;
        let normalized = format!(
            "{}|from:{}|until:{}",
            parsed.normalized,
            from.map_or(i64::MIN, Timestamp::unix_millis),
            until.map_or(i64::MAX, Timestamp::unix_millis)
        );
        let digest = cursor_digest(project_id, &normalized, CursorKind::Feedback);
        let before = cursor
            .map(|value| decode_feedback_anchor(value, digest))
            .transpose()?;
        let status = positive_predicate(parsed.expression.as_ref(), QueryField::Status)
            .map(|value| FeedbackStatus::parse(&value.value).map_err(|_| QueryError::Syntax))
            .transpose()?;
        let replay_id = positive_predicate(parsed.expression.as_ref(), QueryField::ReplayId)
            .map(|value| EventId::parse(&value.value).map_err(|_| QueryError::Syntax))
            .transpose()?;
        let event_id = positive_predicate(parsed.expression.as_ref(), QueryField::EventId)
            .map(|value| EventId::parse(&value.value).map_err(|_| QueryError::Syntax))
            .transpose()?;
        let trace_id = positive_predicate(parsed.expression.as_ref(), QueryField::TraceId)
            .map(|value| TraceId::parse(&value.value).map_err(|_| QueryError::Syntax))
            .transpose()?;
        let page = self
            .feedback_store()?
            .list_feedback(
                project_id,
                FeedbackQuery {
                    status,
                    event_id,
                    trace_id,
                    replay_id,
                    before,
                    limit: limit.min(100),
                },
            )
            .await
            .map_err(map_feedback_error)?;
        let next_cursor = page.next.map(|anchor| {
            encode_cursor(
                CursorKind::Feedback,
                anchor.received_at,
                &anchor.feedback_id.as_bytes(),
                digest,
            )
        });
        let mut items = page
            .items
            .into_iter()
            .filter(|feedback| {
                in_time_range(feedback.received_at, from, until)
                    && feedback_matches(parsed.expression.as_ref(), feedback)
            })
            .collect::<Vec<_>>();
        for feedback in &mut items {
            self.enrich_feedback(feedback).await?;
        }
        Ok(NativePage { items, next_cursor })
    }

    #[allow(clippy::too_many_arguments)]
    async fn query_releases(
        &self,
        organization_id: metric_domain::OrganizationId,
        project_id: ProjectId,
        parsed: &ParsedQuery,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<NativePage<metric_domain::api::ReleaseView>, NativeApiError> {
        validate_optional_time_range(from, until)?;
        let normalized = format!(
            "{}|from:{}|until:{}",
            parsed.normalized,
            from.map_or(i64::MIN, Timestamp::unix_millis),
            until.map_or(i64::MAX, Timestamp::unix_millis)
        );
        let digest = cursor_digest(project_id, &normalized, CursorKind::Release);
        let before = cursor
            .map(|value| decode_release_anchor(value, digest))
            .transpose()?;
        let page = self
            .investigation
            .list_releases(organization_id, project_id, before, limit.min(100))
            .await
            .map_err(map_store_error)?;
        let next_cursor = page.next.map(|anchor| {
            encode_cursor(
                CursorKind::Release,
                anchor.activity_at,
                &anchor.id.as_bytes(),
                digest,
            )
        });
        Ok(NativePage {
            items: page
                .items
                .into_iter()
                .filter(|release| {
                    in_time_range(release.activity_at, from, until)
                        && release_matches(parsed.expression.as_ref(), release)
                })
                .collect(),
            next_cursor,
        })
    }

    async fn query_values(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        parsed: ParsedQuery,
        field: QueryField,
        limit: usize,
    ) -> Result<UnifiedQueryResult, NativeApiError> {
        self.authorize(context, project_id, query_permission(parsed.source))
            .await?;
        let limit = limit.min(MAX_QUERY_VALUES);
        let items: Vec<Box<str>> = match field {
            QueryField::Level if parsed.source == QuerySource::Errors => {
                ["debug", "info", "warning", "error", "fatal"]
                    .into_iter()
                    .take(limit)
                    .map(Into::into)
                    .collect()
            }
            QueryField::Level if parsed.source == QuerySource::Logs => {
                ["trace", "debug", "info", "warning", "error", "fatal"]
                    .into_iter()
                    .take(limit)
                    .map(Into::into)
                    .collect()
            }
            QueryField::Status if parsed.source == QuerySource::Issues => {
                ["open", "resolved", "ignored"]
                    .into_iter()
                    .take(limit)
                    .map(Into::into)
                    .collect()
            }
            QueryField::Status if parsed.source == QuerySource::Feedback => {
                ["open", "resolved", "spam"]
                    .into_iter()
                    .take(limit)
                    .map(Into::into)
                    .collect()
            }
            QueryField::MetricKind if parsed.source == QuerySource::Metrics => {
                ["counter", "gauge", "distribution"]
                    .into_iter()
                    .take(limit)
                    .map(Into::into)
                    .collect()
            }
            QueryField::Environment => self
                .investigation
                .list_environments(project_id, None, limit)
                .await
                .map_err(map_store_error)?
                .items
                .into_iter()
                .map(|value| value.name)
                .collect(),
            QueryField::Release => self
                .investigation
                .list_releases(context.organization_id, project_id, None, limit)
                .await
                .map_err(map_store_error)?
                .items
                .into_iter()
                .map(|value| value.version)
                .collect(),
            QueryField::MetricName if parsed.source == QuerySource::Metrics => {
                let now = self.clock.now();
                let from =
                    Timestamp::from_unix_millis(now.unix_millis().saturating_sub(7 * DAY_MILLIS))
                        .map_err(|_| QueryError::Unavailable)?;
                let query = ExploreQuery {
                    dataset: ExploreDataset::Metrics,
                    from,
                    until: now,
                    predicates: Vec::new(),
                    expression: None,
                    aggregates: vec![ExploreAggregate {
                        kind: ExploreAggregateKind::Count,
                        field: None,
                        alias: "count".into(),
                    }],
                    group_by: vec![ExploreField::Name],
                    interval: None,
                    cursor: None,
                    limit,
                };
                self.explore(context, project_id, query, None)
                    .await
                    .map_err(map_unified_explore_error)?
                    .0
                    .rows
                    .into_iter()
                    .filter_map(|mut row| match row.values.remove("name") {
                        Some(ExploreValue::String(value)) => Some(value),
                        _ => None,
                    })
                    .collect()
            }
            _ => return Err(QueryError::CapabilityUnavailable.into()),
        };
        Ok(UnifiedQueryResult::Values {
            source: parsed.source,
            field,
            items,
            normalized: parsed.normalized,
            cost: 25,
        })
    }

    pub async fn replays(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
        limit: Option<usize>,
    ) -> Result<ReplayPage, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        if let (Some(from), Some(until)) = (from, until) {
            validate_time_range(from, until)?;
        }
        self.replay_store()?
            .list_replays(
                project_id,
                ReplayQuery {
                    from,
                    until,
                    error_id: None,
                    trace_id: None,
                    before: None,
                    limit: page_size(limit)?,
                },
            )
            .await
            .map_err(map_signal_error)
    }

    pub async fn replay(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        replay_id: EventId,
    ) -> Result<ReplayRecord, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        self.replay_store()?
            .load_replay(project_id, replay_id)
            .await
            .map_err(map_signal_error)
    }

    pub async fn open_replay_segment(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        replay_id: EventId,
        segment_id: u32,
    ) -> Result<(ReplaySegment, Box<dyn BlobReadSession>), NativeApiError> {
        let replay = self.replay(context, project_id, replay_id).await?;
        let segment = replay
            .segments
            .into_iter()
            .find(|segment| segment.segment_id == segment_id)
            .ok_or(NativeApiError::NotFound)?;
        if segment
            .object
            .key
            .replay_relation()
            .map_err(|_| NativeApiError::Unavailable)?
            != (project_id, replay_id, segment_id)
        {
            return Err(NativeApiError::Unavailable);
        }
        let reader = self
            .blob_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .open(&segment.object.key)
            .await
            .map_err(map_blob_error)?;
        Ok((segment, reader))
    }

    pub async fn monitors(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        limit: Option<usize>,
    ) -> Result<MonitorPage, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        self.monitor_store()?
            .list_monitors(project_id, None, monitors_page_size(limit)?)
            .await
            .map_err(map_signal_error)
    }

    pub async fn monitor_runs(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        monitor_id: MonitorId,
        request: MonitorRunListRequest<'_>,
    ) -> Result<NativePage<MonitorRun>, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        if request
            .from
            .zip(request.until)
            .is_some_and(|(from, until)| from >= until)
        {
            return Err(NativeApiError::InvalidRequest);
        }
        let normalized = monitor_run_cursor_scope(monitor_id, request.from, request.until);
        let digest = cursor_digest(project_id, &normalized, CursorKind::MonitorRun);
        let before = request
            .cursor
            .map(|value| decode_monitor_run_anchor(value, digest))
            .transpose()?;
        let page = self
            .monitor_store()?
            .list_monitor_runs(
                project_id,
                monitor_id,
                request.from,
                request.until,
                before,
                monitors_page_size(request.limit)?,
            )
            .await
            .map_err(map_signal_error)?;
        Ok(NativePage {
            next_cursor: page.next.map(|anchor| {
                encode_cursor(
                    CursorKind::MonitorRun,
                    anchor.started_at,
                    &anchor.run_id.as_bytes(),
                    digest,
                )
            }),
            items: page.items,
        })
    }

    pub async fn delete_monitor(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        monitor_id: MonitorId,
    ) -> Result<(), NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::ProjectAdmin)
            .await?;
        self.monitor_store()?
            .delete_monitor(project_id, monitor_id)
            .await
            .map_err(map_signal_error)
    }

    pub async fn upsert_monitor(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        input: MonitorInput,
    ) -> Result<MonitorDefinition, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectAdmin)
            .await?;
        let now = self.clock.now();
        let schedule = match input.schedule_type.as_ref() {
            "interval" => MonitorSchedule::interval(
                input
                    .schedule
                    .parse()
                    .map_err(|_| NativeApiError::InvalidRequest)?,
            ),
            "crontab" => MonitorSchedule::crontab(&input.schedule),
            _ => return Err(NativeApiError::InvalidRequest),
        }
        .map_err(|_| NativeApiError::InvalidRequest)?;
        let config = MonitorConfig {
            schedule,
            checkin_margin_seconds: input.checkin_margin_seconds,
            max_runtime_seconds: input.max_runtime_seconds,
        };
        config
            .validate()
            .map_err(|_| NativeApiError::InvalidRequest)?;
        let id = if input.uptime.is_some() {
            MonitorId::derive_uptime(project_id, &input.slug, &input.environment)
        } else {
            MonitorId::derive(project_id, &input.slug, &input.environment)
        };
        let previous = self
            .monitor_store()?
            .load_monitor(project_id, id)
            .await
            .ok();
        let next_expected_at = config
            .schedule
            .next_after(now)
            .map_err(|_| NativeApiError::InvalidRequest)?;
        let monitor = MonitorDefinition {
            id,
            project_id,
            slug: input.slug,
            name: input.name,
            environment_id: derive_environment_id(project_id, &input.environment),
            environment: input.environment,
            enabled: input.enabled,
            managed_by_web: true,
            revision: previous
                .as_ref()
                .map_or(1, |value| value.revision.saturating_add(1)),
            config,
            uptime: input.uptime,
            next_expected_at,
            last_run_id: previous.as_ref().and_then(|value| value.last_run_id),
            last_status: previous.as_ref().and_then(|value| value.last_status),
            last_check_in_at: previous.as_ref().and_then(|value| value.last_check_in_at),
            created_at: previous.as_ref().map_or(now, |value| value.created_at),
            updated_at: now,
        };
        monitor
            .validate()
            .map_err(|_| NativeApiError::InvalidRequest)?;
        self.monitor_store()?
            .upsert_monitor(monitor)
            .await
            .map_err(map_signal_error)
    }

    #[must_use]
    pub fn with_explore(mut self, explore: Arc<ExploreService>) -> Self {
        self.explore = Some(explore);
        self
    }

    #[must_use]
    pub fn with_dashboards(mut self, dashboards: Arc<DashboardService>) -> Self {
        self.dashboards = Some(dashboards);
        self
    }

    pub async fn list_saved_queries(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
    ) -> Result<Vec<SavedQuery>, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        Ok(self
            .dashboard_service()?
            .list_saved_queries(project_id)
            .await?)
    }

    pub async fn saved_query(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        id: SavedQueryId,
    ) -> Result<SavedQuery, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        Ok(self
            .dashboard_service()?
            .load_saved_query(project_id, id)
            .await?)
    }

    pub async fn create_saved_query(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        input: SavedQueryInput,
    ) -> Result<SavedQuery, NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::IssueWrite)
            .await?;
        Ok(self
            .dashboard_service()?
            .create_saved_query(project_id, context.user_id, input)
            .await?)
    }

    pub async fn update_saved_query(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        id: SavedQueryId,
        expected_revision: u64,
        input: SavedQueryInput,
    ) -> Result<SavedQuery, NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::IssueWrite)
            .await?;
        Ok(self
            .dashboard_service()?
            .update_saved_query(project_id, id, context.user_id, expected_revision, input)
            .await?)
    }

    pub async fn delete_saved_query(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        id: SavedQueryId,
    ) -> Result<(), NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::IssueWrite)
            .await?;
        Ok(self
            .dashboard_service()?
            .delete_saved_query(project_id, id)
            .await?)
    }

    pub async fn list_dashboards(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
    ) -> Result<Vec<Dashboard>, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        Ok(self
            .dashboard_service()?
            .list_dashboards(project_id)
            .await?)
    }

    pub async fn dashboard(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        id: DashboardId,
    ) -> Result<Dashboard, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        Ok(self
            .dashboard_service()?
            .load_dashboard(project_id, id)
            .await?)
    }

    pub async fn create_dashboard(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        input: DashboardInput,
    ) -> Result<Dashboard, NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::IssueWrite)
            .await?;
        Ok(self
            .dashboard_service()?
            .create_dashboard(project_id, context.user_id, input)
            .await?)
    }

    pub async fn update_dashboard(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        id: DashboardId,
        expected_revision: u64,
        input: DashboardInput,
    ) -> Result<Dashboard, NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::IssueWrite)
            .await?;
        Ok(self
            .dashboard_service()?
            .update_dashboard(project_id, id, context.user_id, expected_revision, input)
            .await?)
    }

    pub async fn delete_dashboard(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        id: DashboardId,
    ) -> Result<(), NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::IssueWrite)
            .await?;
        Ok(self
            .dashboard_service()?
            .delete_dashboard(project_id, id)
            .await?)
    }

    pub async fn refresh_dashboard(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        id: DashboardId,
        variables: DashboardVariables,
    ) -> Result<DashboardRefresh, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        Ok(self
            .dashboard_service()?
            .refresh(project_id, id, variables)
            .await?)
    }

    pub async fn explore(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        mut query: ExploreQuery,
        cursor: Option<&str>,
    ) -> Result<(ExploreResult, Box<str>, u32), NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        query.cursor = None;
        let initial = self
            .explore
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .plan(project_id, query)?;
        if let Some(value) = cursor {
            query = initial.query;
            query.cursor = Some(decode_explore_cursor(
                value,
                project_id,
                &initial.normalized,
                query.dataset as u8,
            )?);
        } else {
            query = initial.query;
        }
        let plan = self
            .explore
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .plan(project_id, query)?;
        let normalized = plan.normalized.clone();
        let cost = plan.cost;
        let result = self
            .explore
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .execute(plan)
            .await?;
        Ok((result, normalized, cost))
    }

    pub async fn feedback(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        feedback_id: EventId,
    ) -> Result<FeedbackRecord, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        let mut feedback = self
            .feedback_store()?
            .load_feedback(project_id, feedback_id)
            .await
            .map_err(map_feedback_error)?;
        self.enrich_feedback(&mut feedback).await?;
        Ok(feedback)
    }

    pub async fn feedback_list(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        request: FeedbackListRequest<'_>,
    ) -> Result<NativePage<FeedbackRecord>, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        let normalized = format!(
            "{}:{}",
            request.status.map_or("all", FeedbackStatus::as_str),
            request
                .replay_id
                .map_or_else(|| "all".to_owned(), |value| value.to_string())
        );
        let digest = cursor_digest(project_id, &normalized, CursorKind::Feedback);
        let before = request
            .cursor
            .map(|value| decode_feedback_anchor(value, digest))
            .transpose()?;
        let page = self
            .feedback_store()?
            .list_feedback(
                project_id,
                FeedbackQuery {
                    status: request.status,
                    event_id: None,
                    trace_id: None,
                    replay_id: request.replay_id,
                    before,
                    limit: page_size(request.limit)?,
                },
            )
            .await
            .map_err(map_feedback_error)?;
        let next_cursor = page.next.map(|anchor| {
            encode_cursor(
                CursorKind::Feedback,
                anchor.received_at,
                &anchor.feedback_id.as_bytes(),
                digest,
            )
        });
        let mut items = page.items;
        for feedback in &mut items {
            self.enrich_feedback(feedback).await?;
        }
        Ok(NativePage { items, next_cursor })
    }

    pub async fn update_feedback_status(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        feedback_id: EventId,
        status: FeedbackStatus,
    ) -> Result<FeedbackRecord, NativeApiError> {
        self.authorize(context, project_id, Permission::IssueWrite)
            .await?;
        let mut feedback = self
            .feedback_store()?
            .update_feedback_status(project_id, feedback_id, status, self.clock.now())
            .await
            .map_err(map_feedback_error)?;
        self.enrich_feedback(&mut feedback).await?;
        Ok(feedback)
    }

    pub async fn open_feedback_attachment(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        feedback_id: EventId,
        attachment_id: BlobObjectId,
    ) -> Result<(AttachmentView, Box<dyn BlobReadSession>), NativeApiError> {
        let feedback = self.feedback(context, project_id, feedback_id).await?;
        let attachment = feedback
            .attachments
            .iter()
            .find(|attachment| attachment.attachment_id == attachment_id)
            .ok_or(NativeApiError::NotFound)?;
        let (related_project, related_event, related_object) = attachment
            .blob
            .key
            .event_relation()
            .map_err(|_| NativeApiError::Unavailable)?;
        if related_project != project_id
            || related_event != feedback_id
            || related_object != attachment_id
        {
            return Err(NativeApiError::Unavailable);
        }
        let view = AttachmentView {
            attachment_id,
            blob_key: attachment.blob.key.clone(),
            filename: attachment.filename.as_str().into(),
            content_type: attachment.content_type.as_str().into(),
            attachment_type: attachment.attachment_type.clone(),
            size: attachment.blob.size,
            checksum: attachment.blob.checksum.to_string().into(),
        };
        let reader = self
            .blob_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .open(&view.blob_key)
            .await
            .map_err(map_blob_error)?;
        Ok((view, reader))
    }

    fn feedback_store(&self) -> Result<&Arc<dyn FeedbackStore>, NativeApiError> {
        self.feedback_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)
    }

    fn monitor_store(&self) -> Result<&Arc<dyn MonitorStore>, NativeApiError> {
        self.monitor_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)
    }

    fn replay_store(&self) -> Result<&Arc<dyn ReplayStore>, NativeApiError> {
        self.replay_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)
    }

    async fn exact_correlations(
        &self,
        project_id: ProjectId,
        event_id: Option<EventId>,
        trace_id: Option<TraceId>,
        replay_id: Option<EventId>,
    ) -> Result<ExactCorrelations, NativeApiError> {
        let replay_ids = if let Some(store) = &self.replay_store
            && (event_id.is_some() || trace_id.is_some())
        {
            store
                .list_replays(
                    project_id,
                    ReplayQuery {
                        from: None,
                        until: None,
                        error_id: event_id,
                        trace_id,
                        before: None,
                        limit: MAX_PAGE,
                    },
                )
                .await
                .map_err(map_signal_error)?
                .items
                .into_iter()
                .map(|replay| replay.replay_id)
                .collect()
        } else {
            Vec::new()
        };
        let feedback_ids = if let Some(store) = &self.feedback_store {
            store
                .list_feedback(
                    project_id,
                    FeedbackQuery {
                        status: None,
                        event_id,
                        trace_id,
                        replay_id,
                        before: None,
                        limit: MAX_PAGE,
                    },
                )
                .await
                .map_err(map_feedback_error)?
                .items
                .into_iter()
                .map(|feedback| feedback.feedback_id)
                .collect()
        } else {
            Vec::new()
        };
        Ok(ExactCorrelations {
            replay_ids,
            feedback_ids,
        })
    }

    fn dashboard_service(&self) -> Result<&Arc<DashboardService>, NativeApiError> {
        self.dashboards.as_ref().ok_or(NativeApiError::Unavailable)
    }

    async fn enrich_feedback(&self, feedback: &mut FeedbackRecord) -> Result<(), NativeApiError> {
        let Some(event_id) = feedback.associated_event_id else {
            return Ok(());
        };
        match self
            .investigation
            .load_event(
                feedback.project_id,
                EventKey::new(feedback.project_id, event_id),
            )
            .await
        {
            Ok(event) => feedback.issue_id = Some(event.issue_id),
            Err(InvestigationStoreError::NotFound) => {}
            Err(InvestigationStoreError::InvalidData | InvestigationStoreError::Unavailable) => {
                return Err(NativeApiError::Unavailable);
            }
        }
        Ok(())
    }

    fn release_service(&self) -> Result<&ReleaseService, NativeApiError> {
        self.releases.as_deref().ok_or(NativeApiError::Unavailable)
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
        let (from, until) = signal_time_range(self.clock.now(), from, until, DAY_MILLIS)?;
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
        let (from, until) = signal_time_range(self.clock.now(), from, until, DAY_MILLIS)?;
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
        let (from, until) = signal_time_range(self.clock.now(), from, until, 7 * DAY_MILLIS)?;
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
        request: IssueListRequest<'_>,
    ) -> Result<NativePage<IssueSnapshot>, NativeApiError> {
        let IssueListRequest {
            status,
            from,
            until,
            cursor,
            limit,
        } = request;
        self.authorize(context, project_id, Permission::IssueRead)
            .await?;
        let limit = page_size(limit)?;
        if let (Some(from), Some(until)) = (from, until) {
            validate_time_range(from, until)?;
        }
        let normalized = format!(
            "issues:status={}:from={}:until={}",
            status_name(status),
            from.map_or_else(|| "*".to_owned(), |value| value.unix_millis().to_string()),
            until.map_or_else(|| "*".to_owned(), |value| value.unix_millis().to_string()),
        );
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
                    from,
                    until,
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
        let (from, until) = event_time_range(
            self.clock.now(),
            request.issue_id.is_some(),
            request.from,
            request.until,
        )?;
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

    pub async fn event_correlations(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        event_id: EventId,
    ) -> Result<ExactCorrelations, NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        self.exact_correlations(project_id, Some(event_id), None, None)
            .await
    }

    pub async fn trace_correlations(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        trace_id: TraceId,
    ) -> Result<ExactCorrelations, NativeApiError> {
        self.authorize(context, project_id, Permission::EventRead)
            .await?;
        self.exact_correlations(project_id, None, Some(trace_id), None)
            .await
    }

    pub async fn replay_correlations(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        replay_id: EventId,
    ) -> Result<ExactCorrelations, NativeApiError> {
        self.authorize(context, project_id, Permission::ProjectRead)
            .await?;
        self.exact_correlations(project_id, None, None, Some(replay_id))
            .await
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

    pub async fn releases(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<NativePage<metric_domain::api::ReleaseView>, NativeApiError> {
        self.authorize(context, project_id, Permission::ReleaseRead)
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
                    anchor.activity_at,
                    &anchor.id.as_bytes(),
                    digest,
                )
            }),
            items: page.items,
        })
    }

    pub async fn release(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
    ) -> Result<ReleaseRecord, NativeApiError> {
        self.authorize(context, project_id, Permission::ReleaseRead)
            .await?;
        let release = self
            .release_service()?
            .load(context, release_id)
            .await
            .map_err(map_release_error)?;
        if !release.project_ids.contains(&project_id) {
            return Err(NativeApiError::NotFound);
        }
        Ok(release)
    }

    pub async fn create_release(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        version: Box<str>,
        url: Option<Box<str>>,
        reference: Option<Box<str>>,
        repositories: Vec<RepositoryReference>,
    ) -> Result<ReleaseRecord, NativeApiError> {
        self.authorize_mutation(context, project_id, Permission::ReleaseWrite)
            .await?;
        self.release_service()?
            .create(
                context,
                vec![project_id],
                version,
                url,
                reference,
                repositories,
            )
            .await
            .map_err(map_release_error)
    }

    pub async fn finalize_release(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
        released_at: Option<Timestamp>,
    ) -> Result<ReleaseRecord, NativeApiError> {
        let release = self.release(context, project_id, release_id).await?;
        self.authorize_mutation(context, project_id, Permission::ReleaseWrite)
            .await?;
        self.release_service()?
            .finalize(context, release.id, released_at)
            .await
            .map_err(map_release_error)
    }

    pub async fn create_deploy(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
        request: CreateDeployRequest,
    ) -> Result<DeployRecord, NativeApiError> {
        let release = self.release(context, project_id, release_id).await?;
        self.authorize_mutation(context, project_id, Permission::ReleaseWrite)
            .await?;
        self.release_service()?
            .create_deploy(context, release.id, vec![project_id], request)
            .await
            .map_err(map_release_error)
    }

    pub async fn release_deploys(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
        limit: Option<usize>,
    ) -> Result<Vec<DeployRecord>, NativeApiError> {
        self.release(context, project_id, release_id).await?;
        self.release_service()?
            .deploys(context, project_id, release_id, page_size(limit)?)
            .await
            .map_err(map_release_error)
    }

    pub async fn release_issues(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
        kind: metric_ports::ReleaseIssueKind,
        limit: Option<usize>,
    ) -> Result<Vec<ReleaseIssueSummary>, NativeApiError> {
        let release = self.release(context, project_id, release_id).await?;
        self.release_service()?
            .issues(
                context,
                project_id,
                release.version,
                kind,
                page_size(limit)?,
            )
            .await
            .map_err(map_release_error)
    }

    pub async fn release_health(
        &self,
        context: &AuthContext,
        project_id: ProjectId,
        release_id: metric_domain::finalization::ReleaseId,
        from: Option<Timestamp>,
        until: Option<Timestamp>,
    ) -> Result<Vec<metric_domain::sessions::ReleaseHealthBucket>, NativeApiError> {
        self.release(context, project_id, release_id).await?;
        let until = until.unwrap_or_else(|| self.clock.now());
        let from = from.unwrap_or_else(|| {
            Timestamp::from_unix_millis(
                until
                    .unix_millis()
                    .saturating_sub(7_i64 * 24 * 60 * 60 * 1_000),
            )
            .expect("seven-day subtraction remains in the timestamp range")
        });
        validate_time_range(from, until)?;
        self.session_store
            .as_ref()
            .ok_or(NativeApiError::Unavailable)?
            .release_health(project_id, release_id, from, until)
            .await
            .map_err(map_signal_error)
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
        self.identity
            .authorize_project_mutation(context, project_id, permission)
            .await
            .map_err(map_auth_error)
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

fn query_cost(parsed: &ParsedQuery, limit: usize) -> Result<u32, QueryError> {
    let predicates = parsed
        .expression
        .as_ref()
        .map_or(0, |value| value.predicates().len());
    let cost = 100_u32
        .saturating_add(
            u32::try_from(predicates)
                .unwrap_or(u32::MAX)
                .saturating_mul(40),
        )
        .saturating_add(u32::try_from(limit).unwrap_or(u32::MAX));
    if cost > 10_000 {
        Err(QueryError::CostExceeded)
    } else {
        Ok(cost)
    }
}

fn query_dataset(source: QuerySource) -> Result<ExploreDataset, QueryError> {
    match source {
        QuerySource::Errors => Ok(ExploreDataset::Errors),
        QuerySource::Logs => Ok(ExploreDataset::Logs),
        QuerySource::Traces => Ok(ExploreDataset::Spans),
        QuerySource::Metrics => Ok(ExploreDataset::Metrics),
        _ => Err(QueryError::CapabilityUnavailable),
    }
}

fn map_unified_explore_error(error: NativeApiError) -> NativeApiError {
    match error {
        NativeApiError::Explore(ExploreError::InvalidQuery) => {
            QueryError::CapabilityUnavailable.into()
        }
        NativeApiError::Explore(ExploreError::CostExceeded) => QueryError::CostExceeded.into(),
        NativeApiError::Explore(ExploreError::Capacity) => QueryError::Capacity.into(),
        NativeApiError::Explore(ExploreError::Unavailable) => QueryError::Unavailable.into(),
        NativeApiError::InvalidCursor => QueryError::InvalidCursor.into(),
        error => error,
    }
}

const fn query_permission(source: QuerySource) -> Permission {
    match source {
        QuerySource::Issues => Permission::IssueRead,
        QuerySource::Errors | QuerySource::Logs | QuerySource::Traces | QuerySource::Metrics => {
            Permission::EventRead
        }
        QuerySource::Replays | QuerySource::Feedback => Permission::ProjectRead,
        QuerySource::Releases => Permission::ReleaseRead,
    }
}

fn unified_time_range(
    now: Timestamp,
    from: Option<Timestamp>,
    until: Option<Timestamp>,
    source: QuerySource,
) -> Result<(Timestamp, Timestamp), NativeApiError> {
    if from.is_none() && until.is_none() {
        let from = Timestamp::from_unix_millis(0).expect("the Unix epoch is a valid timestamp");
        let until = Timestamp::from_unix_millis(ALL_TIME_UNTIL_MILLIS)
            .expect("the all-time upper bound is a valid timestamp");
        return Ok((from, until));
    }

    let default = if source == QuerySource::Metrics {
        7 * DAY_MILLIS
    } else {
        DAY_MILLIS
    };
    signal_time_range(now, from, until, default)
}

fn positive_predicate(
    expression: Option<&QueryExpression>,
    field: QueryField,
) -> Option<&QueryPredicate> {
    match expression? {
        QueryExpression::Predicate(value)
            if value.field == field && value.operator == QueryOperator::Equal =>
        {
            Some(value)
        }
        QueryExpression::Predicate(value)
            if value.field == field
                && field == QueryField::Title
                && value.operator == QueryOperator::Contains =>
        {
            Some(value)
        }
        QueryExpression::And(values) => values
            .iter()
            .find_map(|value| positive_predicate(Some(value), field)),
        QueryExpression::Predicate(_) | QueryExpression::Not(_) | QueryExpression::Or(_) => None,
    }
}

fn issue_matches(expression: Option<&QueryExpression>, issue: &IssueSnapshot) -> bool {
    matches_expression(expression, &mut |predicate| match predicate.field {
        QueryField::IssueId => string_matches(&issue.issue_id.to_string(), predicate),
        QueryField::Title => string_matches(issue.title.as_str(), predicate),
        QueryField::Status => string_matches(status_name(Some(issue.status)), predicate),
        QueryField::Timestamp => numeric_matches(
            issue.last_seen.unix_millis(),
            predicate_value_i64(predicate),
            predicate.operator,
        ),
        _ => false,
    })
}

fn replay_matches(expression: Option<&QueryExpression>, replay: &ReplayRecord) -> bool {
    matches_expression(expression, &mut |predicate| match predicate.field {
        QueryField::EventId => replay
            .error_ids
            .iter()
            .any(|value| string_matches(&value.to_string(), predicate)),
        QueryField::ReplayId => string_matches(&replay.replay_id.to_string(), predicate),
        QueryField::TraceId => replay
            .trace_ids
            .iter()
            .any(|value| string_matches(&value.to_string(), predicate)),
        QueryField::Url => string_matches(replay.url.as_deref().unwrap_or_default(), predicate),
        QueryField::Environment => {
            string_matches(replay.environment.as_deref().unwrap_or_default(), predicate)
        }
        QueryField::Release => {
            string_matches(replay.release.as_deref().unwrap_or_default(), predicate)
        }
        QueryField::Timestamp => numeric_matches(
            replay.received_at.unix_millis(),
            predicate_value_i64(predicate),
            predicate.operator,
        ),
        _ => false,
    })
}

fn feedback_matches(expression: Option<&QueryExpression>, feedback: &FeedbackRecord) -> bool {
    matches_expression(expression, &mut |predicate| match predicate.field {
        QueryField::EventId => feedback
            .associated_event_id
            .is_some_and(|value| string_matches(&value.to_string(), predicate)),
        QueryField::FeedbackId => string_matches(&feedback.feedback_id.to_string(), predicate),
        QueryField::ReplayId => feedback
            .replay_id
            .is_some_and(|value| string_matches(&value.to_string(), predicate)),
        QueryField::TraceId => feedback
            .trace_id
            .is_some_and(|value| string_matches(&value.to_string(), predicate)),
        QueryField::Status => string_matches(feedback.status.as_str(), predicate),
        QueryField::Message => string_matches(&feedback.message, predicate),
        QueryField::Timestamp => numeric_matches(
            feedback.received_at.unix_millis(),
            predicate_value_i64(predicate),
            predicate.operator,
        ),
        _ => false,
    })
}

fn release_matches(
    expression: Option<&QueryExpression>,
    release: &metric_domain::api::ReleaseView,
) -> bool {
    matches_expression(expression, &mut |predicate| match predicate.field {
        QueryField::Release => string_matches(&release.version, predicate),
        QueryField::Timestamp => numeric_matches(
            release.activity_at.unix_millis(),
            predicate_value_i64(predicate),
            predicate.operator,
        ),
        _ => false,
    })
}

fn string_matches(value: &str, predicate: &QueryPredicate) -> bool {
    match predicate.operator {
        QueryOperator::Equal => value == predicate.value.as_ref(),
        QueryOperator::Contains => value
            .to_lowercase()
            .contains(&predicate.value.to_lowercase()),
        QueryOperator::Greater
        | QueryOperator::GreaterOrEqual
        | QueryOperator::Less
        | QueryOperator::LessOrEqual => false,
    }
}

fn predicate_value_i64(predicate: &QueryPredicate) -> Option<i64> {
    predicate.value.parse::<i64>().ok().or_else(|| {
        OffsetDateTime::parse(&predicate.value, &Rfc3339)
            .ok()
            .and_then(|value| {
                i64::try_from(value.unix_timestamp_nanos().div_euclid(1_000_000)).ok()
            })
    })
}

fn numeric_matches(left: i64, right: Option<i64>, operator: QueryOperator) -> bool {
    let Some(right) = right else {
        return false;
    };
    match operator {
        QueryOperator::Equal => left == right,
        QueryOperator::Greater => left > right,
        QueryOperator::GreaterOrEqual => left >= right,
        QueryOperator::Less => left < right,
        QueryOperator::LessOrEqual => left <= right,
        QueryOperator::Contains => false,
    }
}

fn parse_issue_status(value: &str) -> Result<IssueStatus, NativeApiError> {
    match value {
        "open" => Ok(IssueStatus::Open),
        "resolved" => Ok(IssueStatus::Resolved),
        "ignored" => Ok(IssueStatus::Ignored),
        _ => Err(QueryError::Syntax.into()),
    }
}

fn hex_16_text(value: &str) -> Result<[u8; 16], NativeApiError> {
    if value.len() != 32 || !value.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err(QueryError::Syntax.into());
    }
    let mut bytes = [0_u8; 16];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| QueryError::Syntax)?;
    Ok(bytes)
}

fn page_size(value: Option<usize>) -> Result<usize, NativeApiError> {
    let value = value.unwrap_or(DEFAULT_PAGE);
    if (1..=MAX_PAGE).contains(&value) {
        Ok(value)
    } else {
        Err(NativeApiError::InvalidRequest)
    }
}

fn monitors_page_size(value: Option<usize>) -> Result<usize, NativeApiError> {
    let value = value.unwrap_or(DEFAULT_PAGE);
    if (1..=MAX_MONITORS_PAGE).contains(&value) {
        Ok(value)
    } else {
        Err(NativeApiError::InvalidRequest)
    }
}

fn monitor_run_cursor_scope(
    monitor_id: MonitorId,
    from: Option<Timestamp>,
    until: Option<Timestamp>,
) -> String {
    format!(
        "monitor-runs:{monitor_id}:{}:{}",
        from.map_or_else(|| "all".to_owned(), |value| value.unix_millis().to_string()),
        until.map_or_else(|| "all".to_owned(), |value| value.unix_millis().to_string())
    )
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

fn event_time_range(
    now: Timestamp,
    issue_scoped: bool,
    from: Option<Timestamp>,
    until: Option<Timestamp>,
) -> Result<(Timestamp, Timestamp), NativeApiError> {
    if issue_scoped && from.is_none() && until.is_none() {
        let from = Timestamp::from_unix_millis(0).expect("the Unix epoch is a valid timestamp");
        let until = Timestamp::from_unix_millis(now.unix_millis().saturating_add(1)).unwrap_or(now);
        return Ok((from, until));
    }
    time_range(now, from, until)
}

fn validate_time_range(from: Timestamp, until: Timestamp) -> Result<(), NativeApiError> {
    if from >= until || until.unix_millis().saturating_sub(from.unix_millis()) > MAX_RANGE_MILLIS {
        Err(NativeApiError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn validate_optional_time_range(
    from: Option<Timestamp>,
    until: Option<Timestamp>,
) -> Result<(), NativeApiError> {
    if from.zip(until).is_some_and(|(from, until)| from >= until) {
        Err(NativeApiError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn in_time_range(value: Timestamp, from: Option<Timestamp>, until: Option<Timestamp>) -> bool {
    from.is_none_or(|from| value >= from) && until.is_none_or(|until| value <= until)
}

fn signal_time_range(
    now: Timestamp,
    from: Option<Timestamp>,
    until: Option<Timestamp>,
    default_range_millis: i64,
) -> Result<(Timestamp, Timestamp), NativeApiError> {
    if from.is_none() && until.is_none() {
        let from = Timestamp::from_unix_millis(0).expect("the Unix epoch is a valid timestamp");
        let until = Timestamp::from_unix_millis(now.unix_millis().saturating_add(1)).unwrap_or(now);
        return Ok((from, until));
    }
    let until = until.unwrap_or(now);
    let from = from.unwrap_or_else(|| {
        Timestamp::from_unix_millis(until.unix_millis().saturating_sub(default_range_millis))
            .expect("bounded signal range remains a valid timestamp")
    });
    validate_time_range(from, until)?;
    Ok((from, until))
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

fn explore_cursor_digest(project_id: ProjectId, normalized: &str, dataset: u8) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric/explore-cursor/v1");
    hasher.update(&[dataset]);
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(normalized.as_bytes());
    hasher.finalize().as_bytes()[..16]
        .try_into()
        .expect("BLAKE3 digest prefix")
}

#[must_use]
pub fn encode_explore_cursor(
    cursor: ExploreCursor,
    project_id: ProjectId,
    normalized: &str,
    dataset: u8,
) -> String {
    let mut bytes = Vec::with_capacity(46);
    bytes.push(1);
    bytes.extend_from_slice(&cursor.time.to_be_bytes());
    bytes.push(cursor.id_len);
    bytes.extend_from_slice(&cursor.id);
    bytes.extend_from_slice(&explore_cursor_digest(project_id, normalized, dataset));
    hex::encode(bytes)
}

fn decode_explore_cursor(
    value: &str,
    project_id: ProjectId,
    normalized: &str,
    dataset: u8,
) -> Result<ExploreCursor, NativeApiError> {
    let bytes = hex::decode(value).map_err(|_| NativeApiError::InvalidCursor)?;
    if bytes.len() != 46
        || bytes[0] != 1
        || !matches!(bytes[9], 16 | 20)
        || bytes[30..] != explore_cursor_digest(project_id, normalized, dataset)
    {
        return Err(NativeApiError::InvalidCursor);
    }
    Ok(ExploreCursor {
        time: i64::from_be_bytes(
            bytes[1..9]
                .try_into()
                .map_err(|_| NativeApiError::InvalidCursor)?,
        ),
        id_len: bytes[9],
        id: bytes[10..30]
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

fn decode_feedback_anchor(value: &str, digest: [u8; 16]) -> Result<FeedbackAnchor, NativeApiError> {
    let (received_at, id) =
        decode_cursor(value, CursorKind::Feedback, 16, digest).map_err(map_cursor_error)?;
    Ok(FeedbackAnchor {
        received_at,
        feedback_id: EventId::from_bytes(id.try_into().map_err(|_| NativeApiError::InvalidCursor)?),
    })
}

fn decode_replay_anchor(
    value: &str,
    digest: [u8; 16],
) -> Result<metric_domain::replays::ReplayCursor, NativeApiError> {
    let (received_at, id) =
        decode_cursor(value, CursorKind::Replay, 16, digest).map_err(map_cursor_error)?;
    Ok(metric_domain::replays::ReplayCursor {
        received_at,
        replay_id: EventId::from_bytes(id.try_into().map_err(|_| NativeApiError::InvalidCursor)?),
    })
}

fn decode_monitor_run_anchor(
    value: &str,
    digest: [u8; 16],
) -> Result<MonitorRunAnchor, NativeApiError> {
    let (started_at, id) =
        decode_cursor(value, CursorKind::MonitorRun, 16, digest).map_err(map_cursor_error)?;
    Ok(MonitorRunAnchor {
        started_at,
        run_id: MonitorRunId::from_bytes(id.try_into().map_err(|_| NativeApiError::InvalidCursor)?),
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
    let (activity_at, id) =
        decode_cursor(value, CursorKind::Release, 16, digest).map_err(map_cursor_error)?;
    Ok(ReleaseAnchor {
        activity_at,
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

fn map_feedback_error(error: FeedbackStoreError) -> NativeApiError {
    match error {
        FeedbackStoreError::NotFound => NativeApiError::NotFound,
        FeedbackStoreError::Conflict => NativeApiError::Conflict,
        FeedbackStoreError::InvalidData => NativeApiError::InvalidRequest,
        FeedbackStoreError::Capacity => NativeApiError::RateLimited,
        FeedbackStoreError::Unavailable => NativeApiError::Unavailable,
    }
}

fn map_release_error(error: ReleaseError) -> NativeApiError {
    match error {
        ReleaseError::InvalidRequest => NativeApiError::InvalidRequest,
        ReleaseError::Forbidden => NativeApiError::Forbidden,
        ReleaseError::NotFound => NativeApiError::NotFound,
        ReleaseError::Conflict => NativeApiError::Conflict,
        ReleaseError::Unavailable => NativeApiError::Unavailable,
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
        AuthError::ProjectDisabled => NativeApiError::ProjectDisabled,
        AuthError::ProjectDeletionPending => NativeApiError::ProjectDeletionPending,
        AuthError::ProjectPurging => NativeApiError::ProjectPurging,
        AuthError::ProjectDeleted => NativeApiError::ProjectDeleted,
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
    fn monitor_pages_have_a_dedicated_hard_ceiling() {
        assert_eq!(monitors_page_size(Some(100_000)), Ok(100_000));
        assert_eq!(
            monitors_page_size(Some(100_001)),
            Err(NativeApiError::InvalidRequest)
        );
        assert_eq!(page_size(Some(101)), Err(NativeApiError::InvalidRequest));
    }

    #[test]
    fn monitor_run_cursor_is_bound_to_monitor_and_time_range() {
        let project_id = ProjectId::new(7).unwrap();
        let monitor_id = MonitorId::from_bytes([3; 16]);
        let started_at = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let run_id = MonitorRunId::from_bytes([4; 16]);
        let scope = monitor_run_cursor_scope(monitor_id, None, None);
        let digest = cursor_digest(project_id, &scope, CursorKind::MonitorRun);
        let cursor = encode_cursor(
            CursorKind::MonitorRun,
            started_at,
            &run_id.as_bytes(),
            digest,
        );

        assert_eq!(
            decode_monitor_run_anchor(&cursor, digest),
            Ok(MonitorRunAnchor { started_at, run_id })
        );

        let other_scope = monitor_run_cursor_scope(MonitorId::from_bytes([5; 16]), None, None);
        let other_digest = cursor_digest(project_id, &other_scope, CursorKind::MonitorRun);
        assert_eq!(
            decode_monitor_run_anchor(&cursor, other_digest),
            Err(NativeApiError::InvalidCursor)
        );
    }

    #[test]
    fn issue_event_pages_default_to_all_retained_time() {
        let now = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let (from, until) = event_time_range(now, true, None, None).unwrap();

        assert_eq!(from.unix_millis(), 0);
        assert_eq!(until.unix_millis(), now.unix_millis() + 1);
    }

    #[test]
    fn project_event_pages_keep_the_bounded_default() {
        let now = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let (from, until) = event_time_range(now, false, None, None).unwrap();

        assert_eq!(from.unix_millis(), now.unix_millis() - DAY_MILLIS);
        assert_eq!(until.unix_millis(), now.unix_millis() + 1);
    }

    #[test]
    fn omitted_signal_bounds_cover_all_retained_data() {
        let now = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let (from, until) = signal_time_range(now, None, None, DAY_MILLIS).unwrap();
        assert_eq!(from.unix_millis(), 0);
        assert_eq!(until.unix_millis(), now.unix_millis() + 1);
    }

    #[test]
    fn unified_all_time_bounds_are_stable_across_requests() {
        let first_now = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let later_now = Timestamp::from_unix_millis(1_800_000_010_000).unwrap();

        let first = unified_time_range(first_now, None, None, QuerySource::Logs).unwrap();
        let later = unified_time_range(later_now, None, None, QuerySource::Logs).unwrap();

        assert_eq!(first, later);
        assert_eq!(first.0.unix_millis(), 0);
        assert_eq!(first.1.unix_millis(), ALL_TIME_UNTIL_MILLIS);
        assert!(first.1.unix_millis().checked_mul(1_000_000).is_some());
    }

    #[test]
    fn partial_signal_bounds_remain_bounded() {
        let until = Timestamp::from_unix_millis(1_800_000_000_000).unwrap();
        let (from, actual_until) = signal_time_range(until, None, Some(until), DAY_MILLIS).unwrap();
        assert_eq!(from.unix_millis(), until.unix_millis() - DAY_MILLIS);
        assert_eq!(actual_until, until);
    }

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
