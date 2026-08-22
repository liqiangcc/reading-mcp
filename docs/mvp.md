# Reading MCP MVP 实施计划

> Status note: Phase 0–6 是项目历史实施路径。当前 runtime 已在 MVP 之后继续完成 ReadCursor、normalized identity/range、Paragraph/Sentence TextUnit foundation 与第 7 个 Tool `get_text_units`。本文件中的阶段描述保留历史目标，但“当前能力”以本节和 `docs/README.md` / `docs/phase6-mcp-stdio.md` 为准。

## 1. MVP 目标

MVP 最初验证一个核心闭环：

> AI 能否通过统一 MCP 接口，安全地发现/打开不同格式文档，查看结构，搜索内容，精确读取相关章节，并获得稳定来源定位。

MVP 不追求支持所有文档格式，也不做 AI 总结、向量数据库或浏览器自动化。

工程实现必须同时证明：

> 来源、格式、搜索、读取、安全、存储和 MCP 协议可以独立演进，而不是通过一个“大 Reader”耦合在一起。

实现与后续演进必须遵守 [设计原则](design-principles.md) 与 [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md)。

---

## 2. 当前能力与 MVP 历史范围

### 输入格式

MVP 首批验证：

- HTML
- Markdown
- Plain Text
- PDF

当前 runtime 还已实现 EPUB、DOCX、OpenAPI/Swagger 等格式扩展。

### 当前 MCP Tools

当前 runtime 实际暴露 7 个 Tool：

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
read_document
get_context
```

演进历史：

```text
Phase 0 初始 document-scoped surface = 5 Tools
+ list_documents                    = 6 Tools
+ get_text_units                    = 7 Tools
```

`list_documents` 是独立 DocumentDiscovery；不打开或解析文档。

`get_text_units` 是由真实 Paragraph/Sentence-first Use Case 推导出的独立 `OrderedTextUnitEnumeration` 能力，而不是为了数量或 convenience 增加。当前已实现 source-order Paragraph/Sentence 枚举、TextLocator、TextUnitCursor、forward/backward pagination、completion 与 non-prose/coverage 语义。

### 当前 precise-reading foundation

```text
ReadCursor / SectionTreeReadStream                  ✓
normalized_document_hash / NormalizedTextRange      ✓
Paragraph TextUnit + Paragraph TextUnitIndex        ✓
Sentence locator + non-prose coverage               ✓
TextLocator enumeration output                      ✓
get_text_units + TextUnitCursor                     ✓
TextLocator input to context/search/read             later
```

### 基础设施

- HTTP Retriever
- File Retriever
- SSRF Policy
- Parser Router
- Normalized Document Model
- Raw / Parsed Cache
- SQLite DocumentRepository
- Paragraph TextUnitIndex
- Full-text Search Index
- MCP stdio / Streamable HTTP transport

---

## 3. 历史实施阶段

### Phase 0：工程骨架、契约与边界

目标：先稳定关注点分离和依赖方向，不急着实现所有解析器。

任务：

- 初始化项目；
- 定义配置系统；
- 定义 `Document` / `Section` / `Location`；
- 定义 Retriever / Parser / Repository / Cache / SearchIndex / SecurityPolicy ports；
- 定义 Location/Citation 边界；
- 定义最初 5 个 document-scoped MCP Tool schema；
- 定义 application use cases；
- 建立单元测试、CI 与架构边界测试。

后续 `list_documents` 作为独立 DocumentDiscovery use case 加入，使 runtime 从 5 增长为 6；再由 ADR 0004 的 Use Case 证据加入 `get_text_units`，形成当前 7 Tool surface。

依赖方向：

```text
mcp → application → domain

