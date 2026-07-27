//! Server-originated Uptime HTTP executor with DNS pinning and redirect revalidation.

use std::{net::IpAddr, time::Instant};

use futures_util::StreamExt;
use metric_domain::monitors::{MonitorDefinition, UptimeFailure, UptimeHeader, UptimeMethod};
use metric_ports::{PortFuture, SignalStoreError, UptimeCheckExecutor, UptimeCheckResult};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue, LOCATION},
    redirect::Policy,
};
use tokio::time::{Duration, timeout};
use url::Url;

use crate::webhook::{WebhookSecretBox, forbidden_ip};

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub struct ReqwestUptimeExecutor {
    secret_box: WebhookSecretBox,
}

impl ReqwestUptimeExecutor {
    #[must_use]
    pub const fn new(secret_box: WebhookSecretBox) -> Self {
        Self { secret_box }
    }

    async fn execute_inner(
        &self,
        monitor: MonitorDefinition,
    ) -> Result<UptimeCheckResult, SignalStoreError> {
        let config = monitor.uptime.ok_or(SignalStoreError::InvalidData)?;
        let started = Instant::now();
        let budget = Duration::from_secs(u64::from(config.timeout_seconds));
        let result = timeout(budget, async {
            let original = Url::parse(config.endpoint.as_str())
                .map_err(|_| UptimeFailure::ForbiddenAddress)?;
            let original_origin = origin(&original).ok_or(UptimeFailure::ForbiddenAddress)?;
            let mut endpoint = original;
            let mut redirects = 0_u8;
            loop {
                validate_endpoint(&endpoint)?;
                let host = endpoint
                    .host_str()
                    .ok_or(UptimeFailure::ForbiddenAddress)?
                    .to_owned();
                let port = endpoint
                    .port_or_known_default()
                    .ok_or(UptimeFailure::ForbiddenAddress)?;
                let addresses = tokio::net::lookup_host((host.as_str(), port))
                    .await
                    .map_err(|_| UptimeFailure::Dns)?
                    .collect::<Vec<_>>();
                if addresses.is_empty() {
                    return Err(UptimeFailure::Dns);
                }
                if addresses.iter().any(|address| forbidden_ip(address.ip())) {
                    return Err(UptimeFailure::ForbiddenAddress);
                }
                let client = Client::builder()
                    .redirect(Policy::none())
                    .resolve(&host, addresses[0])
                    .build()
                    .map_err(|_| UptimeFailure::Connect)?;
                let same_origin =
                    redirects == 0 || origin(&endpoint).as_ref() == Some(&original_origin);
                let headers = self.headers(&config.headers, same_origin, redirects == 0)?;
                let request = match config.method {
                    UptimeMethod::Get => client.get(endpoint.clone()),
                    UptimeMethod::Head => client.head(endpoint.clone()),
                };
                let response = request
                    .headers(headers)
                    .send()
                    .await
                    .map_err(classify_request)?;
                let status = response.status().as_u16();
                if response.status().is_redirection() {
                    if redirects >= config.max_redirects {
                        return Ok((Some(status), Some(UptimeFailure::TooManyRedirects)));
                    }
                    let location = response
                        .headers()
                        .get(LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or(UptimeFailure::Redirect)?;
                    endpoint = endpoint
                        .join(location)
                        .map_err(|_| UptimeFailure::Redirect)?;
                    redirects = redirects.saturating_add(1);
                    continue;
                }
                let mut received = 0_usize;
                let mut body = response.bytes_stream();
                while let Some(chunk) = body.next().await {
                    let chunk = chunk.map_err(classify_request)?;
                    received = received
                        .checked_add(chunk.len())
                        .ok_or(UptimeFailure::ResponseTooLarge)?;
                    if received > MAX_RESPONSE_BYTES {
                        return Ok((Some(status), Some(UptimeFailure::ResponseTooLarge)));
                    }
                }
                let failure = (!(config.expected_status_min..=config.expected_status_max)
                    .contains(&status))
                .then_some(UptimeFailure::UnexpectedStatus);
                return Ok((Some(status), failure));
            }
        })
        .await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match result {
            Err(_) => Ok(UptimeCheckResult {
                http_status: None,
                failure: Some(UptimeFailure::Timeout),
                duration_ms,
            }),
            Ok(Err(failure)) => Ok(UptimeCheckResult {
                http_status: None,
                failure: Some(failure),
                duration_ms,
            }),
            Ok(Ok((http_status, failure))) => Ok(UptimeCheckResult {
                http_status,
                failure,
                duration_ms,
            }),
        }
    }

    fn headers(
        &self,
        configured: &[UptimeHeader],
        same_origin: bool,
        first_hop: bool,
    ) -> Result<HeaderMap, UptimeFailure> {
        let mut headers = HeaderMap::new();
        if !same_origin {
            return Ok(headers);
        }
        for header in configured {
            if header.sensitive && !first_hop {
                continue;
            }
            let name = HeaderName::from_bytes(header.name.as_bytes())
                .map_err(|_| UptimeFailure::Connect)?;
            let plaintext = self
                .secret_box
                .open_uptime_header(&header.value)
                .map_err(|_| UptimeFailure::Connect)?;
            let value = HeaderValue::from_bytes(&plaintext).map_err(|_| UptimeFailure::Connect)?;
            headers.insert(name, value);
        }
        Ok(headers)
    }
}

impl UptimeCheckExecutor for ReqwestUptimeExecutor {
    fn execute(
        &self,
        monitor: MonitorDefinition,
    ) -> PortFuture<'_, Result<UptimeCheckResult, SignalStoreError>> {
        Box::pin(self.execute_inner(monitor))
    }
}

