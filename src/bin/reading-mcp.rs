use reading_mcp::mcp::ReadingMcpServer;
use rmcp::{ServiceExt, transport::stdio};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = ReadingMcpServer::from_env().map_err(std::io::Error::other)?;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
