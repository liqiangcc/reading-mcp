# Reading MCP 架构设计

## 1. 设计目标

Reading MCP 以稳定边界为优先：

```text
来源获取 ≠ 格式解析
格式解析 ≠ canonical normalization
canonical facts ≠ derived TextUnit/Search index
搜索 ≠ 阅读
定位 ≠ cursor
MCP transport ≠ application logic
AI reasoning ≠ Reading MCP
```

Tool Contract 的设计顺序：

```text
Actor Goal → Use Case → Capability / State Machine → Tool
```

相关稳定决策：`docs/adr/0002-text-index-locator-identity.md`、`docs/adr/0003-epub-first-structure-reliability.md`、`docs/adr/0004-use-case-first-tool-contracts.md`。

---

## 2. 总体依赖

```text
MCP Adapter
    ↓
Application Use Cases
    ↓
Domain + Ports
    ↑
Retrieval / Parsing / Security / Infrastructure
```

依赖方向：

```text
mcp → application → domain
infrastructure/retrieval/parsing/security → application/domain ports
```

禁止 parser/retriever/index 依赖 rmcp DTO。

---

## 3. Source Truth 与 Derived State

```text
DocumentRepository
→ canonical normalized Document / Section facts

TextUnitIndex
→ rebuildable Paragraph derived state

SearchIndex
→ rebuildable lexical derived state
```

当前 source truth：

```text
Document
└── recursive Section
    ├── structural identity / parentage / order
    ├── exact Section.content
    └── native/legacy Location provenance
```

Paragraph/Sentence、FTS rows、snippets、MCP rendering、cursor 都不能替代 canonical Document。

---

## 4. 当前 Application / Tool Surface

当前 8 个 MCP Tool：

```text
list_documents
list_directory
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

Application Use Cases：

```text
ListDocumentsUseCase
ListDirectoryUseCase
OpenDocumentUseCase
GetDocumentStructureUseCase
GetTextUnitsUseCase
SearchDocumentUseCase
GetContextUseCase
ReadDocumentUseCase
```

没有格式专属 Tool，也没有 `get_sentences` / `get_paragraphs`。

---

## 5. Retriever / Security / Parser

### Retriever

```text
RetrieverRouter
├── LimitedFileRetriever
└── RevalidatingHttpRetriever
```

概念输出：

```text
RetrievedResource
├── final source
├── media type
├── bytes
├── etag / last_modified
└── metadata
```

### Security

职责包括：

- scheme/host/IP policy；
- DNS 与每跳 redirect validation；
- endpoint pinning；
- canonical local-root allowlist；
- credential profile/host isolation；
- body/archive/parser/document budgets。

### Parser

```text
ParserRouter
├── Text
├── Markdown
├── HTML/XHTML
├── PDF
├── EPUB
├── DOCX
└── OpenAPI/Swagger
```

Parser 输出 canonical `Document / Section / Location`。EPUB 结构可靠性独立遵循 ADR 0003。

---

## 6. Normalized Identity 与 Range

Raw：

```text
content_hash
= retrieved source bytes provenance
```

Precise source identity：

```text
normalized_document_hash
= addressing-relevant persisted Document/Section fingerprint
```

当前 normalized range：

```text
owner    = exact persisted Section.content
base     = zero
interval = half-open [start, end)
unit     = Unicode scalar
space    = section-content-unicode-scalar/v1
```

Legacy `Location.char_start/char_end` 不被静默解释为 normalized range。

---

## 7. TextUnit Foundation

### Paragraph

```text
Section.content
  ↓ text-segmentation/v2
Paragraph TextUnit
  ├── owner_section_id
  ├── paragraph_index
  ├── source_order
  ├── exact NormalizedTextRange
  ├── exact text slice
  └── deterministic TextUnitId
```

Paragraph 当前可持久化到独立 TextUnitIndex。

### Sentence

```text
Paragraph
  ↓ deterministic eligibility / segmentation
SentenceTextUnit
  ├── paragraph_index
  ├── sentence_index
  ├── parent Paragraph
  ├── exact Section-relative range
  └── deterministic identity
```

Recognized code/table/non-prose 不生成 fake Sentence；以 coarse Paragraph coverage 表示。

Sentence persistence 当前不是正确性依赖。

---

## 8. Canonical TextLocator 与 Shared Resolver

```text
TextLocator
├── document_id
├── content_hash
├── normalized_document_hash
├── owner_section_id
├── section_path
├── paragraph_index?
├── sentence_index?
├── normalized_range?
├── segmentation_version?
└── native_location?
```

Application 共用：

```text
resolve_text_locator(Document, TextLocator)
```

统一验证：

```text
document/raw/normalized identity
owner Section
locator shape
Paragraph/Sentence ordinal
segmentation version
normalized range equality
```

返回合法 kind：

```text
Section | CharacterRange | Paragraph | Sentence
```

Capability 再决定是否支持该 kind：

- exact read：全部四种；
- context：Section / Paragraph / Sentence；
- SearchHit：当前 Section / Paragraph / Sentence candidates。

禁止 fuzzy relocation。

---

## 9. Ordered TextUnit Enumeration

```text
get_text_units(section)
→ Paragraph or Sentence-first stream
→ source-ordered pages
→ TextLocator per item
→ TextUnitCursor
→ completion + coverage
```

`get_document_structure` 不承担 TextUnit enumeration。

`preserve_source` 遇到 non-prose 返回 coarse Paragraph；`eligible_only` 不宣称 all-source completion。

当前 v1 从 Section 边界开始；anchor-based `before/after(locator)` 仍是后续独立 extension。

---

## 10. Read State Machines

### Legacy Section tree

```text
section_id
→ SectionTreeReadStream/v1
→ selected Section + descendants
→ read-cursor/v2 when bounded
```

Rendered stream offset 不是 source range。

### Exact target

```text
target_locator
→ exact_target / exact-normalized-source/v1
→ Section.content | Paragraph | Sentence | CharacterRange
```

响应区分：

```text
resolved_target_locator = logical target
returned_locator        = exact source CharacterRange for this segment
ReadCursor              = target-stream progress
```

---

## 11. Context Semantics

```text
neighbor(section | paragraph | sentence)
container(paragraph | section)
structural(owner_section | ancestors | siblings | children)
```

Legacy `section_id + before/after` 只等价于 `neighbor(section)`。

Context 是 bounded expansion around known anchor，不承担完整 stream continuation。

---

## 12. Lexical Search Architecture

### Candidate source

InMemory 与 SQLite 使用同一个 canonical candidate builder：

```text
Section title
→ Section candidate + Section TextLocator

