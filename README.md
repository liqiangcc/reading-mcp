# Reading MCP

> 面向 AI 的统一文档与书籍阅读上下文基础设施。

Reading MCP 让 MCP Client / Agent 能够**精确地与用户阅读同一份文档**：先发现来源、打开文档、查看结构、枚举或搜索精确位置，再读取 canonical normalized source；当公式、图表、排版或 parser fidelity 需要核对时，还可以从 `TextLocator` 回到绑定的原始 PDF 页面。

它只提供可靠的文档上下文，不在内核中实现 AI 总结、问答、教学、笔记或通用 RAG。

## v0.1.0 当前能力

当前 runtime 实际暴露 **9 个 MCP Tool**：

```text
list_documents
list_directory
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
get_source_view
```

其中：

- `list_directory` 负责已授权 Source Workspace 的目录导航；
- `list_documents` 负责已知目录 scope 内的可打开文档发现；
- `get_source_view` 负责从精确 `TextLocator` 回到 identity-bound 原始视觉 Source，当前首个实现为 PDF page fidelity review。

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
 stdio / Streamable HTTP
        ↓
       AI
```

Agent 的推荐调用顺序：

```text
list_directory（浏览授权 roots 和已知目录的直接 children）
      ↓
list_documents（在已知目录中发现可打开文档）
      ↓
open_document
      ↓
get_document_structure
      ↓
get_text_units / search_document
      ↓
get_context / read_document
      ↓
必要时：TextLocator → get_source_view（原始视觉 fidelity review）
```

需要逐句阅读时，在 `open_document` 后先读取 `reading_profile/v1`，再用
`get_document_structure`（`StructureCursor`）枚举 Section，用
`get_text_units(requested_kind=sentence, coverage_policy=preserve_source)` 按
`TextLocator` 和 `TextUnitCursor` 逐项推进。`body-order/v1` 是整本/多 Section
组合的 canonical body 顺序；结构 preorder 不被当作 EPUB 正文阅读顺序。

`list_directory` 使用独立的 `directory-cursor/v1` continuation；目录 entry 与 document
candidate 有明确类型区分。`list_documents` 使用 `discovery-cursor/v1` continuation。所有 cursor 都只表示
各自 bounded stream 的进度，不是 citation，也不会 fuzzy rebase；raw 或 normalized
identity 变化会明确返回 stale。

搜索单元可以是较小段落或精确 Sentence，但读取单元保持 canonical source identity：

```text
Search Unit ≠ Read Unit
Index ≠ Document
Search ≠ Read
Original visual view ≠ Canonical normalized text
```

`search_document` 的 `SearchHit.text_locator` 可以直接交给 `read_document`、
`get_context`、anchored `get_text_units`，或在需要视觉核对时交给 `get_source_view`；禁止复制 snippet 后重新搜索定位。

## Original Source View / Source Fidelity

正常阅读仍然走：

```text
TextLocator
→ read_document
→ exact canonical normalized source
```

只有公式、Figure / Table、多栏排版、特殊符号或 parser fidelity 可疑时，才进入：

```text
TextLocator normalized range
→ original-source-binding/v1
→ original PDF page
→ get_source_view
→ image/png
```

PDF parser 会持久化 Section-relative normalized range → original page 的精确绑定，因此一个逻辑 Section 跨多页时，page 2 中的句子不会错误回到 Section 起始页。跨越多个原始页面的 locator 会被拒绝，调用方需要缩小到 Paragraph/Sentence locator；缺少精确 page-binding evidence 的旧多页持久化 PDF 会 fail closed，并要求使用当前 parser 重新打开。

`get_source_view` 响应包含 `content_hash`、`normalized_document_hash`、`normalized_document_hash_version`、`source_binding_version`、`page_number`、PDF page count、image dimensions/bytes 等审计信息，并返回真正从原始 PDF bytes 渲染的 `image/png`，不会用 OCR 或 normalized text 重绘冒充原始页面。

## 安全默认

公共网络来源默认只允许 HTTPS，并启用：

- SSRF scheme / hostname / DNS / IP 校验；
- 每次 redirect 重新校验；
- 禁止 proxy 破坏已验证 endpoint 的安全证据链；
- HTTP timeout / redirect / concurrency / body size 限制；
- Content-Type allowlist；
- PDF 总页数与单页解压上限；
- EPUB/DOCX ZIP entry 数、单 entry 和总解压大小限制；
- Parser timeout；
- Normalized Document 字符数、Section 数和深度限制；
- Source View 的 page count、DPI、尺寸、pixel、decoded stream、encoded image size 限制；
- Source View 在独立 worker process 中渲染，输入/输出/metadata/diagnostic 使用私有临时目录，超时 worker 会 `kill + wait`，不会仅停止等待而让渲染继续后台消耗资源。

本地文件默认关闭。只有部署者配置允许目录后才能读取：

```bash
READING_MCP_LOCAL_ROOTS=/home/me/books:/home/me/docs reading-mcp
```

请求路径和授权目录都会 canonicalize，目标必须位于显式 root 内。目录导航会跳过 symlink entry；broken symlink 不会导致整个目录 discovery 失败，外部 symlink 也不能逃出授权 root。

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
normalized_document_hash / normalized_document_hash_version
section_id / parent_id / title
page
chapter
section_path
paragraph
anchor
normalized char range
native_location
source_binding_version（Original Source View）
```

