# Runtime Configuration

`reading-mcp`（stdio）和 `reading-mcp-http`（Streamable HTTP）都从环境变量构建同一个文档 `RuntimeConfig`。配置只属于 composition/runtime/transport 层，Domain/Application/Parser 不读取环境变量。

## 默认行为

默认：

```text
Document HTTP   = HTTPS only
Local file      = disabled
State           = ~/.reading-mcp
Repository      = SQLite
Search          = SQLite FTS5
Raw cache       = persistent file cache
Parsed cache    = persistent file cache
Telemetry       = stderr JSON enabled
MCP HTTP bind   = 127.0.0.1:8000（reading-mcp-http）
MCP HTTP path   = /mcp
MCP Origin      = loopback origins for the configured bind port
```

Windows 上默认 state root 使用 `%USERPROFILE%\.reading-mcp`。

如果不希望保留状态：

```bash
READING_MCP_STATE_DIR=memory reading-mcp
```

## 两类 HTTP 配置不要混淆

```text
READING_MCP_SERVER_* = MCP inbound transport
READING_MCP_HTTP_*   = document outbound retrieval
```

前者决定 MCP Client 如何连接 Reading MCP；后者决定 Reading MCP 如何下载需要阅读的在线文档。这两个关注点独立。

## MCP Streamable HTTP Transport

`reading-mcp-http` 默认监听：

```text
http://127.0.0.1:8000/mcp
```

当前 transport SDK：

```text
rmcp 3.1.2
```

### Bind

```bash
READING_MCP_SERVER_BIND=127.0.0.1:8000 reading-mcp-http
```

Phase 7 只接受 loopback IP，例如：

```text
127.0.0.1:8000
[::1]:8000
```

非 loopback bind 会 fail-fast。远程客户端应通过 Secure MCP Tunnel 或受信 reverse proxy 访问，而不是直接把服务暴露到公网。

### Host allowlist

rmcp Streamable HTTP 默认执行 Host 校验以降低 DNS rebinding 风险。某些受信 tunnel / proxy 如果需要额外 Host，可显式配置：

```bash
READING_MCP_SERVER_ALLOWED_HOSTS=localhost,127.0.0.1,my-tunnel.example.com
```

### Origin allowlist

Reading MCP 不使用 rmcp 的“空 Origin allowlist = 不校验 Origin”默认行为，而是在 transport composition 层显式启用 Origin 校验。

默认会根据 `READING_MCP_SERVER_BIND` 的端口生成：

```text
http://localhost:<port>
http://127.0.0.1:<port>
http://[::1]:<port>
```

默认端口 `8000` 即：

```text
http://localhost:8000
http://127.0.0.1:8000
http://[::1]:8000
```

如果受信 tunnel / reverse proxy 会发送其他 `Origin`，显式替换 allowlist：

```bash
READING_MCP_SERVER_ALLOWED_ORIGINS=https://example.com,https://app.example.com
```

不要通过清空 Origin 校验来规避接入问题；应把实际可信 Origin 加入 allowlist。

详细设计和 ChatGPT 验证路径见 [`phase7-streamable-http.md`](phase7-streamable-http.md)。

## 本地文件授权

默认不允许本地文件。显式设置允许目录：

```bash
READING_MCP_LOCAL_ROOTS=/home/me/books:/home/me/docs reading-mcp
```

该变量使用操作系统 path-list 语义，因此 Windows 使用 `;`，Unix 使用 `:`。

策略会 canonicalize 请求路径与 root，然后要求目标必须位于某个授权 root 下。它不会把 MCP 客户端提供的路径直接当作授权依据。

## State

```text
READING_MCP_STATE_DIR
```

默认：`~/.reading-mcp`

结构：

```text
~/.reading-mcp/
├── reading-mcp.sqlite
└── cache/
    ├── raw/
    └── parsed/
```

SQLite 中：

```text
documents    = DocumentRepository
search_units = FTS5 SearchIndex
```

两者可以共享数据库文件，但仍通过两个独立 Port 使用。

## Document HTTP Retrieval

默认只允许公共 HTTPS。

显式允许普通 HTTP：

```bash
READING_MCP_ALLOW_HTTP=true
```

这不会关闭 SSRF 防护。DNS/IP、redirect 每跳校验仍然执行。

Document HTTP 相关配置：

