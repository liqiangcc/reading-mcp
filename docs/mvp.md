# Reading MCP MVP 实施计划

## 1. MVP 目标

MVP 只验证一个核心闭环：

> AI 能否通过统一 MCP 接口，安全地发现/打开不同格式文档，查看结构，搜索内容，精确读取相关章节，并获得稳定来源定位。

MVP 不追求支持所有文档格式，也不做 AI 总结、向量数据库或浏览器自动化。

MVP 的工程实现必须同时证明：

> 来源、格式、搜索、读取、安全、存储和 MCP 协议可以独立演进，而不是通过一个“大 Reader”耦合在一起。

实现前必须阅读并遵守 [设计原则](design-principles.md)。新的 Tool/Contract 演进还必须遵守 [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md)。

---

## 2. MVP 能力范围

### 输入格式

- HTML
- Markdown
- Plain Text
- PDF

### 当前 MCP Tools

当前 runtime 实际暴露 6 个 Tool：

```text
list_documents
open_document
get_document_structure
search_document
read_document
get_context
```

其中 `list_documents` 是后来加入的本地授权目录发现能力；它不打开或解析文档。最初 MVP 的 5 个 document-scoped Tool 仍保持原有语义。

Use-Case-First 设计已经接受一个未来的通用 `get_text_units` Tool，用于 Paragraph/Sentence-first 有序枚举；它不属于当前已实现 MVP surface，必须等 normalized range、TextUnit、deterministic segmentation 和 cursor invariant 完成后再实现。

### 基础设施

- HTTP Retriever
- 可选 File Retriever
- SSRF Policy
- Parser Router
- Normalized Document Model
- Local Cache
- Full-text Search Index
- MCP stdio transport

---

## 3. 实施阶段

### Phase 0：工程骨架、契约与边界

目标：先稳定关注点分离和依赖方向，不急着实现所有解析器。

任务：

- 初始化项目；
- 定义配置系统；
- 定义 `Document` / `Section` / `Location`；
- 定义 Retriever port；
- 定义 Parser port；
- 定义 DocumentRepository port；
- 定义 Cache port；
- 定义 SearchIndex port；
- 定义 SecurityPolicy port；
- 定义 Location/Citation 边界；
- 定义最初 5 个 document-scoped MCP Tool request/response schema；
- 定义 application use cases；
- 建立单元测试和 CI；
- 建立禁止依赖规则或等价的架构测试。

后续 `list_documents` 作为独立 `DocumentDiscovery` use case 加入，使当前 runtime surface 成为 6 个 Tool；这一事实不得与最初 Phase 0 历史范围混淆。

必须先确定依赖方向：

```text
mcp → application → domain

retrieval ─┐
parsing   ─┤
storage   ─┤→ application/domain ports
cache     ─┤
index     ─┤
security  ─┘
```

禁止出现：

```text
parser → mcp
retriever → parser-specific code
application → concrete HTTP/PDF/SQLite library
index → MCP DTO
MCP handler → HTTP/PDF/SQL implementation
```

Phase 0 验收：

- application 层可以使用 fake Retriever/Parser/Repository/Index 完成完整调用；
- MCP 层不依赖具体 HTTP/PDF/SQLite 库；
- Retriever 输出 `RawResource`，Parser 输入 `RawResource`，两者不直接互相调用；
- Source Type 和 Document Format 可以自由组合；
- SearchIndex 可以替换为 fake/in-memory 实现，而不修改 application API；
- 删除索引后理论上可以由 Document Repository 重建；
- Security Policy 可以独立测试，不需要真实 Parser；
- 不存在 `HttpPdfReader`、`LocalMarkdownReader` 之类按来源×格式组合的核心抽象；
- 不存在 LLM/AI SDK 依赖。

如果上述验收不满足，不进入 Phase 1。

### Phase 1：Text / Markdown 闭环

先用最简单格式验证整体设计。

任务：

- File/HTTP 获取；
- Plain Text Parser；
- Markdown heading tree；
- stable section id；
- `open_document`；
- `get_document_structure`；
- `read_document`。

验收：

```text
open markdown
→ show TOC
→ read one section
```

完整可运行。

同时验证：

```text
同一个 Markdown Parser
← 可以处理 FileRetriever 和 HttpRetriever 的 RawResource
```

增加来源不应复制 Markdown 解析逻辑。

### Phase 2：全文搜索与上下文展开

任务：

- 建立全文索引；
- `search_document`；
- snippet；
- location mapping；
- `get_context`；
- search unit 与 read unit 分离。

