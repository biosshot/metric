//! Shared saved-query lifecycle and bounded dashboard refresh orchestration.

use std::sync::Arc;

use metric_domain::{
    ProjectId, Timestamp,
    auth::UserId,
    dashboards::{
        Dashboard, DashboardId, DashboardRefresh, DashboardRefreshInterval, DashboardVariables,
        DashboardWidget, DashboardWidgetId, DashboardWidgetResult, MAX_DASHBOARD_WIDGETS,
        SavedQuery, SavedQueryId, WidgetShape,
    },
    explore::{
        ExploreExpression, ExploreField, ExplorePredicate, ExplorePredicateOp, ExploreQuery,
        ExploreValue,
    },
};
use metric_ports::{Clock, DashboardStore, DashboardStoreError, RandomSource};
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::explore::{ExploreError, ExploreService};

const MAX_LIST: usize = 100;

#[derive(Debug, Clone, Copy)]
pub struct DashboardConfig {
    pub maximum_widgets: usize,
    pub maximum_total_cost: u32,
    pub maximum_refresh_concurrency: usize,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            maximum_widgets: MAX_DASHBOARD_WIDGETS,
            maximum_total_cost: 25_000,
            maximum_refresh_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SavedQueryInput {
    pub name: Box<str>,
    pub query: ExploreQuery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardWidgetInput {
    pub id: Option<DashboardWidgetId>,
    pub title: Box<str>,
    pub saved_query_id: SavedQueryId,
    pub shape: WidgetShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardInput {
    pub name: Box<str>,
    pub widgets: Vec<DashboardWidgetInput>,
    pub refresh_interval: DashboardRefreshInterval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DashboardError {
    #[error("dashboard request is invalid")]
    InvalidRequest,
    #[error("dashboard resource does not exist")]
    NotFound,
    #[error("dashboard resource conflicts with current state")]
    Conflict,
    #[error("dashboard refresh exceeds its total query-cost budget")]
    CostExceeded,
    #[error("dashboard refresh concurrency is exhausted")]
    Capacity,
    #[error("dashboard service is temporarily unavailable")]
    Unavailable,
}

impl DashboardError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "dashboard_invalid_request",
            Self::NotFound => "dashboard_not_found",
            Self::Conflict => "dashboard_conflict",
            Self::CostExceeded => "dashboard_cost_exceeded",
            Self::Capacity => "dashboard_capacity",
            Self::Unavailable => "dashboard_unavailable",
        }
    }
}

pub struct DashboardService {
    store: Arc<dyn DashboardStore>,
    explore: Arc<ExploreService>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    config: DashboardConfig,
    refreshes: Arc<Semaphore>,
}

impl DashboardService {
    pub fn new(
        store: Arc<dyn DashboardStore>,
        explore: Arc<ExploreService>,
        clock: Arc<dyn Clock>,
        random: Arc<dyn RandomSource>,
        config: DashboardConfig,
    ) -> Result<Self, DashboardError> {
        if config.maximum_widgets == 0
            || config.maximum_widgets > MAX_DASHBOARD_WIDGETS
            || config.maximum_total_cost == 0
            || config.maximum_refresh_concurrency == 0
        {
            return Err(DashboardError::InvalidRequest);
        }
        Ok(Self {
            store,
            explore,
            clock,
            random,
            config,
            refreshes: Arc::new(Semaphore::new(config.maximum_refresh_concurrency)),
        })
    }

