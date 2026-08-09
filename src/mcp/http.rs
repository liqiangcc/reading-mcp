use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::{Json, Router, routing::get};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};

use super::ReadingMcpServer;

pub const MCP_HTTP_PATH: &str = "/mcp";
pub const HEALTH_PATH: &str = "/healthz";
pub const READY_PATH: &str = "/readyz";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpTransportConfig {
    pub bind: SocketAddr,
    pub allowed_hosts: Option<Vec<String>>,
    pub allowed_origins: Vec<String>,
}

impl Default for HttpTransportConfig {
    fn default() -> Self {
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8000);
        Self {
            bind,
            allowed_hosts: None,
            allowed_origins: default_loopback_origins(bind.port()),
        }
    }
}

impl HttpTransportConfig {
    pub fn from_env() -> Result<Self, String> {
        let mut config = Self::default();

        if let Ok(value) = std::env::var("READING_MCP_SERVER_BIND") {
            config.bind = value.parse::<SocketAddr>().map_err(|error| {
                format!("READING_MCP_SERVER_BIND must be an IP socket address: {error}")
            })?;
        }

        validate_bind(config.bind)?;
        config.allowed_hosts = env_csv("READING_MCP_SERVER_ALLOWED_HOSTS");
        config.allowed_origins = env_csv("READING_MCP_SERVER_ALLOWED_ORIGINS")
            .unwrap_or_else(|| default_loopback_origins(config.bind.port()));

        Ok(config)
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}{}", self.bind, MCP_HTTP_PATH)
    }
}

pub fn streamable_http_router(server: ReadingMcpServer, config: &HttpTransportConfig) -> Router {
    let mut transport_config =
        StreamableHttpServerConfig::default().with_allowed_origins(config.allowed_origins.clone());
    if let Some(hosts) = &config.allowed_hosts {
        transport_config = transport_config.with_allowed_hosts(hosts.clone());
    }

    let shared_server = server.clone();
    let service = StreamableHttpService::new(
        move || Ok(shared_server.clone()),
        LocalSessionManager::default().into(),
        transport_config,
    );

    Router::new()
        .route(HEALTH_PATH, get(healthz))
        .route(READY_PATH, get(readyz))
        .nest_service(MCP_HTTP_PATH, service)
}

pub async fn serve_streamable_http(
    server: ReadingMcpServer,
    config: HttpTransportConfig,
) -> std::io::Result<()> {
    let router = streamable_http_router(server, &config);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let bound = listener.local_addr()?;
    eprintln!(
        "{}",
        serde_json::json!({
            "event": "mcp_http_listen",
            "endpoint": format!("http://{bound}{MCP_HTTP_PATH}"),
            "health": format!("http://{bound}{HEALTH_PATH}"),
            "ready": format!("http://{bound}{READY_PATH}"),
            "remote_access": "use a trusted MCP tunnel or reverse proxy; direct non-loopback bind is disabled"
        })
    );
    axum::serve(listener, router).await
}

async fn healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "reading-mcp",
        "transport": "streamable-http"
    }))
}

async fn readyz() -> Json<Value> {
    Json(json!({
        "status": "ready",
        "service": "reading-mcp",
        "transport": "streamable-http",
        "mcp_path": MCP_HTTP_PATH
    }))
}

fn validate_bind(bind: SocketAddr) -> Result<(), String> {
    if bind.ip().is_loopback() {
        Ok(())
    } else {
        Err(
            "READING_MCP_SERVER_BIND must use a loopback address in Phase 7; use a trusted MCP tunnel or reverse proxy for remote access"
                .into(),
        )
    }
}

fn default_loopback_origins(port: u16) -> Vec<String> {
    vec![
        format!("http://localhost:{port}"),
        format!("http://127.0.0.1:{port}"),
        format!("http://[::1]:{port}"),
    ]
}

fn env_csv(name: &str) -> Option<Vec<String>> {
    let values = std::env::var(name)
        .ok()?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_transport_is_loopback_only_and_validates_origins() {
        let config = HttpTransportConfig::default();
        assert!(config.bind.ip().is_loopback());
        assert_eq!(config.bind.port(), 8000);
        assert_eq!(config.endpoint(), "http://127.0.0.1:8000/mcp");
        assert_eq!(
            config.allowed_origins,
            vec![
                "http://localhost:8000",
                "http://127.0.0.1:8000",
                "http://[::1]:8000",
            ]
        );
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let public_bind = "0.0.0.0:8000".parse().expect("valid socket address");
        assert!(validate_bind(public_bind).is_err());

        let private_bind = "192.168.1.10:8000".parse().expect("valid socket address");
        assert!(validate_bind(private_bind).is_err());

        let loopback_bind = "[::1]:8000".parse().expect("valid socket address");
        assert!(validate_bind(loopback_bind).is_ok());
    }
}
