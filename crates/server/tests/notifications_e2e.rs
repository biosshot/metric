use std::{
    error::Error,
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use hmac::{Hmac, Mac};
use metric_application::notifications::{NotificationConfig, NotificationDispatcher};
use metric_domain::{
    EventId, ProjectId, SecretBytes, Timestamp,
    grouping::{
        GroupingComponent, GroupingComponentKind, GroupingExplanation, GroupingKey,
        GroupingStrategy, derive_issue_id,
    },
    issue::{IssueGroupingDetail, IssueNotificationKind, IssueOccurrence, IssueTitle},
    notifications::{
        AlertRule, AlertRuleId, NotificationDestination, NotificationDestinationId, RuleName,
        WebhookEndpoint,
    },
};
use metric_mongo::{IssueCodecConfig, MongoProjectStore};
use metric_ports::{
    Clock, IssueStore, NotificationStore, PortFuture, WebhookDeliveryAdapter, WebhookDeliveryError,
    WebhookDeliveryReceipt,
};
use metric_server::webhook::{ReqwestWebhookAdapter, WebhookAdapterConfig, WebhookSecretBox};
use mongodb::{Client, Database, bson::doc};
use sha2::Sha256;
use tokio::sync::{mpsc, oneshot};

#[tokio::test]
#[ignore = "requires MongoDB 8.0.12 configured by METRIC_TEST_MONGODB_URI"]
async fn cumulative_issue_transition_reaches_signed_webhook_once() {
    let database = test_database().await.unwrap();
    let result = exercise(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "real MongoDB Phase 20 performance baseline"]
async fn performance_notification_transition_expansion_rps() {
    let database = test_database().await.unwrap();
    let result = measure_expansion(&database).await;
    let cleanup = database.drop().await;
    result.unwrap();
    cleanup.unwrap();
}

async fn measure_expansion(database: &Database) -> Result<(), Box<dyn Error>> {
    const TRANSITIONS: usize = 300;
    let project_store =
        MongoProjectStore::from_database(database.clone(), SecretBytes::new([9; 32]), 16);
    project_store.bootstrap_or_validate().await?;
    let issue_store = project_store.issue_store(IssueCodecConfig::default());
    let notification_store = Arc::new(project_store.notification_store());
    let project_id = ProjectId::new(21)?;
    let now = Timestamp::from_unix_millis(20_000)?;
    let destination_id = NotificationDestinationId::from_bytes([31; 16]);
    notification_store
        .upsert_destination(NotificationDestination {
            id: destination_id,
            project_id,
            endpoint: WebhookEndpoint::new("https://example.com/benchmark")?,
            sealed_secret: metric_domain::notifications::SealedWebhookSecret::new(vec![1; 32])?,
            enabled: true,
            created_at: now,
            updated_at: now,
        })
        .await?;
    notification_store
        .upsert_rule(AlertRule {
            id: AlertRuleId::from_bytes([32; 16]),
            project_id,
            name: RuleName::new("benchmark")?,
            enabled: true,
            triggers: vec![IssueNotificationKind::NewIssue].into_boxed_slice(),
            destination_ids: vec![destination_id].into_boxed_slice(),
            created_at: now,
            updated_at: now,
        })
        .await?;
    for number in 0..TRANSITIONS {
        issue_store
            .apply_occurrence(numbered_occurrence(project_id, number))
            .await?;
    }
    let dispatcher = NotificationDispatcher::new(
        notification_store,
        Arc::new(NeverAdapter),
        Arc::new(FixedClock(now)),
        NotificationConfig {
            transition_batch_size: 100,
            ..NotificationConfig::default()
        },
    )?;
    let started = Instant::now();
    let mut expanded = 0;
    loop {
        let count = dispatcher.expand_once().await?;
        expanded += count;
        if count == 0 {
            break;
        }
    }
    let elapsed = started.elapsed();
    let rps = expanded as f64 / elapsed.as_secs_f64();
    assert_eq!(expanded, TRANSITIONS);
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("notification_deliveries")
            .count_documents(doc! {})
            .await?,
        TRANSITIONS as u64
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("issues")
            .count_documents(doc! { "j": true })
            .await?,
        0
    );
    println!(
        "Phase20 Notification: transitions={expanded},expansion_rps={rps:.2},elapsed_ms={}",
        elapsed.as_millis()
    );
    Ok(())
}

async fn exercise(database: &Database) -> Result<(), Box<dyn Error>> {
    let project_store =
        MongoProjectStore::from_database(database.clone(), SecretBytes::new([9; 32]), 16);
    project_store.bootstrap_or_validate().await?;
    let issue_store = project_store.issue_store(IssueCodecConfig::default());
    let notification_store = Arc::new(project_store.notification_store());
    let master = SecretBytes::new([7; 32]);
    let secret_box = WebhookSecretBox::new(&master);
    let signing_secret = b"phase-20-webhook-secret";

    let (captured_tx, mut captured_rx) = mpsc::channel(1);
    let (stop_tx, stop_rx) = oneshot::channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Router::new()
        .route("/hook", post(capture_webhook))
        .with_state(captured_tx);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });

    let project_id = ProjectId::new(20)?;
    let now = Timestamp::from_unix_millis(10_000)?;
    let destination_id = NotificationDestinationId::from_bytes([3; 16]);
    notification_store
        .upsert_destination(NotificationDestination {
            id: destination_id,
            project_id,
            endpoint: WebhookEndpoint::new(format!("http://{address}/hook"))?,
            sealed_secret: secret_box.seal(signing_secret)?,
            enabled: true,
            created_at: now,
            updated_at: now,
        })
        .await?;
    let rule_id = AlertRuleId::from_bytes([4; 16]);
    notification_store
        .upsert_rule(AlertRule {
            id: rule_id,
            project_id,
            name: RuleName::new("New issues")?,
            enabled: true,
            triggers: vec![IssueNotificationKind::NewIssue].into_boxed_slice(),
            destination_ids: vec![destination_id].into_boxed_slice(),
            created_at: now,
            updated_at: now,
        })
        .await?;

    let occurrence = occurrence(project_id);
    let created = issue_store.apply_occurrence(occurrence.clone()).await?;
    let adapter = Arc::new(ReqwestWebhookAdapter::new(
        secret_box,
        WebhookAdapterConfig {
            allow_http: true,
            allow_private_networks: true,
            ..WebhookAdapterConfig::default()
        },
    )?);
    let dispatcher = NotificationDispatcher::new(
        notification_store.clone(),
        adapter,
        Arc::new(FixedClock(now)),
        NotificationConfig {
            poll_interval: Duration::from_millis(10),
            ..NotificationConfig::default()
        },
    )?;

    assert_eq!(dispatcher.expand_once().await?, 1);
    assert_eq!(dispatcher.expand_once().await?, 0);
    let claim = dispatcher.claim_once().await?.expect("due delivery");
    assert_eq!(claim.attempt, 1);

    // Repeating the crash window after delivery upsert cannot create another job.
    let transition = metric_domain::notifications::IssueNotificationTransition {
        transition_id: claim.delivery.transition_id,
        project_id,
        issue_id: created.issue.issue_id,
        kind: IssueNotificationKind::NewIssue,
        event_id: occurrence.event_id,
        created_at: occurrence.received_at,
        title: occurrence.title.clone(),
    };
    notification_store
        .expand_transition(transition, vec![claim.delivery.clone()])
        .await?;
    dispatcher.deliver_claim(claim.clone()).await?;

    let captured = tokio::time::timeout(Duration::from_secs(2), captured_rx.recv())
        .await?
        .expect("webhook request");
    let delivery_id = hex::encode(claim.delivery.id.as_bytes());
    assert_eq!(
        captured.headers.get("idempotency-key").unwrap(),
        delivery_id.as_str()
    );
    assert_eq!(
        captured.headers.get("x-delivery-id").unwrap(),
        delivery_id.as_str()
    );
    let timestamp = captured
        .headers
        .get("x-metric-timestamp")
        .unwrap()
        .to_str()?;
    let expected = signature(signing_secret, &delivery_id, timestamp, &captured.body);
    assert_eq!(
        captured
            .headers
            .get("x-metric-signature")
            .unwrap()
            .to_str()?,
        format!("sha256={expected}")
    );
    let payload: serde_json::Value = serde_json::from_slice(&captured.body)?;
    assert_eq!(payload["type"], "new_issue");
    assert!(
        !captured
            .body
            .windows(signing_secret.len())
            .any(|window| window == signing_secret)
    );

    let deliveries = database.collection::<mongodb::bson::Document>("notification_deliveries");
    assert_eq!(deliveries.count_documents(doc! {}).await?, 1);
    assert_eq!(
        deliveries.find_one(doc! {}).await?.unwrap().get_str("s")?,
        "delivered"
    );
    let raw_issue = database
        .collection::<mongodb::bson::Document>("issues")
        .find_one(doc! { "_id": binary(created.issue.issue_id.as_bytes()) })
        .await?
        .unwrap();
    assert!(!raw_issue.contains_key("j"));
    assert!(!raw_issue.contains_key("n"));
    let _ = stop_tx.send(());
    server.await?;
    Ok(())
}

