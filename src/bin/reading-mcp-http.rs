use reading_mcp::mcp::{HttpTransportConfig, ReadingMcpServer, serve_streamable_http};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = ReadingMcpServer::from_env()?;
    let transport = HttpTransportConfig::from_env()?;
    serve_streamable_http(server, transport).await?;
    Ok(())
}
