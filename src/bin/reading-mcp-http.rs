use std::{env, net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use reading_mcp::mcp::ReadingMcpServer;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};

const DEFAULT_BIND: &str = "127.0.0.1:8787";

#[derive(Clone)]
struct AuthState {
    token: Arc<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = env::var("READING_MCP_HTTP_TOKEN")
        .map_err(|_| "READING_MCP_HTTP_TOKEN must be set for the HTTP server")?;
    if token.len() < 32 {
        return Err("READING_MCP_HTTP_TOKEN must contain at least 32 characters".into());
    }

    let bind = env::var("READING_MCP_HTTP_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let address: SocketAddr = bind.parse()?;

    let mut http_config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_stateful_mode(true);
    if let Ok(hosts) = env::var("READING_MCP_HTTP_ALLOWED_HOSTS") {
        let hosts = hosts
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if !hosts.is_empty() {
            http_config = http_config.with_allowed_hosts(hosts);
        }
    } else {
        // A tunnel's public hostname is generated dynamically. The bearer token
        // middleware below remains mandatory when host validation is disabled.
        http_config = http_config.disable_allowed_hosts();
    }

    let service = StreamableHttpService::new(
        || ReadingMcpServer::from_env().map_err(std::io::Error::other),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );
    let auth = AuthState {
        token: Arc::new(token),
    };

    let app = Router::new()
        .route("/health", get(health))
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(auth, authorize));

    let listener = tokio::net::TcpListener::bind(address).await?;
    eprintln!("reading-mcp HTTP listening on http://{bind}/mcp");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> &'static str {
    "ok"
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
