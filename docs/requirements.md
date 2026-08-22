# Reading MCP v0.1 需求

## 目标

Reading MCP 是面向 AI/MCP Client 的统一文档阅读上下文基础设施。它负责获取、解析、结构化、定位、搜索、有序 TextUnit 枚举、精确读取、上下文展开、缓存与持久化；总结、问答、推理、教学、笔记和通用 RAG 继续由上层 AI 完成。

核心流程：

```text
list_documents（可选）
→ open_document
→ get_document_structure
→ [get_text_units | search_document]
→ get_context / read_document
```

Tool 调用成功不等于阅读任务完成。长 read/TextUnit stream 必须有明确 completion/continuation，并验证 no-gap/no-overlap。

## 来源与格式

v0.1 来源：

- 公共 HTTPS；
- 显式授权 local roots 内的本地文件。

安全默认：HTTPS-only、本地文件 default-deny；`READING_MCP_ALLOW_HTTP=true` 才允许明文 HTTP。

独立 Parser：

- Plain Text；
- Markdown；
- HTML/XHTML；
- 原生文本 PDF；
- EPUB；
- DOCX；
- OpenAPI / Swagger JSON/YAML。

GitHub README/Wiki、Javadoc、MkDocs/Docusaurus/GitBook 静态输出复用 Markdown/HTML，不创建品牌专属 Parser。

## MCP Tools

当前 runtime 实际暴露 7 个 Tool：

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

格式扩展不得增加格式专属 Tool。

### `get_document_structure`

只暴露 structural Section tree，不把 Paragraph/Sentence 塞进 TOC。

### `get_text_units`

独立承担 `OrderedTextUnitEnumeration`：

```text
one Section.content
→ paragraph | sentence-first items
→ source-order pagination
→ TextLocator per item
→ TextUnitCursor
→ completion + coverage
```

当前 v1 支持 Section-boundary start、forward/backward、cursor continuation 和
exact `anchor_locator` continuation。anchor 是独立 TextLocator 语义，不会被解释成
nearest-text relocation。

### `get_context`

Legacy：

```text
document_id + section_id + before/after
→ neighbor(unit=section)
```

Structured：

```text
document_id + (section_id | target_locator)
relation =
  neighbor(section | paragraph | sentence)
  | container(paragraph | section)
  | structural(owner_section | ancestors | siblings | children)
```

Paragraph/Sentence context 必须从 canonical TextLocator 解析，不通过标题/snippet 二次搜索。

### `read_document`

Legacy：

```text
document_id + section_id + max_chars? + cursor?
→ SectionTreeReadStream/v1
→ selected Section + descendants
```

Precise：

```text
document_id + target_locator + max_chars? + cursor?
→ exact_target
```

支持：

```text
Section locator        → exact Section.content
Paragraph locator      → exact Paragraph slice
Sentence locator       → exact Sentence slice
CharacterRange locator → exact Section-relative normalized range
```

Oversized exact target 使用 version-bound ReadCursor continuation。

### `search_document`

请求保持：

```text
document_id
query
limit
```

SearchHit 保留：

```text
section_id
title
source
snippet
score
location
```

并返回：

```text
candidate_kind: section | paragraph | sentence
text_locator: TextLocator
```

当前 runtime 已建立 canonical lexical candidates：

```text
Section title → Section TextLocator
Paragraph     → canonical Paragraph TextLocator
Sentence      → canonical Sentence TextLocator
```

Title-only Section 必须保留 Section-level identity。Recognized non-prose 可作为 Paragraph candidate，但不得伪造 Sentence candidate。

搜索结果可直接 handoff：

```text
SearchHit.text_locator ─┬→ read_document
                        └→ get_context
```

Snippet / legacy `location` 只作为 preview/provenance，不能充当 canonical identity。

## 文档模型、TextUnit 与索引

所有格式统一为 `Document / Section / Location`；Paragraph/Sentence 从 persisted canonical `Section.content` 确定性派生。

```text
DocumentRepository = canonical normalized facts
TextUnitIndex       = rebuildable derived state（当前持久化 Paragraph）
SearchIndex         = rebuildable lexical retrieval state

StructuralNode ≠ TextUnit
TextUnit       ≠ Search row
Search Unit    ≠ Read stream
Index          ≠ Document
```

Sentence persistence 当前不是正确性依赖；Sentence enumeration/context/read/search identity 都可以从 canonical Document + deterministic segmentation 重建。

## Canonical TextLocator

统一模型：

```text
TextLocator
├── document_id
├── content_hash                 # raw-source provenance
├── normalized_document_hash
├── owner_section_id
├── section_path
├── paragraph_index?
├── sentence_index?
├── normalized_range?
├── segmentation_version?
└── native_location?
```

Normalized range：

```text
owner    = exact persisted Section.content
base     = zero
interval = half-open [start, end)
unit     = Unicode scalar / Rust char
space    = section-content-unicode-scalar/v1
```

Legacy `Location.char_start/char_end` 保持 parser-defined 语义，不被静默解释成 normalized range。

Locator-consuming application paths共用 shared resolver，统一验证：

```text
document_id
raw content_hash
normalized_document_hash
owner Section
Section / CharacterRange / Paragraph / Sentence shape
Paragraph/Sentence ordinal
segmentation version
normalized range equality
```

Identity failure：

```text
INVALID_LOCATOR
STALE_LOCATOR
```

不得 fuzzy rebase。

Capability support 与 locator validity 分开：例如 CharacterRange 是合法 locator，但当前 context 不接受 CharacterRange anchor。

## Cursor 与 source identity

