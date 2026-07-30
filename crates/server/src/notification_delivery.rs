//! Provider dispatch after a durable notification delivery has been claimed.

use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::Mailbox,
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use metric_domain::notifications::{
    ClaimedNotificationDelivery, NotificationDestinationKind, SmtpSecurity,
};
use metric_ports::{
    NotificationDeliveryAdapter, NotificationDeliveryError, NotificationDeliveryReceipt, PortFuture,
};
use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};

use crate::webhook::{ReqwestWebhookAdapter, WebhookSecretBox, forbidden_ip};

#[derive(Debug, Clone)]
pub struct ProviderAdapterConfig {
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_retry_after: Duration,
    pub allow_private_networks: bool,
    pub telegram_api_base: Box<str>,
}

pub struct ProviderDeliveryAdapter {
    webhook: Arc<ReqwestWebhookAdapter>,
    secret_box: WebhookSecretBox,
    http: Client,
    config: ProviderAdapterConfig,
}

impl ProviderDeliveryAdapter {
    pub fn new(
        webhook: ReqwestWebhookAdapter,
        secret_box: WebhookSecretBox,
        config: ProviderAdapterConfig,
    ) -> Result<Self, NotificationDeliveryError> {
        if config.timeout.is_zero()
            || config.max_retry_after.is_zero()
            || !(1..=1024 * 1024).contains(&config.max_response_bytes)
            || config.telegram_api_base.is_empty()
        {
            return Err(NotificationDeliveryError::Rejected);
        }
        let http = Client::builder()
            .redirect(Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|_| NotificationDeliveryError::Retryable)?;
        Ok(Self {
            webhook: Arc::new(webhook),
            secret_box,
            http,
            config,
        })
    }

    async fn deliver_inner(
        &self,
        claim: ClaimedNotificationDelivery,
    ) -> Result<NotificationDeliveryReceipt, NotificationDeliveryError> {
        if !claim.destination.enabled
            || claim.destination.project_id != claim.delivery.project_id
            || claim.destination.id != claim.delivery.destination_id
        {
            return Err(NotificationDeliveryError::Rejected);
        }
        match claim.destination.kind {
            NotificationDestinationKind::Webhook => self.webhook.deliver(claim).await,
            NotificationDestinationKind::Telegram => self.deliver_telegram(claim).await,
            NotificationDestinationKind::SmtpEmail => self.deliver_email(claim).await,
        }
    }

    async fn deliver_telegram(
        &self,
        claim: ClaimedNotificationDelivery,
    ) -> Result<NotificationDeliveryReceipt, NotificationDeliveryError> {
        let token = self.secret_box.open(&claim.destination.sealed_secret)?;
        let token =
            std::str::from_utf8(&token).map_err(|_| NotificationDeliveryError::InvalidSecret)?;
        if !valid_telegram_token(token) {
            return Err(NotificationDeliveryError::InvalidSecret);
        }
        let summary = notification_summary(claim.delivery.payload.as_bytes());
        let text = format!(
            "<b>{}</b>\n{}\n<code>project {}</code>",
            escape_html(&summary.kind),
            escape_html(&summary.title),
            claim.delivery.project_id.get(),
        );
        let response = self
            .http
            .post(format!(
                "{}/bot{token}/sendMessage",
                self.config.telegram_api_base.trim_end_matches('/')
            ))
            .json(&json!({
                "chat_id": claim.destination.endpoint.as_str(),
                "text": text,
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            }))
            .send()
            .await
            .map_err(classify_reqwest)?;
        let status = response.status().as_u16();
        let body = bounded_body(response, self.config.max_response_bytes).await?;
        let retry_after = if status == 429 {
            serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| value.pointer("/parameters/retry_after")?.as_u64())
                .map(Duration::from_secs)
                .filter(|value| *value <= self.config.max_retry_after)
        } else {
            None
        };
        Ok(NotificationDeliveryReceipt {
            status,
            retry_after,
        })
    }

    async fn deliver_email(
        &self,
        claim: ClaimedNotificationDelivery,
    ) -> Result<NotificationDeliveryReceipt, NotificationDeliveryError> {
        let smtp = claim
            .destination
            .smtp
            .as_ref()
            .ok_or(NotificationDeliveryError::Rejected)?;
        let host = claim.destination.endpoint.as_str();
        let address =
            resolve_smtp_host(host, smtp.port, self.config.allow_private_networks).await?;
        let password = self.secret_box.open(&claim.destination.sealed_secret)?;
        let password =
            String::from_utf8(password).map_err(|_| NotificationDeliveryError::InvalidSecret)?;
        let credentials = Credentials::new(smtp.username.as_str().to_owned(), password);
        let tls =
            TlsParameters::new(host.to_owned()).map_err(|_| NotificationDeliveryError::Rejected)?;
        let tls = match smtp.security {
            SmtpSecurity::StartTls => Tls::Required(tls),
            SmtpSecurity::Tls => Tls::Wrapper(tls),
        };
        let transport =
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(address.ip().to_string())
                .port(smtp.port)
                .tls(tls)
                .credentials(credentials)
                .timeout(Some(self.config.timeout))
                .build();
        let summary = notification_summary(claim.delivery.payload.as_bytes());
        let mut message = Message::builder()
            .from(parse_mailbox(smtp.from.as_str())?)
            .subject(format!("[Metric] {}", summary.kind));
        for recipient in &smtp.recipients {
            message = message.to(parse_mailbox(recipient.as_str())?);
        }
        let message = message
            .body(format!(
                "{}\n\nProject: {}\nDelivery: {}\n",
                summary.title,
                claim.delivery.project_id.get(),
                hex::encode(claim.delivery.id.as_bytes()),
            ))
            .map_err(|_| NotificationDeliveryError::Rejected)?;
        transport
            .send(message)
            .await
            .map(|_| NotificationDeliveryReceipt {
                status: 202,
                retry_after: None,
            })
            .map_err(|error| {
                if error.is_permanent() {
                    NotificationDeliveryError::Rejected
                } else if error.is_timeout() {
                    NotificationDeliveryError::Timeout
                } else {
                    NotificationDeliveryError::Retryable
                }
            })
    }
}

