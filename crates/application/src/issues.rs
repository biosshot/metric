//! Issue summary construction and project-scoped Issue commands.

use std::{num::NonZeroU64, sync::Arc};

use metric_domain::{
    event::{NormalizedEvent, NormalizedEventBody, NormalizedFrame},
    grouping::{GroupingResult, verify_issue_id},
    issue::{
        IssueCommand, IssueCommandResult, IssueCulprit, IssueGroupingDetail, IssueMutationResult,
        IssueOccurrence, IssueSearchQuery, IssueSearchResult, IssueTitle, IssueValueError,
        MAX_ISSUE_CULPRIT_BYTES, MAX_ISSUE_TITLE_BYTES,
    },
};
use metric_ports::{IssueStore, IssueStoreError};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IssueServiceError {
    #[error("grouping identity does not match the Event project")]
    InvalidGroupingIdentity,
    #[error("Issue summary cannot satisfy the bounded domain contract")]
    InvalidSummary,
    #[error("Issue identity collides with another complete GroupingKey")]
    IdentityCollision,
    #[error("Issue does not exist")]
    NotFound,
    #[error("Issue persistence contains invalid data")]
    InvalidData,
    #[error("Issue persistence is temporarily unavailable")]
    Unavailable,
}

pub struct IssueService {
    store: Arc<dyn IssueStore>,
}

impl IssueService {
    #[must_use]
    pub fn new(store: Arc<dyn IssueStore>) -> Self {
        Self { store }
    }

    pub async fn record_occurrence(
        &self,
        event: &NormalizedEvent,
        grouping: &GroupingResult,
    ) -> Result<IssueMutationResult, IssueServiceError> {
        let occurrence = self.prepare_occurrence(event, grouping)?;
        self.store
            .apply_occurrence(occurrence)
            .await
            .map_err(map_store_error)
    }

    pub fn prepare_occurrence(
        &self,
        event: &NormalizedEvent,
        grouping: &GroupingResult,
    ) -> Result<IssueOccurrence, IssueServiceError> {
        prepare_issue_occurrence(event, grouping)
    }

    pub async fn apply_command(
        &self,
        command: IssueCommand,
    ) -> Result<IssueCommandResult, IssueServiceError> {
        self.store
            .apply_command(command)
            .await
            .map_err(map_store_error)
    }

    pub async fn search_titles(
        &self,
        project_id: metric_domain::ProjectId,
        query: IssueSearchQuery,
    ) -> Result<Vec<IssueSearchResult>, IssueServiceError> {
        self.store
            .search_titles(project_id, query)
            .await
            .map_err(map_store_error)
    }

    pub async fn load(
        &self,
        project_id: metric_domain::ProjectId,
        issue_id: metric_domain::grouping::IssueId,
    ) -> Result<metric_domain::issue::IssueSnapshot, IssueServiceError> {
        self.store
            .load(project_id, issue_id)
            .await
            .map_err(map_store_error)
    }
}

pub fn prepare_issue_occurrence(
    event: &NormalizedEvent,
    grouping: &GroupingResult,
) -> Result<IssueOccurrence, IssueServiceError> {
    if !verify_issue_id(event.project_id, grouping.key, grouping.issue_id) {
        return Err(IssueServiceError::InvalidGroupingIdentity);
    }
    Ok(IssueOccurrence {
        project_id: event.project_id,
        issue_id: grouping.issue_id,
        grouping_key: grouping.key,
        event_id: event.event_id,
        occurred_at: event.body.occurred_at,
        received_at: event.received_at,
        release: event
            .body
            .release
            .as_deref()
            .map(metric_domain::issue::IssueRelease::new)
            .transpose()
            .map_err(map_value_error)?,
        title: build_title(&event.body)?,
        culprit: build_culprit(&event.body)?,
        grouping: IssueGroupingDetail {
            strategy: grouping.strategy,
            explanation: grouping.explanation.clone(),
        },
        increment: NonZeroU64::MIN,
    })
}

