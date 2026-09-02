use reading_mcp::mcp::ReadingMcpServer;
use rmcp::{
    RoleServer, ServiceExt,
    model::{ClientJsonRpcMessage, ErrorCode, ErrorData, ServerJsonRpcMessage},
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{Transport, async_rw::AsyncRwTransport, io::stdio},
};
use tokio::io::{Stdin, Stdout};

/// `tunnel-client` may probe an MCP server with `server/discover` before the
/// regular MCP `initialize` request. rmcp 2.2.0 rejects every pre-initialize
/// request except `ping`, so answer the probe with JSON-RPC method-not-found
/// and let the client fall back to the standard initialize handshake. Reject
/// other pre-initialize requests without forwarding them so a stale tunnel
/// binding cannot terminate the stdio process.
struct DiscoveryCompatibleStdio {
    inner: AsyncRwTransport<RoleServer, Stdin, Stdout>,
    initialized: bool,
}

impl DiscoveryCompatibleStdio {
    fn new() -> Self {
        let (stdin, stdout) = stdio();
        Self {
            inner: AsyncRwTransport::new_server(stdin, stdout),
            initialized: false,
        }
    }
}

impl Transport<RoleServer> for DiscoveryCompatibleStdio {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let message = self.inner.receive().await?;

            match &message {
                ClientJsonRpcMessage::Request(request)
                    if request.request.method() == "server/discover" =>
                {
                    let response = ServerJsonRpcMessage::error(
                        ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "Method not found", None),
                        Some(request.id.clone()),
                    );
                    self.inner.send(response).await.ok()?;
                    continue;
                }
                ClientJsonRpcMessage::Request(request)
                    if !self.initialized && request.request.method() == "initialize" =>
                {
                    self.initialized = true;
                }
                ClientJsonRpcMessage::Request(request)
                    if !self.initialized && request.request.method() != "ping" =>
                {
                    let response = ServerJsonRpcMessage::error(
                        ErrorData::new(ErrorCode::INVALID_REQUEST, "Server not initialized", None),
                        Some(request.id.clone()),
                    );
                    self.inner.send(response).await.ok()?;
                    continue;
                }
                ClientJsonRpcMessage::Notification(_)
                | ClientJsonRpcMessage::Response(_)
                | ClientJsonRpcMessage::Error(_)
                    if !self.initialized =>
                {
                    continue;
                }
                _ => {}
            }

            return Some(message);
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if reading_mcp::parsing::run_file_source_view_worker_if_requested()? {
        return Ok(());
    }
    if reading_mcp::parsing::run_source_view_worker_if_requested()? {
        return Ok(());
    }

    let server = ReadingMcpServer::from_env().map_err(std::io::Error::other)?;
    let service = server.serve(DiscoveryCompatibleStdio::new()).await?;
    service.waiting().await?;
    Ok(())
}
