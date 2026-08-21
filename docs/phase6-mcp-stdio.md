# Phase 6：MCP stdio Server 与真实调用验证

## 目标

通过真正的 MCP stdio transport 暴露既有 Application UseCase，而不让协议 SDK 进入 Domain/Application/Parsing/Retrieval。

```text
MCP Client
  ↓ stdio / JSON-RPC
reading-mcp binary
  ↓
ReadingMcpServer
  ↓
Application UseCases
  ↓
Domain + Ports + Adapters
```

`rmcp` 只进入 MCP adapter/binary。

## 6 个 Tool

```text
list_documents
open_document
get_document_structure
search_document
get_context
read_document
```

`list_documents` 只枚举 `READING_MCP_LOCAL_ROOTS` 授权目录中的可读文档，不打开文档；支持递归扫描和结果数量上限。`open_document` 返回 document/source/media type、raw/normalized identity、normalization diagnostics 与 section count；structure 返回 section tree；search 返回 owning section + source/title/snippet/score/location；context/read 从 DocumentRepository 读取规范化正文。

v0.1 不增加 PDF/EPUB/DOCX 专属 Tool，格式位置统一放在 `Location`。

## Normalized document identity and text coordinates

`open_document` 以 additive 方式返回：

```text
content_hash
normalized_document_hash
normalized_document_hash_version = normalized-document-hash/v1
normalization_version             = reading-mcp-normalization/v1
normalized_text_coordinate_space = section-content-unicode-scalar/v1
```

其中：

```text
content_hash
= retrieved source bytes provenance

normalized_document_hash
= canonical addressing-relevant Document/Section fingerprint
```

`normalized_document_hash/v1` 按 persisted tree/source order 覆盖：

```text
Section id
parent id
Section title
Section level
exact Section.content
child count/order
```

不覆盖 raw source、legacy `Location`、native provenance、MCP rendering 或 SearchIndex row。未来影响 Paragraph/Sentence segmentation 的 canonical persisted block/boundary facts 必须通过新 hash contract version 纳入。

Normalized range 的唯一通用语义是：

```text
owner    = exact persisted Section.content
base     = zero
interval = half-open [start, end)
unit     = Unicode scalar / Rust char
```

`Location.char_start/char_end` 保留 parser-defined legacy/source meaning，不被静默改写为 normalized range。

Parsed Cache key 现在包含：

```text
final_source
raw_sha256
normalization_version
```

因此 normalization policy 升级可以继续复用 Raw Cache，但不会错误复用旧 Parsed Document。

完整契约见 [Normalized Document Identity and Text Range Contract](normalized-text-range-contract.md)。

## Section read continuation

`read_document` 保留现有请求：

```text
document_id
section_id
max_chars?
```

并以 additive 方式支持：

```text
cursor?
```

首次读取从 Section subtree 的确定性逻辑流开始：

```text
SectionTreeReadStream/v1
rendering_version = section-tree-markdown/v1
coordinate_space  = section-tree-rendered-unicode-scalar/v1
```

当响应预算不足时，返回：

```text
truncated = true
complete = false
next_cursor
stream.coordinate_space
stream.start_char
stream.end_char
stream.total_chars
```

继续调用时仍传入同一个 `document_id`、`section_id`，并附带 `next_cursor`。服务端验证 cursor 绑定：

```text
document_id
raw content_hash
normalized_document_hash
root section_id
read_mode = section_tree
rendering_version
next stream character position
cursor schema version
```

`stream.start_char/end_char` 是 Unicode scalar 计数的 rendered stream coordinates，仅用于 continuation 验证；它不是 canonical source locator、normalized range 或 citation。Cursor 遇到 document/normalized facts、target、mode 或 rendering version 不匹配时 fail closed，不做 fuzzy rebasing。

重复 continuation 必须满足：

```text
segment[n].end_char == segment[n+1].start_char
all segments concatenated == complete SectionTreeReadStream/v1
no gap
no overlap
terminal complete = true
terminal next_cursor = null
```

首次 `max_chars=0` 保留旧行为，返回空 incomplete segment 和 position 0 cursor；cursor continuation 的 `max_chars=0` 被拒绝，避免产生无法前进的 continuation stream。

## 当前默认 Runtime

```text
SourcePolicyRouter
├── LocalFileSourcePolicy(default deny, allowed roots)
└── PublicHttpAccessPolicy(HTTPS-only by default)

RetrieverRouter
├── LimitedFileRetriever
└── RevalidatingHttpRetriever(HttpRetriever + Raw Cache)

ParserRouter::release
├── Text
├── Markdown
├── HTML/XHTML
├── PDF
├── EPUB
├── DOCX
└── OpenAPI/Swagger JSON/YAML

BudgetedParser
CachingParser

Default persistent state
├── File Raw Cache
├── File Parsed Cache
├── SQLite DocumentRepository
└── SQLite FTS5 SearchIndex
```

设置 `READING_MCP_STATE_DIR=memory` 可使用纯内存运行时。完整配置见 `runtime-configuration.md`。

## 本地文件安全

本地文件默认关闭；只有 `READING_MCP_LOCAL_ROOTS` 显式配置的 canonical root 可读。请求路径同样 canonicalize 后必须位于授权 root 内，并受最大文件字节预算限制。

## 真实 stdio 验收

测试不是直接调用 UseCase，而是启动 `reading-mcp` 子进程，经 stdio 完成 MCP initialize、tools/list 和完整阅读流程。

测试覆盖：

- 6 Tool discovery/调用；
- structured DTO；
- raw/normalized source identity 与 location traceability；
- Text/Markdown/HTML/PDF acceptance matrix；
- 持久化 state 重启后继续使用旧 document_id；
- normalized hash 从 persisted canonical Document 确定性重建；
- Section-relative Unicode-scalar normalized range validator；
- normalization-version-scoped Parsed Cache；
- SectionTreeReadStream continuation 的 actionable cursor；
- 多段拼接无 gap/overlap，并精确等于一次完整读取；
- cursor 对 raw/normalized document identity、target 和 rendering contract 的 fail-closed 验证；
- stderr telemetry 不污染 stdout MCP transport。

## 架构约束

```text
mcp → application → domain
retrieval/security/parsing/infrastructure → application/domain ports
```

禁止：

```text
domain/application → rmcp
parser → MCP
retriever → MCP
search index → MCP DTO
```

`tests/architecture_boundaries.rs` 把关键依赖方向固化为自动化测试。

## 当前支持的远程访问

除 stdio 外，`reading-mcp-http` 提供带 Bearer Token 的 Streamable HTTP 单机入口，默认监听 `127.0.0.1:8787/mcp`，可通过 Cloudflare Tunnel、Tailscale Funnel 等 HTTPS 隧道供远程 MCP Client 使用。公网多租户、OAuth 和稳定生产网关仍不在 v0.1 范围内。

## v0.1 明确非目标

- 公网多租户 transport；
- browser rendering；
- OCR；
- OAuth/Cookie 交互登录；
- 企业产品 API；
- MCP Resources/Prompts；
- AI 总结/问答/笔记/通用向量 RAG。