验收：

```text
search "virtual memory"
→ hit paragraph/chunk
→ get_context
→ read owning section
```

结果位置一致。

架构验收：

- `Section` 仍是阅读结构；
- `Chunk` 只属于索引/检索技术结构；
- Search Result 必须能映射回 Section + Location；
- `search_document` 不承担长正文读取职责；
- 更换 SearchIndex 实现不需要修改 Parser。

未来精确阅读增量把 `Section + Location` handoff 演进为 version-bound `TextLocator`，但仍保持 Search ≠ Read。

### Phase 3：HTML

任务：

- Content-Type routing；
- HTML 正文提取；
- heading hierarchy；
- anchor/location；
- 忽略 nav/footer/script/style 等噪声；
- canonical/final URL 记录。

验收：

- 常见静态技术文档页能得到合理 TOC；
- 搜索和章节读取与 Markdown 使用相同 Tool；
- 增加 HtmlParser 不修改 MCP Tool schema；
- HtmlParser 不包含 HTTP 获取逻辑。

### Phase 4：PDF

任务：

- PDF native text extraction；
- page mapping；
- TOC/bookmark 提取（存在时）；
- 无 TOC 时先降级为稳定的 page-level Sections，不在 MVP 中猜测章节层级；
- section 到 page/native location 映射；
- 对单页解压文本设置资源上限；
- 无可提取文本时明确返回 OCR unsupported 错误。

验收：

- PDF 搜索结果带页码；
- 有目录的教材可以按章节导航和递归读取；
- 无目录的 PDF 仍可通过 `section://page-N` 稳定定位和读取；
- 页范围通过标准 `Location.page` / `native_location` 表达，不新增 PDF 专属 MCP Tool；
- PdfParser 与 HttpRetriever/FileRetriever 正交；
- PDF 特定 page/native location 不污染其他 Parser、Application 或 MCP 接口。

设计取舍：

> MVP 优先使用 PDF 自带目录作为作者定义的阅读结构。无目录时不通过不稳定的字号/版式 heuristic 冒充章节结构，而是先保持 page-level 可定位性。只有真实教材 fixture 证明有必要时，再把 heading heuristic 作为 PdfParser 内部可替换策略演进。

同时不新增 `read_pdf_page_range` 之类格式专属 Tool。PDF 页码属于 `Location` 的来源定位信息，读取仍统一通过 `read_document` / Section 语义完成，避免 PDF 概念泄漏到 MCP/Application 契约。

不在 MVP 强求 OCR。扫描 PDF 先返回明确 unsupported/insufficient-text 错误。

### Phase 5：安全与缓存强化

任务：

- URL scheme policy；
- DNS/IP 校验；
- redirect 每跳校验；
- response size limit；
- timeout；
- concurrency limit；
- ETag/Last-Modified；
- content hash；
- raw/parsed/index cache；
- cache invalidation。

验收：

- localhost 请求被拒绝；
- public URL redirect 到私网仍被拒绝；
- 相同未变化文档第二次打开不重复解析；
- SSRF 规则可以独立单测；
- HTTP 客户端不散落重复的安全判断；
- Storage、Cache、Index 三者职责保持独立。

### Phase 6：真实 Agent 验证

至少准备 4 类测试资料：

1. Markdown 技术文档；
2. HTML 文档站页面；
3. 带 TOC 的技术 PDF；
4. 长教材章节。

让 Agent 实际执行当前 6-Tool 粗粒度流程：

```text
list_documents（本地发现时可选）
→ open
→ structure
→ search
→ context
→ read
→ answer with source location
```

记录：

- Tool 调用次数；
- 返回字符数；
- 是否命中正确章节；
- 是否能追溯来源；
- 是否存在无意义重复读取；
- 是否为了某种格式出现特殊 MCP 调用路径；
- 是否出现搜索接口代替读取接口的趋势。

这一步验证的是当前 coarse Section workflow，不等同于未来 Paragraph/Sentence complete-reading acceptance。

---

## 4. MVP 暂不实现

明确延后：

- 精确 Paragraph/Sentence TextUnit 枚举（已由后续设计接受，尚未实现）；
- ReadCursor / TextUnitCursor；
- OCR；
- Playwright/browser rendering；
- JavaScript-heavy 网站；
- Confluence/Notion/飞书/语雀；
- 向量数据库；
- embedding；
- reranker；
- cross-document semantic search；
- AI summarization；
- AI Q&A；
- note generation。

