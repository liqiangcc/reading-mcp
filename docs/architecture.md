# Reading MCP 架构设计

## 1. 设计目标

Reading MCP 的架构重点是保持边界稳定：

```text
文档从哪里来
≠
文档是什么格式
≠
文档如何索引
≠
MCP 如何暴露能力
≠
AI 如何理解内容
```

因此系统按职责拆分，而不是按“某一种格式的完整流程”拆分。

---

## 2. 总体架构

```text
                     MCP Adapter
                         │
                  Document Service
                         │
        ┌────────────────┼────────────────┐
        │                │                │
    Retriever          Parser           Index
        │                │                │
        │         ┌──────┼──────┐         │
        │        HTML   PDF  Markdown     │
        │                │                │
        └────────────────┼────────────────┘
                         │
                Normalized Document
                         │
              ┌──────────┴──────────┐
              │                     │
            Cache               Location Map
```

---

## 3. 模块职责

### 3.1 MCP Adapter

职责：

- 定义 MCP tools；
- 参数校验；
- 调用 Application Service；
- 将内部错误映射为稳定 MCP 错误；
- 控制返回大小。

不负责：

- 下载 URL；
- 解析 PDF；
- 搜索实现；
- 缓存实现。

### 3.2 Document Service

系统用例编排层。

负责：

- `open_document`；
- `get_document_structure`；
- `search_document`；
- `read_document`；
- `get_context`。

它依赖抽象接口，不直接依赖具体 PDF/HTTP 实现。

### 3.3 Retriever

统一输入获取层。

```text
Retriever
├── HttpRetriever
├── FileRetriever
└── BrowserRetriever    # future
```

输出建议：

```text
RetrievedResource
├── source
├── final_source
├── media_type
├── bytes
├── etag
├── last_modified
└── metadata
```

Retriever 不理解章节结构。

### 3.4 Security Policy

放在 Retriever 之前/内部的独立策略组件，而不是散落在 HTTP 代码里。

职责：

- scheme policy；
- host/IP policy；
- DNS validation；
- redirect validation；
- max size；
- timeout；
- content type policy。

建议接口：

```text
SourcePolicy
NetworkTargetPolicy
ResourceLimitPolicy
CredentialPolicy
```

### 3.5 Parser

统一解析器接口：

```text
Parser
├── HtmlParser
├── MarkdownParser
├── TextParser
├── PdfParser
└── EpubParser
```

输入：原始资源。

输出：`NormalizedDocument`。

Parser 不关心 HTTP、认证和缓存。

### 3.6 Index

MVP 先做全文检索。

建议索引字段：

- document id；
- section id；
- heading；
- body；
- location；
- page；
- source metadata。

MVP 不引入 embedding/vector DB，除非全文检索被真实场景证明不足。

### 3.7 Cache

建议逻辑分层：

```text
RawResourceCache
ParsedDocumentCache
SearchIndexCache
```

缓存 key 需要同时考虑 source 和 content version。

---

## 4. Domain Model

### 4.1 Document

```text
Document
├── id: DocumentId
├── source: Source
├── title
├── media_type
├── content_hash
├── metadata
├── root_sections[]
└── assets[]
```

### 4.2 Section

```text
Section
├── id: SectionId
├── parent_id
├── title
├── level
├── content
├── location
└── children[]
```

### 4.3 Location

`Location` 必须是统一概念，但允许格式特定字段缺失。

```text
Location
├── page
├── chapter
├── section_path
├── anchor
├── paragraph
├── char_start
├── char_end
└── native_location
```

其中 `native_location` 用于保留 EPUB spine、PDF object/page 等底层定位信息。

---

## 5. 稳定 ID 设计

### Document ID

建议：

```text
DocumentId = hash(normalized_source + content_hash)
```

这样文档内容变化时得到新版本 ID。

### Section ID

优先基于结构路径生成：

```text
section://chapter-7/page-tables
```

标题冲突时加入稳定 ordinal 或内容摘要。

禁止使用随机 UUID 作为唯一 section 定位，否则同一文档重新解析后位置无法复用。

---

## 6. 文档切分

优先级：

```text
native document structure
        ↓
heading / section boundary
        ↓
paragraph boundary
        ↓
sentence boundary
        ↓
hard size limit
```

Index 可以索引较小的 search units，但 `read_document` 应尽量返回完整逻辑 section。

即：

> 搜索单元和阅读单元不必相同。

这是避免 RAG 式机械切块破坏书籍连续性的关键。

---

## 7. Tool 到内部用例映射

```text
open_document
  → OpenDocumentUseCase
  → Retriever
  → Parser
  → Cache / Index

get_document_structure
  → DocumentRepository

search_document
  → SearchIndex

read_document
  → DocumentRepository + LocationResolver

get_context
  → LocationResolver + DocumentRepository
```

---

## 8. `open_document` 流程

```text
source
  ↓
normalize source
  ↓
validate source policy
  ↓
lookup source metadata/cache
  ↓
retrieve resource if needed
  ↓
validate final target/content
  ↓
calculate content hash
  ↓
parsed cache hit?
  ├─ yes → return
  └─ no
      ↓
    choose parser
      ↓
    parse to NormalizedDocument
      ↓
    build index
      ↓
    persist cache
      ↓
    return metadata
```

---

## 9. SSRF 防护流程

HTTP 请求每一跳都必须：

```text
URL
 ↓
validate scheme
 ↓
resolve DNS
 ↓
validate all resolved IPs
 ↓
connect
 ↓
redirect?
 ├─ no → continue
 └─ yes
      ↓
   repeat validation
```

不能只在最初 URL 检查 host 字符串。

---

## 10. Browser Retriever 边界

MVP 不实现浏览器渲染。

未来 BrowserRetriever 只能作为可选实现：

```text
HttpRetriever failed to obtain useful document
           ↓
explicit policy allows browser fallback
           ↓
BrowserRetriever
```

BrowserRetriever 不应改变 Parser、Document Model 和 MCP Tool。

---

## 11. 错误模型

建议内部错误分类：

```text
InvalidSource
BlockedSource
FetchTimeout
ResourceTooLarge
UnsupportedMediaType
ParseFailed
DocumentNotFound
SectionNotFound
InvalidLocation
IndexFailed
CredentialUnavailable
```

MCP Adapter 将其转换为稳定错误码和用户可理解信息。

---

## 12. 推荐项目目录

语言确定后可以映射到具体 package/crate/module。逻辑结构建议：

```text
src/
├── mcp/
├── application/
│   ├── open_document
│   ├── search_document
│   └── read_document
├── domain/
│   ├── document
│   ├── section
│   └── location
├── retrieval/
│   ├── http
│   └── file
├── parsing/
│   ├── html
│   ├── markdown
│   ├── text
│   └── pdf
├── security/
├── search/
├── cache/
└── config/
```

---

## 13. 核心设计原则

```text
Document acquisition ≠ parsing
Parsing ≠ indexing
Indexing ≠ reading
Reading ≠ reasoning
Search unit ≠ reading unit
MCP transport ≠ application logic
Security policy ≠ HTTP implementation
```

只要这些边界保持稳定，后续增加 EPUB、DOCX、Browser、Confluence 等能力都不需要破坏核心架构。
