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
 MCP stdio / HTTP
        ↓
       AI
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

SQLite FTS5 / BM25 是主检索路径。CJK 查询或主索引召回不足时，会在已经受资源预算约束的 canonical Document 上进行受控段落 fallback；不会因此引入向量数据库或把 Index 变成事实来源。Search snippet 会围绕真实命中位置截取。

## 安全默认

公共网络来源默认只允许 HTTPS，并启用：

- SSRF scheme / hostname / DNS / IP 校验；
- 每次 redirect 重新校验；
- 禁止 proxy 破坏已验证 endpoint 的安全证据链；
- HTTP timeout / redirect / concurrency / body size 限制；
- Content-Type allowlist；
- PDF 总页数与单页解压上限；
- EPUB/DOCX ZIP entry 数、单 entry 和总解压大小限制；
- 同步格式解析进入 blocking pool，避免占住 async runtime worker；
- Parser timeout；
- Normalized Document 字符数、Section 数和深度限制；
- MCP response server-side hard limit，避免客户端省略限制后把超大章节注入模型上下文。

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

服务端还会限制 Tool response：`read_document` 默认 40,000 字符、最多 80,000；`get_context` 默认 24,000、最多 48,000；`get_document_structure` 最多返回 2,000 个可见节点。更大的结构应降低 `max_depth` 或先搜索定位。

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

概念上的 stdio MCP 配置：

```json
{
  "mcpServers": {
    "reading": {
      "command": "/absolute/path/to/reading-mcp"
    }
  }
}
```

详细运行配置见 [`docs/runtime-configuration.md`](docs/runtime-configuration.md)。完整 hardening 状态见 [`docs/release-hardening-plan.md`](docs/release-hardening-plan.md)。

### Streamable HTTP / 隧道模式

如果 GPT 或其他远程 MCP Client 需要通过 HTTPS 访问，可使用 HTTP binary：

```bash
export READING_MCP_HTTP_TOKEN="$(openssl rand -hex 32)"
./target/release/reading-mcp-http
```

安全默认：

```text
Bind          127.0.0.1:8787 only
Bearer        mandatory
Host check     enabled
Origin check   enabled
MCP path       /mcp
Health         /healthz
Ready          /readyz
```

服务拒绝直接绑定 `0.0.0.0` 或 LAN 地址。所有 HTTP 端点都必须携带：

```text
Authorization: Bearer <READING_MCP_HTTP_TOKEN>
```

稳定命名隧道应配置明确 Host/Origin：

```bash
export READING_MCP_HTTP_ALLOWED_HOSTS='reading.example.com'
export READING_MCP_HTTP_ALLOWED_ORIGINS='https://reading.example.com'
```

如果只做临时 Quick Tunnel 验证，随机公网 Host 无法预先固定时必须**显式**关闭 Host validation，而不是默认关闭：

```bash
export READING_MCP_HTTP_DISABLE_HOST_VALIDATION=true
cloudflared tunnel --no-autoupdate --url http://127.0.0.1:8787
```

这不会关闭 Bearer 或 Origin validation。如果远程客户端发送 `Origin`，仍需将实际可信 Origin 配到 `READING_MCP_HTTP_ALLOWED_ORIGINS`。生产环境优先使用稳定命名隧道和明确 allowlist。

### Secure MCP Tunnel / GitHub Actions

本项目也支持通过 OpenAI Secure MCP Tunnel 暴露 stdio MCP。仓库的 `Deploy reading-mcp tunnel` workflow 仅用于 GitHub-hosted cloud runner 临时测试；它以前台方式运行隧道，job 结束或超时后隧道也会结束。生产运行由部署服务器上的 `reading-mcp-tunnel.service` 负责，并把 `server/discover` 不兼容请求回退到标准 `initialize`。

云端 runner 是临时环境，无法读取部署服务器 `/root` 下的本地文档；`local_roots` 只能填写该临时 runner 中的目录。最终本机部署使用 `/root/.env` 和 systemd，不依赖 GitHub runner。

## 明确非目标

v0.1.0 不包含：

- OCR / 扫描 PDF；
- JavaScript-heavy 页面浏览器渲染；
- Confluence / Notion / 飞书 / 语雀等产品 API；
- OAuth / Cookie 交互登录；
- 公网多租户服务；HTTP 服务本身只提供单机部署入口；
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