例如 PDF canonical structure 通过 `page` / `pdf:page:N` 保留结构定位，并通过 `original-source-binding/v1` 把精确 normalized ranges 绑定回实际 PDF page；HTML 通过 anchor；EPUB 通过 spine + archive entry/anchor；DOCX 通过 paragraph 位置；OpenAPI 通过 JSON-pointer-like location。

Canonical normalized reading 继续由统一的 `read_document` / `get_text_units` 契约负责；原始视觉核对使用统一的 `get_source_view`，不会新增 `read_pdf_page` / `read_pdf_page_range` 这类永久格式特化 Tool。

## 结构优先

Reading MCP 不把整篇文档默认切成固定字符块。

```text
Section
  ↓
Paragraph / Sentence / Search Candidate
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

可区分参数错误、安全策略拒绝、认证失败、资源限制、来源故障、解析失败、stale/invalid locator/cursor、Source View failure，以及 Repository/Cache/Index 内部故障。

## 架构边界

```text
Source discovery / directory navigation
≠ 来源获取
≠ 安全策略
≠ 格式解析
≠ Normalized Document
≠ Original Source View rendering
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
cargo build --release --locked --bin reading-mcp --bin reading-mcp-http
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

详细运行配置见 [`docs/runtime-configuration.md`](docs/runtime-configuration.md)。Original Source View 见 [`docs/source-view.md`](docs/source-view.md)。目录导航见 [`docs/directory-navigation-contract.md`](docs/directory-navigation-contract.md)。完整 hardening 状态见 [`docs/release-hardening-plan.md`](docs/release-hardening-plan.md)。

### Streamable HTTP / 隧道模式

如果 GPT 或其他远程 MCP Client 需要通过 HTTPS 访问，可使用 HTTP binary：

```bash
export READING_MCP_HTTP_TOKEN="$(openssl rand -hex 32)"
./target/release/reading-mcp-http
```

默认监听 `127.0.0.1:8787`，MCP 地址为 `/mcp`，请求必须携带：

```text
Authorization: Bearer <READING_MCP_HTTP_TOKEN>
```

stdio 与 Streamable HTTP 使用同一 Application/Tool surface；Source View 的隔离 worker mode 在两个 binary 中都可用，并分别有真实 E2E 验证。

临时隧道示例：

```bash
cloudflared tunnel --no-autoupdate --url http://127.0.0.1:8787
```

将 Cloudflare 输出的 `https://...trycloudflare.com/mcp` 配置给远程 MCP Client，并配置同一个 Bearer Token。生产环境应使用稳定的命名隧道和独立认证层；Quick Tunnel 只适合临时验证。

### Secure MCP Tunnel / GitHub Actions

本项目也支持通过 OpenAI Secure MCP Tunnel 暴露 stdio MCP。仓库的 `Deploy reading-mcp tunnel` workflow 仅用于 GitHub-hosted cloud runner 临时测试；它以前台方式运行隧道，job 结束或超时后隧道也会结束。生产运行由部署服务器上的 `reading-mcp-tunnel.service` 负责，并把 `server/discover` 不兼容请求回退到标准 `initialize`。

云端 runner 是临时环境，无法读取部署服务器 `/root` 下的本地文档；`local_roots` 只能填写该临时 runner 中的目录。最终本机部署使用 `/root/.env` 和 systemd，不依赖 GitHub runner。

## 明确非目标

v0.1.0 不包含：

- OCR / 扫描 PDF 全面支持；
- 浏览器式完整 PDF UI、PDF 编辑或标注；
- 任意多页批量截图或用视觉结果替代 canonical normalized source；
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
