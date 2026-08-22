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

`list_documents` 只枚举 `READING_MCP_LOCAL_ROOTS` 授权目录中的可读文档，不打开文档；`open_document` 返回 raw/normalized identity；structure 返回 Section tree；`get_text_units` 枚举 Paragraph/Sentence；`get_context` 支持 legacy Section-neighbor 与 locator-driven tagged context；`read_document` 同时支持 legacy Section-tree 与 exact TextLocator read。格式扩展不增加 PDF/EPUB/DOCX 专属 Tool。

## Normalized document identity and text coordinates

`open_document` additive 返回：

```text
content_hash
normalized_document_hash
normalized_document_hash_version = normalized-document-hash/v1
normalization_version             = reading-mcp-normalization/v1
normalized_text_coordinate_space = section-content-unicode-scalar/v1
```

其中 raw `content_hash` 是 source-byte provenance，`normalized_document_hash` 是 addressing-relevant canonical Document/Section facts 的确定性指纹。

Normalized range 的统一语义：

```text
owner    = exact persisted Section.content
base     = zero
interval = half-open [start, end)
unit     = Unicode scalar / Rust char
```

Legacy `Location.char_start/char_end` 不被静默解释为 normalized range。

Parsed Cache key 包含：

```text
final_source
raw_sha256
normalization_version
```

完整定义见 [Normalized Document Identity and Text Range Contract](normalized-text-range-contract.md)。

## Paragraph / Sentence TextUnit foundation

```text
text-segmentation/v1
Section.content
  → Paragraph exact ranges
  → deterministic Sentence ranges/ownership
```

Paragraph TextUnit 进入独立 derived `TextUnitIndex`；Sentence locator/coverage 从 persisted canonical Document 确定性 materialize，不依赖 Sentence SQLite rows。SearchIndex/FTS 仍是另一职责。

完整定义见 [Paragraph TextUnit Index Contract](paragraph-text-unit-index.md) 与 [Sentence Locator and Coverage Contract](sentence-locator-contract.md)。

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

`preserve_source` 遇到识别出的 code/table 时返回 coarse Paragraph item，不伪造 Sentence；`eligible_only` 不宣称 all-source completion。TextUnit 是 enumeration 原子项，不因 `max_chars` 被切开。

当前 v1 从 Section 边界开始；anchor-based `before/after(locator)` 仍是兼容后续扩展。

完整定义见 [TextUnit Enumeration Contract](text-unit-enumeration-contract.md)。

## Locator-driven context

`get_context` 保留：

```text
legacy: document_id + section_id + before/after + max_chars?
       → neighbor(unit=section)
```

并 additive 支持：

```text
document_id + (section_id | target_locator)
relation:
  neighbor { unit: section | paragraph | sentence, before, after }
  | container { kind: paragraph | section }
  | structural { kind: owner_section | ancestors | siblings | children }
```

TextLocator 校验 document/raw/normalized identity、owner、Paragraph/Sentence ordinal、`text-segmentation/v1` 和 exact range；失败使用 `INVALID_LOCATOR / STALE_LOCATOR`，不做 fuzzy rebasing。

Sentence neighbor 与 `get_text_units(... preserve_source)` 保持同一 source-order/coarse non-prose 语义。Locator-driven structured正文只出现在 `items[]`，不重复到顶层 legacy `content`。

完整定义见 [Context Granularity Contract](context-granularity-contract.md)。

## Exact TextLocator read

`read_document` 现在有两条明确路径。

Legacy：

```text
document_id
section_id
max_chars?
cursor?

→ SectionTreeReadStream/v1
→ section-tree-markdown/v1
→ selected Section + descendants
```

Precise：

```text
document_id
target_locator
max_chars?
cursor?

→ exact_target
→ exact-normalized-source/v1
```

`section_id` 和 `target_locator` 互斥。

Exact target 支持：

```text
Section        → only exact Section.content
Paragraph      → exact Paragraph range
Sentence       → exact Sentence range
CharacterRange → exact Section-relative normalized range
```

Exact response 区分：

```text
resolved_target_locator
= logical target

returned_locator
= exact CharacterRange represented by this response segment
```

每个 exact segment 必须满足：

```text
content == owner_section.normalized_text_slice(returned_locator.normalized_range)
```

Exact stream：

```text
read_mode         = exact_target
rendering_version = exact-normalized-source/v1
coordinate_space  = exact-target-unicode-scalar/v1
```

`stream.start_char/end_char` 只表示 target-local continuation progress，不是 canonical source range。

Oversized target 使用 `read-cursor/v2` continuation。Exact cursor 在既有 raw/normalized identity、Section、mode/rendering version、next stream position 之外绑定 target kind、ordinal/range/segmentation facts。Legacy Section-tree v2 cursor 不携带这些 exact fields，因此保持旧序列化语义。

Legacy Section-tree response 的 rendered subtree 不能 truthful 映射成一个 contiguous source range，所以：

```text
returned_locator = null
```

完整定义见 [Precise Read Locator Contract](precise-read-locator-contract.md)。

## Section-tree read continuation

Legacy Section read 继续使用：

```text
SectionTreeReadStream/v1
read-cursor/v2
section-tree-rendered-unicode-scalar/v1
```

Repeated continuation 必须满足：

```text
segment[n].end_char == segment[n+1].start_char
concatenated segments == complete declared stream
no gap
no overlap
terminal complete = true
terminal next_cursor = null
```

首次 `max_chars=0` 保留旧行为；continuation `max_chars=0` 被拒绝以避免零进度。

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

Sentence enumeration/context/read facts remain deterministically rebuildable from canonical Document; there is no Sentence persistence correctness dependency.

## 真实 stdio 验收

测试启动真正的 `reading-mcp` 子进程，经 stdio 完成 MCP initialize、tools/list 和调用链。

覆盖：

- 7 Tool discovery/调用；
- raw/normalized source identity；
- normalized range / TextUnit exact-slice；
- Paragraph/Sentence deterministic rebuild；
- `get_text_units` forward/backward continuation；
- source-preserving non-prose 与 eligible-only completion；
- persisted repository restart 后 TextUnitCursor 继续；
- `get_text_units → TextLocator → get_context` handoff；
- tagged neighbor/container/structural context；
- locator malformed/stale fail-closed；
- context 与 enumeration Sentence/coarse 语义 parity；
- `get_text_units → TextLocator → read_document` exact handoff；
- exact Section/Paragraph/Sentence/CharacterRange read；
- exact-target multi-page no-gap/no-overlap reconstruction；
- every `returned_locator` exact slice equals response content；
- exact cursor cannot change target；
- read/context overlapping locator stale semantics parity；
- legacy Section-tree read/context compatibility；
- old read-cursor/v1 stale behavior；
- stderr telemetry 不污染 stdout MCP transport。

## 架构约束

```text
mcp → application → domain
retrieval/security/parsing/infrastructure → application/domain ports
```

禁止 parser/retriever/index 直接依赖 MCP DTO，也禁止将 cursor offset 当成 source identity。

当前 read/context 各自仍有重叠的 Locator resolution 实现；跨 consumer parity test 锁定行为。在下一步 SearchHit 成为第三个 Locator consumer 前，先抽取共享 resolver。

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
