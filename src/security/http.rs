use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use async_trait::async_trait;
use tokio::net::lookup_host;
use url::Url;

use crate::application::ports::{ApplicationError, SourcePolicy};
use crate::domain::DocumentSource;

#[async_trait]
pub trait HttpAccessPolicy: Send + Sync {
    fn parse_and_validate(&self, source: &DocumentSource) -> Result<Url, ApplicationError>;

    fn validate_url(&self, url: &Url) -> Result<(), ApplicationError>;

    async fn resolve_public_endpoint(&self, url: &Url) -> Result<SocketAddr, ApplicationError>;
}

#[derive(Clone, Debug, Default)]
pub struct PublicHttpAccessPolicy {
    allow_http: bool,
}

impl PublicHttpAccessPolicy {
    pub fn https_only() -> Self {
        Self::default()
    }

    pub fn allow_http() -> Self {
        Self { allow_http: true }
    }
}

#[async_trait]
impl HttpAccessPolicy for PublicHttpAccessPolicy {
    fn parse_and_validate(&self, source: &DocumentSource) -> Result<Url, ApplicationError> {
        let url = Url::parse(source.0.trim()).map_err(|error| {
            ApplicationError::InvalidRequest(format!("invalid HTTP document URL: {error}"))
        })?;
        self.validate_url(&url)?;
        Ok(url)
    }

    fn validate_url(&self, url: &Url) -> Result<(), ApplicationError> {
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            "http" => {
                return Err(ApplicationError::BlockedSource(
                    "plain HTTP is disabled by policy".into(),
                ));
            }
            scheme => {
                return Err(ApplicationError::BlockedSource(format!(
                    "unsupported URL scheme: {scheme}"
                )));
            }
        }

        if !url.username().is_empty() || url.password().is_some() {
            return Err(ApplicationError::BlockedSource(
                "credentials embedded in document URLs are not allowed".into(),
            ));
        }

        let host = url
            .host_str()
            .ok_or_else(|| ApplicationError::InvalidRequest("URL must contain a host".into()))?;
        let normalized = host.trim_end_matches('.').to_ascii_lowercase();
        if normalized == "localhost"
            || normalized.ends_with(".localhost")
            || normalized.ends_with(".local")
            || normalized.ends_with(".internal")
            || normalized.ends_with(".home.arpa")
        {
            return Err(ApplicationError::BlockedSource(format!(
                "local hostname is not allowed: {host}"
            )));
        }

        if let Ok(ip) = host.parse::<IpAddr>() {
            ensure_public_ip(ip)?;
        }

        Ok(())
    }

    async fn resolve_public_endpoint(&self, url: &Url) -> Result<SocketAddr, ApplicationError> {
        self.validate_url(url)?;
        let host = url
            .host_str()
            .ok_or_else(|| ApplicationError::InvalidRequest("URL must contain a host".into()))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| ApplicationError::InvalidRequest("URL has no usable port".into()))?;

        if let Ok(ip) = host.parse::<IpAddr>() {
            ensure_public_ip(ip)?;
            return Ok(SocketAddr::new(ip, port));
        }

        let addresses = lookup_host((host, port)).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("DNS lookup failed for {host}: {error}"))
        })?;

        let mut selected = None;
        for address in addresses {
            if let Err(error) = ensure_public_ip(address.ip()) {
                return Err(ApplicationError::BlockedSource(format!(
                    "DNS for {host} resolved to a blocked address {}: {error}",
                    address.ip()
                )));
            }
            selected.get_or_insert(address);
        }

        selected.ok_or_else(|| {
            ApplicationError::RetrievalFailed(format!("DNS lookup returned no address for {host}"))
        })
    }
}

#[async_trait]
impl SourcePolicy for PublicHttpAccessPolicy {
    async fn validate(&self, source: &DocumentSource) -> Result<(), ApplicationError> {
        let url = self.parse_and_validate(source)?;
        self.resolve_public_endpoint(&url).await?;
        Ok(())
    }
}

fn ensure_public_ip(ip: IpAddr) -> Result<(), ApplicationError> {
    let blocked = match ip {
        IpAddr::V4(address) => blocked_ipv4(address),
        IpAddr::V6(address) => blocked_ipv6(address),
    };

    if blocked {
        Err(ApplicationError::BlockedSource(format!(
            "non-public network address is not allowed: {ip}"
        )))
    } else {
        Ok(())
    }
}

fn blocked_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_documentation()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || octets[0] >= 240
}

fn blocked_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return blocked_ipv4(mapped);
    }

    let segments = address.segments();
    address.is_unspecified()
        || address.is_loopback()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        || address.is_multicast()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] & 0xffc0) == 0xfec0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_and_special_ipv4_ranges() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "0.0.0.0",
        ] {
            assert!(ensure_public_ip(value.parse().expect("valid test IP")).is_err());
        }
        assert!(ensure_public_ip("1.1.1.1".parse().expect("valid public IP")).is_ok());
    }

    #[test]
    fn blocks_private_and_special_ipv6_ranges() {
        for value in ["::1", "fe80::1", "fc00::1", "fd00::1", "2001:db8::1"] {
            assert!(ensure_public_ip(value.parse().expect("valid test IP")).is_err());
        }
        assert!(ensure_public_ip("2606:4700:4700::1111".parse().expect("valid public IP")).is_ok());
    }

    #[test]
    fn rejects_credentials_and_local_hostnames() {
        let policy = PublicHttpAccessPolicy::allow_http();
        for value in [
            "http://localhost/a",
            "http://service.internal/a",
            "http://user:pass@example.com/a",
            "file:///tmp/book.pdf",
        ] {
            assert!(
                policy
                    .parse_and_validate(&DocumentSource(value.into()))
                    .is_err()
            );
        }
    }
}
