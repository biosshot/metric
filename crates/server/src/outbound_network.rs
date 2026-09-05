//! Shared DNS resolution, private-address policy and HTTP hostname pinning.

use std::{net::IpAddr, time::Duration};

use reqwest::{Client, redirect::Policy};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub(crate) enum OutboundNetworkError {
    #[error("outbound endpoint has no resolvable host")]
    InvalidHost,
    #[error("outbound DNS resolution failed")]
    Dns,
    #[error("outbound endpoint resolves to a forbidden address")]
    ForbiddenAddress,
    #[error("outbound HTTP client could not be built")]
    Client,
}

pub(crate) async fn pinned_http_client(
    endpoint: &Url,
    allow_private_networks: bool,
    timeout: Option<Duration>,
) -> Result<Client, OutboundNetworkError> {
    let host = endpoint
        .host_str()
        .ok_or(OutboundNetworkError::InvalidHost)?
        .to_owned();
    let port = endpoint
        .port_or_known_default()
        .ok_or(OutboundNetworkError::InvalidHost)?;
    let address = resolve_socket(&host, port, allow_private_networks).await?;
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .resolve(&host, address);
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder.build().map_err(|_| OutboundNetworkError::Client)
}

pub(crate) async fn resolve_socket(
    host: &str,
    port: u16,
    allow_private_networks: bool,
) -> Result<std::net::SocketAddr, OutboundNetworkError> {
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| OutboundNetworkError::Dns)?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(OutboundNetworkError::Dns);
    }
    if !allow_private_networks && addresses.iter().any(|address| forbidden_ip(address.ip())) {
        return Err(OutboundNetworkError::ForbiddenAddress);
    }
    addresses
        .into_iter()
        .next()
        .ok_or(OutboundNetworkError::Dns)
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
                || ip.octets()[0] >= 240
                || matches!(ip.octets(), [100, 64..=127, _, _])
                || matches!(ip.octets(), [198, 18..=19, _, _])
                || matches!(ip.octets(), [169, 254, 169, 254])
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|ipv4| forbidden_ip(IpAddr::V4(ipv4)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_metadata_addresses_are_forbidden() {
        for address in ["127.0.0.1", "10.0.0.1", "169.254.169.254", "::1", "fc00::1"] {
            assert!(forbidden_ip(address.parse().unwrap()), "accepted {address}");
        }
        assert!(!forbidden_ip("1.1.1.1".parse().unwrap()));
        assert!(!forbidden_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