    pub async fn list_saved_queries(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<SavedQuery>, DashboardError> {
        self.store
            .list_saved_queries(project_id, MAX_LIST)
            .await
            .map_err(map_store)
    }

    pub async fn load_saved_query(
        &self,
        project_id: ProjectId,
        id: SavedQueryId,
    ) -> Result<SavedQuery, DashboardError> {
        self.store
            .load_saved_query(project_id, id)
            .await
            .map_err(map_store)
    }

    pub async fn create_saved_query(
        &self,
        project_id: ProjectId,
        actor: UserId,
        mut input: SavedQueryInput,
    ) -> Result<SavedQuery, DashboardError> {
        input.query.cursor = None;
        promote_query_v2(&mut input.query);
        self.explore
            .plan(project_id, input.query.clone())
            .map_err(map_explore_validation)?;
        let now = self.clock.now();
        let saved_query = SavedQuery {
            id: SavedQueryId::from_bytes(self.random_id()?),
            project_id,
            name: input.name,
            query: input.query,
            revision: 1,
            created_by: actor,
            updated_by: actor,
            created_at: now,
            updated_at: now,
        };
        saved_query
            .validate()
            .map_err(|_| DashboardError::InvalidRequest)?;
        self.store
            .insert_saved_query(saved_query.clone())
            .await
            .map_err(map_store)?;
        Ok(saved_query)
    }

    pub async fn update_saved_query(
        &self,
        project_id: ProjectId,
        id: SavedQueryId,
        actor: UserId,
        expected_revision: u64,
        mut input: SavedQueryInput,
    ) -> Result<SavedQuery, DashboardError> {
        input.query.cursor = None;
        promote_query_v2(&mut input.query);
        self.explore
            .plan(project_id, input.query.clone())
            .map_err(map_explore_validation)?;
        let current = self.load_saved_query(project_id, id).await?;
        let updated = SavedQuery {
            id,
            project_id,
            name: input.name,
            query: input.query,
            revision: expected_revision
                .checked_add(1)
                .ok_or(DashboardError::Conflict)?,
            created_by: current.created_by,
            updated_by: actor,
            created_at: current.created_at,
            updated_at: self.clock.now(),
        };
        updated
            .validate()
            .map_err(|_| DashboardError::InvalidRequest)?;
        self.store
            .replace_saved_query(updated.clone(), expected_revision)
            .await
            .map_err(map_store)?;
        Ok(updated)
    }

    pub async fn delete_saved_query(
        &self,
        project_id: ProjectId,
        id: SavedQueryId,
    ) -> Result<(), DashboardError> {
        self.store
            .delete_saved_query(project_id, id)
            .await
            .map_err(map_store)
    }

    pub async fn list_dashboards(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<Dashboard>, DashboardError> {
        self.store
            .list_dashboards(project_id, MAX_LIST)
            .await
            .map_err(map_store)
    }

    pub async fn load_dashboard(
        &self,
        project_id: ProjectId,
        id: DashboardId,
    ) -> Result<Dashboard, DashboardError> {
        self.store
            .load_dashboard(project_id, id)
            .await
            .map_err(map_store)
    }

    pub async fn create_dashboard(
        &self,
        project_id: ProjectId,
        actor: UserId,
        input: DashboardInput,
    ) -> Result<Dashboard, DashboardError> {
        let widgets = self.validate_widgets(project_id, input.widgets).await?;
        let now = self.clock.now();
        let dashboard = Dashboard {
            id: DashboardId::from_bytes(self.random_id()?),
            project_id,
            name: input.name,
            widgets,
            refresh_interval: input.refresh_interval,
            revision: 1,
            created_by: actor,
            updated_by: actor,
            created_at: now,
            updated_at: now,
        };
        dashboard
            .validate()
            .map_err(|_| DashboardError::InvalidRequest)?;
        self.store
            .insert_dashboard(dashboard.clone())
            .await
            .map_err(map_store)?;
        Ok(dashboard)
    }

    pub async fn update_dashboard(
        &self,
        project_id: ProjectId,
        id: DashboardId,
        actor: UserId,
        expected_revision: u64,
        input: DashboardInput,
    ) -> Result<Dashboard, DashboardError> {
        let current = self.load_dashboard(project_id, id).await?;
        let widgets = self.validate_widgets(project_id, input.widgets).await?;
        let updated = Dashboard {
            id,
            project_id,
            name: input.name,
            widgets,
            refresh_interval: input.refresh_interval,
            revision: expected_revision
                .checked_add(1)
                .ok_or(DashboardError::Conflict)?,
            created_by: current.created_by,
            updated_by: actor,
            created_at: current.created_at,
            updated_at: self.clock.now(),
        };
        updated
            .validate()
            .map_err(|_| DashboardError::InvalidRequest)?;
        self.store
            .replace_dashboard(updated.clone(), expected_revision)
            .await
            .map_err(map_store)?;
        Ok(updated)
    }

    pub async fn delete_dashboard(
        &self,
        project_id: ProjectId,
        id: DashboardId,
    ) -> Result<(), DashboardError> {
        self.store
            .delete_dashboard(project_id, id)
            .await
            .map_err(map_store)
    }

    pub async fn refresh(
        &self,
        project_id: ProjectId,
        id: DashboardId,
        variables: DashboardVariables,
    ) -> Result<DashboardRefresh, DashboardError> {
        validate_variable(&variables.environment)?;
        validate_variable(&variables.release)?;
        let _permit = Arc::clone(&self.refreshes)
            .try_acquire_owned()
            .map_err(|_| DashboardError::Capacity)?;
        let dashboard = self.load_dashboard(project_id, id).await?;
        let now = self.clock.now();
        let mut prepared = Vec::with_capacity(dashboard.widgets.len());
        let mut total_cost = 0_u32;

        for widget in &dashboard.widgets {
            let saved = match self
                .store
                .load_saved_query(project_id, widget.saved_query_id)
                .await
            {
                Ok(saved) => saved,
                Err(DashboardStoreError::NotFound) => {
                    prepared.push(PreparedWidget::Failure(
                        widget.id,
                        "saved_query_missing".into(),
                    ));
                    continue;
                }
                Err(DashboardStoreError::InvalidData) => {
                    prepared.push(PreparedWidget::Failure(
                        widget.id,
                        "saved_query_invalid".into(),
                    ));
                    continue;
                }
                Err(error) => return Err(map_store(error)),
            };
            let query = match refresh_query(saved.query, now, &variables) {
                Ok(query) => query,
                Err(code) => {
                    prepared.push(PreparedWidget::Failure(widget.id, code.into()));
                    continue;
                }
            };
            let plan = match self.explore.plan(project_id, query) {
                Ok(plan) => plan,
                Err(error) => {
                    prepared.push(PreparedWidget::Failure(widget.id, error.code().into()));
                    continue;
                }
            };
            if shape_for(&plan.query) != widget.shape {
                prepared.push(PreparedWidget::Failure(
                    widget.id,
                    "widget_shape_mismatch".into(),
                ));
                continue;
            }
            total_cost = total_cost.saturating_add(plan.cost);
            prepared.push(PreparedWidget::Plan(widget.id, Box::new(plan)));
        }

        if total_cost > self.config.maximum_total_cost {
            return Err(DashboardError::CostExceeded);
        }

        let mut widgets = Vec::with_capacity(prepared.len());
        for widget in prepared {
            match widget {
                PreparedWidget::Failure(widget_id, error_code) => {
                    widgets.push(DashboardWidgetResult {
                        widget_id,
                        cost: None,
                        result: None,
                        error_code: Some(error_code),
                    });
                }
                PreparedWidget::Plan(widget_id, plan) => {
                    let cost = plan.cost;
                    match self.explore.execute(*plan).await {
                        Ok(result) => widgets.push(DashboardWidgetResult {
                            widget_id,
                            cost: Some(cost),
                            result: Some(result),
                            error_code: None,
                        }),
                        Err(error) => widgets.push(DashboardWidgetResult {
                            widget_id,
                            cost: Some(cost),
                            result: None,
                            error_code: Some(error.code().into()),
                        }),
                    }
                }
            }
        }
        Ok(DashboardRefresh {
            dashboard_id: dashboard.id,
            refreshed_at: now,
            total_cost,
            widgets,
        })
    }

    async fn validate_widgets(
        &self,
        project_id: ProjectId,
        inputs: Vec<DashboardWidgetInput>,
    ) -> Result<Vec<DashboardWidget>, DashboardError> {
        if inputs.is_empty() || inputs.len() > self.config.maximum_widgets {
            return Err(DashboardError::InvalidRequest);
        }
        let mut widgets = Vec::with_capacity(inputs.len());
        for input in inputs {
            let saved = self
                .store
                .load_saved_query(project_id, input.saved_query_id)
                .await
                .map_err(map_store)?;
            let plan = self
                .explore
                .plan(project_id, saved.query)
                .map_err(map_explore_validation)?;
            if shape_for(&plan.query) != input.shape {
                return Err(DashboardError::InvalidRequest);
            }
            widgets.push(DashboardWidget {
                id: input
                    .id
                    .unwrap_or(DashboardWidgetId::from_bytes(self.random_id()?)),
                title: input.title,
                saved_query_id: input.saved_query_id,
                shape: input.shape,
            });
        }
        Ok(widgets)
    }

    fn random_id(&self) -> Result<[u8; 16], DashboardError> {
        let mut id = [0_u8; 16];
        self.random
            .fill_bytes(&mut id)
            .map_err(|_| DashboardError::Unavailable)?;
        Ok(id)
    }
}

fn promote_query_v2(query: &mut ExploreQuery) {
    if query.expression.is_none() {
        query.expression = Some(ExploreExpression::And(
            std::mem::take(&mut query.predicates)
                .into_iter()
                .map(ExploreExpression::Predicate)
                .collect(),
        ));
    }
}

enum PreparedWidget {
    Failure(DashboardWidgetId, Box<str>),
    Plan(DashboardWidgetId, Box<metric_domain::explore::ExplorePlan>),
}

fn refresh_query(
    mut query: ExploreQuery,
    now: Timestamp,
    variables: &DashboardVariables,
) -> Result<ExploreQuery, &'static str> {
    let all_time = query.from.unix_millis() == 0;
    let range = query
        .until
        .unix_millis()
        .saturating_sub(query.from.unix_millis());
    query.until = now;
    if !all_time {
        query.from = Timestamp::from_unix_millis(now.unix_millis().saturating_sub(range))
            .map_err(|_| "saved_query_invalid")?;
    }
    query.cursor = None;
    apply_variable(
        &mut query,
        ExploreField::Environment,
        variables.environment.as_deref(),
    )?;
    apply_variable(
        &mut query,
        ExploreField::Release,
        variables.release.as_deref(),
    )?;
    Ok(query)
}

fn apply_variable(
    query: &mut ExploreQuery,
    field: ExploreField,
    value: Option<&str>,
) -> Result<(), &'static str> {
    let Some(value) = value else {
        return Ok(());
    };
    if !field.accepted_by(query.dataset) {
        return Err("dashboard_variable_unsupported");
    }
    query
        .predicates
        .retain(|predicate| predicate.field != field);
    query.predicates.push(ExplorePredicate {
        field,
        op: ExplorePredicateOp::Exact,
        value: Some(ExploreValue::String(value.into())),
        upper: None,
    });
    Ok(())
}

fn shape_for(query: &ExploreQuery) -> WidgetShape {
    if query.aggregates.len() == 1 && query.group_by.is_empty() && query.interval.is_none() {
        WidgetShape::Number
    } else if query.interval.is_some() {
        WidgetShape::Timeseries
    } else {
        WidgetShape::Table
    }
}

fn validate_variable(value: &Option<Box<str>>) -> Result<(), DashboardError> {
    if value.as_deref().is_some_and(|value| {
        value.trim().is_empty()
            || value.len() > 256
            || value.chars().any(|character| character == '\0')
    }) {
        return Err(DashboardError::InvalidRequest);
    }
    Ok(())
}

fn map_store(error: DashboardStoreError) -> DashboardError {
    match error {
        DashboardStoreError::NotFound => DashboardError::NotFound,
        DashboardStoreError::Conflict => DashboardError::Conflict,
        DashboardStoreError::InvalidData => DashboardError::InvalidRequest,
        DashboardStoreError::Unavailable => DashboardError::Unavailable,
    }
}

fn map_explore_validation(error: ExploreError) -> DashboardError {
    match error {
        ExploreError::InvalidQuery | ExploreError::CostExceeded => DashboardError::InvalidRequest,
        ExploreError::Capacity => DashboardError::Capacity,
        ExploreError::Unavailable => DashboardError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicU8, Ordering},
    };