fn build_title(body: &NormalizedEventBody) -> Result<IssueTitle, IssueServiceError> {
    let candidate = body
        .exceptions
        .last()
        .and_then(
            |exception| match (exception.ty.as_deref(), exception.value.as_deref()) {
                (Some(ty), Some(value)) => Some(format!("{ty}: {value}")),
                (Some(ty), None) => Some(ty.to_owned()),
                (None, Some(value)) => Some(value.to_owned()),
                (None, None) => None,
            },
        )
        .or_else(|| body.message.as_deref().map(str::to_owned))
        .or_else(|| body.logger.as_deref().map(str::to_owned))
        .unwrap_or_else(|| "Error event".to_owned());
    IssueTitle::new(normalize_summary(&candidate, MAX_ISSUE_TITLE_BYTES)).map_err(map_value_error)
}

fn build_culprit(body: &NormalizedEventBody) -> Result<Option<IssueCulprit>, IssueServiceError> {
    let exception_frames = body
        .exceptions
        .last()
        .map(|exception| exception.stacktrace.as_slice())
        .unwrap_or_default();
    let frame = select_frame(exception_frames).or_else(|| select_frame(&body.stacktrace));
    frame
        .and_then(frame_summary)
        .map(|value| {
            IssueCulprit::new(normalize_summary(value, MAX_ISSUE_CULPRIT_BYTES))
                .map_err(map_value_error)
        })
        .transpose()
}

fn select_frame(frames: &[NormalizedFrame]) -> Option<&NormalizedFrame> {
    frames
        .iter()
        .rev()
        .find(|frame| frame.in_app == Some(true) && frame_summary(frame).is_some())
        .or_else(|| {
            frames
                .iter()
                .rev()
                .find(|frame| frame.in_app != Some(false) && frame_summary(frame).is_some())
        })
        .or_else(|| {
            frames
                .iter()
                .rev()
                .find(|frame| frame_summary(frame).is_some())
        })
}

fn frame_summary(frame: &NormalizedFrame) -> Option<&str> {
    frame
        .function
        .as_deref()
        .or(frame.module.as_deref())
        .or(frame.filename.as_deref())
        .or(frame.absolute_path.as_deref())
}

