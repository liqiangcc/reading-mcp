# Reading MCP 架构设计

## 1. 设计目标

Reading MCP 的架构重点是保持边界稳定：

```text
文档从哪里来
≠
文档是什么格式
≠
文档如何规范化/定位
≠
文档如何索引
≠
MCP 如何暴露能力
≠
AI 如何理解内容
```

因此系统按职责和变化原因拆分，而不是按“某一种格式的完整流程”或“尽量少的 Tool”拆分。

Tool Contract 的设计顺序必须遵守：

```text
Actor Goal → Use Case → Capability / State Machine → Tool
```

详细决策见 [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md)、[ADR 0004](adr/0004-use-case-first-tool-contracts.md)、[TextUnit Enumeration Contract](text-unit-enumeration-contract.md)、[Precise Read Locator Contract](precise-read-locator-contract.md) 与 [Search Locator Handoff Contract](search-locator-contract.md)。

---

## 2. 总体架构

```text
                         MCP Adapter
                              │
                    Application Use Cases
                              │
       ┌──────────────┬───────┼──────────┬──────────────┐
       │              │       │          │              │
  Source Policy   Retriever  Parser  Repositories   Derived Indexes
       │              │       │          │              │
       │              │       │     Document facts      │
       │              │       │          │         TextUnit/Search
       └──────────────┴───────┴──────────┴──────────────┘
                              │
                    Canonical Document/Section
                              │
          ┌───────────────────┼───────────────────┐
          │                   │                   │
   Structural facts     Native provenance   Reliability/Coverage
```

依赖方向：

```text
mcp → application → domain

retrieval/security/parsing/infrastructure
              ↓
      application/domain ports
```

`Document / Section` 是 canonical normalized facts；Paragraph/Sentence TextUnit 与 SearchIndex 是可重建派生状态。

---

## 3. 模块职责

### 3.1 MCP Adapter

职责：

- 定义 MCP Tools 和 structured schema；
- transport-level 参数解析；
- 调用 Application Use Case；
- 将内部错误映射为稳定 `code + retryable`；
- 保持 response budget、cursor 与 backward compatibility 的外部契约。

不负责：

- 下载 URL；
- 解析 PDF/EPUB；
- 直接查询 SQLite/FTS；
- 定义 Paragraph/Sentence segmentation；
- 通过文本相似度修复 stale locator；
- AI 总结或推理。

### 3.2 Application Use Cases

当前 runtime 已实现：

```text
ListDocumentsUseCase
OpenDocumentUseCase
GetDocumentStructureUseCase
GetTextUnitsUseCase
SearchDocumentUseCase
GetContextUseCase
ReadDocumentUseCase
```

对应当前 7 个 MCP Tool：

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

已经实现的 precise handoff：

```text
get_text_units ─→ TextLocator ─┬→ read_document
                               └→ get_context

search_document → SearchHit.text_locator ─┬→ read_document
                                         └→ get_context
```

`OrderedTextUnitEnumeration` 独立映射为 `get_text_units`；没有把 Paragraph/Sentence 枚举塞进 `read_document` 或 `get_context`。

当前仍待独立演进的能力主要包括：

```text
canonical Paragraph/Sentence lexical candidates
independently-versioned CJK/mixed technical tokenizer
anchor-based get_text_units before/after(locator)
Reliability / Coverage richer inspection
```

Application 只依赖抽象 ports，不直接依赖具体 PDF/HTTP/SQLite/MCP SDK 实现。

### 3.3 Document Discovery

`list_documents` 只枚举部署者显式授权本地目录中的候选来源：

- 不打开；
- 不解析；
- 不写入 DocumentRepository；
- 不建立 SearchIndex；
- 不假定公共 URL 也必须通过发现获得。

未来 bounded discovery 需要 completion/continuation，但其 cursor 仍不是 source locator。

### 3.4 Retriever

统一输入获取层：

```text
RetrieverRouter
├── LimitedFileRetriever
└── RevalidatingHttpRetriever
```

概念输出：

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

Retriever 不理解章节、Paragraph、Sentence 或 MCP DTO。

### 3.5 Security Policy

