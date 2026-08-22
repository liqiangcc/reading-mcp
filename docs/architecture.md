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

详细决策见 [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md) 与 [ADR 0004](adr/0004-use-case-first-tool-contracts.md)。

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

Document/Section 是 canonical normalized facts；Paragraph/Sentence TextUnit 与 SearchIndex 是可重建派生状态。

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
- 重建 TextUnit；
- 通过文本相似度修复 stale locator；
- AI 总结或推理。

### 3.2 Application Use Cases

系统用例编排层。当前 runtime 已实现：

```text
ListDocumentsUseCase
OpenDocumentUseCase
GetDocumentStructureUseCase
SearchDocumentUseCase
GetContextUseCase
ReadDocumentUseCase
```

对应当前 6 个 MCP Tool：

```text
list_documents
open_document
get_document_structure
search_document
get_context
read_document
```

Use-Case-First 设计接受的未来独立能力包括：

```text
OrderedTextUnitEnumeration
SequentialContinuation
Precise locator handoff
Neighbor / Container / Structural context
Reliability / Coverage inspection
```

其中 `OrderedTextUnitEnumeration` 最终映射到一个通用未来 Tool：`get_text_units`。当前 runtime 尚未实现它。

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

它是读取事实来源。Search snippet、FTS row、rendered MCP response、Sentence rows都不能替代 canonical Document。

### 3.8 TextUnit Index 与 Sentence Locator

Paragraph/Sentence 是 deterministic、versioned、rebuildable TextUnits：

```text
Document / Section.content
          ↓
TextUnit segmentation policy
          ↓
Paragraph / Sentence TextUnits
```

当前已实现 Paragraph v1 持久化派生索引：

```text
text-segmentation/v1
Section.content
  → exact Paragraph NormalizedTextRange
  → deterministic Paragraph TextUnitId
  → Paragraph source_order
  → TextUnitIndex
```

当前也已实现 Sentence locator foundation：

```text
Paragraph TextUnit
  → conservative persisted-text eligibility/classification
  → deterministic English/CJK Sentence segmentation
  → exact Section-relative Sentence NormalizedTextRange
  → paragraph_index + sentence_index + parent_paragraph_id
  → deterministic Sentence TextUnitId/source_order
  → per-Paragraph Sentence/coarse coverage
```

`TextUnitIndex` 已有独立 application port，以及 InMemory / SQLite adapter，但当前持久化 schema 仍是 Paragraph-only。Sentence locator state 由 canonical Document + Paragraph TextUnit 确定性重建；Sentence persistence、TextUnitCursor 与 MCP enumeration 仍属于后续增量。

TextUnit identity 必须依赖 addressing-relevant normalized identity 与 segmentation version，而不能依赖易失 SearchIndex row ID。

### 3.9 Search Index

搜索是 derived retrieval state：

```text
Section title candidates
Paragraph candidates
Sentence candidates
        ↓
SearchHit + TextLocator
```

当前实现以 owning Section + legacy Location handoff；未来必须返回 version-bound `TextLocator`，直接进入 read/context。

当前 FTS `SearchIndex` 尚未改用 Paragraph TextUnitIndex 或 Sentence locator；职责和迁移节奏保持独立。

Search answers “where?”，不承担 unbounded canonical body read。

### 3.10 Cache / Persistent State

逻辑职责保持独立：

```text
RawResourceCache
ParsedDocumentCache
DocumentRepository
TextUnitIndex        # Paragraph persisted; Sentence locator rebuildable
SearchIndex
```

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

精确定位基础已实现逻辑 `normalized_document_hash`。它来自 addressing-relevant persisted normalized facts，不得通过静默重定义现有 raw `content_hash` 获得。

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

### 4.3 TextUnit（Paragraph persisted; Sentence locator implemented）

当前 Paragraph TextUnit：

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

当前 Sentence locator：

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

明显 fenced/indented code 与 Markdown table 以 coarse Paragraph coverage 表示，不伪造 Sentence。当前分类来自 persisted-text strong signals，不冒充 parser-native block provenance。

Sentence 不是 child Section；canonical Section 也不能通过拼接 TextUnit rows 重建。TextLocator wire contract 仍未实现。

### 4.4 Location 与 TextLocator

当前 legacy `Location`：

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

未来统一精确地址：

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

三个坐标空间必须分离：

```text
parser/native source coordinates
normalized owner Section.content coordinates
rendered MCP response/read-stream coordinates
```

只有 normalized owner coordinates 能作为通用精确 source range。

### 4.5 Cursor

```text
TextLocator = canonical source address
Cursor      = one versioned stream's progress
```

可能的 cursor 包括：

```text
DiscoveryCursor
StructureCursor
TextUnitCursor
SearchCursor
ReadCursor
```

它们彼此不可交换，也不能用于引用。rendered read-stream offset 不能转成 source locator。

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

Parser/normalization 行为改变 canonical facts 时，即使 raw bytes 未变，旧 fine-grained locator 也必须 stale。

### Section identity

优先基于稳定结构路径/native target/source order，避免随机 UUID。重复标题需要 deterministic disambiguation。

### Fine-grained identity

```text
document/version identity
+ owner Section
+ normalized range / Paragraph/Sentence ordinal
+ segmentation version
```

当前 Paragraph TextUnitId 与 Sentence TextUnitId 均按上述原则确定性生成；未来 TextLocator 继续复用该身份基础。

禁止旧 locator fuzzy-map 到新文档中“最相似”的句子。

---

## 6. Source Structure、TextUnit 与 Search Unit

