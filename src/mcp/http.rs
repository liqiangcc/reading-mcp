use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};

use super::ReadingMcpServer;

pub const MCP_HTTP_PATH: &str = "/mcp";
pub const HEALTH_PATH: &str = "/healthz";
pub const READY_PATH: &str = "/readyz";
const LEGACY_HEALTH_PATH: &str = "/health";
const DEFAULT_BIND_PORT: u16 = 8787;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpTransportConfig {
    pub bind: SocketAddr,
    pub token: String,
    pub allowed_hosts: Option<Vec<String>>,
    pub allowed_origins: Vec<String>,
    pub disable_host_validation: bool,
}

impl HttpTransportConfig {
    pub fn from_env() -> Result<Self, String> {
        let token = std::env::var("READING_MCP_HTTP_TOKEN")
            .map_err(|_| "READING_MCP_HTTP_TOKEN must be set for the HTTP server".to_string())?;
        if token.len() < 32 {
            return Err("READING_MCP_HTTP_TOKEN must contain at least 32 characters".into());
        }

        let bind = std::env::var("READING_MCP_HTTP_BIND")
            .unwrap_or_else(|_| format!("127.0.0.1:{DEFAULT_BIND_PORT}"))
            .parse::<SocketAddr>()
            .map_err(|error| format!("READING_MCP_HTTP_BIND must be an IP socket address: {error}"))?;
        validate_bind(bind)?;

        let allowed_hosts = env_csv("READING_MCP_HTTP_ALLOWED_HOSTS");
        let allowed_origins = env_csv("READING_MCP_HTTP_ALLOWED_ORIGINS")
            .unwrap_or_else(|| default_loopback_origins(bind.port()));
        let disable_host_validation = env_bool("READING_MCP_HTTP_DISABLE_HOST_VALIDATION", false)?;
        if disable_host_validation && allowed_hosts.is_some() {
            return Err(
                "READING_MCP_HTTP_ALLOWED_HOSTS and READING_MCP_HTTP_DISABLE_HOST_VALIDATION=true are mutually exclusive"
                    .into(),
            );
        }

        Ok(Self {
            bind,
            token,
            allowed_hosts,
            allowed_origins,
            disable_host_validation,
        })
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}{}", self.bind, MCP_HTTP_PATH)
    }
}

#[derive(Clone)]
struct AuthState {
    token: Arc<String>,
}

pub fn streamable_http_router(server: ReadingMcpServer, config: &HttpTransportConfig) -> Router {
    let mut transport_config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_stateful_mode(true)
        .with_allowed_origins(config.allowed_origins.clone());
    if config.disable_host_validation {
        transport_config = transport_config.disable_allowed_hosts();
    } else if let Some(hosts) = &config.allowed_hosts {
        transport_config = transport_config.with_allowed_hosts(hosts.clone());
    }

    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        transport_config,
    );
    let auth = AuthState {
        token: Arc::new(config.token.clone()),
    };

    Router::new()
        .route(HEALTH_PATH, get(healthz))
        .route(READY_PATH, get(readyz))
        .route(LEGACY_HEALTH_PATH, get(healthz))
        .nest_service(MCP_HTTP_PATH, service)
        .layer(middleware::from_fn_with_state(auth, authorize))
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
        json!({
            "event": "mcp_http_listen",
            "endpoint": format!("http://{bound}{MCP_HTTP_PATH}"),
            "health": format!("http://{bound}{HEALTH_PATH}"),
            "ready": format!("http://{bound}{READY_PATH}"),
            "host_validation_disabled": config.disable_host_validation,
            "remote_access": "use a trusted tunnel or reverse proxy; direct non-loopback bind is disabled"
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

async fn authorize(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let valid = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| candidate == auth.token.as_str());

    if valid {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn validate_bind(bind: SocketAddr) -> Result<(), String> {
    if bind.ip().is_loopback() {
        Ok(())
    } else {
        Err(
            "READING_MCP_HTTP_BIND must use a loopback address; expose Reading MCP through a trusted tunnel or reverse proxy"
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

fn env_bool(name: &str, default: bool) -> Result<bool, String> {
    let Ok(value) = std::env::var(name) else {
        return Ok(default);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{name} must be true/false")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_rejects_non_loopback_bind() {
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8787);
        assert!(validate_bind(public).is_err());
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8787);
        assert!(validate_bind(loopback).is_ok());
    }

    #[test]
    fn origin_defaults_follow_the_bound_port() {
        assert_eq!(
            default_loopback_origins(8787),
            vec![
                "http://localhost:8787",
                "http://127.0.0.1:8787",
                "http://[::1]:8787",
            ]
        );
    }
}
