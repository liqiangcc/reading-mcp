# Reading MCP

> 面向 AI 的统一文档与书籍阅读上下文基础设施。

Reading MCP 让 MCP Client / Agent 能够**精确地与用户阅读同一份文档**：先打开来源、查看结构、搜索位置，再按逻辑章节读取并展开上下文，同时保留来源和格式特有定位。

它只提供可靠的文档上下文，不在内核中实现 AI 总结、问答、教学、笔记或通用 RAG。

## v0.1.0 候选能力

统一 MCP Tools：

```text
open_document
get_document_structure
search_document
get_context
read_document
```

独立格式 Parser：

```text
Plain Text
Markdown
HTML
PDF
EPUB
DOCX
OpenAPI / Swagger JSON/YAML
```

可直接复用已有格式能力：

```text
GitHub README / Wiki            → Markdown / HTML
Javadoc                         → HTML
MkDocs / Docusaurus / GitBook   → HTML
```

不会因为站点品牌不同而创建重复 Parser；只有新的文档格式才产生新的解析职责。

## 核心阅读流程

```text
File / Public HTTPS
        ↓
 Source Policy
        ↓
    Retriever
        ↓
    Raw Cache
        ↓
   Parser Router
        ↓
Normalized Document
   ┌────┴────┐
Repository  SearchIndex
   └────┬────┘
        ↓
 Application UseCases
        ↓
 ReadingMcpServer
   ┌────┴──────────────┐
 stdio        Streamable HTTP
   ↓                  ↓
local client     tunnel / remote client
```

Agent 的推荐调用顺序：

```text
open_document
      ↓
get_document_structure
      ↓
search_document
      ↓
get_context
      ↓
read_document
```

搜索单元可以是较小段落，但读取单元保持逻辑章节：

```text
Search Unit ≠ Read Unit
Index ≠ Document
Search ≠ Read
```

## MCP Transport

Reading MCP 现在提供两个 binary，但共享完全相同的 5 个 Tool、Application UseCase、Repository、SearchIndex 和安全策略。

### stdio

适合 MCP Inspector、Claude/Cursor 等可以启动本地 MCP 进程的客户端：

```bash
cargo build --release --locked --bin reading-mcp
./target/release/reading-mcp
```

概念配置：

```json
{
  "mcpServers": {
    "reading": {
      "command": "/absolute/path/to/reading-mcp"
    }
  }
}
```

### Streamable HTTP

适合需要 remote MCP URL 的客户端：

```bash
cargo build --release --locked --bin reading-mcp-http
./target/release/reading-mcp-http
```

默认 endpoint：

```text
http://127.0.0.1:8000/mcp
```

Phase 7 故意只允许 loopback bind，并显式校验 Origin；默认只接受当前监听端口对应的 `localhost` / `127.0.0.1` / `[::1]` Origin。远程访问应通过受信 MCP tunnel / reverse proxy，而不是把 Reading MCP 裸露到公网。

OpenAI 当前说明 ChatGPT 不能直接连接本地 MCP server；开发机、私网或 on-prem MCP 应通过 Secure MCP Tunnel 连接。因此 ChatGPT 的推荐验证路径是：

```text
ChatGPT
   ↓
Secure MCP Tunnel
   ↓
http://127.0.0.1:8000/mcp
   ↓
Reading MCP
```

详细步骤见 [`docs/phase7-streamable-http.md`](docs/phase7-streamable-http.md)。

## 安全默认

公共网络文档来源默认只允许 HTTPS，并启用：

- SSRF scheme / hostname / DNS / IP 校验；
- 每次 redirect 重新校验；
- 禁止 proxy 破坏已验证 endpoint 的安全证据链；
- HTTP timeout / redirect / concurrency / body size 限制；
- Content-Type allowlist；
- PDF 总页数与单页解压上限；
- EPUB/DOCX ZIP entry 数、单 entry 和总解压大小限制；
- Parser timeout；
- Normalized Document 字符数、Section 数和深度限制。

本地文件默认关闭。只有部署者配置允许目录后才能读取：

```bash
READING_MCP_LOCAL_ROOTS=/home/me/books:/home/me/docs reading-mcp
```

请求路径和授权目录都会 canonicalize，目标必须位于显式 root 内。

## 持久化状态

默认状态目录：

```text
~/.reading-mcp
```

使用：

```text
File Raw Cache
File Parsed Cache
SQLite DocumentRepository
SQLite FTS5 SearchIndex
```

MCP 进程重启后，已经打开过的 `document_id` 仍可直接用于 `read_document` / `search_document`。

如果需要完全临时运行：

