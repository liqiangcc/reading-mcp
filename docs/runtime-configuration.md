# Runtime Configuration

`reading-mcp` 的 stdio binary 从环境变量构建 `RuntimeConfig`。配置只属于 composition/runtime 层，Domain/Application/Parser 不读取环境变量。

## 默认行为

默认：

```text
HTTP            = HTTPS only
Local file      = disabled
State           = ~/.reading-mcp
Repository      = SQLite
Search          = SQLite FTS5 + bounded adaptive fallback
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

## HTTP 来源获取

默认只允许公共 HTTPS。

显式允许普通 HTTP：

```bash
READING_MCP_ALLOW_HTTP=true
```

这不会关闭 SSRF 防护。DNS/IP、redirect 每跳校验仍然执行。

HTTP 来源获取相关配置：

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

同步 PDF/ZIP/XML 等格式解析会放到 Tokio blocking pool，避免占住 async runtime worker。Parser timeout 仍不是 OS 级硬抢占：已经开始运行的 blocking task 不能被 Tokio 强制杀死，因此 PDF 页数、ZIP 解压大小、Raw/Normalized 大小等前置预算仍是资源安全的主要证据。

### MCP Response Budget

输出大小还有独立的服务端硬边界，不由客户端完全决定：

```text
read_document
  default max_chars = 40,000
  hard max_chars    = 80,000

get_context
  default max_chars = 24,000
  hard max_chars    = 48,000

get_document_structure
  max visible nodes = 2,000
```

`read_document` / `get_context` 即使省略 `max_chars` 也会按默认值截断；客户端请求超过 hard limit 时服务端会 clamp。`max_chars=0` 会返回参数错误。

结构树超过 2,000 个可见节点时不会生成超大 MCP payload，而是返回 `RESOURCE_LIMIT_EXCEEDED`；此时应降低 `max_depth`，或先用 `search_document` 定位章节。

## Search Strategy

SQLite FTS5 / BM25 仍然是主索引，不引入向量数据库。

搜索流程：

```text
FTS5 / BM25
    ↓
中心化命中 snippet
    ↓
CJK 查询或主索引召回不足？
    ├─ no  → 返回索引结果
    └─ yes → 在已受 ResourceBudget 约束的 canonical Document 上做段落 fallback
```

fallback 对中文、日文、韩文连续文字生成重叠二元字符项，用于改善默认 Unicode tokenizer 对 CJK 自然语言问题的召回；英文仍优先使用 FTS5。fallback 只扫描已经打开并规范化的单个文档，不是 Web crawler，也不是通用 RAG。

Search snippet 会围绕实际命中位置截取，而不是固定返回段落开头。

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

## Streamable HTTP MCP Transport

HTTP binary：

```text
target/release/reading-mcp-http
```

配置：

```text
READING_MCP_HTTP_BIND                       默认 127.0.0.1:8787；只接受 loopback
READING_MCP_HTTP_TOKEN                      必填，至少 32 个字符
READING_MCP_HTTP_ALLOWED_HOSTS              可选，逗号分隔的 Host allowlist
READING_MCP_HTTP_ALLOWED_ORIGINS            可选，逗号分隔；默认当前端口的 localhost/127.0.0.1/[::1]
READING_MCP_HTTP_DISABLE_HOST_VALIDATION    默认 false；仅临时随机域名隧道显式使用
```

HTTP MCP endpoint 为 `/mcp`，健康端点为 `/healthz`，就绪端点为 `/readyz`；兼容保留 `/health`。所有端点都要求：

```text
Authorization: Bearer <READING_MCP_HTTP_TOKEN>
```

安全默认：

- HTTP server 只能绑定 loopback，不能直接监听 `0.0.0.0` 或 LAN 地址；
- RMCP Host validation 默认开启；
- Origin validation 默认开启；
- Bearer Token 始终强制；
- 远程访问应通过可信 tunnel/reverse proxy。

如果使用稳定命名隧道，优先显式配置其 Host/Origin：

```bash
export READING_MCP_HTTP_ALLOWED_HOSTS='reading.example.com'
export READING_MCP_HTTP_ALLOWED_ORIGINS='https://reading.example.com'
```

只有临时随机域名隧道无法预先固定 Host 时，才显式关闭 Host validation：

```bash
export READING_MCP_HTTP_DISABLE_HOST_VALIDATION=true
```

此开关不会关闭 Bearer Token，也不会自动关闭 Origin validation。若客户端发送 `Origin`，仍应把可信 Origin 加入 `READING_MCP_HTTP_ALLOWED_ORIGINS`。`READING_MCP_HTTP_ALLOWED_HOSTS` 与 `READING_MCP_HTTP_DISABLE_HOST_VALIDATION=true` 互斥。

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