#[derive(Clone)]
struct CapturedWebhook {
    headers: HeaderMap,
    body: Bytes,
}

async fn capture_webhook(
    State(sender): State<mpsc::Sender<CapturedWebhook>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let _ = sender.send(CapturedWebhook { headers, body }).await;
    StatusCode::OK
}

struct FixedClock(Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

fn occurrence(project_id: ProjectId) -> IssueOccurrence {
    let mut grouping_bytes = [2; 34];
    grouping_bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    let grouping_key = GroupingKey::parse(&grouping_bytes).unwrap();
    IssueOccurrence {
        project_id,
        issue_id: derive_issue_id(project_id, grouping_key),
        grouping_key,
        event_id: EventId::from_bytes([5; 16]),
        occurred_at: Timestamp::from_unix_millis(5_000).unwrap(),
        received_at: Timestamp::from_unix_millis(6_000).unwrap(),
        release: None,
        title: IssueTitle::new("Phase 20 E2E failure").unwrap(),
        culprit: None,
        grouping: IssueGroupingDetail {
            strategy: GroupingStrategy::Message,
            explanation: GroupingExplanation {
                summary: "phase 20 message".into(),
                components: vec![GroupingComponent {
                    kind: GroupingComponentKind::Message,
                    value: "phase-20".into(),
                }],
            },
        },
        increment: NonZeroU64::new(1).unwrap(),
    }
}

fn numbered_occurrence(project_id: ProjectId, number: usize) -> IssueOccurrence {
    let seed = u8::try_from(number % 251 + 1).unwrap();
    let mut grouping_bytes = [seed; 34];
    grouping_bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
    grouping_bytes[26..].copy_from_slice(&(number as u64).to_be_bytes());
    let grouping_key = GroupingKey::parse(&grouping_bytes).unwrap();
    IssueOccurrence {
        project_id,
        issue_id: derive_issue_id(project_id, grouping_key),
        grouping_key,
        event_id: EventId::from_bytes((number as u128 + 1).to_be_bytes()),
        occurred_at: Timestamp::from_unix_millis(10_000 + number as i64).unwrap(),
        received_at: Timestamp::from_unix_millis(10_000 + number as i64).unwrap(),
        release: None,
        title: IssueTitle::new(format!("notification benchmark {number}")).unwrap(),
        culprit: None,
        grouping: IssueGroupingDetail {
            strategy: GroupingStrategy::Message,
            explanation: GroupingExplanation {
                summary: "benchmark".into(),
                components: vec![GroupingComponent {
                    kind: GroupingComponentKind::Message,
                    value: format!("notification benchmark {number}").into_boxed_str(),
                }],
            },
        },
        increment: NonZeroU64::MIN,
    }
}

struct NeverAdapter;

impl WebhookDeliveryAdapter for NeverAdapter {
    fn deliver(
        &self,
        _claim: metric_domain::notifications::ClaimedNotificationDelivery,
    ) -> PortFuture<'_, Result<WebhookDeliveryReceipt, WebhookDeliveryError>> {
        Box::pin(async { panic!("expansion benchmark must not deliver") })
    }
}

fn signature(secret: &[u8], delivery_id: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).unwrap();
    mac.update(b"v1\n");
    mac.update(delivery_id.as_bytes());
    mac.update(b"\n");
    mac.update(timestamp.as_bytes());
    mac.update(b"\n");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn binary(bytes: impl AsRef<[u8]>) -> mongodb::bson::Binary {
    mongodb::bson::Binary {
        subtype: mongodb::bson::spec::BinarySubtype::Generic,
        bytes: bytes.as_ref().to_vec(),
    }
}

async fn test_database() -> Result<Database, mongodb::error::Error> {
    let uri = std::env::var("METRIC_TEST_MONGODB_URI")
        .unwrap_or_else(|_| "mongodb://127.0.0.1:27018/?directConnection=true".to_owned());
    let client = Client::with_uri_str(uri).await?;
    Ok(client.database(&format!(
        "metric_notification_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    )))
}