放在 Retriever 之前/内部的独立策略组件，而不是散落在 HTTP 代码里。

职责：

- scheme/host/IP policy；
- DNS 与每跳 redirect validation；
- local root canonical allowlist；
- credential profile/host isolation；
- body/archive/parser/document resource budgets；
- timeout/concurrency/content-type policy。

概念接口：

```text
SourcePolicy
NetworkTargetPolicy
ResourceLimitPolicy
CredentialPolicy
```

### 3.6 Parser / Normalizer

统一解析器接口：

```text
ParserRouter
├── TextParser
├── MarkdownParser
├── HtmlParser
├── PdfParser
├── EpubParser
├── DocxParser
└── OpenApiParser
```

输入是 `RetrievedResource`，输出 canonical `Document / Section / Location` facts。Parser 不关心 MCP、HTTP 缓存或 FTS 查询。

EPUB 精确结构必须区分 manifest、spine、navigation，保留 provenance/resolution/coverage；不能先扁平化再冒充 native structure。详见 ADR 0003。

### 3.7 Document Repository

保存 canonical normalized Document facts：

```text
DocumentRepository
└── Document / Section / addressing-relevant persisted facts
```

它是读取与定位事实来源。Search snippet、FTS row、rendered MCP response、Paragraph/Sentence rows都不能替代 canonical Document。

### 3.8 TextUnit、Locator 与 Enumeration

Paragraph/Sentence 是 deterministic、versioned、rebuildable TextUnits：

```text
Document / Section.content
          ↓
TextUnit segmentation policy
          ↓
Paragraph / Sentence TextUnits
```

Paragraph v1 持久化派生索引：

```text
text-segmentation/v1
Section.content
  → exact Paragraph NormalizedTextRange
  → deterministic Paragraph TextUnitId
  → Paragraph source_order
  → TextUnitIndex
```

Sentence locator foundation：

```text
Paragraph TextUnit
  → conservative persisted-text eligibility/classification
  → deterministic English/CJK Sentence segmentation
  → exact Section-relative Sentence NormalizedTextRange
  → paragraph_index + sentence_index + parent_paragraph_id
  → deterministic Sentence TextUnitId/source_order
  → per-Paragraph Sentence/coarse coverage
```

`get_text_units` 直接消费 canonical Document 的 deterministic Paragraph/Sentence materialization，并输出：

```text
TextLocator
TextUnitCursor
complete / section_complete
coverage
stream indices
```

`TextUnitIndex` 有独立 application port 与 InMemory / SQLite adapter，但持久化 schema 当前仍是 Paragraph-only。Sentence persistence 不是枚举、read、context 的正确性依赖；SQLite DocumentRepository reopen 后仍可重建相同 Sentence stream。

TextUnit identity 依赖 addressing-relevant normalized identity 与 segmentation version，不能依赖 SearchIndex row ID。

### 3.9 Shared TextLocator Resolver

Exact read 与 structured context 共用一个 application-level locator resolver：

```text
TextLocator
   ↓
resolve_text_locator(Document, locator)
   ↓
validated Section | CharacterRange | Paragraph | Sentence
```

统一负责：

```text
document_id
raw content_hash
normalized_document_hash
owner Section
locator shape
Paragraph/Sentence ordinal
segmentation version
normalized range equality
INVALID_LOCATOR / STALE_LOCATOR
```

Resolver 只判断 canonical identity。Capability 再决定是否接受某种合法 locator：

- exact read 接受 Section / CharacterRange / Paragraph / Sentence；
- current context 接受 Section / Paragraph / Sentence；
- CharacterRange 对 context 是 unsupported relation semantics，不是 malformed locator。

禁止每个 consumer 私有实现不同的 fuzzy/stale-repair 规则。

### 3.10 Search Index

SearchIndex 是 derived retrieval state，而不是 source identity：

```text
current Section title candidates
current paragraph-like retrieval rows
future canonical Paragraph candidates
future canonical Sentence candidates
        ↓
rank / snippet
```

当前 InMemory/SQLite SearchIndex 的 paragraph-like row 使用历史 retrieval split / legacy `Location`，没有 canonical `normalized_range + segmentation_version`，因此不能被解释成 Paragraph TextUnit。

