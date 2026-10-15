//! Shared real-adapter regressions for whole-hour Span-statistic repair.
use metric_domain::{
    ProjectId, Timestamp,
    signals::{SignalBody, SpanId, SpanOperationClass, SpanRecord, SpanRecordId, TraceId},
};
use metric_ports::{DurableOutcome, PerformanceQuery, SignalStore, SignalStoreError};
use std::time::{SystemTime, UNIX_EPOCH};

/// The caller owns an isolated database, project bootstrap and cleanup.
pub async fn whole_hour_rebuild(
    store: &dyn SignalStore,
    project: ProjectId,
) -> Result<(), SignalStoreError> {
    // Current synthetic dates keep source and derived rows inside their TTLs.
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let day = now.div_euclid(86_400_000) * 86_400_000;
    for (case, (from, until, repaired)) in [
        (1_800_000, 5_400_000, 4),
        (1_800_000, 3_600_000, 2),
        (2_700_000, 3_000_000, 2),
        (3_600_000, 7_200_000, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let base = day + case as i64 * 14_400_000;
        let records: Vec<_> = [600_000, 2_400_000, 4_200_000, 6_000_000]
            .into_iter()
            .enumerate()
            .map(|(index, start)| {
                let trace = TraceId::from_bytes([case as u8 + 1; 16]);
                let span = SpanId::from_bytes([index as u8 + 1; 8]);
                SpanRecord {
                    id: SpanRecordId::deterministic(project, trace, span),
                    project_id: project,
                    received_at: at(now),
                    started_at_ns: (base + start) * 1_000_000,
                    duration_ns: (index as i64 + 1) * 1_000_000,
                    trace_id: trace,
                    span_id: span,
                    parent_span_id: None,
                    is_segment: true,
                    operation_class: SpanOperationClass::HttpServer,
                    operation: "http.server".into(),
                    status: if index % 2 == 0 {
                        "ok"
                    } else {
                        "internal_error"
                    }
                    .into(),
                    name: format!("whole-hour-{case}").into(),
                    environment: Some("production".into()),
                    release: Some("api@1".into()),
                    service: Some("api".into()),
                    insight_flags: 0,
                    body: SignalBody::new(b"{}".to_vec()),
                }
            })
            .collect();
        assert_eq!(
            store.persist_spans(records).await?,
            vec![DurableOutcome::Accepted; 4]
        );
        let query = PerformanceQuery {
            from: at(base),
            until: at(base + 7_200_000),
            environment: None,
            release: None,
            service: None,
            limit: 200,
        };
        let before = store.performance(project, query.clone()).await?;
        assert_eq!(before.len(), 2);
        for bucket in &before {
            assert_eq!(bucket.count, 2);
            assert_eq!(bucket.failure_count, 1);
        }
        for _ in 0..2 {
            assert_eq!(
                store
                    .rebuild_span_stats(project, at(base + from), at(base + until))
                    .await?,
                repaired
            );
            assert_eq!(store.performance(project, query.clone()).await?, before);
        }
        for (from, until) in [
            (base, base),
            (base + 1, base),
            (253_402_300_799_998, 253_402_300_799_999),
        ] {
            assert_eq!(
                store.rebuild_span_stats(project, at(from), at(until)).await,
                Err(SignalStoreError::InvalidData)
            );
            assert_eq!(store.performance(project, query.clone()).await?, before);
        }
    }
    Ok(())
}
fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
