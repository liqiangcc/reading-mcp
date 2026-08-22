# Phase 6：MCP stdio Server 与真实调用验证

## 目标

通过真正的 MCP stdio transport 暴露既有 Application UseCase，而不让协议 SDK 进入 Domain/Application/Parsing/Retrieval。

```text
MCP Client
  ↓ stdio / JSON-RPC
reading-mcp binary
  ↓
ReadingMcpServer
  ↓
Application UseCases
  ↓
Domain + Ports + Adapters
```

`rmcp` 只进入 MCP adapter/binary。

## 7 个 Tool

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

`list_documents` 只枚举授权 local roots；`open_document` 建立 raw/normalized identity；structure 返回 Section tree；`get_text_units` 枚举 Paragraph/Sentence；`search_document` 返回 bounded retrieval candidates + direct TextLocator handoff；`get_context` 支持 legacy Section-neighbor 与 locator-driven tagged context；`read_document` 支持 legacy Section-tree 与 exact TextLocator read。格式扩展不增加格式专属 Tool。

## Normalized identity / TextUnit foundation

`open_document` additive 返回：

```text
content_hash
normalized_document_hash
normalized_document_hash_version = normalized-document-hash/v1
normalization_version             = reading-mcp-normalization/v1
normalized_text_coordinate_space = section-content-unicode-scalar/v1
```

Normalized range：

```text
owner    = exact persisted Section.content
base     = zero
interval = half-open [start, end)
unit     = Unicode scalar / Rust char
```

Legacy `Location.char_start/char_end` 不被静默解释为 normalized range。

```text
text-segmentation/v1
Section.content
  → Paragraph exact ranges
  → deterministic Sentence ranges/ownership
```

Paragraph TextUnit 是独立 derived `TextUnitIndex`；Sentence facts 从 persisted canonical Document 确定性 materialize，不依赖 Sentence SQLite rows。SearchIndex/FTS 是另一职责。

## TextUnit enumeration

`get_text_units` 支持：

```text
document_id
section_id
requested_kind = paragraph | sentence
direction      = forward | backward
coverage_policy= preserve_source | eligible_only
max_items
max_chars?
cursor?
```

`TextUnitCursor = text-unit-cursor/v1` 绑定 raw/normalized identity、Section、segmentation、kind、direction、coverage policy、next index 与 stream length。分页保证 source order、no-gap/no-overlap 和 actionable continuation。

`preserve_source` 遇到识别出的 code/table 时返回 coarse Paragraph item，不伪造 Sentence；`eligible_only` 不宣称 all-source completion。当前 v1 从 Section 边界开始；anchor-based `before/after(locator)` 仍是兼容后续扩展。

## Shared TextLocator resolution

Exact read 与 structured context 现在共用一个 application-level resolver。统一验证：

```text
document_id
raw content_hash
normalized_document_hash
owner Section
Section / CharacterRange / Paragraph / Sentence shape
Paragraph/Sentence ordinal
text-segmentation/v1
exact normalized range
```

失败使用 `INVALID_LOCATOR / STALE_LOCATOR`，不做 fuzzy rebasing。

Resolver 只判断 canonical locator identity；consumer 决定 capability。Exact read 接受 CharacterRange，当前 context 不接受 CharacterRange anchor，因此 context 对一个合法 CharacterRange 返回 unsupported request semantics，而不是把它误判 malformed locator。

## Locator-driven context

Legacy：

```text
document_id + section_id + before/after + max_chars?
→ neighbor(unit=section)
```

Structured：

```text
document_id + (section_id | target_locator)
relation:
  neighbor { unit: section | paragraph | sentence, before, after }
  | container { kind: paragraph | section }
  | structural { kind: owner_section | ancestors | siblings | children }
```

Sentence neighbor 与 `get_text_units(... preserve_source)` 保持同一 source-order/coarse non-prose 语义。Locator-driven structured 正文只出现在 `items[]`，不重复到顶层 legacy `content`。

## Exact TextLocator read

Legacy：

```text
document_id + section_id + max_chars? + cursor?
→ SectionTreeReadStream/v1
→ selected Section + descendants
```

Precise：

```text
document_id + target_locator + max_chars? + cursor?
→ exact_target / exact-normalized-source/v1
```

Exact target 支持：

```text
Section        → exact Section.content only
Paragraph      → exact Paragraph range
Sentence       → exact Sentence range
CharacterRange → exact Section-relative normalized range
```

Exact response 区分：

```text
resolved_target_locator = logical target
returned_locator        = exact CharacterRange represented by this segment
```

每个 exact segment 必须满足：

```text
content == owner_section.normalized_text_slice(returned_locator.normalized_range)
```

`stream.start_char/end_char` 是 `exact-target-unicode-scalar/v1` target-local continuation progress，不是 source range。Oversized target 使用 version-bound `read-cursor/v2` continuation。

