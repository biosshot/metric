//! Generic webhook delivery adapter with signing, secret sealing and SSRF controls.

use std::{net::IpAddr, sync::Arc, time::Duration};

use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Payload},
};
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use metric_domain::{
    SecretBytes,
    notifications::{
        ClaimedNotificationDelivery, NotificationDestinationKind, SealedWebhookSecret,
    },
};
use metric_ports::{
    NotificationDeliveryAdapter, NotificationDeliveryError, NotificationDeliveryReceipt, PortFuture,
};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
};
use sha2::{Digest, Sha256};
use url::Url;

const SECRET_VERSION: u8 = 1;
const SECRET_NONCE_BYTES: usize = 12;
const SECRET_AAD: &[u8] = b"metric/webhook-secret/v1";
const SIGNATURE_HEADER: &str = "x-metric-signature";
const TIMESTAMP_HEADER: &str = "x-metric-timestamp";

#[derive(Debug, Clone, Copy)]
pub struct WebhookAdapterConfig {
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_retry_after: Duration,
    pub allow_http: bool,
    pub allow_private_networks: bool,
}

impl Default for WebhookAdapterConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_response_bytes: 64 * 1024,
            max_retry_after: Duration::from_secs(60 * 60),
            allow_http: false,
            allow_private_networks: false,
        }
    }
}

#[derive(Clone)]
pub struct WebhookSecretBox {
    cipher: Arc<ChaCha20Poly1305>,
}

impl WebhookSecretBox {
    #[must_use]
    pub fn new(master: &SecretBytes) -> Self {
        let mut derivation = Sha256::new();
        derivation.update(b"metric/webhook-secret-key/v1");
        derivation.update(master.expose());
        let key = derivation.finalize();
        Self {
            cipher: Arc::new(ChaCha20Poly1305::new_from_slice(&key).expect("SHA-256 key length")),
        }
    }

    pub fn seal(&self, secret: &[u8]) -> Result<SealedWebhookSecret, NotificationDeliveryError> {
        if secret.is_empty() || secret.len() > 4_096 {
            return Err(NotificationDeliveryError::InvalidSecret);
        }
        let mut nonce = [0_u8; SECRET_NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| NotificationDeliveryError::InvalidSecret)?;
        let ciphertext = self
            .cipher
            .encrypt(
                chacha20poly1305::Nonce::from_slice(&nonce),
                Payload {
                    msg: secret,
                    aad: SECRET_AAD,
                },
            )
            .map_err(|_| NotificationDeliveryError::InvalidSecret)?;
        let mut sealed = Vec::with_capacity(1 + nonce.len() + ciphertext.len());
        sealed.push(SECRET_VERSION);
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        SealedWebhookSecret::new(sealed).map_err(|_| NotificationDeliveryError::InvalidSecret)
    }

    pub(crate) fn open(
        &self,
        sealed: &SealedWebhookSecret,
    ) -> Result<Vec<u8>, NotificationDeliveryError> {
        let bytes = sealed.expose_ciphertext();
        if bytes.len() <= 1 + SECRET_NONCE_BYTES || bytes[0] != SECRET_VERSION {
            return Err(NotificationDeliveryError::InvalidSecret);
        }
        self.cipher
            .decrypt(
                chacha20poly1305::Nonce::from_slice(&bytes[1..1 + SECRET_NONCE_BYTES]),
                Payload {
                    msg: &bytes[1 + SECRET_NONCE_BYTES..],
                    aad: SECRET_AAD,
                },
            )
            .map_err(|_| NotificationDeliveryError::InvalidSecret)
    }
}

pub struct ReqwestWebhookAdapter {
    secret_box: WebhookSecretBox,
    config: WebhookAdapterConfig,
}