```text
StructuralNode ≠ TextUnit
TextUnit       ≠ Search candidate/index row
Search Unit    ≠ Read stream
Index          ≠ Document
```

结构优先保留 source/native hierarchy；Paragraph/Sentence 在 canonical persisted state 上确定性派生；SearchIndex 引用 locator；canonical read 最终回到 Document/TextUnit source facts。

当前 Section read 会渲染 Section subtree。未来精确读取与有序枚举分别具有不同职责：

```text
read_document
  = read already-known target / continue one read stream

get_text_units
  = discover/enumerate ordered child reading items
```

当前已经具备 Paragraph TextUnitIndex 与 Sentence locator/coverage 底层基础，但不等于 `get_text_units` 已可用。

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

search_document
  → LexicalSearch
  → SearchIndex

read_document
  → PreciseRead (current: Section subtree)
  → DocumentRepository

get_context
  → NeighborContext (current: Section level)
  → DocumentRepository
```

Sentence locator is currently a deterministic domain capability, not a seventh runtime Tool.

### Accepted future evolution

```text
get_text_units
  → OrderedTextUnitEnumeration
  → Paragraph TextUnitIndex + Sentence locator/materialization + Locator validation

read_document
  → Section/TextLocator read + ReadCursor continuation

get_context
  → tagged Neighbor | Container | Structural context

search_document
  → Section/Paragraph/Sentence candidate + direct TextLocator handoff
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

Sentence locator state does not add a second source store and is currently materialized deterministically from canonical Document + Paragraph units when needed by domain/tests. The future enumeration contract will decide its pagination/storage boundary.

重复打开相同 normalized version 应保持身份稳定；source 或 canonical normalized facts 变化必须可观察。可读但 precise capability 降级时，open 成功并显式报告，不得伪造完整 native structure。

---

## 9. Complete Reading State Machines

### Section stream

```text
read target
  ↓
response budget reached?
 ├─ no  → complete
 └─ yes → next ReadCursor
              ↓
          continue until complete
```

`truncated=true` 没有 continuation 不构成完整阅读语义。

### TextUnit stream

```text
select Section
  ↓
get_text_units(requested=sentence)
  ↓
Sentence or explicit coarse non-prose reading item
  ↓
next TextUnitCursor
  ↓
section_complete
```

在 source-preserving policy 下，每个 source region 必须被 reading item 表示或被 coverage 显式说明。code/table 不得为了 coverage 被伪造为 Sentence。

上述 TextUnit stream 是已接受的未来 Tool workflow；当前已实现 Paragraph 派生索引和 Sentence locator/non-prose coverage，但尚未实现 TextUnitCursor、分页或 `get_text_units` Tool。

---

## 10. Context Semantics

必须明确区分：

```text
neighbor context    # same-level before/after
container context   # Sentence→Paragraph, TextUnit→Section
structural context  # owner/ancestors/siblings/children
```

可以共用 `get_context`，但请求必须是 tagged relation。不能通过一个模糊 `mode/unit/before/after` 参数袋隐式改变语义。

当前 legacy Section `before/after` 只映射到 `neighbor(unit=section)`。

---

## 11. Search Handoff

当前 coarse handoff：

```text
SearchHit → owning Section → read/context
```

未来 precise handoff：

```text
SearchHit(candidate_kind + TextLocator)
             ├→ read_document
             └→ get_context
```

禁止：

```text
SearchHit.snippet → copy → search again
```

Title-only Section hit 必须保留，不能伪造 Paragraph/Sentence 来统一 schema。

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

当前 Paragraph v1 维护每个 Section 的：

```text
owner_chars / paragraph_chars / separator_chars / paragraph_count
```

当前 Sentence locator foundation 维护每个 Paragraph 的：

```text
content_class / eligibility
paragraph_chars / sentence_chars / separator_chars / coarse_only_chars
sentence_count
```

并保证：

```text
paragraph_chars = sentence_chars + separator_chars + coarse_only_chars
```

明显 code/table 内容使用 coarse-only coverage，不伪造 Sentence；`prose_or_unknown` 只表示没有 strong non-prose signal，不声称 native prose provenance。

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

当前稳定类别包括 source/retrieval/parse/repository/index/text-unit-index/document/section/invalid-request 等。

未来 precise contracts 至少需要逻辑错误类别：

```text
STALE_LOCATOR
STALE_CURSOR
CURSOR_TARGET_MISMATCH
UNSUPPORTED_CAPABILITY
INVALID_NORMALIZED_RANGE
STRUCTURE_INVARIANT_FAILED
TEXT_UNIT_INVARIANT_FAILED
COVERAGE_INCOMPLETE
```

当前 TextUnitIndex adapter 的持久化/派生错误映射到独立 `TEXT_UNIT_INDEX_FAILED`，不与 SearchIndex 错误混为一类。

规则：

- locator/cursor identity mismatch fail closed；
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
│   ├── search_document
│   ├── get_context
│   └── read_document
├── domain/
│   ├── document
│   ├── section
│   ├── normalized_text
│   ├── text_unit
│   └── location
├── retrieval/
├── parsing/
├── security/
├── infrastructure/
├── runtime/
└── config/
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
Search unit ≠ canonical read target
TextLocator ≠ Cursor
Neighbor context ≠ Container context ≠ Structural context
MCP transport ≠ application logic
Security policy ≠ HTTP implementation
Fallback ≠ native precision
```

只要这些边界保持稳定，后续增加 Parser、Retriever、TextUnit/FTS 或 precise-reading capability 都不需要破坏核心架构。
