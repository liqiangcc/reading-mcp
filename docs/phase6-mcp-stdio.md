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

## 5 个 Tool

```text
open_document
get_document_structure
search_document
get_context
read_document
```

`open_document` 返回 document_id/source/title/media_type/content_hash/section_count；structure 返回 section tree；search 返回 owning section + source/title/snippet/score/location；context/read 从 DocumentRepository 读取规范化正文。

v0.1 不增加 PDF/EPUB/DOCX 专属 Tool，格式位置统一放在 `Location`。

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

- 5 Tool discovery/调用；
- structured DTO；
- source/location traceability；
- Text/Markdown/HTML/PDF acceptance matrix；
- 持久化 state 重启后继续使用旧 document_id；
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

## v0.1 明确非目标

- Streamable HTTP/public multi-user transport；
- browser rendering；
- OCR；
- OAuth/Cookie 交互登录；
- 企业产品 API；
- MCP Resources/Prompts；
- AI 总结/问答/笔记/通用向量 RAG。
