use reading_mcp::mcp::{HttpTransportConfig, ReadingMcpServer, serve_streamable_http};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = ReadingMcpServer::from_env().map_err(std::io::Error::other)?;
    let transport = HttpTransportConfig::from_env().map_err(std::io::Error::other)?;
    serve_streamable_http(server, transport).await?;
    Ok(())
}
