use super::*;
use crate::{
    MigrationPolicy, PostgresSchemaManager,
    test_support::{CommitReplyProxy, Fixture, seed_projects},
};
use metric_domain::{
    explore::ExploreDataset,
    monitors::{MonitorId, MonitorRunStatus},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn time(v: i64) -> Timestamp {
    at(v).unwrap()
}
fn project(v: i32) -> ProjectId {
    ProjectId::new(v).unwrap()
}
fn destination(p: i32, seed: u8) -> NotificationDestination {
    NotificationDestination {
        id: NotificationDestinationId::from_bytes([seed; 16]),
        project_id: project(p),
        kind: NotificationDestinationKind::Webhook,
        endpoint: WebhookEndpoint::new("https://example.com/hook?q=';DROP%20TABLE").unwrap(),
        sealed_secret: SealedWebhookSecret::new(vec![42; 48]).unwrap(),
        telegram: None,
        smtp: None,
        enabled: true,
        created_at: time(1000),
        updated_at: time(1000),
    }
}
fn rule(p: i32, seed: u8, destination: NotificationDestinationId) -> AlertRule {
    AlertRule {
        id: AlertRuleId::from_bytes([seed; 16]),
        project_id: project(p),
        name: RuleName::new("'; SELECT pg_sleep(999); -- Юникод").unwrap(),
        enabled: true,
        triggers: vec![
            IssueNotificationKind::NewIssue,
            IssueNotificationKind::Regression,
            IssueNotificationKind::Resolved,
        ]
        .into_boxed_slice(),
        aggregate: None,
        monitor: None,
        destination_ids: vec![destination].into_boxed_slice(),
        cooldown_minutes: 0,
        storm_limit_per_hour: 10,
        next_evaluation_at: None,
        last_triggered_at: None,
        storm_window_started_at: None,
        storm_count: 0,
        threshold_met: false,
        created_at: time(1000),
        updated_at: time(1000),
    }
}
fn delivery(p: i32, sequence: u128, r: &AlertRule) -> NotificationDelivery {
    NotificationDelivery {
        id: notification_delivery_id(
            IssueTransitionId::from_bytes(sequence.to_be_bytes()),
            r.id,
            r.destination_ids[0],
        ),
        project_id: project(p),
        issue_id: IssueId::from_bytes([3; 16]),
        transition_id: IssueTransitionId::from_bytes(sequence.to_be_bytes()),
        rule_id: r.id,
        action_id: r.destination_ids[0],
        destination_id: r.destination_ids[0],
        payload: NotificationPayload::new(b"synthetic\0 payload".to_vec()).unwrap(),
        status: NotificationDeliveryStatus::Pending,
        attempts: 0,
        next_attempt_at: time(1000),
        last_error: None,
        created_at: time(1000 + sequence as i64),
        delivered_at: None,
        delete_at: None,
    }
}
async fn setup() -> (Fixture, PostgresDatabase, PostgresNotificationStore) {
    let fixture = Fixture::new().await;
    PostgresSchemaManager::ensure(&fixture.uri, &fixture.policy, &MigrationPolicy::default())
        .await
        .unwrap();
    let database = PostgresDatabase::connect(&fixture.uri, &fixture.policy)
        .await
        .unwrap();
    seed_projects(database.clone(), 32).await;
    let store = PostgresNotificationStore::new(database.clone());
    (fixture, database, store)
}
async fn issue_transition(
    database: &PostgresDatabase,
    store: &PostgresNotificationStore,
) -> IssueNotificationTransition {
    use metric_ports::IssueStore;
    let issues =
        crate::PostgresIssueStore::new(database.clone(), crate::IssueCodecConfig::default())
            .unwrap();
    let mut issue =
        crate::test_support::finalized(1, 1, EventId::from_bytes([1; 16]), 1000, 1000, None, None)
            .issue;
    issue.title = IssueTitle::new("Synthetic\0 panic '; SELECT 1; --").unwrap();
    issues.apply_occurrence(issue).await.unwrap();
    store.pending_transitions(100).await.unwrap().remove(0)
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn shared_terminal_history_and_retry_immutability() {
    let (fixture, database, store) = setup().await;
    metric_testkit::notification_conformance::terminal_history(&store, project(1), project(2))
        .await;
    database.close().await;
    fixture.cleanup().await;
}
#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn shared_all_aggregate_datasets_roundtrip_claim_complete() {
    let (fixture, database, store) = setup().await;
    metric_testkit::notification_conformance::aggregate_datasets(&store, project(1), project(2))
        .await;
    database.close().await;
    fixture.cleanup().await;
}
#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn scoped_control_plane_all_destination_kinds_rule_kinds_and_corruption() {
    let (fixture, database, store) = setup().await;
    let webhook = destination(1, 11);
    let mut telegram = destination(1, 12);
    telegram.kind = NotificationDestinationKind::Telegram;
    telegram.endpoint = WebhookEndpoint::new("-1001234567890").unwrap();
    telegram.telegram = Some(TelegramDestination {
        api_base: TelegramApiBase::new("http://telegram.internal:8081/proxy/").unwrap(),
        message_thread_id: Some(73),
        bot_id: Some(123),
        bot_username: Some(NotificationText::new("metric_bot", 64).unwrap()),
        bot_display_name: Some(NotificationText::new("Metric Bot", 512).unwrap()),
        chat_kind: Some(TelegramChatKind::Supergroup),
        chat_username: Some(NotificationText::new("metric_alerts", 64).unwrap()),
        chat_display_name: Some(NotificationText::new("Metric Alerts", 512).unwrap()),
    });
    let mut smtp = destination(1, 13);
    smtp.kind = NotificationDestinationKind::SmtpEmail;
    smtp.endpoint = WebhookEndpoint::new("smtp.example.com").unwrap();
    smtp.smtp = Some(SmtpDestination {
        port: 587,
        security: SmtpSecurity::StartTls,
        username: NotificationText::new("metric", 320).unwrap(),
        from: NotificationText::new("metric@example.com", 320).unwrap(),
        recipients: vec![NotificationText::new("ops@example.com", 320).unwrap()].into_boxed_slice(),
    });
    for d in [&webhook, &telegram, &smtp] {
        store.upsert_destination(d.clone()).await.unwrap();
    }
    let actual = store.list_destinations(project(1), 100).await.unwrap();
    assert_eq!(
        actual,
        vec![webhook.clone(), telegram.clone(), smtp.clone()]
    );
    assert!(
        store
            .list_destinations(project(2), 100)
            .await
            .unwrap()
            .is_empty()
    );
    let mut stolen = webhook.clone();
    stolen.project_id = project(2);
    assert_eq!(
        store.upsert_destination(stolen).await,
        Err(NotificationStoreError::InvalidData)
    );
    store
        .delete_destination(project(2), webhook.id)
        .await
        .unwrap();
    assert_eq!(
        store
            .list_destinations(project(1), 100)
            .await
            .unwrap()
            .len(),
        3
    );
    let basic = rule(1, 31, webhook.id);
    store.upsert_rule(basic.clone()).await.unwrap();
    for kind in [
        IssueNotificationKind::NewIssue,
        IssueNotificationKind::Regression,
        IssueNotificationKind::Resolved,
    ] {
        assert_eq!(
            store.matching_rules(project(1), kind, 1).await.unwrap(),
            vec![basic.clone()]
        );
    }
    let mut aggregate = rule(1, 32, telegram.id);
    aggregate.triggers = Box::new([]);
    aggregate.aggregate = Some(AggregateAlert {
        dataset: ExploreDataset::Metrics,
        lookback_minutes: 5,
        evaluation_interval_minutes: 1,
        threshold: u64::MAX,
        environment: None,
        release: None,
        notify_resolved: true,
    });
    aggregate.next_evaluation_at = Some(time(1000));
    let mut monitor = rule(1, 33, smtp.id);
    monitor.triggers = Box::new([]);
    monitor.monitor = Some(MonitorAlert {
        monitor_id: MonitorId::derive(project(1), "cron", "prod"),
        outcomes: vec![
            MonitorRunStatus::Error,
            MonitorRunStatus::Timeout,
            MonitorRunStatus::Missed,
        ]
        .into_boxed_slice(),
        notify_resolved: true,
    });
    store.upsert_rule(aggregate.clone()).await.unwrap();
    store.upsert_rule(monitor.clone()).await.unwrap();
    assert_eq!(
        store.list_rules(project(1), 100).await.unwrap(),
        vec![basic.clone(), aggregate.clone(), monitor.clone()]
    );
    let mut stolen = basic.clone();
    stolen.project_id = project(2);
    assert_eq!(
        store.upsert_rule(stolen).await,
        Err(NotificationStoreError::InvalidData)
    );
    let mut second = basic.clone();
    second.id = AlertRuleId::from_bytes([34; 16]);
    store.upsert_rule(second).await.unwrap();
    assert_eq!(
        store
            .matching_rules(project(1), IssueNotificationKind::NewIssue, 1)
            .await,
        Err(NotificationStoreError::InvalidData)
    );
    store.delete_rule(project(2), basic.id).await.unwrap();
    assert_eq!(store.list_rules(project(1), 100).await.unwrap().len(), 4);
    store.delete_rule(project(1), basic.id).await.unwrap();
    sqlx::query("UPDATE metric.notification_rules SET config=$2 WHERE rule_id=$1")
        .bind(aggregate.id.as_bytes().as_slice())
        .bind(vec![2u8, 0])
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.list_rules(project(1), 100).await,
        Err(NotificationStoreError::InvalidData)
    );
    assert!(store.list_rules(project(2), 100).await.unwrap().is_empty());
    store.upsert_rule(aggregate).await.unwrap();
    sqlx::query("UPDATE metric.notification_destinations SET config_size=config_size+1 WHERE destination_id=$1").bind(telegram.id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    assert_eq!(
        store.list_destinations(project(1), 100).await,
        Err(NotificationStoreError::InvalidData)
    );
    store.upsert_destination(telegram).await.unwrap();
    store.delete_destination(project(1), smtp.id).await.unwrap();
    assert_eq!(
        store
            .list_destinations(project(1), 100)
            .await
            .unwrap()
            .len(),
        2
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn transition_expansion_is_atomic_scoped_and_retryable_after_lost_commit() {
    let (fixture, database, store) = setup().await;
    let destination = destination(1, 11);
    store.upsert_destination(destination.clone()).await.unwrap();
    let r = rule(1, 31, destination.id);
    store.upsert_rule(r.clone()).await.unwrap();
    let transition = issue_transition(&database, &store).await;
    let mut d = delivery(1, 1, &r);
    d.transition_id = transition.transition_id;
    d.issue_id = transition.issue_id;
    d.id = notification_delivery_id(d.transition_id, r.id, d.action_id);
    let mut invalid = d.clone();
    invalid.project_id = project(2);
    assert_eq!(
        store
            .expand_transition(transition.clone(), vec![invalid])
            .await,
        Err(NotificationStoreError::InvalidData)
    );
    assert_eq!(store.pending_transitions(10).await.unwrap().len(), 1);
    sqlx::raw_sql("CREATE FUNCTION metric.fail_notification_consume() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic consume failure''; END'; CREATE TRIGGER fail_notification_consume BEFORE DELETE ON metric.issue_transitions FOR EACH ROW EXECUTE FUNCTION metric.fail_notification_consume()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .expand_transition(transition.clone(), vec![d.clone()])
            .await,
        Err(NotificationStoreError::Unavailable)
    );
    assert!(
        store
            .list_delivery_history(project(1), 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.pending_transitions(10).await.unwrap().len(), 1);
    sqlx::query("DROP TRIGGER fail_notification_consume ON metric.issue_transitions")
        .execute(&database.pool)
        .await
        .unwrap();
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxy_db = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied = PostgresNotificationStore::new(proxy_db.clone());
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied
            .expand_transition(transition.clone(), vec![d.clone()])
            .await,
        Err(NotificationStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    assert!(store.pending_transitions(10).await.unwrap().is_empty());
    store
        .expand_transition(transition, vec![d.clone()])
        .await
        .unwrap();
    assert_eq!(
        store.list_delivery_history(project(1), 100).await.unwrap(),
        vec![d]
    );
    proxy_db.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_claims_fairness_retries_missing_targets_and_project_fences() {
    let (fixture, database, store) = setup().await;
    let a = destination(1, 11);
    let b = destination(2, 12);
    store.upsert_destination(a.clone()).await.unwrap();
    store.upsert_destination(b.clone()).await.unwrap();
    let ra = rule(1, 31, a.id);
    let rb = rule(2, 32, b.id);
    store.upsert_rule(ra.clone()).await.unwrap();
    store.upsert_rule(rb.clone()).await.unwrap();
    for sequence in 1..=3 {
        store
            .enqueue_delivery(delivery(1, sequence, &ra))
            .await
            .unwrap();
    }
    store.enqueue_delivery(delivery(2, 4, &rb)).await.unwrap();
    let first = store
        .claim_due(time(1000), time(2000), 10)
        .await
        .unwrap()
        .unwrap();
    let other = store
        .claim_due(time(1000), time(2000), 10)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.delivery.project_id, other.delivery.project_id);
    assert_eq!(first.attempt, 1);
    assert_eq!(other.attempt, 1);
    let (x, y) = tokio::join!(
        store.claim_due(time(1000), time(2000), 10),
        store.claim_due(time(1000), time(2000), 10)
    );
    let x = x.unwrap().unwrap();
    let y = y.unwrap().unwrap();
    assert_ne!(x.delivery.id, y.delivery.id);
    assert!(
        store
            .claim_due(time(1999), time(3000), 10)
            .await
            .unwrap()
            .is_none()
    );
    let again = store
        .claim_due(time(2000), time(3000), 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.attempt, 2);
    let mut foreign = delivery(1, 99, &rb);
    foreign.rule_id = ra.id;
    assert_eq!(
        store.enqueue_delivery(foreign).await,
        Err(NotificationStoreError::InvalidData)
    );
    sqlx::query("UPDATE metric.projects SET state=2 WHERE id=2")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store.enqueue_delivery(delivery(2, 99, &rb)).await,
        Err(NotificationStoreError::InvalidData)
    );
    let orphan_rule = rule(1, 41, NotificationDestinationId::from_bytes([42; 16]));
    store
        .enqueue_delivery(delivery(1, 100, &orphan_rule))
        .await
        .unwrap();
    assert_eq!(
        store.claim_due(time(1001), time(4000), 10).await,
        Err(NotificationStoreError::InvalidData)
    );
    let orphan = store
        .list_delivery_history(project(1), 100)
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.rule_id == orphan_rule.id)
        .unwrap();
    assert_eq!(orphan.attempts, 1);
    assert_eq!(orphan.next_attempt_at, time(4000));
    assert!(
        store
            .claim_due(time(2500), time(3500), 10)
            .await
            .unwrap()
            .is_some_and(|c| c.delivery.project_id == project(1))
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn aggregate_completion_checks_lease_and_monitor_outbox_commits_with_state() {
    let (fixture, database, store) = setup().await;
    let d = destination(1, 11);
    store.upsert_destination(d.clone()).await.unwrap();
    let mut r = rule(1, 31, d.id);
    r.triggers = Box::new([]);
    r.aggregate = Some(AggregateAlert {
        dataset: ExploreDataset::Logs,
        lookback_minutes: 5,
        evaluation_interval_minutes: 1,
        threshold: 1,
        environment: None,
        release: None,
        notify_resolved: false,
    });
    r.next_evaluation_at = Some(time(1000));
    store.upsert_rule(r.clone()).await.unwrap();
    assert_eq!(
        store
            .claim_due_aggregate_rule(time(1000), time(2000))
            .await
            .unwrap(),
        Some(r.clone())
    );
    assert!(
        store
            .claim_due_aggregate_rule(time(1999), time(3000))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .claim_due_aggregate_rule(time(2000), time(3000))
            .await
            .unwrap()
            .is_some()
    );
    let notification = delivery(1, 1, &r);
    store
        .complete_aggregate_rule(
            r.id,
            time(2000),
            time(4000),
            true,
            Some(time(2000)),
            None,
            1,
            vec![notification.clone()],
        )
        .await
        .unwrap();
    assert!(
        store
            .list_delivery_history(project(1), 10)
            .await
            .unwrap()
            .is_empty()
    );
    store
        .complete_aggregate_rule(
            r.id,
            time(3000),
            time(4000),
            true,
            Some(time(2000)),
            Some(time(1000)),
            1,
            vec![notification.clone()],
        )
        .await
        .unwrap();
    assert_eq!(
        store.list_delivery_history(project(1), 10).await.unwrap(),
        vec![notification]
    );
    let actual = store.list_rules(project(1), 10).await.unwrap();
    assert_eq!(actual[0].next_evaluation_at, Some(time(4000)));
    assert_eq!(actual[0].storm_count, 1);
    let mut m = rule(1, 32, d.id);
    m.triggers = Box::new([]);
    m.monitor = Some(MonitorAlert {
        monitor_id: MonitorId::derive(project(1), "cron", "prod"),
        outcomes: vec![MonitorRunStatus::Error].into_boxed_slice(),
        notify_resolved: true,
    });
    store.upsert_rule(m.clone()).await.unwrap();
    let notification = delivery(1, 2, &m);
    store
        .complete_monitor_alert(m.id, time(2500), time(2000), 1, true, vec![notification])
        .await
        .unwrap();
    assert_eq!(
        store
            .list_delivery_history(project(1), 10)
            .await
            .unwrap()
            .len(),
        2
    );
    let actual = store
        .list_rules(project(1), 10)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == m.id)
        .unwrap();
    assert_eq!(actual.last_triggered_at, Some(time(2500)));
    assert_eq!(actual.updated_at, time(2500));
    assert!(actual.threshold_met);
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn second_bulk_chunk_failure_and_claim_commit_reply_have_durable_boundaries() {
    let (fixture, database, store) = setup().await;
    let d = destination(1, 11);
    store.upsert_destination(d.clone()).await.unwrap();
    let r = rule(1, 31, d.id);
    store.upsert_rule(r.clone()).await.unwrap();
    let transition = issue_transition(&database, &store).await;
    let deliveries = (1..=1001)
        .map(|i| {
            let mut d = delivery(1, i, &r);
            d.transition_id = transition.transition_id;
            d.issue_id = transition.issue_id;
            d.id = NotificationDeliveryId::from_bytes(i.to_be_bytes());
            d
        })
        .collect::<Vec<_>>();
    sqlx::raw_sql("CREATE SEQUENCE metric.notification_fault; CREATE FUNCTION metric.fail_notification_chunk() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN IF nextval(''metric.notification_fault'')=2 THEN RAISE EXCEPTION ''synthetic chunk failure''; END IF; RETURN NULL; END'; CREATE TRIGGER fail_notification_chunk BEFORE INSERT ON metric.notification_deliveries FOR EACH STATEMENT EXECUTE FUNCTION metric.fail_notification_chunk()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .expand_transition(transition.clone(), deliveries.clone())
            .await,
        Err(NotificationStoreError::Unavailable)
    );
    assert!(
        store
            .list_delivery_history(project(1), 500)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.pending_transitions(10).await.unwrap().len(), 1);
    sqlx::query("DROP TRIGGER fail_notification_chunk ON metric.notification_deliveries")
        .execute(&database.pool)
        .await
        .unwrap();
    store
        .expand_transition(transition, deliveries)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.notification_deliveries")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1001);
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxy_db = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied = PostgresNotificationStore::new(proxy_db.clone());
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied.claim_due(time(1000), time(2000), 1).await,
        Err(NotificationStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    let attempts: i64 =
        sqlx::query_scalar("SELECT sum(attempts)::bigint FROM metric.notification_deliveries")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    proxy_db.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn native_large_batches_response_budgets_limits_and_index_plans() {
    let (fixture, database, store) = setup().await;
    let d = destination(1, 11);
    store.upsert_destination(d.clone()).await.unwrap();
    let r = rule(1, 31, d.id);
    store.upsert_rule(r.clone()).await.unwrap();
    let transition = issue_transition(&database, &store).await;
    for limit in [0, 1001] {
        assert_eq!(
            store.list_destinations(project(1), limit).await,
            Err(NotificationStoreError::InvalidData)
        );
        assert_eq!(
            store.list_rules(project(1), limit).await,
            Err(NotificationStoreError::InvalidData)
        );
        assert_eq!(
            store
                .matching_rules(project(1), IssueNotificationKind::NewIssue, limit)
                .await,
            Err(NotificationStoreError::InvalidData)
        );
        assert_eq!(
            store.claim_due(time(1000), time(2000), limit).await,
            Err(NotificationStoreError::InvalidData)
        );
    }
    for limit in [0, 10_001] {
        assert_eq!(
            store.pending_transitions(limit).await,
            Err(NotificationStoreError::InvalidData)
        );
    }
    assert_eq!(
        store.claim_due(time(1000), time(1000), 1).await,
        Err(NotificationStoreError::InvalidData)
    );
    assert_eq!(
        store.claim_due_aggregate_rule(time(1000), time(999)).await,
        Err(NotificationStoreError::InvalidData)
    );
    let make = |i: u128| {
        let mut v = delivery(1, i, &r);
        v.id = NotificationDeliveryId::from_bytes(i.to_be_bytes());
        v.issue_id = transition.issue_id;
        v.transition_id = transition.transition_id;
        v
    };
    assert_eq!(
        store
            .expand_transition(transition.clone(), (1..=10_001).map(make).collect())
            .await,
        Err(NotificationStoreError::InvalidData)
    );
    let large = NotificationPayload::new(vec![b'x'; MAX_NOTIFICATION_PAYLOAD_BYTES]).unwrap();
    let oversized = (1..=1000)
        .map(|i| {
            let mut v = make(i);
            v.payload = large.clone();
            v
        })
        .collect();
    assert_eq!(
        store.expand_transition(transition.clone(), oversized).await,
        Err(NotificationStoreError::InvalidData)
    );
    assert_eq!(store.pending_transitions(1).await.unwrap().len(), 1);
    let values = (1..=10_000)
        .map(|i| {
            let mut v = make(i);
            if i > 9500 {
                v.payload = large.clone();
            }
            v
        })
        .collect();
    let started = std::time::Instant::now();
    store.expand_transition(transition, values).await.unwrap();
    println!(
        "notification 10000-row atomic expansion: {} ms",
        started.elapsed().as_millis()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metric.notification_deliveries")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 10_000);
    assert_eq!(
        store.list_delivery_history(project(1), 500).await,
        Err(NotificationStoreError::InvalidData)
    );
    assert_eq!(
        store
            .list_delivery_history(project(1), 496)
            .await
            .unwrap()
            .len(),
        496
    );
    assert!(
        store
            .list_delivery_history(project(2), 500)
            .await
            .unwrap()
            .is_empty()
    );
    // Representative bounded fixtures include endpoint and ciphertext bytes in
    // the response budget, even when the compressed configuration is tiny.
    sqlx::query("INSERT INTO metric.notification_destinations(destination_id,project_id,kind,endpoint,sealed_secret,config,config_size,enabled,created_at,updated_at) SELECT decode(md5(i::text),'hex'),1,kind,$1,$2,config,config_size,enabled,created_at+i,updated_at FROM metric.notification_destinations CROSS JOIN generate_series(1,1000) i WHERE destination_id=$3")
        .bind(vec![b'a'; MAX_ENDPOINT_BYTES]).bind(vec![42u8; MAX_SEALED_SECRET_BYTES]).bind(d.id.as_bytes().as_slice()).execute(&database.pool).await.unwrap();
    assert_eq!(
        store.list_destinations(project(1), 1000).await,
        Err(NotificationStoreError::InvalidData)
    );
    assert_eq!(
        store
            .list_destinations(project(1), 700)
            .await
            .unwrap()
            .len(),
        700
    );
    sqlx::raw_sql(
        "ANALYZE metric.notification_deliveries; ANALYZE metric.notification_destinations",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    for (sql, index) in [
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT delivery_id FROM metric.notification_deliveries WHERE status=0 AND next_attempt_at<=1000 ORDER BY next_attempt_at,delivery_id LIMIT 10",
            "notification_delivery_due",
        ),
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT delivery_id FROM metric.notification_deliveries WHERE project_id=1 ORDER BY created_at DESC,delivery_id DESC LIMIT 10",
            "notification_delivery_page",
        ),
        (
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT destination_id FROM metric.notification_destinations WHERE project_id=1 ORDER BY created_at,destination_id LIMIT 10",
            "notification_destination_page",
        ),
    ] {
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let plan: sqlx::types::Json<serde_json::Value> = row.get(0);
        assert!(plan.0.to_string().contains(index), "{}", plan.0);
    }
    // A corrupt declared decoded size must not defeat the transfer budget.
    // Fixed metadata is reserved for every selected row, including suppressed
    // bodies, before any variable-size fields are returned by PostgreSQL.
    sqlx::query("UPDATE metric.notification_destinations SET config=$1,config_size=1")
        .bind(vec![7_u8; MAX_CONFIG + 2])
        .execute(&database.pool)
        .await
        .unwrap();
    let mut q = start(ConfigTable::Destination);
    q.push("project_id=").push_bind(1_i32);
    let rows = finish(
        &mut q,
        ConfigTable::Destination,
        "created_at,destination_id",
        1000,
    )
    .build()
    .fetch_all(&database.pool)
    .await
    .unwrap();
    let transferred = rows.len() * 512
        + rows
            .iter()
            .map(|r| {
                ["config", "endpoint", "sealed_secret"]
                    .into_iter()
                    .map(|name| r.get::<Option<Vec<u8>>, _>(name).map_or(0, |v| v.len()))
                    .sum::<usize>()
            })
            .sum::<usize>();
    assert!(transferred <= READ_BYTES as usize);
    assert!(
        rows.iter()
            .any(|r| r.get::<Option<Vec<u8>>, _>("config").is_none())
    );
    assert_eq!(
        store.list_destinations(project(1), 1000).await,
        Err(NotificationStoreError::InvalidData)
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn concurrent_aggregate_leases_and_completion_state_outbox_rollback() {
    let (fixture, database, store) = setup().await;
    let d = destination(1, 11);
    store.upsert_destination(d.clone()).await.unwrap();
    let mut a = rule(1, 31, d.id);
    a.triggers = Box::new([]);
    a.aggregate = Some(AggregateAlert {
        dataset: ExploreDataset::Metrics,
        lookback_minutes: 5,
        evaluation_interval_minutes: 1,
        threshold: 1,
        environment: None,
        release: None,
        notify_resolved: true,
    });
    a.next_evaluation_at = Some(time(1000));
    let mut b = a.clone();
    b.id = AlertRuleId::from_bytes([32; 16]);
    store.upsert_rule(a.clone()).await.unwrap();
    store.upsert_rule(b.clone()).await.unwrap();
    let (x, y) = tokio::join!(
        store.claim_due_aggregate_rule(time(1000), time(2000)),
        store.claim_due_aggregate_rule(time(1000), time(2000))
    );
    assert_ne!(x.unwrap().unwrap().id, y.unwrap().unwrap().id);
    assert!(
        store
            .claim_due_aggregate_rule(time(1000), time(2000))
            .await
            .unwrap()
            .is_none()
    );
    sqlx::raw_sql("CREATE FUNCTION metric.fail_notification_state() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic state failure''; END'; CREATE TRIGGER fail_notification_state BEFORE UPDATE ON metric.notification_rules FOR EACH STATEMENT EXECUTE FUNCTION metric.fail_notification_state()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .complete_aggregate_rule(
                a.id,
                time(2000),
                time(4000),
                true,
                Some(time(2000)),
                Some(time(1000)),
                1,
                vec![delivery(1, 1, &a)]
            )
            .await,
        Err(NotificationStoreError::Unavailable)
    );
    assert_eq!(
        store.list_rules(project(1), 100).await.unwrap(),
        vec![a.clone(), b]
    );
    assert!(
        store
            .list_delivery_history(project(1), 100)
            .await
            .unwrap()
            .is_empty()
    );
    let lease: Option<i64> =
        sqlx::query_scalar("SELECT lease_until FROM metric.notification_rules WHERE rule_id=$1")
            .bind(a.id.as_bytes().as_slice())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(lease, Some(2000));
    sqlx::query("DROP TRIGGER fail_notification_state ON metric.notification_rules")
        .execute(&database.pool)
        .await
        .unwrap();
    let proxy = CommitReplyProxy::start(&fixture.uri).await;
    let proxy_db = PostgresDatabase::connect(&proxy.uri, &fixture.policy)
        .await
        .unwrap();
    let proxied = PostgresNotificationStore::new(proxy_db.clone());
    proxy.drop_next_commit_reply();
    assert_eq!(
        proxied
            .complete_aggregate_rule(
                a.id,
                time(2000),
                time(4000),
                true,
                Some(time(2000)),
                Some(time(1000)),
                1,
                vec![delivery(1, 1, &a)]
            )
            .await,
        Err(NotificationStoreError::Unavailable)
    );
    assert!(proxy.did_drop_reply());
    store
        .complete_aggregate_rule(
            a.id,
            time(2000),
            time(4000),
            true,
            Some(time(2000)),
            Some(time(1000)),
            1,
            vec![delivery(1, 1, &a)],
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list_delivery_history(project(1), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let actual = store.list_rules(project(1), 100).await.unwrap().remove(0);
    assert_eq!(actual.storm_count, 1);
    assert_eq!(actual.next_evaluation_at, Some(time(4000)));
    // Monitor state and its outbox use the same transaction boundary.
    let mut m = rule(1, 33, d.id);
    m.triggers = Box::new([]);
    m.monitor = Some(MonitorAlert {
        monitor_id: MonitorId::derive(project(1), "cron", "prod"),
        outcomes: vec![MonitorRunStatus::Error].into_boxed_slice(),
        notify_resolved: true,
    });
    store.upsert_rule(m.clone()).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_notification_state BEFORE UPDATE ON metric.notification_rules FOR EACH STATEMENT EXECUTE FUNCTION metric.fail_notification_state()").execute(&database.pool).await.unwrap();
    assert_eq!(
        store
            .complete_monitor_alert(
                m.id,
                time(2500),
                time(2000),
                1,
                true,
                vec![delivery(1, 2, &m)]
            )
            .await,
        Err(NotificationStoreError::Unavailable)
    );
    assert_eq!(
        store
            .list_delivery_history(project(1), 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .list_rules(project(1), 100)
            .await
            .unwrap()
            .into_iter()
            .find(|v| v.id == m.id),
        Some(m)
    );
    proxy_db.close().await;
    proxy.close().await;
    database.close().await;
    fixture.cleanup().await;
}

struct Adapter(AtomicUsize);

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn existing_aggregate_evaluator_queries_all_datasets_and_enqueues_resolution_once() {
    use metric_application::{
        explore::{ExploreConfig, ExploreService},
        notifications::AggregateAlertEvaluator,
    };
    use std::time::Duration;
    let (fixture, database, store) = setup().await;
    let d = destination(1, 11);
    store.upsert_destination(d.clone()).await.unwrap();
    for (i, dataset) in [
        ExploreDataset::Errors,
        ExploreDataset::Logs,
        ExploreDataset::Spans,
        ExploreDataset::Metrics,
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = rule(1, 31 + i as u8, d.id);
        r.triggers = Box::new([]);
        r.aggregate = Some(AggregateAlert {
            dataset,
            lookback_minutes: 5,
            evaluation_interval_minutes: 1,
            threshold: 1,
            environment: None,
            release: None,
            notify_resolved: true,
        });
        r.threshold_met = true;
        r.next_evaluation_at = Some(time(2_000_000));
        store.upsert_rule(r).await.unwrap();
    }
    let explore = Arc::new(
        ExploreService::new(
            Arc::new(crate::PostgresExploreStore::new(database.clone())),
            ExploreConfig::default(),
        )
        .unwrap(),
    );
    let evaluator = AggregateAlertEvaluator::new(
        Arc::new(store.clone()),
        explore,
        Arc::new(metric_testkit::FixedClock(time(2_000_000))),
        Duration::from_secs(1),
        Duration::from_secs(30),
        10,
    )
    .unwrap();
    assert_eq!(evaluator.evaluate_once().await.unwrap(), 4);
    assert_eq!(evaluator.evaluate_once().await.unwrap(), 0);
    let history = store.list_delivery_history(project(1), 100).await.unwrap();
    assert_eq!(history.len(), 4);
    for d in history {
        let body: serde_json::Value = serde_json::from_slice(d.payload.as_bytes()).unwrap();
        assert_eq!(body["type"], "aggregate_resolved");
    }
    let rules = store.list_rules(project(1), 100).await.unwrap();
    assert!(rules.iter().all(|r| !r.threshold_met
        && r.storm_count == 1
        && r.next_evaluation_at == Some(time(2_060_000))));
    assert!(
        store
            .list_delivery_history(project(2), 100)
            .await
            .unwrap()
            .is_empty()
    );
    database.close().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn existing_monitor_evaluator_enqueues_failure_and_consumes_alert_marker() {
    use metric_application::notifications::MonitorAlertEvaluator;
    use metric_domain::{
        finalization::derive_environment_id,
        monitors::{
            MonitorConfig, MonitorDefinition, MonitorRun, MonitorRunId, MonitorRunSource,
            MonitorSchedule, MonitorUpdate,
        },
    };
    use metric_ports::MonitorStore;
    use std::time::Duration;
    let (fixture, database, store) = setup().await;
    let d = destination(1, 11);
    store.upsert_destination(d.clone()).await.unwrap();
    let p = project(1);
    let id = MonitorId::derive(p, "cron", "production");
    let monitors =
        crate::PostgresMonitorStore::new(database.clone(), crate::MonitorRetention::default())
            .unwrap();
    monitors
        .upsert_monitor(MonitorDefinition {
            id,
            project_id: p,
            slug: "cron".into(),
            name: "Synthetic monitor".into(),
            environment_id: derive_environment_id(p, "production"),
            environment: "production".into(),
            enabled: true,
            managed_by_web: true,
            revision: 1,
            config: MonitorConfig {
                schedule: MonitorSchedule::interval(1).unwrap(),
                checkin_margin_seconds: 5,
                max_runtime_seconds: 30,
            },
            uptime: None,
            next_expected_at: time(60_000),
            last_run_id: None,
            last_status: None,
            last_check_in_at: None,
            created_at: time(0),
            updated_at: time(0),
        })
        .await
        .unwrap();
    let run = MonitorRun {
        id: MonitorRunId::sdk(id, [1; 16]),
        project_id: p,
        monitor_id: id,
        check_in_id: Some([1; 16]),
        status: MonitorRunStatus::Error,
        source: MonitorRunSource::Sdk,
        scheduled_for: None,
        started_at: time(1000),
        finished_at: Some(time(1001)),
        duration_ms: Some(1),
        received_at: time(1001),
        release_id: None,
        timeout_at: None,
        delete_at: Some(time(86_400_000)),
        http_status: None,
        uptime_failure: None,
    };
    monitors
        .persist_monitors(vec![MonitorUpdate {
            definition: None,
            run,
        }])
        .await
        .unwrap();
    let mut r = rule(1, 31, d.id);
    r.triggers = Box::new([]);
    r.monitor = Some(MonitorAlert {
        monitor_id: id,
        outcomes: vec![MonitorRunStatus::Error].into_boxed_slice(),
        notify_resolved: true,
    });
    store.upsert_rule(r.clone()).await.unwrap();
    let evaluator = MonitorAlertEvaluator::new(
        Arc::new(store.clone()),
        Arc::new(monitors.clone()),
        Arc::new(metric_testkit::FixedClock(time(2000))),
        Duration::from_secs(1),
        10,
    )
    .unwrap();
    assert_eq!(evaluator.evaluate_once().await.unwrap(), 1);
    assert_eq!(evaluator.evaluate_once().await.unwrap(), 0);
    assert!(
        monitors
            .pending_monitor_alerts(10)
            .await
            .unwrap()
            .is_empty()
    );
    let history = store.list_delivery_history(p, 100).await.unwrap();
    assert_eq!(history.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(history[0].payload.as_bytes()).unwrap();
    assert_eq!(body["kind"], "cron_monitor");
    assert_eq!(body["state"], "firing");
    assert_eq!(body["status"], "error");
    let actual = store.list_rules(p, 100).await.unwrap().remove(0);
    assert_eq!(actual.last_triggered_at, Some(time(2000)));
    assert!(actual.threshold_met);
    assert_eq!(actual.storm_count, 1);
    database.close().await;
    fixture.cleanup().await;
}

impl metric_ports::NotificationDeliveryAdapter for Adapter {
    fn deliver(
        &self,
        c: ClaimedNotificationDelivery,
    ) -> PortFuture<
        '_,
        std::result::Result<
            metric_ports::NotificationDeliveryReceipt,
            metric_ports::NotificationDeliveryError,
        >,
    > {
        Box::pin(async move {
            assert_eq!(c.destination.sealed_secret.expose_ciphertext(), &[42; 48]);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(metric_ports::NotificationDeliveryReceipt {
                status: 200,
                retry_after: None,
            })
        })
    }
}
#[tokio::test]
#[ignore = "requires Docker PostgreSQL"]
async fn existing_dispatcher_expands_real_issue_transitions_and_records_terminal_history() {
    use metric_application::notifications::{NotificationConfig, NotificationDispatcher};
    let (fixture, database, store) = setup().await;
    let d = destination(1, 11);
    store.upsert_destination(d.clone()).await.unwrap();
    store.upsert_rule(rule(1, 31, d.id)).await.unwrap();
    issue_transition(&database, &store).await;
    let adapter = Arc::new(Adapter(AtomicUsize::new(0)));
    let dispatcher = NotificationDispatcher::new(
        Arc::new(store.clone()),
        adapter.clone(),
        Arc::new(metric_testkit::FixedClock(time(2000))),
        NotificationConfig::default(),
    )
    .unwrap();
    assert_eq!(dispatcher.expand_once().await.unwrap(), 1);
    assert_eq!(dispatcher.expand_once().await.unwrap(), 0);
    let claim = dispatcher.claim_once().await.unwrap().unwrap();
    dispatcher.deliver_claim(claim).await.unwrap();
    assert_eq!(adapter.0.load(Ordering::SeqCst), 1);
    let history = store.list_delivery_history(project(1), 100).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, NotificationDeliveryStatus::Delivered);
    assert_eq!(history[0].next_attempt_at, time(2000));
    assert!(dispatcher.claim_once().await.unwrap().is_none());
    database.close().await;
    fixture.cleanup().await;
}
