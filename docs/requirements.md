# Reading MCP v0.1 需求

## 目标

Reading MCP 是面向 AI/MCP Client 的统一文档阅读上下文基础设施。它负责获取、解析、结构化、定位、搜索、有序 TextUnit 枚举、按章节读取、上下文展开、缓存与持久化；总结、问答、推理、教学、笔记和通用 RAG 继续由上层 AI 完成。

当前核心流程：

```text
list_documents（可选，仅用于授权本地目录发现）
→ open_document
→ get_document_structure
→ [get_text_units | search_document]
→ get_context / read_document
```

Tool 成功不等于阅读任务成功。长 Section 的完整读取或 TextUnit 枚举必须能够在响应预算下继续，直到声明的目标流明确完成，并验证无 gap / overlap。精确阅读契约的目标与演进见 [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md)。

## 来源

v0.1 支持：

- 公共 HTTPS；
- 显式允许根目录内的本地文件。

安全默认：HTTPS-only；本地文件 default-deny。`READING_MCP_ALLOW_HTTP=true` 才允许明文 HTTP。

## 格式

独立 Parser：

- Plain Text；
- Markdown；
- HTML/XHTML；
- 原生文本 PDF；
- EPUB；
- DOCX；
- OpenAPI / Swagger JSON/YAML。

GitHub README/Wiki、Javadoc、MkDocs/Docusaurus/GitBook 静态输出直接复用 Markdown/HTML，不创建品牌专属 Parser。

## MCP Tools

当前运行时实际暴露 7 个 Tool：

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

`list_documents` 只发现部署者显式授权本地目录中的候选文档，不打开或解析文档。格式扩展不得增加格式专属 Tool。

`get_document_structure` 继续只暴露 structural Section tree；不得把 Paragraph/Sentence 塞进 TOC 树。

`get_text_units` 实现独立 `OrderedTextUnitEnumeration` 职责：在一个 Section 自身的 canonical content 中按 source order 分页枚举 Paragraph 或 Sentence-first reading items，返回精确 TextLocator、完成状态、TextUnitCursor 和 non-prose/coverage 语义。不得拆成 `get_sentences`、`get_paragraphs` 或格式专属 Tool。

当前 v1 支持 Section-boundary start、forward/backward 和 cursor continuation。anchor-based `before/after(locator)` 起点属于后续 locator/context handoff，不影响当前完整 Section 枚举闭环。

## 文档模型、TextUnit 与搜索

所有格式统一为 `Document / Section / Location`；精确阅读在 canonical Section 上确定性派生 Paragraph/Sentence TextUnit。

```text
DocumentRepository = 规范化事实来源
TextUnitIndex       = 可重建派生状态（当前持久化 Paragraph）
SearchIndex         = 可重建检索派生状态
Search Unit         ≠ Read Unit
StructuralNode      ≠ TextUnit
```

Sentence locator/enumeration 当前无需 Sentence SQLite row：同一个 persisted canonical Document + `text-segmentation/v1` 必须确定性重建相同 Sentence stream。Sentence persistence 只有在性能证据需要时才能作为 derived optimization 引入。

搜索结果当前必须映射回 owning Section，并返回 source/title/snippet/score/location。未来精确搜索结果还必须返回可直接交给 read/context 的 version-bound `TextLocator`，不能要求 Agent 复制 snippet 再搜索一次。

## 可追溯性

Tool 结果必须尽可能保留：

```text
document_id
source
content_hash
normalized_document_hash
normalized_document_hash_version / normalization_version
section_id / parent_id / title
page / chapter / section_path
paragraph / sentence / normalized range（能力可用时）
anchor / native_location / provenance
```

当前 normalized range 基础契约：

```text
owner    = exact persisted Section.content
base     = zero
interval = half-open [start, end)
unit     = Unicode scalar / Rust char
space    = section-content-unicode-scalar/v1
```

`Location.char_start/char_end` 继续保持 parser-defined legacy/source semantics。它们不是 normalized range。正式定义见 [Normalized Document Identity and Text Range Contract](normalized-text-range-contract.md)。

`get_text_units` 当前返回 canonical `TextLocator`：Paragraph/Sentence ordinal 与 normalized range 绑定 `normalized_document_hash + segmentation_version`。