Paragraph TextUnit
→ Paragraph candidate + Paragraph TextLocator

eligible SentenceTextUnit
→ Sentence candidate + Sentence TextLocator
```

Non-prose Paragraph 可检索，但不会伪造 Sentence candidate。

### Version separation

```text
normalized_document_hash/v2 + text-segmentation/v2
→ TextUnit / TextLocator identity

lexical-tokenizer/v1
→ lexical projection / query matching
```

Tokenizer 变化只触发 SearchIndex rebuild，不改变 source identity。

### Tokenizer v1

Deterministic、non-LLM：

- technical identifiers：完整 normalized token + components；
- Han/Hiragana/Katakana/Hangul：unigram + adjacent bigram；
- mixed technical text：组合上述规则。

### SQLite v2

```text
lexical-search-index/v3
```

FTS row 保存：

```text
candidate_kind
canonical TextLocator
tokenizer_version
source_order
encoded lexical terms
preview metadata
```

只有 encoded terms 进入 FTS tokenizer，从而避免 SQLite 重新定义 CJK/technical token boundary。

Index/tokenizer version 不兼容时，仅重建 lexical derived state。

如果 canonical Document 存在但 lexical index 缺失：

```text
search_document
→ load canonical Document
→ rebuild SearchIndex from Document
→ retry query
```

不重新 retrieve/reparse source。

Historical SQLite search adapter 仅通过 hidden compatibility alias 保留；runtime `SqliteSearchIndex` 指向 lexical v2。

---

## 13. Search Handoff

当前完整链路：

```text
search_document
→ SearchHit(candidate_kind + TextLocator)
        ├→ read_document(target_locator)
        └→ get_context(target_locator, relation)
```

SearchDocumentUseCase 对 index hit 做 canonical validation：

```text
source consistency
tokenizer version
shared locator resolution
candidate_kind == resolved locator kind
owner Section consistency
```

Search snippet/score/legacy `Location` 是 preview/provenance，不是 source identity。

---

## 14. Cursor Taxonomy

```text
TextLocator    = source address
ReadCursor     = read-stream progress
TextUnitCursor = enumeration-stream progress
```

未来若引入 DiscoveryCursor / StructureCursor / SearchCursor，也必须绑定自己的 stream contract，不能复用 TextLocator 语义。

---

## 15. Persistent State

```text
RawResourceCache
ParsedDocumentCache
DocumentRepository
TextUnitIndex            # Paragraph derived
SearchIndex              # lexical-search-index/v3
```

物理上可以共享 SQLite 文件，逻辑 ports/事实语义必须独立。

Parsed Cache identity：

```text
final_source + raw hash + normalization_version
```

---

## 16. Reliability / Coverage

精确能力需要显式 provenance/degradation/coverage，而不是单一 parse success。

TextUnit coverage 至少区分：

- eligible prose represented by Paragraph/Sentence；
- coarse non-prose；
- intentionally skipped；
- unsupported gaps。

EPUB structure coverage/provenance 继续由 ADR 0003 约束。

在没有独立 Use Case 前，不增加单独 reliability-inspection Tool。

---

## 17. 错误与 Fail-Closed

稳定错误包括：

```text
INVALID_LOCATOR
STALE_LOCATOR
INVALID_CURSOR
STALE_CURSOR
CURSOR_TARGET_MISMATCH
RESOURCE_LIMIT_EXCEEDED
INDEX_FAILED
```

规则：

- locator/cursor identity mismatch fail closed；
- SearchIndex inconsistent locator fails as index error；
- re-open/source refresh 是显式 workflow；
- derived lexical-state rebuild 可以只从 canonical persisted Document 完成；
- 不根据 snippet/相似文本自动修复 source identity。

---

## 18. 核心设计原则

```text
Actor goal ≠ Tool call success
Use Case precedes Tool
Document acquisition ≠ parsing
Parsing ≠ indexing
Indexing ≠ reading
Reading ≠ reasoning
StructuralNode ≠ TextUnit
TextUnit ≠ lexical row
Search result ≠ source truth
TextLocator ≠ Cursor
Segmentation version ≠ Tokenizer version
ReadCursor ≠ TextUnitCursor
Neighbor ≠ Container ≠ Structural context
Fallback ≠ native precision
```

保持这些边界后，后续 ranking、EPUB reliability、anchor-based enumeration 或新的 Retriever/Parser 都可以独立演进，而不重新定义 source identity。
