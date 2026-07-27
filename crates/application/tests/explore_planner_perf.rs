use std::{hint::black_box, sync::Arc, time::Instant};

use metric_application::explore::{ExploreConfig, ExploreService};
use metric_domain::{
    ProjectId, Timestamp,
    explore::{
        ExploreAggregate, ExploreAggregateKind, ExploreDataset, ExploreField, ExplorePlan,
        ExplorePredicate, ExplorePredicateOp, ExploreQuery, ExploreResult, ExploreValue,
    },
};
use metric_ports::{ExploreStore, ExploreStoreError, PortFuture};

struct NoopStore;

impl ExploreStore for NoopStore {
    fn execute(&self, _: ExplorePlan) -> PortFuture<'_, Result<ExploreResult, ExploreStoreError>> {
        Box::pin(async {
            Ok(ExploreResult {
                rows: Vec::new(),
                next: None,
            })
        })
    }
}

#[test]
#[ignore = "explicit Phase 32 RPS regression baseline"]
fn explore_typed_planner_rps() {
    const QUERIES: u32 = 250_000;
    const MINIMUM_RPS: f64 = 100_000.0;
    let service = ExploreService::new(Arc::new(NoopStore), ExploreConfig::default()).unwrap();
    let project_id = ProjectId::new(42).unwrap();
    let query = ExploreQuery {
        dataset: ExploreDataset::Spans,
        from: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        until: Timestamp::from_unix_millis(1_700_003_600_000).unwrap(),
        predicates: vec![ExplorePredicate {
            field: ExploreField::Service,
            op: ExplorePredicateOp::Exact,
            value: Some(ExploreValue::String("checkout".into())),
            upper: None,
        }],
        aggregates: vec![
            ExploreAggregate {
                kind: ExploreAggregateKind::Count,
                field: None,
                alias: "count".into(),
            },
            ExploreAggregate {
                kind: ExploreAggregateKind::P95,
                field: Some(ExploreField::DurationMs),
                alias: "p95_duration_ms".into(),
            },
        ],
        group_by: vec![ExploreField::OperationClass, ExploreField::IsSegment],
        interval: Some(metric_domain::explore::ExploreInterval::FiveMinutes),
        cursor: None,
        limit: 50,
    };
    let started = Instant::now();
    for _ in 0..QUERIES {
        black_box(service.plan(project_id, black_box(query.clone())).unwrap());
    }
    let elapsed = started.elapsed();
    let rps = f64::from(QUERIES) / elapsed.as_secs_f64();
    eprintln!(
        "Explore typed planner: {rps:.0} queries/s, queries={QUERIES}, elapsed_ms={}",
        elapsed.as_millis()
    );
    assert!(
        rps >= MINIMUM_RPS,
        "Explore planner {rps:.0} RPS is below {MINIMUM_RPS:.0} RPS"
    );
}