impl ReqwestWebhookAdapter {
    pub fn new(
        secret_box: WebhookSecretBox,
        config: WebhookAdapterConfig,
    ) -> Result<Self, NotificationDeliveryError> {
        if config.timeout.is_zero()
            || !(1..=1024 * 1024).contains(&config.max_response_bytes)
            || config.max_retry_after.is_zero()
        {
            return Err(NotificationDeliveryError::Rejected);
        }
        Ok(Self { secret_box, config })
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
        if claim.destination.kind != NotificationDestinationKind::Webhook {
            return Err(NotificationDeliveryError::Rejected);
        }
        let endpoint = Url::parse(claim.destination.endpoint.as_str())
            .map_err(|_| NotificationDeliveryError::Rejected)?;
        validate_url(&endpoint, self.config)?;
        let host = endpoint
            .host_str()
            .ok_or(NotificationDeliveryError::Rejected)?
            .to_owned();
        let port = endpoint
            .port_or_known_default()
            .ok_or(NotificationDeliveryError::Rejected)?;
        let addresses = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| NotificationDeliveryError::Retryable)?
            .collect::<Vec<_>>();
        if addresses.is_empty()
            || (!self.config.allow_private_networks
                && addresses.iter().any(|address| forbidden_ip(address.ip())))
        {
            return Err(NotificationDeliveryError::Rejected);
        }
        let pinned = addresses[0];
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(self.config.timeout)
            .resolve(&host, pinned)
            .build()
            .map_err(|_| NotificationDeliveryError::Retryable)?;
        let secret = self.secret_box.open(&claim.destination.sealed_secret)?;
        let timestamp = claim.attempted_at.unix_millis().to_string();
        let delivery_id = hex::encode(claim.delivery.id.as_bytes());
        let signature = signature(
            &secret,
            &delivery_id,
            &timestamp,
            claim.delivery.payload.as_bytes(),
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("idempotency-key"),
            HeaderValue::from_str(&delivery_id).map_err(|_| NotificationDeliveryError::Rejected)?,
        );
        headers.insert(
            HeaderName::from_static("x-delivery-id"),
            HeaderValue::from_str(&delivery_id).map_err(|_| NotificationDeliveryError::Rejected)?,
        );
        headers.insert(
            HeaderName::from_static(TIMESTAMP_HEADER),
            HeaderValue::from_str(&timestamp).map_err(|_| NotificationDeliveryError::Rejected)?,
        );
        headers.insert(
            HeaderName::from_static(SIGNATURE_HEADER),
            HeaderValue::from_str(&format!("sha256={signature}"))
                .map_err(|_| NotificationDeliveryError::Rejected)?,
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let response = client
            .post(endpoint)
            .headers(headers)
            .body(claim.delivery.payload.as_bytes().to_vec())
            .send()
            .await
            .map_err(classify_reqwest)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .filter(|value| *value <= self.config.max_retry_after);
        let mut bytes = 0_usize;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(classify_reqwest)?;
            bytes = bytes
                .checked_add(chunk.len())
                .ok_or(NotificationDeliveryError::ResponseTooLarge)?;
            if bytes > self.config.max_response_bytes {
                break;
            }
        }
        Ok(NotificationDeliveryReceipt {
            status,
            retry_after,
        })
    }
}

impl NotificationDeliveryAdapter for ReqwestWebhookAdapter {
    fn deliver(
        &self,
        claim: ClaimedNotificationDelivery,
    ) -> PortFuture<'_, Result<NotificationDeliveryReceipt, NotificationDeliveryError>> {
        Box::pin(self.deliver_inner(claim))
    }
}

fn validate_url(
    endpoint: &Url,
    config: WebhookAdapterConfig,
) -> Result<(), NotificationDeliveryError> {
    if endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
        || !matches!(endpoint.scheme(), "https" | "http")
        || (endpoint.scheme() == "http" && !config.allow_http)
    {
        return Err(NotificationDeliveryError::Rejected);
    }
    if let Some(ip) = endpoint.host().and_then(|host| match host {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(_) => None,
    }) && !config.allow_private_networks
        && forbidden_ip(ip)
    {
        return Err(NotificationDeliveryError::Rejected);
    }
    Ok(())
}

pub(crate) fn forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

fn signature(secret: &[u8], delivery_id: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC accepts any key");
    mac.update(b"v1\n");
    mac.update(delivery_id.as_bytes());
    mac.update(b"\n");
    mac.update(timestamp.as_bytes());
    mac.update(b"\n");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
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

    #[test]
    fn canonical_signature_vector_is_stable() {
        assert_eq!(
            signature(b"secret", "001122", "1700000000000", br#"{"ok":true}"#),
            "edc74156fa0e664e8eda81d199d10ddbd55269f1cc8b9582647d3ab1f84ab946"
        );
    }

    #[test]
    fn secret_is_encrypted_and_authenticated() {
        let secret_box = WebhookSecretBox::new(&SecretBytes::new([7; 32]));
        let sealed = secret_box.seal(b"not-in-storage").unwrap();
        assert!(
            !sealed
                .expose_ciphertext()
                .windows(b"not-in-storage".len())
                .any(|window| window == b"not-in-storage")
        );
        assert_eq!(secret_box.open(&sealed).unwrap(), b"not-in-storage");
        let mut corrupt = sealed.expose_ciphertext().to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(
            secret_box
                .open(&SealedWebhookSecret::new(corrupt).unwrap())
                .is_err()
        );
    }

    #[test]
    fn ssrf_and_redirect_inputs_fail_closed() {
        let config = WebhookAdapterConfig::default();
        for endpoint in [
            "http://example.com/hook",
            "https://127.0.0.1/hook",
            "https://10.0.0.1/hook",
            "https://169.254.169.254/latest",
            "https://[::1]/hook",
            "https://user:pass@example.com/hook",
            "file:///tmp/hook",
        ] {
            assert!(validate_url(&Url::parse(endpoint).unwrap(), config).is_err());
        }
        assert!(validate_url(&Url::parse("https://example.com/hook").unwrap(), config).is_ok());
    }
}
