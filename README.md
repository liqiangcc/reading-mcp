# Reading MCP

> 面向 AI 的统一文档与书籍阅读上下文基础设施。

Reading MCP 的目标不是让 AI 替用户读书，而是让 AI 能够与用户**精确地阅读同一份文档、同一本书**，并保留来源、章节与格式特有位置。

它负责文档的获取、解析、结构化、定位、搜索、按需读取和缓存；解释、总结、推理、教学、生成问题等能力继续由上层 AI 完成。

## 当前 MVP 状态

当前已经打通：

```text
Local File / Public HTTPS
          ↓
      Security Policy
          ↓
       Retriever
          ↓
     Raw Resource Cache
          ↓
       Parser Router
   ┌──────┼──────┬──────┐
 Text Markdown  HTML   PDF
   └──────┼──────┴──────┘
          ↓
 Normalized Document
     ┌────┴────┐
 Repository   SearchIndex
     └────┬────┘
          ↓
  Application UseCases
          ↓
       MCP stdio
          ↓
       AI Client
```

MVP 支持：

- Plain Text
- Markdown
- 静态 HTML
- 原生文本 PDF
- 本地文件（默认关闭，需显式授权目录）
- 公共 HTTPS 文档
- SSRF / DNS / redirect 安全校验
- Raw / Parsed Cache
- 文档内全文搜索
- 真实 MCP stdio Server

## MCP Tools

MVP 只暴露 5 个稳定 Tool：

```text
open_document
get_document_structure
search_document
get_context
read_document
```

推荐调用顺序：

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

### `open_document`

打开、校验、获取、解析并索引文档，返回：

```text
document_id
source
title
media_type
content_hash
section_count
```

### `get_document_structure`

返回结构化 Section Tree，不返回整篇正文。节点包含：

```text
section_id
parent_id
title
level
location
children
```

### `search_document`

在已打开文档中执行小粒度搜索，返回：

```text
section_id
title
source
snippet
score
location
```

Search Unit 可以小于阅读章节，但结果必须映射回 owning Section。

### `get_context`

围绕 owning Section 展开前后逻辑上下文。正文来自规范化 Document，而不是搜索 snippet 拼接。

### `read_document`

按 `section_id` 读取完整逻辑章节，并递归包含其子章节。

MVP **不声明尚未实现的 page-range / arbitrary-range Tool**。PDF 页码、HTML anchor 等格式特有位置统一放在 `Location` 中返回。

## 为什么不是固定 Chunk Reader

默认不要把长文档机械切成：

```text
0-5000 chars
5000-10000 chars
10000-15000 chars
```

Reading MCP 优先保留作者定义或格式可恢复的阅读结构：

```text
section://installation/linux/docker
section://configuration/database
section://chapter-7/page-tables
```

核心原则：

> 结构优先，长度限制兜底。

搜索可以使用 paragraph/search-unit 粒度；读取仍尽量保持完整逻辑 Section。

## Normalized Document

不同格式最终统一为：

```text
Document
├── id
├── source
├── title
├── media_type
├── content_hash
├── metadata
└── sections[]
    ├── id
    ├── parent_id
    ├── title
    ├── level
    ├── content
    ├── location
    └── children[]
```

`Location` 尽可能保留：

```text
page
chapter
section_path
paragraph
anchor
char range
native_location
```

PDF 更偏向 page/native locator，HTML/Markdown 更偏向 section/anchor；格式差异不会扩散成不同 MCP Tool。

## 架构边界

```text
来源获取
≠
安全策略
≠
格式解析
≠
规范化文档
≠
缓存
≠
Repository
≠
SearchIndex
≠
MCP Adapter
≠
AI 理解与推理
```

几个关键约束：

```text
Retrieval ≠ Parsing
Search ≠ Read
Section ≠ Search Unit
Document ≠ Index
Storage ≠ Cache ≠ Index
MCP ≠ Application Logic
Reading MCP ≠ AI Application
```

新增格式主要修改 `parsing`；新增来源主要修改 `retrieval/security`；更换 SearchIndex 不应修改 Parser；更换 MCP transport 不应修改 Domain/Application。

## 安全默认

### 公共 URL

默认只允许 HTTPS，并拒绝：

- localhost / loopback
- RFC1918 私网
- link-local
- cloud metadata 地址
- IPv6 private/link-local
- URL 内嵌 username/password

每一次 redirect 都重新执行 URL、DNS 和目标 IP 校验。

HTTP Retriever 默认禁用环境/system proxy，以保持“已验证 DNS → pinned endpoint → direct request”的安全证据链。

### 本地文件

**默认不能读取任何本地文件。**

只有部署者显式设置允许访问的根目录后才开启：

```bash
READING_MCP_LOCAL_ROOTS=/home/me/books:/home/me/docs \
  ./target/release/reading-mcp
```

程序会 canonicalize 请求路径和授权根目录，并要求目标位于某个 root 下。

这意味着：

```text
未配置 local roots
→ /etc/passwd        拒绝
→ ~/books/os.md      拒绝

允许 ~/books
→ ~/books/os.md      允许
→ ~/secrets/a.md     拒绝
```

## 构建与运行

```bash
cargo build --release --bin reading-mcp
./target/release/reading-mcp
```

默认运行时：

```text
Public HTTPS → enabled
Local File   → disabled
MCP Transport→ stdio
```

如果 MCP Client 需要本地文档，把 `READING_MCP_LOCAL_ROOTS` 作为该 MCP Server 的环境变量配置即可。

## 缓存语义

缓存分层：

```text
RawResourceCache
      ↓
ParsedDocumentCache
      ↓
DocumentRepository
      ↓
SearchIndex
```

`force_refresh=true` 会绕过 Raw Cache 重新获取来源；如果获取后的 bytes 未变化，Parsed Cache 仍可复用。

当前已经保存 HTTP `ETag` / `Last-Modified`，但尚未实现自动 `If-None-Match` / `If-Modified-Since` 条件重验证，所以长期运行时需要 `force_refresh` 主动确认远端变化。

## 当前明确限制

MVP 适合：

```text
单用户
本地 stdio
受信部署者
公共 HTTPS
显式授权本地目录
```

尚未完成：

- PDF 总页数资源上限
- Parser 全局 timeout / cancellation
- 本地文件最大读取大小
- HTTP 条件缓存重验证
- 默认 runtime 的持久化 Repository/SearchIndex
- HTTP auth_profile credential provider / host binding
- OCR
- EPUB / DOCX
- JavaScript-heavy Browser Retriever
- Streamable HTTP MCP transport
- AI 总结 / 问答 / 笔记
- 跨文档向量检索

因此当前不应直接定位成公网多租户文档服务。

## 项目原则

```text
结构优先 > 固定切块
按需读取 > 整篇注入
可追溯 > 无来源回答
职责分离 > MCP 内置智能
统一抽象 > 格式专属 Tool
安全默认 > 任意来源可访问
全文搜索优先 > 过早引入向量数据库
MVP 简单可用 > 一开始支持所有格式
```

详细设计与 Review：

- `docs/requirements.md`
- `docs/design-principles.md`
- `docs/architecture.md`
- `docs/mvp.md`
- `docs/phase5-security-cache.md`
- `docs/phase6-mcp-stdio.md`
- `docs/mvp-review.md`
