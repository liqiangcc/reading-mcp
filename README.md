# Reading MCP

> 面向 AI 的统一文档与书籍阅读上下文基础设施。

Reading MCP 的目标不是让 AI 替用户读书，而是让 AI 能够与用户**精确地阅读同一份文档、同一本书**，并始终保留章节、页码/位置和来源边界。

它负责文档的获取、解析、结构化、定位、搜索、按需读取和缓存；解释、总结、推理、教学、生成问题等能力继续由上层 AI 完成。

## 为什么需要 Reading MCP

AI 阅读长文档时常见问题：

- 整份文档一次进入上下文，Token 消耗大；
- 固定字符数切块破坏章节结构和连续论证；
- HTML、PDF、EPUB、Markdown 等格式接口不统一；
- AI 难以明确指出内容来自哪一章、哪一节、哪一页；
- 同一文档被重复下载、重复解析；
- 在线 URL 读取存在 SSRF、重定向和凭据泄漏风险；
- “获取文档”和“理解文档”的职责容易混在一起。

Reading MCP 将这些问题收敛到一个统一的文档上下文层。

## 核心流程

```text
URL / Local Document
        ↓
     Retriever
        ↓
 Security Validation
        ↓
 Content-Type Router
        ↓
       Parser
        ↓
Normalized Document
        ↓
TOC / Search / Section / Range
        ↓
       Cache
        ↓
        MCP
        ↓
        AI
```

对 AI 而言，无论文档原始格式是什么，都遵循统一阅读过程：

```text
open
  ↓
inspect structure
  ↓
search / locate
  ↓
read section
  ↓
expand context when needed
```

## 适用场景

- 在线技术文档
- RFC / KIP / ADR / 设计文档
- PDF 教材与技术书
- EPUB 电子书
- Markdown Book
- GitHub README / Wiki
- OpenAPI / Swagger 文档
- Javadoc / MkDocs / Docusaurus / GitBook

尤其适合教材和技术书阅读：AI 可以先查看目录，再按章节阅读，搜索概念后展开上下文，并返回准确来源位置，而不是把整本书机械切块后塞入上下文。

## MCP 核心工具

MVP 建议保持少而稳定：

```text
open_document
get_document_structure
search_document
read_document
get_context
```

### `open_document`

获取、校验、解析并缓存文档，返回 `document_id` 和元数据。

### `get_document_structure`

返回目录、章节层级和可导航位置，不返回整篇正文。

### `search_document`

只搜索已经打开或缓存的文档内容，不承担 Web 搜索职责。

### `read_document`

按 section、page/location 或 range 精确读取正文。

### `get_context`

围绕搜索命中位置展开前后文，避免把孤立 chunk 当作完整语义。

## 核心边界

Reading MCP 负责：

```text
获取
解析
结构化
搜索
定位
读取
引用
缓存
```

Reading MCP 不负责：

```text
Web 搜索
通用浏览器自动化
表单提交
网站操作
AI 总结
AI 问答
AI 推理
自动生成笔记
自动生成题目
通用 RAG 平台
```

核心原则：

> MCP 负责可靠地提供文档上下文，AI 负责理解、解释、推理和教学。

## 结构优先，而不是机械切块

不要默认：

```text
0-5000 chars
5000-10000 chars
10000-15000 chars
```

优先保留：

```text
section://installation/linux/docker
section://configuration/database
section://chapter-7/page-tables
```

当一个 section 仍然过大时，再按以下顺序降级切分：

```text
section boundary
      ↓
paragraph boundary
      ↓
sentence boundary
      ↓
hard size limit
```

## Normalized Document Model

不同格式最终统一为类似模型：

```text
Document
├── id
├── source
├── title
├── media_type
├── content_hash
├── metadata
├── sections[]
│   ├── id
│   ├── parent_id
│   ├── title
│   ├── level
│   ├── content
│   ├── location
│   └── children[]
├── links[]
└── assets[]
```

位置需要尽可能保留：

```text
page
chapter
section
paragraph
anchor
char range
```

不是所有格式都具备所有字段：PDF 更偏向 page，EPUB 更偏向 spine/location，HTML/Markdown 更偏向 section/anchor。

## 架构原则

获取与解析分离：

```text
Retriever
├── HttpRetriever
├── FileRetriever
└── BrowserRetriever    # future / optional

Parser
├── HtmlParser
├── MarkdownParser
├── PdfParser
├── TextParser
└── EpubParser          # phase 2
```

`Retriever` 只负责得到原始内容，不理解文档结构；`Parser` 不关心文档来自 URL 还是本地文件；搜索层只依赖 Normalized Document。

## 安全原则

在线文档读取必须默认防御 SSRF。

默认只允许 `http/https`，并阻止访问 localhost、loopback、link-local 和 RFC1918 私网地址；每一次 DNS 解析和 redirect 后都必须重新校验目标地址。

还应限制：

- 最大响应大小
- 最大 PDF 页数
- HTTP / parse timeout
- 最大 redirect 次数
- 最大并发
- Content-Type allowlist
- 解压炸弹

认证信息不应作为模型可见的明文 Header 传入，推荐使用：

```json
{
  "auth_profile": "company-docs"
}
```

真实凭据由环境变量、Keychain、Secret Manager 或 Vault 管理。

## 缓存

同一文档不应反复下载和解析：

```text
normalized source
      ↓
ETag / Last-Modified
      ↓
content hash
      ↓
raw cache
      ↓
parsed cache
      ↓
search index
```

文档状态不应绑定单个 MCP session。

## 格式路线图

### MVP

- HTML
- Markdown
- Plain Text
- PDF

### Phase 2

- EPUB
- DOCX
- GitHub README / Wiki
- OpenAPI / Swagger
- Javadoc
- MkDocs / Docusaurus / GitBook

### Phase 3

- JavaScript-heavy 文档站
- Confluence
- Notion
- 飞书文档
- 语雀
- 企业内部认证文档

## 书籍辅助阅读

Reading MCP 特别适合作为 AI 辅助阅读教材/书籍的底层能力：

```text
打开书籍
  ↓
读取目录
  ↓
建立章节知识框架
  ↓
逐节阅读原文
  ↓
搜索概念
  ↓
展开上下文
  ↓
AI 解释 / 对比 / 推理
  ↓
必要时回到原文验证
```

最终目标：

> 让 AI 不替用户读书，而是让 AI 能够精确地和用户一起读同一本书。

## 项目原则

```text
结构优先 > 固定切块
按需读取 > 整篇注入
可追溯 > 无来源回答
职责分离 > MCP 内置智能
统一抽象 > 格式特化接口
安全默认 > 任意 URL 可访问
全文搜索优先 > 过早引入向量数据库
MVP 简单可用 > 一开始支持所有格式
```

详细需求、架构设计和 MVP 实施计划将在 `docs/` 中继续完善。
