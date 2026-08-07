use reading_mcp::mcp::ReadingMcpServer;
use rmcp::{ServiceExt, transport::stdio};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = ReadingMcpServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
