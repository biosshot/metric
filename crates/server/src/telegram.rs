//! Telegram Bot API transport shared by administration and delivery paths.

use std::time::Duration;

use futures_util::StreamExt;
use metric_domain::notifications::TelegramApiBase;
use reqwest::Response;
use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::outbound_network::{OutboundNetworkError, pinned_http_client};

#[derive(Debug, Clone, Copy)]
pub struct TelegramApiConfig {
    pub timeout: Duration,
    pub maximum_response_bytes: usize,
    pub allow_private_networks: bool,
}

#[derive(Debug, Error)]
pub enum TelegramApiError {
    #[error("Telegram API request is invalid")]
    InvalidRequest,
    #[error("Telegram API endpoint is forbidden")]
    ForbiddenEndpoint,
    #[error("Telegram API request timed out")]
    Timeout,
    #[error("Telegram API is temporarily unavailable")]
    Unavailable,
    #[error("Telegram API response is too large")]
    ResponseTooLarge,
    #[error("Telegram API rejected the request")]
    Rejected(TelegramApiRejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramApiRejection {
    pub http_status: u16,
    pub error_code: Option<i64>,
    pub description: Option<Box<str>>,
}

#[derive(Debug)]
pub struct TelegramApiResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl TelegramApiResponse {
    pub fn successful_json(self) -> Result<Value, TelegramApiError> {
        let value = serde_json::from_slice::<Value>(&self.body).map_err(|_| {
            TelegramApiError::Rejected(TelegramApiRejection {
                http_status: self.status,
                error_code: None,
                description: None,
            })
        })?;
        if !(200..300).contains(&self.status)
            || value.get("ok").and_then(Value::as_bool) != Some(true)
        {
            return Err(TelegramApiError::Rejected(TelegramApiRejection {
                http_status: self.status,
                error_code: value.get("error_code").and_then(Value::as_i64),
                description: value
                    .get("description")
                    .and_then(Value::as_str)
                    .map(|value| value.chars().take(256).collect::<String>().into_boxed_str()),
            }));
        }
        Ok(value)
    }
}

#[derive(Debug)]
pub struct TelegramApiClient {
    config: TelegramApiConfig,
}

impl TelegramApiClient {
    pub fn new(config: TelegramApiConfig) -> Result<Self, TelegramApiError> {
        if config.timeout.is_zero() || !(1..=1024 * 1024).contains(&config.maximum_response_bytes) {
            return Err(TelegramApiError::InvalidRequest);
        }
        Ok(Self { config })
    }

    pub async fn call(
        &self,
        api_base: &TelegramApiBase,
        token: &str,
        method: &'static str,
        body: Value,
    ) -> Result<TelegramApiResponse, TelegramApiError> {
        if !valid_telegram_token(token)
            || method.is_empty()
            || !method.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(TelegramApiError::InvalidRequest);
        }
        let endpoint = telegram_method_url(api_base, token, method)?;
        let client = pinned_http_client(
            &endpoint,
            self.config.allow_private_networks,
            Some(self.config.timeout),
        )
        .await
        .map_err(map_network_error)?;
        let response = client
            .post(endpoint)
            .json(&body)
            .send()
            .await
            .map_err(classify_reqwest)?;
        let status = response.status().as_u16();
        let body = bounded_body(response, self.config.maximum_response_bytes).await?;
        Ok(TelegramApiResponse { status, body })
    }
}

fn telegram_method_url(
    api_base: &TelegramApiBase,
    token: &str,
    method: &str,
) -> Result<Url, TelegramApiError> {
    let mut endpoint =
        Url::parse(api_base.as_str()).map_err(|_| TelegramApiError::InvalidRequest)?;
    let path = format!(
        "{}/bot{token}/{method}",
        endpoint.path().trim_end_matches('/')
    );
    endpoint.set_path(&path);
    Ok(endpoint)
}

async fn bounded_body(response: Response, maximum: usize) -> Result<Vec<u8>, TelegramApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(TelegramApiError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(classify_reqwest)?;
        if body.len().saturating_add(chunk.len()) > maximum {
            return Err(TelegramApiError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn map_network_error(error: OutboundNetworkError) -> TelegramApiError {
    match error {
        OutboundNetworkError::InvalidHost | OutboundNetworkError::ForbiddenAddress => {
            TelegramApiError::ForbiddenEndpoint
        }
        OutboundNetworkError::Dns | OutboundNetworkError::Client => TelegramApiError::Unavailable,
    }
}

fn classify_reqwest(error: reqwest::Error) -> TelegramApiError {
    if error.is_timeout() {
        TelegramApiError::Timeout
    } else {
        TelegramApiError::Unavailable
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::StatusCode, routing::post};

    #[test]
    fn method_url_preserves_a_proxy_path_and_accepts_http() {
        let base = TelegramApiBase::new("http://example.com:8080/proxy/tg/").unwrap();
        assert_eq!(
            telegram_method_url(&base, "123:abcdefghijklmnopqrstuvwxyz", "sendMessage")
                .unwrap()
                .as_str(),
            "http://example.com:8080/proxy/tg/bot123:abcdefghijklmnopqrstuvwxyz/sendMessage"
        );
    }

    #[test]
    fn token_shape_is_bounded_before_network_io() {
        assert!(valid_telegram_token(
            "123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcd-1234"
        ));
        assert!(!valid_telegram_token("not-a-token"));
        assert!(!valid_telegram_token("123:https://secret"));
    }

    #[test]
    fn rejected_response_preserves_a_bounded_provider_reason() {
        let error = TelegramApiResponse {
            status: 400,
            body: serde_json::to_vec(&serde_json::json!({
                "ok": false,
                "error_code": 400,
                "description": "Bad Request: chat not found"
            }))
            .unwrap(),
        }
        .successful_json()
        .unwrap_err();
        assert!(matches!(
            error,
            TelegramApiError::Rejected(TelegramApiRejection {
                http_status: 400,
                error_code: Some(400),
                description: Some(description),
            }) if description.as_ref() == "Bad Request: chat not found"
        ));

        let long_reason = "x".repeat(300);
        let error = TelegramApiResponse {
            status: 400,
            body: serde_json::to_vec(&serde_json::json!({
                "ok": false,
                "description": long_reason
            }))
            .unwrap(),
        }
        .successful_json()
        .unwrap_err();
        let TelegramApiError::Rejected(rejection) = error else {
            panic!("expected a Telegram rejection");
        };
        assert_eq!(rejection.description.unwrap().chars().count(), 256);
    }

    #[tokio::test]
    async fn transport_bounds_responses_disables_redirects_and_applies_private_policy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/large/bot123:abcdefghijklmnopqrstuvwxyz/getMe",
                        post(|| async { "x".repeat(128) }),
                    )
                    .route(
                        "/redirect/bot123:abcdefghijklmnopqrstuvwxyz/getMe",
                        post(|| async {
                            (
                                StatusCode::FOUND,
                                [(reqwest::header::LOCATION.as_str(), "/large")],
                            )
                        }),
                    ),
            )
            .await
            .unwrap();
        });
        let permitted = TelegramApiClient::new(TelegramApiConfig {
            timeout: Duration::from_secs(2),
            maximum_response_bytes: 32,
            allow_private_networks: true,
        })
        .unwrap();
        let token = "123:abcdefghijklmnopqrstuvwxyz";
        let large = TelegramApiBase::new(format!("http://{address}/large")).unwrap();
        assert!(matches!(
            permitted.call(&large, token, "getMe", Value::Null).await,
            Err(TelegramApiError::ResponseTooLarge)
        ));
        let redirect = TelegramApiBase::new(format!("http://{address}/redirect")).unwrap();
        assert_eq!(
            permitted
                .call(&redirect, token, "getMe", Value::Null)
                .await
                .unwrap()
                .status,
            302
        );

        let restricted = TelegramApiClient::new(TelegramApiConfig {
            timeout: Duration::from_secs(2),
            maximum_response_bytes: 32,
            allow_private_networks: false,
        })
        .unwrap();
        assert!(matches!(
            restricted.call(&large, token, "getMe", Value::Null).await,
            Err(TelegramApiError::ForbiddenEndpoint)
        ));
        server.abort();
        let _ = server.await;
    }
}
