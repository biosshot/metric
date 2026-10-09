//! Shared real-adapter regression for whole-hour Session-statistic repair.
use metric_domain::{
    ProjectId, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
    sessions::{SessionId, SessionState, SessionUpdate},
};
use metric_ports::{DurableOutcome, SessionStore, SignalStoreError};

/// Use an isolated database; the adapter caller owns project bootstrap and cleanup.
pub async fn whole_hour_rebuild(
    store: &dyn SessionStore,
    project: ProjectId,
) -> Result<(), SignalStoreError> {
    // Half-hour bounds, exact upper edge, a window with no selected raw Session,
    // exact whole-hour bounds, and negative dates test both interval edges.
    for (case, (from, until, starts, expected)) in [
        (
            1_800_000,
            5_400_000,
            [600_000, 2_400_000, 4_200_000, 6_000_000],
            2,
        ),
        (
            1_800_000,
            3_600_000,
            [600_000, 2_400_000, 4_200_000, 6_000_000],
            1,
        ),
        (
            2_700_000,
            3_000_000,
            [600_000, 2_400_000, 4_200_000, 6_000_000],
            1,
        ),
        (
            3_600_000,
            7_200_000,
            [600_000, 2_400_000, 4_200_000, 6_000_000],
            1,
        ),
        (
            -1_800_000,
            1_800_000,
            [-3_000_000, -600_000, 600_000, 3_000_000],
            2,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        // Rebuild counts every release/environment bucket in the project. Keep
        // cases in disjoint time ranges and genuinely negative dates in the last.
        let offset = if case == 4 {
            0
        } else {
            (case as i64 + 1) * 172_800_000
        };
        let release = ReleaseId::from_bytes([case as u8 + 30; 16]);
        let environment = EnvironmentId::from_bytes([case as u8 + 60; 16]);
        let updates: Vec<_> = starts
            .into_iter()
            .enumerate()
            .map(|(index, start)| {
                let mut sdk_id = [case as u8 + 1; 16];
                sdk_id[15] = index as u8;
                let mut user = [case as u8; 16];
                user[1] = index as u8;
                SessionUpdate {
                    id: SessionId::derive(project, sdk_id),
                    project_id: project,
                    release_id: release,
                    environment_id: environment,
                    started_at: at(offset + start),
                    updated_at: at(offset + start + 1),
                    state: if index % 2 == 0 {
                        SessionState::Exited
                    } else {
                        SessionState::Crashed
                    },
                    sequence: Some(1),
                    duration_ms: Some(1),
                    user_digest: Some(user),
                }
            })
            .collect();
        assert_eq!(
            store.persist_sessions(updates.clone()).await?,
            vec![DurableOutcome::Accepted; 4]
        );
        let before = store
            .release_health(
                project,
                release,
                at(offset - 3_600_000),
                at(offset + 7_200_000),
            )
            .await?;
        assert_eq!(before.len(), 2);
        for bucket in &before {
            assert_eq!(bucket.sessions, 2);
            assert_eq!(bucket.crashed, 1);
            assert_eq!(bucket.exited, 1);
            assert_eq!(bucket.approximate_users, 2);
            assert_eq!(bucket.approximate_crashed_users, 1);
        }
        // Two passes verify both repair and repeatability, retaining all counters
        // and user sketches, including hours outside the requested raw interval.
        for _ in 0..2 {
            assert_eq!(
                store
                    .rebuild_session_stats(project, at(offset + from), at(offset + until))
                    .await?,
                expected
            );
            assert_eq!(
                store
                    .release_health(
                        project,
                        release,
                        at(offset - 3_600_000),
                        at(offset + 7_200_000)
                    )
                    .await?,
                before
            );
        }
    }
    Ok(())
}
fn at(value: i64) -> Timestamp {
    Timestamp::from_unix_millis(value).unwrap()
}