retrieval ─┐
parsing   ─┤
storage   ─┤→ application/domain ports
cache     ─┤
index     ─┤
security  ─┘
```

禁止：

```text
parser → mcp
retriever → parser-specific code
application → concrete HTTP/PDF/SQLite library
index → MCP DTO
MCP handler → HTTP/PDF/SQL implementation
```

### Phase 1：Text / Markdown 闭环

验证：

```text
open markdown
→ show TOC
→ read one section
```

同一个 Parser 可以服务 FileRetriever / HttpRetriever；来源不复制解析逻辑。

### Phase 2：全文搜索与上下文展开

实现：

```text
search "virtual memory"
→ hit owning Section / legacy Location
→ get_context
→ read owning section
```

保持：

```text
Search ≠ Read
Section ≠ Search Unit
Index ≠ Document
```

未来 precise SearchHit→TextLocator handoff 仍是独立后续增量。

### Phase 3：HTML

增加 HtmlParser，不修改来源层/MCP Tool schema；保留 heading/anchor/source provenance。

### Phase 4：PDF

优先 native text + TOC/bookmark；无 TOC 时稳定降级 page-level Section；页码属于 Location，不创建 PDF 专属 Tool。OCR 仍是非目标。

### Phase 5：安全与缓存强化

完成：

- URL scheme / DNS / IP / redirect 每跳校验；
- response/time/concurrency budgets；
- ETag/Last-Modified；
- raw/parsed cache；
- source/normalization identity 与 cache invalidation。

### Phase 6：真实 Agent / MCP 验证

最初验证的是 6-Tool coarse workflow：

```text
list_documents（可选）
→ open
→ structure
→ search
→ context
→ read
```

当前 Phase 6 stdio acceptance 已扩展到 7 Tool，并增加：

```text
open
→ structure
→ get_text_units(requested=sentence)
→ TextUnitCursor continuation
→ search / context / read
```

真实 stdio 测试会启动 `reading-mcp` 子进程，经 MCP initialize/tools/list/call 验证，而不是只直接调用 UseCase。

---

## 4. 当前仍明确延后

以下能力仍未实现或仍需独立 Use Case/证据：

- TextLocator 输入到 `read_document`；
- Paragraph/Sentence tagged context；
- SearchHit → Paragraph/Sentence TextLocator；
- Paragraph/Sentence FTS；
- anchor-based `get_text_units before/after(locator)` start；
- Sentence SQLite persistence（仅在性能证据需要时考虑）；
- OCR；
- Playwright/browser rendering；
- JavaScript-heavy 网站；
- Confluence/Notion/飞书/语雀产品 API；
- 向量数据库 / embedding / reranker；
- cross-document semantic search；
- AI summarization / Q&A / note generation。

Sentence persistence 不属于“待补正确性”：当前 TextUnitCursor 已能基于 persisted canonical Document + deterministic segmentation 在 repository restart 后继续。未来若增加 Sentence rows，也只能作为 rebuildable performance optimization。

EPUB 与 DOCX 已实现基础 Parser；其更高精度结构可靠性/provenance/coverage 仍按 ADR 0003 独立推进。

---

## 5. 测试策略

### Architecture tests

持续验证：

- MCP 不依赖具体 Retriever/Parser；
- Parser 不依赖 MCP；
- Retriever 不依赖 Parser；
- application 不依赖具体第三方基础设施；
- Source × Format 不产生专用 Reader 类；
- SearchIndex / TextUnitIndex 可替换且不成为 source truth；
- AI SDK 不进入核心依赖。

### Domain tests

已覆盖并继续扩展：

- Section tree / stable ids；
- raw/normalized identity；
- exact normalized range；
- Paragraph/Sentence TextUnit ownership/order；
- non-prose classification/coverage；
- TextLocator identity；
- ReadCursor / TextUnitCursor stream invariants。

### Parser fixture tests

每种 Parser 使用固定 fixture。EPUB 精确能力必须继续覆盖 nav/NCX/heading/spine fallback、unresolved target、non-prose 与 coverage。

### Security tests

必须覆盖 localhost/private/metadata endpoint、redirect to private、oversized response、timeout 等。

### Integration / MCP contract tests

确保：

- schema/error code 稳定；
- response budget 生效；
- Tool 不泄露凭据；
- Tool discovery 与真实 runtime 数量一致；
- 旧 6 Tool 调用不因 `get_text_units` 增加而破坏；
- `get_text_units` forward/backward continuation no-gap/no-overlap；
- `preserve_source` non-prose coarse fallback；
- `eligible_only` 不宣称 all-source completion；
- cursor stale/target mismatch fail closed；
- repository restart 后 TextUnitCursor 可继续。

---

## 6. 当前成功标准

```text
[x] 当前支持格式统一打开
[x] 授权本地文档可发现
[x] 结构导航可用
[x] 全文搜索可用
[x] Section read continuation 可用
[x] Section 上下文展开可用
[x] raw/normalized 来源身份稳定
[x] Paragraph TextUnit 可确定性重建
[x] Sentence locator/coverage 可确定性重建
[x] Paragraph/Sentence-first stream 可按 Section 完整枚举
[x] TextUnitCursor gap/overlap/stale invariant 已验证
[x] non-prose source-preserving fallback 不伪造 Sentence
[x] 缓存/持久化生效
[x] SSRF 默认阻断
[x] MCP/Parsing/Retrieval/Search/TextUnit responsibilities 保持解耦
[x] 没有 LLM 依赖
```

仍待完成的 precise handoff：

```text
[ ] TextLocator → read/context
[ ] SearchHit → TextLocator → read/context
[ ] tagged Paragraph/Sentence context
[ ] EPUB reliability/degradation/coverage 完整闭环
```

---

## 7. 后续优先级

当前最合理依赖顺序：

```text
get_text_units / TextUnitCursor               ✓
        ↓
feat/context-granularity                      next
        ↓
feat/search-locator
        ↓
feat/lexical-text-unit-index
        ↓
EPUB reliability/coverage increments as evidence requires
```

继续遵守：

> 先把“读准一份文档”做好，再考虑“搜索所有知识”。

以及：

> 如果扩展一个能力需要同时修改来源层、解析层、搜索层和 MCP 层，优先检查架构边界，而不是继续堆条件分支。