```bash
READING_MCP_STATE_DIR=memory reading-mcp
```

虽然 Repository 和 SearchIndex 可以共享同一个 SQLite 文件，它们仍然是两个独立 Port：Document 是事实来源，Index 是可重建派生状态。

## HTTP 缓存新鲜度

Raw Cache 保存 `ETag` / `Last-Modified`。后续普通打开会进行条件重验证：

```text
If-None-Match
If-Modified-Since
      ↓
304 Not Modified
      ↓
复用 Raw + Parsed Cache
```

需要强制重新获取时：

```json
{
  "force_refresh": true
}
```

## auth_profile

模型只传 profile 名，不传明文 Secret：

```json
{
  "source": "https://docs.example.com/private/book.html",
  "auth_profile": "company-docs"
}
```

部署者配置：

```bash
READING_MCP_AUTH_COMPANY_DOCS_HOSTS=docs.example.com,*.internal.example.com
READING_MCP_AUTH_COMPANY_DOCS_BEARER_TOKEN='...'
```

每一次 redirect 都重新执行 profile → host 校验，因此 Bearer Token 不会自动泄漏到未授权 host。认证 Raw Cache 也按 auth profile 隔离。

## 可追溯性

MCP 返回尽可能保留：

```text
source
content_hash
section_id / parent_id / title
page
chapter
section_path
paragraph
anchor
char range
native_location
```

例如 PDF 通过 `page` / `pdf:page:N` 定位；HTML 通过 anchor；EPUB 通过 spine + archive entry/anchor；DOCX 通过 paragraph 位置；OpenAPI 通过 JSON-pointer-like location。

MVP 的读取单元是 `Section`。不会增加 `read_pdf_page_range` 等格式专属 Tool；如真实使用证明需要范围读取，应扩展统一的 `read_document` 契约。

## 结构优先

Reading MCP 不把整篇文档默认切成固定字符块。

```text
Section
  ↓
Paragraph / Search Unit
  ↓
必要时长度限制
```

这让 Agent 能先精确定位，再读取完整逻辑上下文，而不是把孤立 chunk 当作章节。

## 可观察性

默认向 **stderr** 输出结构化 JSON，stdout 专用于 MCP JSON-RPC：

- Raw / Parsed cache hit/miss；
- retrieve duration / bytes / media type；
- parse duration / section count / PDF page count；
- index/search duration / hit count。

不会记录文档正文、Bearer Token、Authorization/Cookie 或完整搜索词。

关闭：

```bash
READING_MCP_TELEMETRY=false reading-mcp
```

## 错误语义

MCP error data 包含稳定：

```json
{
  "code": "RESOURCE_LIMIT_EXCEEDED",
  "retryable": false
}
```

可区分参数错误、安全策略拒绝、认证失败、资源限制、来源故障、解析失败以及 Repository/Cache/Index 内部故障。

## 架构边界

```text
来源获取
≠ 安全策略
≠ 格式解析
≠ Normalized Document
≠ Repository
≠ Cache
≠ SearchIndex
≠ MCP Transport
≠ AI 理解与推理
```

依赖方向：

```text
mcp ───────→ application ───────→ domain
                   ↑
                   │ ports
retrieval ─────────┤
security ──────────┤
parsing ───────────┤
infrastructure ────┘
```

核心判断标准：

> 按变化原因划分职责，按数据流组合能力。

## 运行

```bash
cargo build --release --locked --bin reading-mcp
./target/release/reading-mcp
```

```bash
cargo build --release --locked --bin reading-mcp-http
./target/release/reading-mcp-http
```

详细运行配置见 [`docs/runtime-configuration.md`](docs/runtime-configuration.md)。完整 hardening 状态见 [`docs/release-hardening-plan.md`](docs/release-hardening-plan.md)。

## 明确非目标

v0.1.0 不包含：

- OCR / 扫描 PDF；
- JavaScript-heavy 页面浏览器渲染；
- Confluence / Notion / 飞书 / 语雀等产品 API；
- OAuth / Cookie 交互登录；
- 公网多租户服务；
- 通用 Web crawler；
- AI 总结 / 问答 / 笔记；
- 向量数据库 / 通用 RAG。

这些需要新的安全模型、来源 Adapter 或产品层职责，不应该继续塞进 Reading MCP 内核。

## 项目原则

```text
结构优先 > 固定切块
按需读取 > 整篇注入
可追溯 > 无来源回答
职责分离 > MCP 内置智能
统一抽象 > 格式特化接口
安全默认 > 任意来源可访问
全文/BM25 优先 > 过早引入向量数据库
真实证据链 > 功能数量
```