fn origin(endpoint: &Url) -> Option<(Box<str>, Box<str>, u16)> {
    Some((
        endpoint.scheme().to_ascii_lowercase().into_boxed_str(),
        endpoint.host_str()?.to_ascii_lowercase().into_boxed_str(),
        endpoint.port_or_known_default()?,
    ))
}

fn validate_endpoint(endpoint: &Url) -> Result<(), UptimeFailure> {
    if !matches!(endpoint.scheme(), "http" | "https")
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(UptimeFailure::ForbiddenAddress);
    }
    if let Some(ip) = endpoint.host().and_then(|host| match host {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(_) => None,
    }) && forbidden_ip(ip)
    {
        return Err(UptimeFailure::ForbiddenAddress);
    }
    Ok(())
}

fn classify_request(error: reqwest::Error) -> UptimeFailure {
    if error.is_timeout() {
        UptimeFailure::Timeout
    } else {
        UptimeFailure::Connect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{SecretBytes, monitors::SealedUptimeHeaderValue};

    #[test]
    fn rejects_private_metadata_and_mapped_ipv6_corpus() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                forbidden_ip(address.parse().unwrap()),
                "{address} must be rejected"
            );
        }
        assert!(!forbidden_ip("1.1.1.1".parse().unwrap()));
        assert!(!forbidden_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn normalized_origin_includes_default_port() {
        assert_eq!(
            origin(&Url::parse("https://EXAMPLE.com/path").unwrap()),
            Some(("https".into(), "example.com".into(), 443))
        );
    }

    #[test]
    fn redirect_header_policy_strips_secrets_and_cross_origin_values() {
        let secret_box = WebhookSecretBox::new(&SecretBytes::new([9; 32]));
        let executor = ReqwestUptimeExecutor::new(secret_box.clone());
        let header = |name: &str, value: &str, sensitive: bool| UptimeHeader {
            name: name.into(),
            value: secret_box.seal_uptime_header(value.as_bytes()).unwrap(),
            sensitive,
        };
        let configured = [
            header("authorization", "Bearer secret", true),
            header("x-health-check", "metric", false),
        ];
        let first = executor.headers(&configured, true, true).unwrap();
        assert_eq!(first.len(), 2);
        let same_origin_redirect = executor.headers(&configured, true, false).unwrap();
        assert!(!same_origin_redirect.contains_key("authorization"));
        assert_eq!(same_origin_redirect["x-health-check"], "metric");
        assert!(
            executor
                .headers(&configured, false, false)
                .unwrap()
                .is_empty()
        );

        let sealed = secret_box.seal_uptime_header(b"secret").unwrap();
        assert!(
            !sealed
                .expose_ciphertext()
                .windows(6)
                .any(|part| part == b"secret")
        );
        assert_eq!(secret_box.open_uptime_header(&sealed).unwrap(), b"secret");
        let _type_fence: SealedUptimeHeaderValue = sealed;
    }
}