impl NotificationDeliveryAdapter for ProviderDeliveryAdapter {
    fn deliver(
        &self,
        claim: ClaimedNotificationDelivery,
    ) -> PortFuture<'_, Result<NotificationDeliveryReceipt, NotificationDeliveryError>> {
        Box::pin(self.deliver_inner(claim))
    }
}

struct Summary {
    kind: String,
    title: String,
}

fn notification_summary(payload: &[u8]) -> Summary {
    let value = serde_json::from_slice::<Value>(payload).unwrap_or(Value::Null);
    Summary {
        kind: value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("alert")
            .replace('_', " "),
        title: value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("A Metric alert was triggered")
            .chars()
            .take(500)
            .collect(),
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn valid_telegram_token(value: &str) -> bool {
    let Some((bot_id, secret)) = value.split_once(':') else {
        return false;
    };
    !bot_id.is_empty()
        && bot_id.bytes().all(|byte| byte.is_ascii_digit())
        && secret.len() >= 20
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

async fn resolve_smtp_host(
    host: &str,
    port: u16,
    allow_private_networks: bool,
) -> Result<std::net::SocketAddr, NotificationDeliveryError> {
    if host.is_empty()
        || host.len() > 253
        || host.eq_ignore_ascii_case("localhost")
        || host.contains(['/', '\\', '@'])
    {
        return Err(NotificationDeliveryError::Rejected);
    }
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| NotificationDeliveryError::Retryable)?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(NotificationDeliveryError::Rejected);
    }
    if !allow_private_networks && addresses.iter().any(|address| forbidden_ip(address.ip())) {
        return Err(NotificationDeliveryError::Rejected);
    }
    addresses
        .into_iter()
        .next()
        .ok_or(NotificationDeliveryError::Rejected)
}

fn parse_mailbox(value: &str) -> Result<Mailbox, NotificationDeliveryError> {
    value
        .parse()
        .map_err(|_| NotificationDeliveryError::Rejected)
}

async fn bounded_body(
    response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, NotificationDeliveryError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(classify_reqwest)?;
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(NotificationDeliveryError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn classify_reqwest(error: reqwest::Error) -> NotificationDeliveryError {
    if error.is_timeout() {
        NotificationDeliveryError::Timeout
    } else {
        NotificationDeliveryError::Retryable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook::WebhookAdapterConfig;
    use axum::{Json, Router, http::StatusCode, routing::post};
    use metric_domain::{
        ProjectId, SecretBytes, Timestamp,
        grouping::IssueId,
        issue::IssueTransitionId,
        notifications::{
            AlertRuleId, NotificationDelivery, NotificationDeliveryId, NotificationDeliveryStatus,
            NotificationDestination, NotificationDestinationId, NotificationPayload,
            NotificationText, SmtpDestination, WebhookEndpoint,
        },
    };

    #[test]
    fn telegram_dynamic_values_are_html_escaped() {
        assert_eq!(
            escape_html(r#"<script attr="x">&"#),
            "&lt;script attr=&quot;x&quot;&gt;&amp;"
        );
    }

    #[test]
    fn telegram_token_shape_is_bounded_before_network_io() {
        assert!(valid_telegram_token(
            "123456789:ABCdef_012345678901234567890"
        ));
        assert!(!valid_telegram_token("not-a-token"));
        assert!(!valid_telegram_token("123:https://secret"));
    }

    #[tokio::test]
    async fn telegram_429_retry_after_is_preserved() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/bot123456789:ABCdef_012345678901234567890/sendMessage",
                    post(|| async {
                        (
                            StatusCode::TOO_MANY_REQUESTS,
                            Json(json!({"ok": false, "parameters": {"retry_after": 17}})),
                        )
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let secret_box = WebhookSecretBox::new(&SecretBytes::new([9; 32]));
        let adapter = provider(secret_box.clone(), format!("http://{address}").into(), true);
        let receipt = adapter
            .deliver(telegram_claim(
                secret_box
                    .seal(b"123456789:ABCdef_012345678901234567890")
                    .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(receipt.status, 429);
        assert_eq!(receipt.retry_after, Some(Duration::from_secs(17)));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn smtp_requires_a_successful_tls_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        let secret_box = WebhookSecretBox::new(&SecretBytes::new([8; 32]));
        let adapter = provider(secret_box.clone(), "https://api.telegram.org".into(), true);
        let result = adapter
            .deliver(smtp_claim(secret_box.seal(b"app-password").unwrap(), port))
            .await;
        assert!(matches!(
            result,
            Err(NotificationDeliveryError::Retryable | NotificationDeliveryError::Timeout)
        ));
        let _ = server.await;
    }

    fn provider(
        secret_box: WebhookSecretBox,
        telegram_api_base: Box<str>,
        allow_private_networks: bool,
    ) -> ProviderDeliveryAdapter {
        let webhook = ReqwestWebhookAdapter::new(
            secret_box.clone(),
            WebhookAdapterConfig {
                allow_http: true,
                allow_private_networks,
                ..WebhookAdapterConfig::default()
            },
        )
        .unwrap();
        ProviderDeliveryAdapter::new(
            webhook,
            secret_box,
            ProviderAdapterConfig {
                timeout: Duration::from_secs(2),
                max_response_bytes: 16 * 1024,
                max_retry_after: Duration::from_secs(60),
                allow_private_networks,
                telegram_api_base,
            },
        )
        .unwrap()
    }

    fn telegram_claim(
        secret: metric_domain::notifications::SealedWebhookSecret,
    ) -> ClaimedNotificationDelivery {
        claim(
            NotificationDestinationKind::Telegram,
            WebhookEndpoint::new("-1001234567890").unwrap(),
            secret,
            None,
        )
    }

    fn smtp_claim(
        secret: metric_domain::notifications::SealedWebhookSecret,
        port: u16,
    ) -> ClaimedNotificationDelivery {
        claim(
            NotificationDestinationKind::SmtpEmail,
            WebhookEndpoint::new("127.0.0.1").unwrap(),
            secret,
            Some(SmtpDestination {
                port,
                security: SmtpSecurity::Tls,
                username: NotificationText::new("alerts@example.com", 320).unwrap(),
                from: NotificationText::new("alerts@example.com", 320).unwrap(),
                recipients: vec![NotificationText::new("owner@example.com", 320).unwrap()]
                    .into_boxed_slice(),
            }),
        )
    }

    fn claim(
        kind: NotificationDestinationKind,
        endpoint: WebhookEndpoint,
        sealed_secret: metric_domain::notifications::SealedWebhookSecret,
        smtp: Option<SmtpDestination>,
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
                payload: NotificationPayload::new(
                    br#"{"version":1,"type":"new_issue","title":"failure"}"#.to_vec(),
                )
                .unwrap(),
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
                kind,
                endpoint,
                sealed_secret,
                smtp,
                enabled: true,
                created_at: Timestamp::from_unix_millis(1).unwrap(),
                updated_at: Timestamp::from_unix_millis(1).unwrap(),
            },
            attempt: 1,
            attempted_at: Timestamp::from_unix_millis(1).unwrap(),
        }
    }
}