Legacy Section-tree output 不是单个 contiguous source range，因此 `returned_locator=null`。

## SearchHit → TextLocator

`search_document` 请求保持：

```text
document_id
query
limit
```

每个 hit 保留：

```text
section_id
title
source
snippet
score
location
```

并新增：

```text
candidate_kind
text_locator
```

Source-first 检查确认当前 InMemory/SQLite SearchIndex 的 paragraph-like retrieval row 不等价于 canonical Paragraph TextUnit：现有 row 没有 canonical normalized range + segmentation identity，且历史 split policy 与 TextUnit segmentation 不共享一个 identity contract。

因此当前 runtime 只返回：

```text
candidate_kind = section
text_locator   = canonical owning Section locator
```

legacy `location` 仍可带更窄的 `search-unit` provenance，但不能被解释成 canonical Paragraph range。

SearchDocumentUseCase 在 SearchIndex ranking 之后读取 canonical DocumentRepository：

```text
SearchIndex hit
  ↓
validate canonical source + owning Section
  ↓
TextLocator::for_section(...)
```

若 index 指向不存在的 Section 或 source 不一致，显式 index failure，不伪造 locator。

Direct stdio workflow：

```text
search_document
  ↓
SearchHit.text_locator
  ├→ read_document(target_locator)
  └→ get_context(target_locator, relation)
```

Paragraph/Sentence candidate kind 只有在后续 lexical TextUnit index 能真实证明 canonical identity 后才允许产生。

## Section-tree read continuation

Legacy Section read 使用：

```text
SectionTreeReadStream/v1
read-cursor/v2
section-tree-rendered-unicode-scalar/v1
```

Repeated continuation 必须满足 no-gap/no-overlap、finite progress、terminal `complete=true` 与 `next_cursor=null`。首次 `max_chars=0` 保留兼容行为；continuation `max_chars=0` 被拒绝。

## 当前默认 Runtime

```text
SourcePolicyRouter
├── LocalFileSourcePolicy(default deny, allowed roots)
└── PublicHttpAccessPolicy(HTTPS-only by default)

RetrieverRouter
├── LimitedFileRetriever
└── RevalidatingHttpRetriever(HttpRetriever + Raw Cache)

ParserRouter::release
├── Text
├── Markdown
├── HTML/XHTML
├── PDF
├── EPUB
├── DOCX
└── OpenAPI/Swagger JSON/YAML

Default persistent state
├── File Raw Cache
├── File Parsed Cache
├── SQLite DocumentRepository
├── SQLite TextUnitIndex (Paragraph v1)
└── SQLite FTS5 SearchIndex
```

本轮 SearchHit locator handoff **不修改** InMemory/SQLite SearchIndex implementation/schema；canonical locator 由 application layer enrich。Sentence persistence 仍不是正确性依赖。

## 真实 stdio 验收

测试启动真正的 `reading-mcp` 子进程，经 stdio 完成 MCP initialize、tools/list 和调用链。

覆盖：

- 7 Tool discovery/调用；
- raw/normalized identity 与 normalized range；
- Paragraph/Sentence deterministic rebuild；
- TextUnit forward/backward continuation；
- source-preserving non-prose 与 eligible-only completion；
- persisted repository restart 后 TextUnitCursor 继续；
- `get_text_units → TextLocator → get_context`；
- tagged neighbor/container/structural context；
- shared locator resolver identity/stale semantics；
- `get_text_units → TextLocator → read_document`；
- exact Section/Paragraph/Sentence/CharacterRange read；
- exact-target continuation 与 truthful returned locator；
- `search_document → Section TextLocator → read_document`；
- `search_document → Section TextLocator → get_context`；
- paragraph-like search row 不伪造 Paragraph locator；
- title-only search hit 保持 Section-level；
- legacy search fields/Location 继续可用；
- legacy Section-tree read/context compatibility；
- cursor/locator malformed/stale fail-closed；
- stderr telemetry 不污染 stdout MCP transport。

## 架构约束

```text
mcp → application → domain
retrieval/security/parsing/infrastructure → application/domain ports
```

禁止 parser/retriever/index 直接依赖 MCP DTO；禁止 cursor offset、snippet、score、index row 或 legacy search-unit location 充当 canonical source identity。

## 当前支持的远程访问

除 stdio 外，`reading-mcp-http` 提供带 Bearer Token 的 Streamable HTTP 单机入口，默认 loopback。远程访问通过受信任 tunnel/reverse proxy；公网多租户/OAuth 不在 v0.1 范围内。

## v0.1 明确非目标

- 公网多租户 transport；
- browser rendering；
- OCR；
- OAuth/Cookie 交互登录；
- 企业产品 API；
- MCP Resources/Prompts；
- AI 总结/问答/笔记/通用向量 RAG。
