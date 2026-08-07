# Phase 6：MCP stdio Server 与真实调用验证

## 1. 目标

Phase 6 的目标不是新增文档能力，而是把已经稳定的 Application UseCase 通过真正的 MCP 协议暴露出去，并验证客户端能够通过 stdio 完成完整阅读流程。

核心约束：

> MCP 只负责协议适配与运行时装配，不承载文档获取、解析、搜索和读取业务逻辑。

因此本阶段不修改 Domain、Parser、SearchIndex 或阅读语义。

---

## 2. 运行时结构

```text
MCP Client
    │
    │ stdio / JSON-RPC
    ▼
reading-mcp binary
    │
    ▼
ReadingMcpServer
    │
    ├── open_document
    ├── get_document_structure
    ├── search_document
    ├── get_context
    └── read_document
          │
          ▼
     Application UseCases
          │
          ▼
Domain + Ports + Adapters
```

`rmcp` 依赖只进入 MCP adapter 和 binary。Domain/Application 不依赖 MCP SDK。

---

## 3. 当前 MCP Tools

### `open_document`

输入本地文件路径或公共 HTTPS URL，完成安全校验、获取、解析、缓存和索引。

返回：

- `document_id`
- `title`
- `media_type`
- `section_count`

### `get_document_structure`

读取已经打开文档的 Section Tree 和 Location，不返回整篇正文。

### `search_document`

在已经打开文档中执行小粒度全文检索，返回 snippet、owning `section_id` 和 Location。

### `get_context`

围绕 owning section 展开相邻逻辑章节。正文来源仍然是 DocumentRepository，而不是搜索 snippet。

### `read_document`

按 `section_id` 读取完整逻辑章节，并递归包含其子章节。

当前 MCP 契约不声明尚未实现的 PDF page-range 专属读取接口。PDF 页码仍通过统一 `Location` 返回。

---

## 4. 默认运行时组合

`ReadingMcpServer::new()` 当前装配：

```text
SourcePolicyRouter
├── LocalFileSourcePolicy
└── PublicHttpAccessPolicy::https_only()

RetrieverRouter
├── FileRetriever
└── HttpRetriever
       ↓
CachingRetriever
       ↓
InMemoryRawResourceCache

ParserRouter::phase4()
├── TextParser
├── MarkdownParser
├── HtmlParser
└── PdfParser
       ↓
CachingParser
       ↓
InMemoryParsedDocumentCache

InMemoryDocumentRepository
InMemorySearchIndex
```

说明：

- HTTP 默认只允许 HTTPS；测试或显式配置下可允许 HTTP。
- SSRF、DNS、redirect 和响应大小策略仍由 Phase 5 的 Security Policy / HttpRetriever 负责。
- 当前 stdio 默认组合使用内存缓存；Phase 5 已提供持久化 Raw/Parsed Cache adapter，可在后续配置层接入。
- 不存在 AI/LLM SDK 依赖。

---

## 5. 启动方式

构建：

```bash
cargo build --release --bin reading-mcp
```

stdio MCP Server：

```bash
./target/release/reading-mcp
```

MCP 客户端只需要把该 binary 配置为 stdio command。例如概念配置：

```json
{
  "mcpServers": {
    "reading": {
      "command": "/absolute/path/to/reading-mcp"
    }
  }
}
```

具体客户端的配置文件位置由客户端自身决定。

---

## 6. 真实 stdio 验收测试

`tests/phase6_mcp_stdio.rs` 不直接调用 Application UseCase，而是：

```text
integration test
      │
      ▼
TokioChildProcess
      │
      ▼
reading-mcp binary
      │
      ▼
MCP initialize
      │
      ▼
tools/list
      │
      ▼
open_document
      │
      ▼
get_document_structure
      │
      ▼
search_document
      │
      ▼
get_context
      │
      ▼
read_document
```

测试使用临时 Markdown 文档，不依赖公网。

验收内容：

- MCP initialize 成功；
- `tools/list` 精确暴露 5 个 Tool；
- Tool 参数通过真实 JSON-RPC/stdin 传递；
- Tool 返回 structured content，并可反序列化为稳定 Contract DTO；
- `open_document` 建立的 Repository/SearchIndex 状态能被后续 Tool 共享；
- search 返回小粒度命中；
- context/read 返回规范化文档正文；
- stdio 子进程可以干净关闭。

---

## 7. 架构验收

Phase 6 需要继续满足：

```text
rmcp → mcp adapter

mcp adapter → application
application → domain + ports

禁止：
domain → rmcp
application → rmcp
parser → rmcp
retriever → rmcp
search index → rmcp DTO
```

同时验证：

> 新增 MCP transport 不应改变文档领域模型和阅读 UseCase。

如果未来增加 Streamable HTTP transport，应新增 transport/runtime adapter，而不是复制 5 个业务 Tool 的实现。

---

## 8. 本阶段明确不做

- Streamable HTTP MCP transport；
- OAuth；
- MCP Resources/Prompts；
- browser rendering；
- OCR；
- EPUB/DOCX；
- AI 总结、问答、笔记；
- 跨文档语义检索。

先验证 stdio + 5 个核心 Tool 足够稳定，再根据真实使用数据决定下一步。