当前 SearchDocumentUseCase 使用两阶段 handoff：

```text
SearchIndex.search(...)
      ↓
ranked bounded hits
      ↓
DocumentRepository.get(document_id)
      ↓
validate source + owning Section
      ↓
SearchHit {
  legacy preview fields,
  candidate_kind = section,
  text_locator = canonical Section TextLocator
}
```

所以当前 SearchHit 已能直接交给 read/context，但 precision 诚实停留在 Section。

Paragraph/Sentence candidate kind 属于后续 `feat/lexical-text-unit-index`，必须由 canonical TextUnit facts 和 tokenizer/index versioning 支撑。

当前 search-locator 增量不修改 InMemory/SQLite SearchIndex implementation/schema。

Search answers “where?”，不承担 unbounded canonical body read。

### 3.11 Cache / Persistent State

逻辑职责保持独立：

```text
RawResourceCache
ParsedDocumentCache
DocumentRepository
TextUnitIndex        # Paragraph persisted
SearchIndex
```

Sentence 当前从 canonical Document materialize，不增加第二个 source store。

实现可共享一个物理 SQLite 文件，但 ports/事实语义不能合并。

---

## 4. Domain Model

### 4.1 Document

```text
Document
├── id: DocumentId
├── source: DocumentSource
├── title
├── media_type
├── content_hash                  # raw-source provenance
├── metadata
└── root_sections[]
```

精确定位基础已实现 `normalized_document_hash`。它来自 addressing-relevant persisted normalized facts，不得通过静默重定义现有 raw `content_hash` 获得。

### 4.2 StructuralNode / current Section

```text
Section
├── id: SectionId
├── parent_id
├── title
├── level
├── content                       # canonical normalized owner text
├── location
└── children[]
```

Chapter/Section/Subsection 是递归 StructuralNode，不是多个不同技术索引层。

### 4.3 TextUnit

Paragraph TextUnit：

```text
TextUnit
├── id: TextUnitId
├── document_id
├── content_hash                  # raw provenance
├── normalized_document_hash
├── owner_section_id
├── kind: paragraph
├── paragraph_index               # 1-based
├── source_order
├── normalized_range              # exact Section.content slice
├── text                          # exact slice
└── segmentation_version
```

Sentence locator：

```text
SentenceTextUnit
├── id: TextUnitId
├── document_id
├── content_hash                  # raw provenance
├── normalized_document_hash
├── owner_section_id
├── paragraph_index               # 1-based container
├── sentence_index                # 1-based within Paragraph
├── parent_paragraph_id
├── source_order                  # deterministic Sentence stream order
├── normalized_range              # exact Section.content slice
├── text                          # exact slice
└── segmentation_version
```

明显 fenced/indented code 与 Markdown table 以 coarse Paragraph coverage 表示，不伪造 Sentence。分类来自 persisted-text strong signals，不冒充 parser-native block provenance。

Sentence 不是 child Section；canonical Section 也不能通过拼接 TextUnit rows 重建。

### 4.4 Location 与 TextLocator

Legacy `Location`：

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

Canonical `TextLocator`：

```text
TextLocator
├── document_id
├── content_hash
├── normalized_document_hash
├── owner_section_id / section_path
├── paragraph_index?
├── sentence_index?
├── normalized_range?
├── segmentation_version?
└── native_location / provenance?
```

当前 locator flow：

```text
get_text_units                 → Section/Paragraph/Sentence locator
search_document                → current Section locator
read_document(target_locator)  ← Section/Paragraph/Sentence/CharacterRange
get_context(target_locator)    ← Section/Paragraph/Sentence
```

三个主要坐标空间必须分离：

```text
parser/native/search-unit source coordinates
normalized owner Section.content coordinates
rendered/read-stream progress coordinates
```

只有 normalized owner coordinates 能作为通用精确 source range。Legacy `Location` 或 search-unit marker 不因 SearchHit 增加 TextLocator 而改变语义。

### 4.5 Cursor

```text
TextLocator = canonical source address
Cursor      = one versioned stream's progress
```

已实现：

```text
ReadCursor
TextUnitCursor
```

