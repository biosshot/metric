use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Response, StatusCode, header},
    routing::post,
};
use faultkeep_domain::{
    ProjectId, SecretBytes, Timestamp,
    grouping::IssueId,
    issue::IssueTransitionId,
    notifications::{
        AlertRuleId, ClaimedNotificationDelivery, NotificationDelivery, NotificationDeliveryId,
        NotificationDeliveryStatus, NotificationDestination, NotificationDestinationId,
        NotificationPayload, WebhookEndpoint,
    },
};
use faultkeep_ports::{WebhookDeliveryAdapter, WebhookDeliveryError};
use faultkeep_server::webhook::{ReqwestWebhookAdapter, WebhookAdapterConfig, WebhookSecretBox};
use tokio::sync::oneshot;

#[tokio::test]
async fn controlled_receiver_enforces_redirect_timeout_retry_after_and_response_bound() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/redirect",
            post(|| async {
                Response::builder()
                    .status(StatusCode::FOUND)
                    .header(header::LOCATION, "http://127.0.0.1/private")
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .route(
            "/retry",
            post(|| async {
                Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header(header::RETRY_AFTER, "30")
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .route(
            "/large",
            post(|| async { (StatusCode::OK, "x".repeat(128 * 1024)) }),
        )
        .route(
            "/slow",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                StatusCode::OK
            }),
        );
    let (stop_tx, stop_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });

    let secret_box = WebhookSecretBox::new(&SecretBytes::new([8; 32]));
    let sealed = secret_box.seal(b"secret").unwrap();
    let adapter = Arc::new(
        ReqwestWebhookAdapter::new(
            secret_box,
            WebhookAdapterConfig {
                timeout: Duration::from_millis(100),
                max_response_bytes: 16,
                allow_http: true,
                allow_private_networks: true,
                ..WebhookAdapterConfig::default()
            },
        )
        .unwrap(),
    );

    let redirect = adapter
        .deliver(claim(format!("http://{address}/redirect"), sealed.clone()))
        .await
        .unwrap();
    assert_eq!(redirect.status, 302, "redirect must not be followed");
    let retry = adapter
        .deliver(claim(format!("http://{address}/retry"), sealed.clone()))
        .await
        .unwrap();
    assert_eq!(retry.status, 429);
    assert_eq!(retry.retry_after, Some(Duration::from_secs(30)));
    let large = adapter
        .deliver(claim(format!("http://{address}/large"), sealed.clone()))
        .await
        .unwrap();
    assert_eq!(large.status, 200, "diagnostic body is discarded at its cap");
    assert_eq!(
        adapter
            .deliver(claim(format!("http://{address}/slow"), sealed))
            .await,
        Err(WebhookDeliveryError::Timeout)
    );

    let _ = stop_tx.send(());
    server.await.unwrap();
}

fn claim(
    endpoint: String,
    secret: faultkeep_domain::notifications::SealedWebhookSecret,
) -> ClaimedNotificationDelivery {
    let project_id = ProjectId::new(1).unwrap();
    let destination_id = NotificationDestinationId::from_bytes([3; 16]);
    ClaimedNotificationDelivery {
        delivery: NotificationDelivery {
            id: NotificationDeliveryId::from_bytes([1; 16]),
            project_id,
            issue_id: IssueId::from_bytes([2; 16]),
            transition_id: IssueTransitionId::from_bytes([4; 16]),
            rule_id: AlertRuleId::from_bytes([5; 16]),
            action_id: destination_id,
            destination_id,
            payload: NotificationPayload::new(br#"{"version":1}"#.to_vec()).unwrap(),
            status: NotificationDeliveryStatus::Pending,
            attempts: 1,
            next_attempt_at: Timestamp::from_unix_millis(1).unwrap(),
            last_error: None,
            created_at: Timestamp::from_unix_millis(1).unwrap(),
            delivered_at: None,
            delete_at: None,
        },
        destination: NotificationDestination {
            id: destination_id,
            project_id,
            endpoint: WebhookEndpoint::new(endpoint).unwrap(),
            sealed_secret: secret,
            enabled: true,
            created_at: Timestamp::from_unix_millis(1).unwrap(),
            updated_at: Timestamp::from_unix_millis(1).unwrap(),
        },
        attempt: 1,
        attempted_at: Timestamp::from_unix_millis(1).unwrap(),
    }
}