```text
READING_MCP_HTTP_MAX_REDIRECTS
READING_MCP_HTTP_MAX_CONCURRENCY
READING_MCP_HTTP_TIMEOUT_SECS
READING_MCP_HTTP_CONNECT_TIMEOUT_SECS
```

## Resource Budget

默认值：

| 变量 | 默认值 | 含义 |
|---|---:|---|
| `READING_MCP_MAX_DOCUMENT_BYTES` | 33554432 | Raw 文档最大 32 MiB |
| `READING_MCP_MAX_PDF_PAGES` | 2000 | PDF 最大页数 |
| `READING_MCP_MAX_ARCHIVE_ENTRIES` | 10000 | EPUB/DOCX ZIP 最大 entry 数 |
| `READING_MCP_MAX_ARCHIVE_ENTRY_BYTES` | 16777216 | 单 ZIP entry 最大解压大小 |
| `READING_MCP_MAX_ARCHIVE_TOTAL_BYTES` | 67108864 | ZIP 总解压读取上限 |
| `READING_MCP_MAX_SECTIONS` | 20000 | Normalized Document 最大 Section 数 |
| `READING_MCP_MAX_SECTION_DEPTH` | 32 | Section Tree 最大深度 |
| `READING_MCP_MAX_NORMALIZED_CHARS` | 16000000 | 规范化正文最大字符数 |
| `READING_MCP_PARSE_TIMEOUT_SECS` | 30 | Parser cooperative timeout |

说明：Parser timeout 是 Tokio cooperative timeout。对于长时间不 yield 的同步 CPU 操作，它不是 OS 级硬抢占。因此 PDF 页数、ZIP 解压大小等前置限制仍然是资源安全的主要证据。

## HTTP Cache Revalidation

如果缓存响应包含：

```text
ETag
Last-Modified
```

后续普通 `open_document` 会发送：

```text
If-None-Match
If-Modified-Since
```

`304 Not Modified` 时复用现有 raw/parsed cache。

```json
{
  "force_refresh": true
}
```

会跳过条件复用并重新获取来源。

## auth_profile

MCP 请求只传 profile 名：

```json
{
  "source": "https://docs.example.com/private/book.html",
  "auth_profile": "company-docs"
}
```

部署环境配置：

```bash
READING_MCP_AUTH_COMPANY_DOCS_HOSTS=docs.example.com,*.internal.example.com
READING_MCP_AUTH_COMPANY_DOCS_BEARER_TOKEN='...'
```

Profile 名允许：

```text
A-Z a-z 0-9 - _
```

环境变量 key 会 uppercase，并把 `-` 转为 `_`。

安全规则：

- profile 必须有 host allowlist；
- wildcard `*.example.com` 不匹配 apex `example.com`；
- redirect 每一跳重新检查 host；
- Token 不会因为 redirect 自动发送到新的未授权域；
- private raw cache key 包含 auth profile，避免不同身份共享认证响应；
- MCP Tool 不接受任意 Authorization/Cookie Header。

当前 Provider 支持 Bearer Token。OAuth、Cookie Session、交互登录应使用未来独立 Credential Provider / Source Adapter，而不是扩展 Tool 参数传 Secret。

## Telemetry

默认开启：

```text
READING_MCP_TELEMETRY=true
```

关闭：

```bash
READING_MCP_TELEMETRY=false reading-mcp
```

结构化 JSON 只写 stderr。stdio 模式下 stdout 专用于 MCP JSON-RPC。

事件包括：

```text
raw_cache_get / raw_cache_put
parsed_cache_get / parsed_cache_put
retrieve
parse
index
search
```

不会记录：

- 文档正文；
- Bearer Token；
- Authorization；
- Cookie；
- 完整搜索词（只记录 query 字符数）。

## Error Metadata

MCP error `data` 提供：

```json
{
  "code": "RESOURCE_LIMIT_EXCEEDED",
  "retryable": false
}
```

稳定 code：

```text
INVALID_REQUEST
BLOCKED_SOURCE
AUTHENTICATION_FAILED
RESOURCE_LIMIT_EXCEEDED
RETRIEVAL_FAILED
PARSE_FAILED
DOCUMENT_NOT_FOUND
SECTION_NOT_FOUND
REPOSITORY_FAILED
CACHE_FAILED
INDEX_FAILED
```

`retryable=true` 目前用于来源/存储/缓存/索引等可能的暂态失败；策略拒绝、解析失败和资源超限默认不可盲目重试。