未来可能继续增加：

```text
DiscoveryCursor
StructureCursor
SearchCursor
```

Cursor 彼此不可交换，也不能用于引用。rendered/exact-target read-stream offset 或 TextUnit stream index 不能转成 source locator。

`TextUnitCursor` 绑定 raw/normalized identity、target Section、segmentation version、requested kind、direction、coverage policy、next index 与 total items。

---

## 5. 稳定 Identity 设计

### Raw source identity

当前实现根据 source + raw content hash 生成 `DocumentId`，使 source bytes 变化时得到不同 ID。

### Normalized document identity

已实现 deterministic `normalized_document_hash`，覆盖当前 addressing-relevant persisted normalized facts：

- Section identity/parentage/order；
- title/level；
- exact persisted `Section.content`。

未来若加入影响 segmentation 的 canonical persisted block/boundary metadata，需要升级 normalized hash contract version。

Parser/normalization 行为改变 canonical facts 时，即使 raw bytes 未变，旧 fine-grained locator/cursor 也必须 stale。

### Section identity

优先基于稳定结构路径/native target/source order，避免随机 UUID。重复标题需要 deterministic disambiguation。

### Fine-grained identity

```text
document/version identity
+ owner Section
+ normalized range / Paragraph/Sentence ordinal
+ segmentation version
```

Paragraph TextUnitId、Sentence TextUnitId 和 precise TextLocator 按上述原则确定性生成。

禁止旧 locator fuzzy-map 到新文档中“最相似”的句子。

---

## 6. Source Structure、TextUnit 与 Search Unit

```text
StructuralNode ≠ TextUnit
TextUnit       ≠ Search candidate/index row
Search Unit    ≠ Read stream
Index          ≠ Document
```

结构优先保留 source/native hierarchy；Paragraph/Sentence 在 canonical persisted state 上确定性派生；SearchIndex 负责 retrieval ranking；SearchDocumentUseCase 回到 canonical Document 构造当前 truthful locator；canonical read 最终回到 source facts。

当前职责：

```text
read_document
  = legacy Section-tree stream
  | exact already-known TextLocator target

get_text_units
  = discover/enumerate ordered Paragraph/Sentence-first items

search_document
  = bounded lexical candidates + truthful locator handoff

get_context
  = bounded neighbor/container/structural expansion around known anchor
```

`get_text_units` 不递归吸收 child Sections；结构顺序由 `get_document_structure` 和 Agent workflow 控制。

---

## 7. Tool 到内部 Capability / Use Case 映射

### 当前 runtime

```text
list_documents
  → DocumentDiscovery
  → configured local roots

open_document
  → DocumentOpenAndVersionResolution
  → SourcePolicy + Retriever + Parser
  → DocumentRepository + Paragraph TextUnitIndex + SearchIndex

get_document_structure
  → StructuralNavigation
  → DocumentRepository

get_text_units
  → OrderedTextUnitEnumeration
  → DocumentRepository + deterministic TextUnit materialization
  → TextLocator + TextUnitCursor + coverage

search_document
  → LexicalSearch + LocatorHandoff
  → SearchIndex ranking
  → DocumentRepository canonical Section enrichment
  → candidate_kind=section + TextLocator

read_document
  → PreciseRead
  → legacy SectionTreeReadStream | exact TextLocator target
  → DocumentRepository + shared locator resolver + ReadCursor

get_context
  → NeighborContext | ContainerContext | StructuralContext
  → DocumentRepository + shared locator resolver
```

### Accepted future evolution

```text
get_text_units
  → anchor-based before/after(locator) start

search_document
  → canonical Paragraph/Sentence candidates
  → independently-versioned tokenizer/index policy

open/structure/enumeration
  → richer reliability / coverage inspection
```

Reliability/Coverage、StableCitation、FreshnessValidation 和 NativeTraceability 是跨结果契约，不自动产生额外 Tool。

---

## 8. `open_document` 流程

