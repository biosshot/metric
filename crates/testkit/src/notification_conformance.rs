//! Shared real-adapter notification lifecycle checks, with dates inside retention.
use metric_domain::{
    ProjectId, Timestamp,
    grouping::IssueId,
    issue::IssueTransitionId,
    notifications::{
        AlertRuleId, NotificationDelivery, NotificationDeliveryId, NotificationDeliveryStatus,
        NotificationDestinationId, NotificationPayload,
    },
};
use metric_ports::{NotificationStore, NotificationStoreError};
use std::time::{SystemTime, UNIX_EPOCH};

pub async fn terminal_history(
    store: &dyn NotificationStore,
    project: ProjectId,
    foreign: ProjectId,
) {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let at = |v| Timestamp::from_unix_millis(v).unwrap();
    for (seed, dead) in [(1u8, false), (2, true)] {
        let mut identity = [seed; 16];
        identity[..4].copy_from_slice(&project.get().to_be_bytes());
        let id = NotificationDeliveryId::from_bytes(identity);
        let delivery = NotificationDelivery {
            id,
            project_id: project,
            issue_id: IssueId::from_bytes(identity),
            transition_id: IssueTransitionId::from_bytes(identity),
            rule_id: AlertRuleId::from_bytes(identity),
            action_id: NotificationDestinationId::from_bytes(identity),
            destination_id: NotificationDestinationId::from_bytes(identity),
            payload: NotificationPayload::new(
                b"synthetic\0 notification '; SELECT pg_sleep(999); --".to_vec(),
            )
            .unwrap(),
            status: NotificationDeliveryStatus::Pending,
            attempts: 0,
            next_attempt_at: at(now),
            last_error: None,
            created_at: at(now + i64::from(seed)),
            delivered_at: None,
            delete_at: None,
        };
        store.enqueue_delivery(delivery.clone()).await.unwrap();
        let pending = store
            .list_delivery_history(project, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|v| v.id == id)
            .unwrap();
        assert_eq!(pending, delivery);
        store
            .schedule_retry(id, at(now + 1000), "synthetic_retry")
            .await
            .unwrap();
        let retried = store
            .list_delivery_history(project, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|v| v.id == id)
            .unwrap();
        assert_eq!(retried.next_attempt_at, at(now + 1000));
        assert_eq!(retried.last_error.as_deref(), Some("synthetic_retry"));
        assert_eq!(
            store.schedule_retry(id, at(now + 2000), "bad\ncode").await,
            Err(NotificationStoreError::InvalidData)
        );
        let finish = at(now + 3000);
        let delete = at(now + 86_400_000);
        if dead {
            store
                .mark_dead(id, finish, delete, "synthetic_rejection")
                .await
                .unwrap();
        } else {
            store.mark_delivered(id, finish, delete).await.unwrap();
        }
        let terminal = store
            .list_delivery_history(project, 100)
            .await
            .unwrap()
            .into_iter()
            .find(|v| v.id == id)
            .unwrap();
        assert_eq!(
            terminal.status,
            if dead {
                NotificationDeliveryStatus::Dead
            } else {
                NotificationDeliveryStatus::Delivered
            }
        );
        assert_eq!(terminal.next_attempt_at, finish);
        assert_eq!(terminal.delivered_at, (!dead).then_some(finish));
        assert_eq!(terminal.delete_at, Some(delete));
        assert_eq!(terminal.payload, delivery.payload);
        assert_eq!(terminal.attempts, 0);
        store.enqueue_delivery(delivery).await.unwrap();
        store
            .schedule_retry(id, at(now + 4000), "late_retry")
            .await
            .unwrap();
        store
            .mark_delivered(id, at(now + 5000), at(now + 90_000_000))
            .await
            .unwrap();
        store
            .mark_dead(id, at(now + 6000), at(now + 90_000_000), "late_dead")
            .await
            .unwrap();
        assert_eq!(
            store
                .list_delivery_history(project, 100)
                .await
                .unwrap()
                .into_iter()
                .find(|v| v.id == id)
                .unwrap(),
            terminal
        );
    }
    let history = store.list_delivery_history(project, 100).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(history[0].created_at > history[1].created_at);
    assert!(
        store
            .list_delivery_history(foreign, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.list_delivery_history(project, 0).await,
        Err(NotificationStoreError::InvalidData)
    );
    assert_eq!(
        store.list_delivery_history(project, 501).await,
        Err(NotificationStoreError::InvalidData)
    );
}