    use metric_domain::{
        dashboards::*,
        explore::{
            ExploreAggregate, ExploreAggregateKind, ExploreDataset, ExplorePlan, ExploreResult,
        },
    };
    use metric_ports::{
        DashboardStoreError, ExploreStore, ExploreStoreError, PortFuture, RandomError,
    };

    use super::*;

    struct FixedClock;
    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_unix_millis(2_000_000).unwrap()
        }
    }

    struct Random(AtomicU8);
    impl RandomSource for Random {
        fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
            output.fill(self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1));
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        saved: Mutex<Vec<SavedQuery>>,
        dashboards: Mutex<Vec<Dashboard>>,
    }

    impl DashboardStore for MemoryStore {
        fn list_saved_queries(
            &self,
            project_id: ProjectId,
            _: usize,
        ) -> PortFuture<'_, Result<Vec<SavedQuery>, DashboardStoreError>> {
            Box::pin(async move {
                Ok(self
                    .saved
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|item| item.project_id == project_id)
                    .cloned()
                    .collect())
            })
        }

        fn load_saved_query(
            &self,
            project_id: ProjectId,
            id: SavedQueryId,
        ) -> PortFuture<'_, Result<SavedQuery, DashboardStoreError>> {
            Box::pin(async move {
                self.saved
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|item| item.project_id == project_id && item.id == id)
                    .cloned()
                    .ok_or(DashboardStoreError::NotFound)
            })
        }

        fn insert_saved_query(
            &self,
            value: SavedQuery,
        ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
            Box::pin(async move {
                self.saved.lock().unwrap().push(value);
                Ok(())
            })
        }

        fn replace_saved_query(
            &self,
            value: SavedQuery,
            expected: u64,
        ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
            Box::pin(async move {
                let mut values = self.saved.lock().unwrap();
                let current = values
                    .iter_mut()
                    .find(|item| item.project_id == value.project_id && item.id == value.id)
                    .ok_or(DashboardStoreError::NotFound)?;
                if current.revision != expected {
                    return Err(DashboardStoreError::Conflict);
                }
                *current = value;
                Ok(())
            })
        }

        fn delete_saved_query(
            &self,
            project_id: ProjectId,
            id: SavedQueryId,
        ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
            Box::pin(async move {
                let mut values = self.saved.lock().unwrap();
                let before = values.len();
                values.retain(|item| item.project_id != project_id || item.id != id);
                (values.len() != before)
                    .then_some(())
                    .ok_or(DashboardStoreError::NotFound)
            })
        }

        fn list_dashboards(
            &self,
            project_id: ProjectId,
            _: usize,
        ) -> PortFuture<'_, Result<Vec<Dashboard>, DashboardStoreError>> {
            Box::pin(async move {
                Ok(self
                    .dashboards
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|item| item.project_id == project_id)
                    .cloned()
                    .collect())
            })
        }

        fn load_dashboard(
            &self,
            project_id: ProjectId,
            id: DashboardId,
        ) -> PortFuture<'_, Result<Dashboard, DashboardStoreError>> {
            Box::pin(async move {
                self.dashboards
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|item| item.project_id == project_id && item.id == id)
                    .cloned()
                    .ok_or(DashboardStoreError::NotFound)
            })
        }

        fn insert_dashboard(
            &self,
            value: Dashboard,
        ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
            Box::pin(async move {
                self.dashboards.lock().unwrap().push(value);
                Ok(())
            })
        }

        fn replace_dashboard(
            &self,
            value: Dashboard,
            expected: u64,
        ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
            Box::pin(async move {
                let mut values = self.dashboards.lock().unwrap();
                let current = values
                    .iter_mut()
                    .find(|item| item.project_id == value.project_id && item.id == value.id)
                    .ok_or(DashboardStoreError::NotFound)?;
                if current.revision != expected {
                    return Err(DashboardStoreError::Conflict);
                }
                *current = value;
                Ok(())
            })
        }

        fn delete_dashboard(
            &self,
            project_id: ProjectId,
            id: DashboardId,
        ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
            Box::pin(async move {
                let mut values = self.dashboards.lock().unwrap();
                let before = values.len();
                values.retain(|item| item.project_id != project_id || item.id != id);
                (values.len() != before)
                    .then_some(())
                    .ok_or(DashboardStoreError::NotFound)
            })
        }
    }

    struct Explore;
    impl ExploreStore for Explore {
        fn execute(
            &self,
            _plan: ExplorePlan,
        ) -> PortFuture<'_, Result<ExploreResult, ExploreStoreError>> {
            Box::pin(async move {
                Ok(ExploreResult {
                    rows: Vec::new(),
                    next: None,
                })
            })
        }
    }

    fn service() -> (Arc<MemoryStore>, DashboardService) {
        let store = Arc::new(MemoryStore::default());
        let explore = Arc::new(
            ExploreService::new(Arc::new(Explore), crate::explore::ExploreConfig::default())
                .unwrap(),
        );
        let service = DashboardService::new(
            store.clone(),
            explore,
            Arc::new(FixedClock),
            Arc::new(Random(AtomicU8::new(0))),
            DashboardConfig::default(),
        )
        .unwrap();
        (store, service)
    }

    fn count_query(dataset: ExploreDataset) -> ExploreQuery {
        ExploreQuery {
            dataset,
            from: Timestamp::from_unix_millis(1_000_000).unwrap(),
            until: Timestamp::from_unix_millis(2_000_000).unwrap(),
            predicates: Vec::new(),
            expression: None,
            aggregates: vec![ExploreAggregate {
                kind: ExploreAggregateKind::Count,
                field: None,
                alias: "count".into(),
            }],
            group_by: Vec::new(),
            interval: None,
            cursor: None,
            limit: 50,
        }
    }

    #[test]
    fn dashboard_refresh_preserves_the_all_time_epoch_sentinel() {
        let mut query = count_query(ExploreDataset::Logs);
        query.from = Timestamp::from_unix_millis(0).unwrap();
        query.until = Timestamp::from_unix_millis(2_000_000).unwrap();
        let now = Timestamp::from_unix_millis(9_000_000).unwrap();

        let refreshed = refresh_query(query, now, &DashboardVariables::default()).unwrap();

        assert_eq!(refreshed.from.unix_millis(), 0);
        assert_eq!(refreshed.until, now);
    }

    #[tokio::test]
    async fn lifecycle_revalidates_queries_and_refresh_reports_deleted_widget() {
        let (_, service) = service();
        let project = ProjectId::new(7).unwrap();
        let actor = UserId::new(9).unwrap();
        let saved = service
            .create_saved_query(
                project,
                actor,
                SavedQueryInput {
                    name: "Log count".into(),
                    query: count_query(ExploreDataset::Logs),
                },
            )
            .await
            .unwrap();
        let dashboard = service
            .create_dashboard(
                project,
                actor,
                DashboardInput {
                    name: "Operations".into(),
                    widgets: vec![DashboardWidgetInput {
                        id: None,
                        title: "Log count".into(),
                        saved_query_id: saved.id,
                        shape: WidgetShape::Number,
                    }],
                    refresh_interval: DashboardRefreshInterval::Manual,
                },
            )
            .await
            .unwrap();
        service.delete_saved_query(project, saved.id).await.unwrap();
        let refresh = service
            .refresh(project, dashboard.id, DashboardVariables::default())
            .await
            .unwrap();
        assert_eq!(
            refresh.widgets[0].error_code.as_deref(),
            Some("saved_query_missing")
        );
    }

    #[tokio::test]
    async fn variables_are_applied_per_widget_and_unsupported_datasets_fail_partially() {
        let (_, service) = service();
        let project = ProjectId::new(7).unwrap();
        let actor = UserId::new(9).unwrap();
        let log = service
            .create_saved_query(
                project,
                actor,
                SavedQueryInput {
                    name: "Logs".into(),
                    query: count_query(ExploreDataset::Logs),
                },
            )
            .await
            .unwrap();
        let errors = service
            .create_saved_query(
                project,
                actor,
                SavedQueryInput {
                    name: "Errors".into(),
                    query: count_query(ExploreDataset::Errors),
                },
            )
            .await
            .unwrap();
        let dashboard = service
            .create_dashboard(
                project,
                actor,
                DashboardInput {
                    name: "Mixed".into(),
                    widgets: vec![
                        DashboardWidgetInput {
                            id: None,
                            title: "Logs".into(),
                            saved_query_id: log.id,
                            shape: WidgetShape::Number,
                        },
                        DashboardWidgetInput {
                            id: None,
                            title: "Errors".into(),
                            saved_query_id: errors.id,
                            shape: WidgetShape::Number,
                        },
                    ],
                    refresh_interval: DashboardRefreshInterval::Manual,
                },
            )
            .await
            .unwrap();
        let refresh = service
            .refresh(
                project,
                dashboard.id,
                DashboardVariables {
                    environment: Some("production".into()),
                    release: None,
                },
            )
            .await
            .unwrap();
        assert!(refresh.widgets[0].error_code.is_none());
        assert_eq!(
            refresh.widgets[1].error_code.as_deref(),
            Some("dashboard_variable_unsupported")
        );
    }
}