```text
source
  ↓
validate source/auth policy
  ↓
retrieve or conditionally revalidate
  ↓
calculate raw content hash
  ↓
parse/cache
  ↓
validate canonical Document
  ↓
calculate normalized identity
  ↓
persist Document facts
  ↓
derive/rebuild Paragraph TextUnitIndex
  ↓
build/rebuild SearchIndex
  ↓
return version + capability/reliability/coverage summary
```

Sentence locator/enumeration 不增加第二个 source store。需要时从 canonical Document 确定性 materialize；Sentence persistence 只有性能证据出现时才进入单独迁移。

重复打开相同 normalized version 应保持身份稳定；source 或 canonical normalized facts 变化必须可观察。可读但 precise capability 降级时，open 成功并显式报告，不得伪造完整 native structure。

---

## 9. Complete Reading State Machines

### Section-tree / exact read stream

```text
known read target
  ↓
response budget reached?
 ├─ no  → complete
 └─ yes → next ReadCursor
              ↓
          continue until complete
```

Legacy Section-tree stream positions是 rendered coordinates；exact-target positions 是 target-local progress。两者都不是 citation/source locator。

Exact response 用 `returned_locator` 单独声明本页真实 source CharacterRange。

`truncated=true` 没有 continuation 不构成完整阅读语义。

### TextUnit stream

```text
select Section
  ↓
get_text_units(requested=sentence, coverage_policy=preserve_source)
  ↓
Sentence or explicit coarse non-prose Paragraph item
  ↓
response page complete?
 ├─ no  → next TextUnitCursor
 └─ yes → section_complete when source_complete
```

Forward/backward traversal 都使用 source-ordered response pages，stream indices 验证无 gap/overlap。

`preserve_source` 对已识别 non-prose 返回 coarse Paragraph；`eligible_only` 是 narrower stream，即使恰好全是 prose 也不声明 all-source completion。

TextUnit 本身是 enumeration 原子项；`max_chars` 不允许把一个 Sentence/Paragraph 切开后沿用同一个 locator。

当前 v1 从 Section 边界起读；anchor-based `before/after(locator)` 仍是后续 extension。

---

## 10. Context Semantics

明确区分：

```text
neighbor context    # same-level before/after
container context   # Sentence→Paragraph, TextUnit→Section
structural context  # owner/ancestors/siblings/children
```

共用 `get_context`，但请求是 tagged relation。不能通过模糊参数袋隐式改变语义。

当前 legacy Section `before/after` 只映射到 `neighbor(unit=section)`；structured path 已接受 Section/Paragraph/Sentence TextLocator。

Identity 先由 shared resolver 验证，再由 relation/capability 验证 anchor kind。

---

## 11. Search Handoff

当前已实现：

```text
SearchIndex ranked hit
        ↓
SearchDocumentUseCase
        ↓
canonical DocumentRepository validation/enrichment
        ↓
SearchHit {
  legacy snippet/score/location,
  candidate_kind = section,
  text_locator = owning Section locator
}
        ├→ read_document(target_locator)
        └→ get_context(target_locator, relation)
```

禁止：

```text
SearchHit.snippet → copy → search again
legacy search-unit Location → pretend canonical Paragraph range
index row ID → source identity
```

Title-only Section hit 必须保留，不能伪造 Paragraph/Sentence 来统一 schema。

未来 lexical TextUnit index 可以把 candidate kind 升级为 Paragraph/Sentence，但前提是索引保存或可验证 canonical TextUnit locator facts。

---

## 12. Reliability / Degradation / Coverage

精确能力必须可分级，而不是单个 `parse_success`：

```text
native / resolved
fallback / partial
coarse but readable
unsupported gap
fatal
```

Reliability 优先返回 factual provenance/status：

```text
epub_nav / epub_ncx / xhtml_heading / spine_item
resolved_fragment / missing_fragment / unsupported_resource
```

Coverage 需要定义清楚 denominator：

- spine/resource coverage；
- structural target coverage；
- eligible prose Paragraph/Sentence coverage；
- non-prose coarse/skipped counts；
- unsupported gaps。

Paragraph v1 维护每个 Section：

```text
owner_chars / paragraph_chars / separator_chars / paragraph_count
```

Sentence foundation 维护每个 Paragraph：

```text
content_class / eligibility
paragraph_chars / sentence_chars / separator_chars / coarse_only_chars
sentence_count
```

