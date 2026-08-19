# Runtime Configuration

`reading-mcp` 的 stdio binary 从环境变量构建 `RuntimeConfig`。配置只属于 composition/runtime 层，Domain/Application/Parser 不读取环境变量。

## 默认行为

默认：

```text
HTTP            = HTTPS only
Local file      = disabled
State           = ~/.reading-mcp
Repository      = SQLite
Search          = SQLite FTS5
Raw cache       = persistent file cache
Parsed cache    = persistent file cache
Telemetry       = stderr JSON enabled
```

Windows 上默认 state root 使用 `%USERPROFILE%\.reading-mcp`。

如果不希望保留状态：

```bash
READING_MCP_STATE_DIR=memory reading-mcp
```

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

## HTTP

默认只允许公共 HTTPS。

显式允许普通 HTTP：

```bash
READING_MCP_ALLOW_HTTP=true
```

这不会关闭 SSRF 防护。DNS/IP、redirect 每跳校验仍然执行。

HTTP 相关配置：

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

## Streamable HTTP

HTTP binary：

```text
target/release/reading-mcp-http
```

配置：

```text
READING_MCP_HTTP_BIND              默认 127.0.0.1:8787
READING_MCP_HTTP_TOKEN             必填，至少 32 个字符
READING_MCP_HTTP_ALLOWED_HOSTS     可选，逗号分隔的 Host allowlist
```

HTTP MCP endpoint 为 `/mcp`，所有请求都必须使用 `Authorization: Bearer <token>`。未设置 `READING_MCP_HTTP_ALLOWED_HOSTS` 时会关闭 rmcp 的 Host 校验，以兼容临时隧道随机域名；公网部署仍必须依赖强随机 Token，并建议配置稳定域名的 Host allowlist。

## Telemetry

默认开启：

```text
READING_MCP_TELEMETRY=true
```

关闭：

```bash
READING_MCP_TELEMETRY=false
```

结构化 JSON 只写 stderr。stdout 专用于 MCP JSON-RPC。

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