```text
TextLocator    = canonical source address
ReadCursor     = progress through one read stream
TextUnitCursor = progress through one enumeration stream
```

Cursor/stream offset 不得作为 citation/source range。

Exact read 每个 segment 区分：

```text
resolved_target_locator = logical target
returned_locator        = exact CharacterRange represented by this segment
```

并满足：

```text
content == owner_section.normalized_text_slice(returned_locator.normalized_range)
```

## Lexical TextUnit index

当前精确 lexical contract：

```text
lexical-search-index/v3
lexical-tokenizer/v1
```

版本职责严格分离：

```text
normalized_document_hash/v2 + text-segmentation/v2
→ TextUnit / TextLocator identity

lexical-tokenizer/v1
→ lexical projection / matching / rebuild
```

Tokenizer 变化不得改变 Paragraph/Sentence ordinals 或 TextLocator。

### Tokenizer v1

Deterministic、non-LLM：

- Latin/technical identifiers：保留完整 normalized token + alphanumeric/underscore components；
- Han/Hiragana/Katakana/Hangul：字符 unigram + adjacent bigram；
- mixed technical text 同时产生各自稳定 lexical terms。

例如：

```text
read-cursor/v2
→ read-cursor/v2 + read + cursor + v2

虚拟内存机制
→ 虚 / 拟 / 内 / 存 / 机 / 制
  + 虚拟 / 拟内 / 内存 / 存机 / 机制
```

InMemory 与 SQLite 必须共用同一 candidate builder/tokenizer policy，不允许独立维护两套 Paragraph/Sentence split。

### SQLite v2

持久化 derived lexical rows 包含：

```text
candidate_kind
canonical TextLocator
tokenizer_version
source_order
encoded lexemes
preview metadata
```

SQLite FTS 只索引 encoded lexemes，避免 SQLite 自身 tokenizer 重新解释 CJK/技术标点边界。

Index/tokenizer version 不兼容时，只清理/重建 lexical derived state，不触碰 DocumentRepository/TextUnit source facts。

如果 persisted canonical Document 存在但 lexical state 缺失，`search_document` 可从该 Document 重建 SearchIndex 后重试；禁止为此重新下载或 reparse 来源。

历史 SQLite search adapter 仅保留隐藏 compatibility alias；runtime `SqliteSearchIndex` 使用 lexical v3。

## TextUnit completion / non-prose

`preserve_source` 是默认完整阅读策略。Sentence-first 遇到已识别 code/table non-prose 时返回 coarse Paragraph item，不伪造 Sentence。

`eligible_only` 只承诺 eligible stream completion，不得宣称 all-source completion。

TextUnit 在 enumeration/context 中是原子项；不得截断后继续沿用原 locator identity。Exact read 可以分页，但每个 page 必须返回新的 truthful `returned_locator`。

## 安全与资源

必须具备：

- SSRF scheme/hostname/DNS/IP 检查；
- 每跳 redirect 重新校验并 resolve；
- endpoint pinning；
- URL embedded credential 拒绝；
- HTTP timeout/redirect/concurrency/body limits；
- Content-Type allowlist；
- canonical local root allowlist + file size limit；
- PDF 页数/解压预算；
- EPUB/DOCX archive budgets；
- Parser timeout；
- normalized chars/Section count/tree depth budgets。

所有 bounded read/enumeration stream 必须有 completion + actionable continuation 或明确 unsupported/degradation。

## 缓存与持久化

```text
RawResourceCache
ParsedDocumentCache
DocumentRepository
TextUnitIndex
SearchIndex
```

Parsed Cache 按：

```text
final_source + raw hash + normalization_version
```

隔离。规范化策略升级不得静默复用旧 Parsed Document。

默认状态目录 `~/.reading-mcp` 使用持久化 Raw/Parsed Cache、SQLite DocumentRepository、SQLite Paragraph TextUnitIndex 与 SQLite lexical-search-index/v3。`READING_MCP_STATE_DIR=memory` 切换纯内存模式。

Structure continuation 使用 `structure-cursor/v1` / `structure-preorder/v1`；discovery
continuation 使用 `discovery-cursor/v1`。整本正文组合额外使用
`body-order/v1`，并要求 structure complete、每个 body-owning Section 一次、每个
preserve-source stream complete 且 reliability 没有隐藏 unsupported gap。

## auth_profile 与 telemetry

模型只传 profile 名，不传 Secret。部署侧提供 credential + host allowlist，每跳 redirect 重新校验。

Telemetry 只写 stderr；不得记录正文、Bearer Token、Authorization/Cookie 或完整搜索词。Search telemetry 可记录 query 字符长度、limit、hit count 等结构信息。

## 非目标

v0.1 不包括：

- OCR / 扫描 PDF；
- JavaScript-heavy 浏览器渲染；
- 企业产品 API；
- OAuth/Cookie 交互登录；
- 公网多租户；
- 通用 Web crawler；
- AI 总结/问答/笔记；
- 通用向量/semantic RAG；
- LLM-defined tokenizer/source identity。

## Release Gate

必须通过：

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

验收范围包括：TextUnit continuation、non-prose coverage、locator context/read、exact continuation、canonical Section/Paragraph/Sentence lexical candidates、CJK/technical retrieval、SQLite reopen/rebuild、SearchHit→precise TextLocator→read/context、stale/malformed fail-closed、持久化重启、安全缓存、HTTP auth/SSRF、各格式 acceptance。

后续重点仍包括 EPUB provenance/degradation/coverage 增量，以及独立评审后确认是否需要 anchor-based `get_text_units before/after(locator)`。
