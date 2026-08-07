# Reading MCP MVP 实施计划

## 1. MVP 目标

MVP 只验证一个核心闭环：

> AI 能否通过统一 MCP 接口，安全地打开不同格式文档，查看结构，搜索内容，精确读取相关章节，并获得稳定来源定位。

MVP 不追求支持所有文档格式，也不做 AI 总结、向量数据库或浏览器自动化。

---

## 2. MVP 能力范围

### 输入格式

- HTML
- Markdown
- Plain Text
- PDF

### MCP Tools

```text
open_document
get_document_structure
search_document
read_document
get_context
```

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

### Phase 0：工程骨架与契约

目标：先稳定边界，不急着实现所有解析器。

任务：

- 初始化项目；
- 定义配置系统；
- 定义 `Document` / `Section` / `Location`；
- 定义 Retriever interface；
- 定义 Parser interface；
- 定义 Cache interface；
- 定义 SearchIndex interface；
- 定义 5 个 MCP Tool request/response schema；
- 建立单元测试和 CI。

验收：

- application 层可以用 fake Retriever/Parser 完成完整调用；
- MCP 层不依赖具体 HTTP/PDF 库。

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
→ hit paragraph
→ get_context
→ read owning section
```

结果位置一致。

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
- 搜索和章节读取与 Markdown 使用相同 Tool。

### Phase 4：PDF

任务：

- PDF text extraction；
- page mapping；
- TOC/bookmark 提取（存在时）；
- heading heuristic（无 TOC 时）；
- page range read；
- section 到 page location 映射。

验收：

- PDF 搜索结果带页码；
- `read_document` 可按 page range 读取；
- 有目录的教材可以按章节导航。

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
- 相同未变化文档第二次打开不重复解析。

### Phase 6：真实 Agent 验证

至少准备 4 类测试资料：

1. Markdown 技术文档；
2. HTML 文档站页面；
3. 带 TOC 的技术 PDF；
4. 长教材章节。

让 Agent 实际执行：

```text
open
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
- 是否存在无意义重复读取。

---

## 4. MVP 暂不实现

明确延后：

- EPUB；
- DOCX；
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

---

## 5. 测试策略

### Domain tests

验证：

- section tree；
- stable ids；
- location range；
- context expansion。

### Parser fixture tests

每种 Parser 准备固定 fixture，输出 snapshot/expected model。

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
- Tool 不泄露内部凭据。

---

## 6. MVP 成功标准

MVP 可以被认为成功，需要同时满足：

```text
[ ] 4 种基础格式统一打开
[ ] 结构导航可用
[ ] 全文搜索可用
[ ] 按章节/位置读取可用
[ ] 上下文展开可用
[ ] 来源位置稳定
[ ] 缓存生效
[ ] SSRF 默认阻断
[ ] MCP 层与解析实现解耦
[ ] 没有 LLM 依赖
[ ] 真实 Agent 能完成一次完整教材辅助阅读
```

---

## 7. 后续优先级

MVP 完成后不要立刻扩格式，先观察真实使用数据。

优先判断：

1. HTML/PDF 结构识别是否足够准确；
2. 全文检索是否真的不足；
3. Agent 最常用哪个 Tool；
4. 哪些响应最浪费 Token；
5. location 是否足够稳定；
6. 用户最需要 EPUB、DOCX 还是动态网页；
7. 是否存在真实的多文档搜索需求。

只有数据证明需要时，再引入 embedding、browser 或更多格式。

原则：

> 先把“读准一份文档”做好，再考虑“搜索所有知识”。