`TextLocator` 是 canonical source address；`ReadCursor`、`TextUnitCursor` 等 cursor 只是特定 versioned stream 的进度，不能作为引用位置。

## TextUnit completion / coverage

`preserve_source` 是默认完整阅读策略：Sentence-first 遇到已识别 code/table non-prose 时返回显式 coarse Paragraph item，而不是伪造 Sentence。

`eligible_only` 只表示 eligible stream 消费；按契约即使当前 Section 恰好全部是 prose，也不得宣称 all-source `source_complete/section_complete`。

TextUnitCursor 至少绑定：

```text
raw + normalized document identity
owner Section
segmentation version
requested kind
direction
coverage policy
next stream index
stream length
cursor schema
```

Incomplete TextUnit response 必须提供 `next_cursor`；terminal response 必须 `complete=true` 且 `next_cursor=null`。Cursor mismatch/stale 必须 fail closed。

## 安全与资源

必须具备：

- SSRF scheme/hostname/DNS/IP 检查；
- 每次 redirect 重新校验并重新 DNS resolve；
- 请求 endpoint pinning，禁用环境/system proxy；
- URL 内嵌 credential 拒绝；
- HTTP timeout/redirect/concurrency/body limit；
- Content-Type allowlist；
- local root canonical allowlist + 文件大小限制；
- PDF 总页数与单页解压限制；
- EPUB/DOCX ZIP entry/单 entry/总解压预算；
- Parser timeout；
- Normalized Document 字符数、section 数和树深度限制。

所有 bounded response 必须明确 complete/truncated 状态；只要目标尚未完成，就必须提供可操作 continuation 或明确 unsupported/degradation，不能只返回不可继续的 `truncated=true`。

TextUnit 单元在 enumeration 中是原子项；不得为了 `max_chars` 截断 Sentence/Paragraph 后继续沿用原 locator 身份。单个 item 超出预算时必须显式返回资源限制。

## 缓存与持久化

缓存/派生状态分层保持独立：

```text
RawResourceCache
ParsedDocumentCache
DocumentRepository
TextUnitIndex
SearchIndex
```

HTTP 保存 ETag/Last-Modified，并使用 `If-None-Match` / `If-Modified-Since` 条件重验证；304 复用缓存；`force_refresh=true` 重新获取来源。

Parsed Cache 必须按 `final_source + raw hash + normalization_version` 隔离。规范化策略升级可以复用未变化的 Raw Cache，但不得静默复用旧 Parsed Document。

默认状态目录为 `~/.reading-mcp`，使用持久化 Raw/Parsed Cache、SQLite DocumentRepository、SQLite Paragraph TextUnitIndex 和 SQLite FTS5 SearchIndex。设置 `READING_MCP_STATE_DIR=memory` 可切换纯内存模式。

## auth_profile

模型只传 profile 名，不传 Secret。部署侧通过环境变量提供 Bearer Token 与 host allowlist；每次 redirect 都重新执行 profile→host 校验，认证 Raw Cache 按 profile 隔离。

## 错误与可观察性

MCP 错误必须提供稳定 `code + retryable`。stale locator/cursor 必须显式 fail closed；禁止将旧 locator 偷偷映射到新版本中“最相似”的句子。Telemetry 只写 stderr，不得记录文档正文、Bearer Token、Authorization/Cookie 或完整搜索词。

## 非目标

v0.1 不包括：

- OCR / 扫描 PDF；
- JavaScript-heavy 浏览器渲染；
- Confluence/Notion/飞书/语雀等产品 API；
- OAuth/Cookie 交互登录；
- 公网多租户服务；
- 通用 Web crawler；
- AI 总结/问答/笔记；
- 通用向量 RAG。

## Release Gate

必须通过：

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

测试范围包括架构边界、真实 stdio MCP E2E、TextUnit forward/backward continuation gap/overlap、non-prose/eligible-only coverage、cursor stale/mismatch、持久化重启、HTTP 条件重验证、auth redirect isolation、资源预算、SQLite FTS、Text/Markdown/HTML/PDF acceptance，以及 EPUB/DOCX/OpenAPI 解析。

后续精确阅读增量还必须增加：TextLocator→read/context、SearchHit→TextLocator、stale locator、EPUB provenance/degradation/coverage 和旧客户端兼容性测试。