EPUB 与 DOCX 已在当前 runtime 的格式扩展阶段实现；其精确结构可靠性与 TextUnit capability 仍属于后续增量，不应继续出现在“格式完全未实现”的旧列表中。

这些能力即使未来加入，也必须先通过 Actor Goal → Use Case → Capability → Tool Contract 与“变化原因和关注点”判断：属于 Reading MCP 内核还是上层产品。

---

## 5. 测试策略

### Architecture tests

优先验证边界而不是只验证功能结果：

- MCP 不依赖具体 Retriever/Parser 实现；
- Parser 不依赖 MCP；
- Retriever 不依赖 Parser；
- application 不依赖具体第三方基础设施库；
- Source 和 Format 组合不产生专用 Reader 类；
- Index 可以替换；
- AI SDK 不进入核心依赖。

如果语言/工具链支持，使用自动化架构测试或依赖检查；否则至少通过模块可见性和 CI lint 强制执行。

### Domain tests

验证：

- section tree；
- stable ids；
- location range；
- context expansion。

未来精确阅读还必须验证 normalized range、TextUnit ownership/order、locator identity 和 cursor stream invariants。

### Parser fixture tests

每种 Parser 准备固定 fixture，输出 snapshot/expected model。EPUB 精确能力必须覆盖 native nav/NCX/heading/spine fallback、unresolved target、non-prose 和 coverage。

### Security tests

必须覆盖：

- localhost；
- IPv4 private；
- IPv6 private/link-local；
- metadata endpoint；
- redirect to private；
- too many redirects；
- oversized response；
- timeout。

### Integration tests

通过本地 test server 模拟 HTTP，不依赖公网稳定性。

### MCP contract tests

确保：

- schema 稳定；
- error code 稳定；
- max response size 生效；
- Tool 不泄露内部凭据；
- 增加 Parser/Retriever 不需要改变已有 Tool 契约；
- current Tool discovery 与真实 runtime 数量一致；
- future additive fields/Tool 不破坏 legacy calls。

---

## 6. MVP 成功标准

当前 MVP 可以被认为成功，需要同时满足：

```text
[ ] 当前支持格式统一打开
[ ] 授权本地文档可发现
[ ] 结构导航可用
[ ] 全文搜索可用
[ ] 按章节/位置读取可用
[ ] Section 上下文展开可用
[ ] 来源位置稳定
[ ] 缓存生效
[ ] SSRF 默认阻断
[ ] MCP 层与解析实现解耦
[ ] Retrieval 与 Parsing 解耦
[ ] Source 与 Format 正交组合
[ ] Search 与 Read 分离
[ ] Section 与 Chunk 分离
[ ] Document 与 Index 分离
[ ] Storage / Cache / Index 职责分离
[ ] Security Policy 与 HTTP 实现分离
[ ] 没有 LLM 依赖
[ ] 真实 Agent 能完成一次 coarse Section 教材辅助阅读
```

未来 precise-reading 成功标准另行增加：

```text
[ ] truncated read 可继续并无 gap/overlap
[ ] Paragraph/Sentence-first stream 可完整消费 Section
[ ] SearchHit 可直接 handoff 到 read/context
[ ] stale locator/cursor fail closed
[ ] non-prose 可读且不伪造 Sentence
[ ] EPUB degradation/coverage 可判定
```

---

## 7. 后续优先级

MVP 完成后不要因为格式或 Tool 数量本身扩展系统，先按真实 Use Case 和依赖顺序推进。

优先判断：

1. 当前 HTML/PDF/EPUB 结构识别的 reliability/coverage 是否足够准确；
2. Section read continuation 是否先完成 gap/overlap 闭环；
3. normalized document identity/range 是否稳定；
4. Paragraph/Sentence TextUnit 是否能从 canonical persisted state 确定性重建；
5. Agent 最常用哪个 Tool，哪些响应最浪费 Token；
6. SearchHit→read/context 是否存在多余 round-trip；
7. 新需求是否导致多个正交模块同时修改；
8. 是否出现职责泄漏、“大 Reader”或为保持 Tool 数量而塞入参数组合的趋势。

只有数据证明需要时，再引入 embedding、browser、更多格式或新的 Tool。

原则：

> 先把“读准一份文档”做好，再考虑“搜索所有知识”。

同时坚持：

> 如果扩展一个能力需要同时修改来源层、解析层、搜索层和 MCP 层，优先检查架构边界，而不是继续堆条件分支。