`get_text_units` 返回 Section enumeration coverage：

```text
owner_chars
section_separator_chars
sentence_separator_chars
paragraph_count
sentence_eligible_paragraphs
non_prose_paragraphs
represented_paragraphs
represented_sentences
coarse_non_prose_items
intentionally_skipped
unsupported_gaps
source_complete
```

`complete` 表示声明的 enumeration stream 已消费；`section_complete` 只有在 stream terminal 且 all-source coverage 可声明时为 true。

明显 code/table 使用 coarse-only coverage，不伪造 Sentence；`prose_or_unknown` 只表示没有 strong non-prose signal，不声称 native prose provenance。

Reliability/Coverage 信息应在 open/structure/TextUnit enumeration 等决策点返回。没有独立 Use Case 前不增加 inspection Tool。

---

## 13. SSRF 防护流程

HTTP 请求每一跳都必须：

```text
URL
 ↓
validate scheme/auth profile host
 ↓
resolve DNS
 ↓
validate all resolved IPs
 ↓
pin/connect validated endpoint
 ↓
redirect?
 ├─ no → continue
 └─ yes → repeat all checks
```

不能只在最初 URL 检查 host 字符串，也不能允许 proxy 破坏已验证 endpoint 的证据链。

---

## 14. Browser Retriever 边界

当前不实现浏览器渲染。未来 BrowserRetriever 只能作为显式策略允许的 Retriever 实现：

```text
HTTP cannot obtain useful supported resource
           ↓
explicit policy allows browser fallback
           ↓
BrowserRetriever
```

它不能改变 Parser、Document Model、TextLocator 或 MCP Tool responsibility。

---

## 15. 错误模型

稳定 locator/cursor 类别包括：

```text
INVALID_LOCATOR
STALE_LOCATOR
INVALID_CURSOR
STALE_CURSOR
CURSOR_TARGET_MISMATCH
CURSOR_ENCODING_FAILED
```

仍需按后续 capability 增量完善的逻辑类别包括：

```text
UNSUPPORTED_CAPABILITY
INVALID_NORMALIZED_RANGE
STRUCTURE_INVARIANT_FAILED
TEXT_UNIT_INVARIANT_FAILED
COVERAGE_INCOMPLETE
```

规则：

- locator/cursor identity mismatch fail closed；
- unsupported locator kind for one capability 不等于 malformed locator；
- re-open/re-parse 是显式 workflow；
- fallback 只在 lower precision 可被真实证明时允许；
- MCP Adapter 返回稳定 `code + retryable`，但不泄露正文/Secret。

---

## 16. 推荐项目目录

逻辑结构：

```text
src/
├── mcp/
├── application/
│   ├── list_documents
│   ├── open_document
│   ├── get_document_structure
│   ├── get_text_units
│   ├── search_document
│   ├── get_context
│   ├── read_document
│   ├── locator_resolution
│   ├── read_cursor
│   └── text_unit_cursor
├── domain/
│   ├── normalized_text
│   ├── text_unit
│   ├── text_locator
│   └── document/section/location facts
├── retrieval/
├── parsing/
├── security/
├── infrastructure/
└── runtime/
```

TextUnit/locator/cursor 模块必须根据 domain/application responsibility 放置，不能把 rmcp DTO、SQLite row 或 parser-specific types 作为 domain identity。

---

## 17. 核心设计原则

```text
Actor goal ≠ Tool call success
Use Case precedes Tool
Document acquisition ≠ parsing
Parsing ≠ indexing
Indexing ≠ reading
Reading ≠ reasoning
StructuralNode ≠ TextUnit
Search unit ≠ canonical TextUnit
Search candidate ≠ SearchIndex row identity
TextLocator ≠ Cursor
ReadCursor ≠ TextUnitCursor
Neighbor context ≠ Container context ≠ Structural context
MCP transport ≠ application logic
Security policy ≠ HTTP implementation
Fallback ≠ native precision
```

只要这些边界保持稳定，后续增加 Parser、Retriever、canonical Paragraph/Sentence lexical index 或 precise-reading capability 都不需要破坏核心架构。