fn normalize_summary(value: &str, maximum: usize) -> String {
    let mut output = String::with_capacity(value.len().min(maximum));
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() || character.is_control() {
            pending_space = !output.is_empty();
            continue;
        }
        let required = usize::from(pending_space) + character.len_utf8();
        if output.len() + required > maximum {
            break;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    if output.is_empty() {
        "Error event".to_owned()
    } else {
        output
    }
}

const fn map_value_error(_: IssueValueError) -> IssueServiceError {
    IssueServiceError::InvalidSummary
}

const fn map_store_error(error: IssueStoreError) -> IssueServiceError {
    match error {
        IssueStoreError::IdentityCollision => IssueServiceError::IdentityCollision,
        IssueStoreError::NotFound => IssueServiceError::NotFound,
        IssueStoreError::InvalidData => IssueServiceError::InvalidData,
        IssueStoreError::Unavailable => IssueServiceError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use metric_domain::{
        EventId, ProjectId, Timestamp,
        event::{EventLevel, EventPlatform, NormalizedException},
        grouping::group,
        issue::{IssueCommand, IssueCommandResult, IssueMutationKind, IssueSnapshot, IssueStatus},
    };
    use metric_ports::{IssueStore, PortFuture};

    use super::*;

    #[derive(Default)]
    struct FakeIssueStore {
        recorded: Mutex<Option<IssueOccurrence>>,
    }

    impl IssueStore for FakeIssueStore {
        fn apply_occurrence(
            &self,
            occurrence: IssueOccurrence,
        ) -> PortFuture<'_, Result<IssueMutationResult, IssueStoreError>> {
            Box::pin(async move {
                *self.recorded.lock().unwrap() = Some(occurrence.clone());
                Ok(IssueMutationResult {
                    kind: IssueMutationKind::Created,
                    issue: snapshot(&occurrence),
                })
            })
        }

        fn apply_command(
            &self,
            _command: IssueCommand,
        ) -> PortFuture<'_, Result<IssueCommandResult, IssueStoreError>> {
            Box::pin(async { Err(IssueStoreError::NotFound) })
        }

        fn load(
            &self,
            _project_id: ProjectId,
            _issue_id: metric_domain::grouping::IssueId,
        ) -> PortFuture<'_, Result<IssueSnapshot, IssueStoreError>> {
            Box::pin(async { Err(IssueStoreError::NotFound) })
        }

        fn search_titles(
            &self,
            _project_id: ProjectId,
            _query: IssueSearchQuery,
        ) -> PortFuture<'_, Result<Vec<IssueSearchResult>, IssueStoreError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    fn event() -> NormalizedEvent {
        NormalizedEvent {
            project_id: ProjectId::new(7).unwrap(),
            event_id: EventId::from_bytes([4; 16]),
            received_at: Timestamp::from_unix_millis(2_000).unwrap(),
            policy_revision: 1,
            body: NormalizedEventBody {
                occurred_at: Timestamp::from_unix_millis(1_000).unwrap(),
                platform: EventPlatform::Rust,
                level: EventLevel::Error,
                logger: None,
                message: Some("fallback".into()),
                transaction: None,
                release: Some("1.0.0".into()),
                dist: None,
                environment: None,
                fingerprint: Vec::new(),
                exceptions: vec![NormalizedException {
                    ty: Some("Panic".into()),
                    value: Some("line one\nline two".into()),
                    module: None,
                    thread_id: None,
                    mechanism: None,
                    stacktrace: vec![NormalizedFrame {
                        filename: Some("main.rs".into()),
                        absolute_path: None,
                        function: Some("crate::serve".into()),
                        module: None,
                        package: None,
                        instruction_address: None,
                        symbol_address: None,
                        line: Some(42),
                        column: None,
                        in_app: Some(true),
                        context_line: None,
                        pre_context: Vec::new(),
                        post_context: Vec::new(),
                        variables: BTreeMap::new(),
                        unknown: BTreeMap::new(),
                    }],
                    raw_stacktrace: Vec::new(),
                    unknown: BTreeMap::new(),
                }],
                stacktrace: Vec::new(),
                tags: Vec::new(),
                request: None,
                user: None,
                contexts: BTreeMap::new(),
                breadcrumbs: Vec::new(),
                unknown: BTreeMap::new(),
            },
            diagnostics: Vec::new(),
        }
    }

    fn snapshot(occurrence: &IssueOccurrence) -> IssueSnapshot {
        IssueSnapshot {
            project_id: occurrence.project_id,
            issue_id: occurrence.issue_id,
            grouping_key: occurrence.grouping_key,
            title: occurrence.title.clone(),
            culprit: occurrence.culprit.clone(),
            first_seen: occurrence.occurred_at,
            last_seen: occurrence.occurred_at,
            first_event_id: occurrence.event_id,
            latest_event_id: occurrence.event_id,
            representative_event_id: occurrence.event_id,
            occurrence_count: occurrence.increment,
            status: IssueStatus::Open,
            assignee: None,
            workflow: None,
            regression: None,
            first_release: occurrence.release.clone(),
            last_release: occurrence.release.clone(),
            grouping: occurrence.grouping.clone(),
        }
    }

    #[tokio::test]
    async fn service_builds_stable_bounded_summary_and_passes_domain_values() {
        let event = event();
        let grouping = group(event.project_id, 1, &event.body, None).unwrap();
        let store = Arc::new(FakeIssueStore::default());
        let service = IssueService::new(store.clone());
        let result = service.record_occurrence(&event, &grouping).await.unwrap();
        assert_eq!(result.kind, IssueMutationKind::Created);
        let recorded = store.recorded.lock().unwrap();
        let recorded = recorded.as_ref().unwrap();
        assert_eq!(recorded.title.as_str(), "Panic: line one line two");
        assert_eq!(recorded.culprit.as_ref().unwrap().as_str(), "crate::serve");
        assert_eq!(recorded.release.as_ref().unwrap().as_str(), "1.0.0");
    }
}
