# Reading MCP v0.1 需求

## 目标

Reading MCP 是面向 AI/MCP Client 的统一文档阅读上下文基础设施。它负责获取、解析、结构化、定位、搜索、按章节读取、上下文展开、缓存与持久化；总结、问答、推理、教学、笔记和通用 RAG 继续由上层 AI 完成。

核心流程：

```text
open_document
→ get_document_structure
→ search_document
→ get_context
→ read_document
```

## 来源

v0.1 支持：

- 公共 HTTPS；
- 显式允许根目录内的本地文件。

安全默认：HTTPS-only；本地文件 default-deny。`READING_MCP_ALLOW_HTTP=true` 才允许明文 HTTP。

## 格式

独立 Parser：

- Plain Text；
- Markdown；
- HTML/XHTML；
- 原生文本 PDF；
- EPUB；
- DOCX；
- OpenAPI / Swagger JSON/YAML。

GitHub README/Wiki、Javadoc、MkDocs/Docusaurus/GitBook 静态输出直接复用 Markdown/HTML，不创建品牌专属 Parser。

## MCP Tools

只暴露 5 个稳定 Tool：

```text
open_document
get_document_structure
search_document
get_context
read_document
```

格式扩展不得增加专属 Tool。v0.1 读取单元为 `Section`；page/chapter/anchor/paragraph/char/native locator 统一通过 `Location` 表达。

## 文档模型与搜索

所有格式统一为 `Document / Section / Location`。

```text
DocumentRepository = 规范化事实来源
SearchIndex         = 可重建派生状态
Search Unit         ≠ Read Unit
```

搜索结果必须映射回 owning section，并返回 source/title/snippet/score/location。

## 可追溯性

Tool 结果必须尽可能保留：

```text
document_id
source
content_hash
section_id / parent_id / title
page / chapter / section_path
paragraph / anchor / char range
native_location
```

## 安全与资源

必须具备：

- SSRF scheme/hostname/DNS/IP 检查；
- 每次 redirect 重新校验并重新 DNS resolve；
- 请求 endpoint pinning，禁用环境/system proxy；
- URL 内嵌 credential 拒绝；
- HTTP timeout/redirect/concurrency/body limit；
- Content-Type allowlist；
- local root canonical allowlist + 文件大小限制；
- PDF 总页数与单页解压限制；
- EPUB/DOCX ZIP entry/单 entry/总解压预算；
- Parser timeout；
- Normalized Document 字符数、section 数和树深度限制。

## 缓存与持久化

缓存分层保持独立：

```text
RawResourceCache
ParsedDocumentCache
DocumentRepository
SearchIndex
```

HTTP 保存 ETag/Last-Modified，并使用 `If-None-Match` / `If-Modified-Since` 条件重验证；304 复用缓存；`force_refresh=true` 重新获取来源。

默认状态目录为 `~/.reading-mcp`，使用持久化 Raw/Parsed Cache、SQLite DocumentRepository 和 SQLite FTS5 SearchIndex。设置 `READING_MCP_STATE_DIR=memory` 可切换纯内存模式。

## auth_profile

模型只传 profile 名，不传 Secret。部署侧通过环境变量提供 Bearer Token 与 host allowlist；每次 redirect 都重新执行 profile→host 校验，认证 Raw Cache 按 profile 隔离。

## 错误与可观察性

MCP 错误必须提供稳定 `code + retryable`。Telemetry 只写 stderr，不得记录文档正文、Bearer Token、Authorization/Cookie 或完整搜索词。

## 非目标

v0.1 不包括：

- OCR / 扫描 PDF；
- JavaScript-heavy 浏览器渲染；
- Confluence/Notion/飞书/语雀等产品 API；
- OAuth/Cookie 交互登录；
- 公网多租户服务；
- 通用 Web crawler；
- AI 总结/问答/笔记；
- 通用向量 RAG。

## Release Gate

必须通过：

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

测试范围包括架构边界、真实 stdio MCP E2E、持久化重启、HTTP 条件重验证、auth redirect isolation、资源预算、SQLite FTS、Text/Markdown/HTML/PDF acceptance，以及 EPUB/DOCX/OpenAPI 解析。
