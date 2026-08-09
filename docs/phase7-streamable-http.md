# Phase 7：Streamable HTTP MCP 与 ChatGPT 验证路径

## 目标

Phase 7 只新增 MCP transport，不新增阅读业务能力。

```text
                       ┌─ stdio → local MCP clients
ReadingMcpServer ──────┤
                       └─ Streamable HTTP → tunnel / remote MCP clients
```

两种 transport 复用同一个：

- `ReadingMcpServer`；
- 5 个 MCP Tools；
- Application UseCases；
- RuntimeConfig；
- Repository / SearchIndex / Cache；
- Security Policy。

因此：

```text
MCP Transport ≠ Application Logic
```

## 为什么增加 Streamable HTTP

OpenAI 当前的 ChatGPT MCP 文档说明：ChatGPT 连接 remote MCP server，不能直接连接本地 MCP server；本机、内网或开发机上的 MCP 可以通过 Secure MCP Tunnel 接入。

参考：

- https://help.openai.com/en/articles/12584461-developer-mode-and-full-mcp-connectors-in-chatgpt

所以现有 stdio server 继续适用于 Inspector、Claude/Cursor 等本地 MCP 客户端；ChatGPT 验证路径增加 Streamable HTTP。

## Binary

### stdio

```bash
cargo run --locked --bin reading-mcp
```

### Streamable HTTP

```bash
cargo run --locked --bin reading-mcp-http
```

默认 endpoint：

```text
http://127.0.0.1:8000/mcp
```

启动日志只写 stderr。

## 协议 SDK

Phase 7 使用官方 Rust MCP SDK：

```text
rmcp = 3.1.2
```

3.x transport 支持 MCP `2026-07-28` 生命周期，同时保留旧协议兼容路径。Reading MCP 不在 Application/Domain 中直接依赖协议生命周期；协议变化被限制在 MCP adapter/transport 边界。

## 安全默认

Phase 7 故意不把 Reading MCP 变成公网服务。

`reading-mcp-http`：

- 默认只绑定 `127.0.0.1:8000`；
- `READING_MCP_SERVER_BIND` 只接受 loopback IP；
- 非 loopback bind 启动时直接失败；
- 使用 rmcp Streamable HTTP server 的 Host 校验防 DNS rebinding；
- **显式启用 Origin 校验**，不依赖 SDK 的空 Origin allowlist 默认；
- 默认只接受当前监听端口上的 `localhost` / `127.0.0.1` / `[::1]` Origin；
- 不在 MCP Tool 参数中加入 transport credential；
- 公网 TLS / 访问控制属于 tunnel / reverse proxy 的职责。

这种边界允许：

```text
ChatGPT
  ↓
Secure MCP Tunnel / trusted reverse proxy
  ↓
127.0.0.1:8000/mcp
  ↓
Reading MCP
```

而不是：

```text
reading-mcp-http --bind 0.0.0.0:8000
  ↓
裸奔公网
```

## Transport 配置

### 监听地址

```text
READING_MCP_SERVER_BIND=127.0.0.1:8000
```

只允许 loopback IP，例如：

```text
127.0.0.1:8000
[::1]:8000
```

### Host allowlist

rmcp 默认只接受 loopback Host。某些受信 tunnel / reverse proxy 如果保留外部 Host，可显式配置：

```text
READING_MCP_SERVER_ALLOWED_HOSTS=localhost,127.0.0.1,my-tunnel.example.com
```

不要使用通配的“允许所有 Host”来绕过 DNS rebinding 防护。

### Origin allowlist

Reading MCP 默认显式允许与监听端口匹配的 loopback Origin。例如默认端口 `8000`：

```text
http://localhost:8000
http://127.0.0.1:8000
http://[::1]:8000
```

如果受信 tunnel / reverse proxy 会发送其他 `Origin`，必须显式配置：

```text
READING_MCP_SERVER_ALLOWED_ORIGINS=https://example.com,https://app.example.com
```

配置该变量会替换默认 loopback Origin 列表。不要为了“先跑起来”而关闭 Origin 校验。

## 与文档来源 HTTP 配置的区别

不要混淆两套 HTTP：

```text
READING_MCP_SERVER_*       = MCP inbound transport
READING_MCP_HTTP_*         = document outbound retrieval
```

前者决定 MCP 客户端如何连接 Reading MCP；后者决定 Reading MCP 如何下载用户要求阅读的在线文档。

这是两个独立关注点。

## ChatGPT 验证步骤

1. 构建：

```bash
cargo build --release --locked --bin reading-mcp-http
```

2. 启动：

```bash
READING_MCP_LOCAL_ROOTS=/absolute/path/to/books \
  ./target/release/reading-mcp-http
```

3. 本地先用 MCP Streamable HTTP client / Inspector 验证：

```text
http://127.0.0.1:8000/mcp
```

4. 通过 OpenAI Secure MCP Tunnel 或受信 tunnel 把本地 MCP endpoint 暴露给 ChatGPT。

5. 如果 tunnel/proxy 向 MCP endpoint 转发非 loopback `Origin`，先把该 Origin 加入 `READING_MCP_SERVER_ALLOWED_ORIGINS`，而不是关闭校验。

6. 在 ChatGPT Developer Mode / custom MCP app 中配置 tunnel 给出的 remote MCP URL。

7. 使用一份测试 Markdown/PDF 验证：

```text
open_document
→ get_document_structure
→ search_document
→ get_context
→ read_document
```

## E2E 证明

`tests/phase7_streamable_http.rs` 使用官方 rmcp Streamable HTTP client 真实连接 Axum HTTP server，并验证：

- tools/list 精确返回同一组 5 Tools；
- `open_document` 可通过 HTTP 打开授权本地文档；
- `search_document` 能找到逻辑 Section；
- `read_document` 返回规范化正文；
- hostile/non-allowlisted `Origin` 请求被拒绝；
- HTTP transport 不要求修改任何 Application UseCase。

## 非目标

Phase 7 不实现：

- 公网裸 HTTP；
- 多租户身份模型；
- OAuth server；
- 自建 TLS 证书管理；
- 用户/租户级 ACL；
- Browser automation；
- 新文档格式。

这些需要独立 threat model，不能因为“加了 HTTP transport”就默认进入 Reading MCP 内核。
