# Phase 6：MCP stdio Server 与真实调用验证

## 目标

通过真实 MCP stdio transport 暴露 Application Use Cases，同时保持：

```text
rmcp only in MCP adapter/binary
mcp → application → domain
infrastructure/retrieval/parsing/security → ports
```

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

格式扩展不增加格式专属 Tool。

## Precise-reading foundation

`open_document` additive 返回 raw/normalized identity：

```text
content_hash
normalized_document_hash
normalized-document-hash/v1
reading-mcp-normalization/v2
section-content-unicode-scalar/v1
```

`reading-mcp-normalization/v2` invalidates Parsed Cache entries created before the EPUB navigation-map parser output was introduced; it does not change `normalized-document-hash/v1` while canonical Section addressing facts remain unchanged.

Paragraph/Sentence：

```text
Section.content
→ text-segmentation/v1
→ Paragraph exact ranges
→ deterministic Sentence ranges / ownership
```

Sentence persistence 不是 enumeration/context/read/search correctness dependency。

## TextUnit enumeration

```text
get_text_units
→ paragraph | sentence
→ forward | backward
→ preserve_source | eligible_only
→ source-order pages
→ TextLocator per item
→ text-unit-cursor/v1
→ completion + coverage
```

`preserve_source` 对 recognized non-prose 返回 coarse Paragraph，不伪造 Sentence。

## Shared TextLocator resolver

统一验证：

```text
document_id
raw content_hash
normalized_document_hash
owner Section
Section / CharacterRange / Paragraph / Sentence shape
Paragraph/Sentence ordinal
segmentation version
normalized range
```

`INVALID_LOCATOR / STALE_LOCATOR` fail closed；不 fuzzy rebase。

## Locator-driven context

Legacy：

```text
section_id + before/after
→ neighbor(section)
```

Structured：

```text
neighbor(section | paragraph | sentence)
container(paragraph | section)
structural(owner_section | ancestors | siblings | children)
```

## Exact read

Legacy：

```text
section_id
→ SectionTreeReadStream/v1
→ read-cursor/v2
```

Precise：

```text
target_locator
→ exact Section | Paragraph | Sentence | CharacterRange
→ exact-normalized-source/v1
→ ReadCursor when oversized
```

Response separates logical target, source range and stream progress:

```text
resolved_target_locator
returned_locator
stream.start_char / end_char
```

## Canonical lexical search

Current precise retrieval contract:

```text
lexical-search-index/v2
lexical-tokenizer/v1
```

Candidate builder is shared by InMemory and SQLite:

```text
Section title      → Section TextLocator
Paragraph TextUnit → Paragraph TextLocator
eligible Sentence  → Sentence TextLocator
```

Title-only Section stays Section-level. Non-prose remains Paragraph-level and does not gain fake Sentence identity.

Tokenizer version is independent of segmentation identity:

```text
text-segmentation/v1
→ source TextUnit identity

lexical-tokenizer/v1
→ lexical projection/rebuild only
```

Tokenizer v1 supports:

- normalized full + component tokens for technical identifiers；
- Han/Hiragana/Katakana/Hangul unigram + adjacent bigram；
- mixed CJK/technical text；
- encoded SQLite lexemes so FTS does not reinterpret logical token boundaries。

SQLite v2 persists candidate kind + canonical TextLocator + tokenizer/index version + source order + encoded lexemes.

If lexical state is missing/incompatible but canonical Document exists：

```text
search_document
→ rebuild derived SearchIndex from DocumentRepository
→ retry
```

No source retrieve/reparse is required.

Historical SearchIndex adapters remain compatible through Section-level fallback. Runtime `SqliteSearchIndex` is v2；historical SQLite search implementation remains hidden compatibility only。

## Direct SearchHit handoff

```text
search_document
→ SearchHit(candidate_kind + text_locator)
        ├→ read_document(target_locator)
        └→ get_context(target_locator, relation)
```

SearchDocumentUseCase revalidates source, tokenizer version, candidate kind and locator identity against canonical Document before returning a precise hit。

Legacy preview fields remain available；`location/search-unit` never becomes canonical identity。

## EPUB navigation-map parser foundation

`feat/epub-navigation-map` adds parser-internal, persisted EPUB navigation facts without changing the seven-Tool surface or rewriting canonical Section hierarchy yet:

```text
EPUB package / manifest
→ properties=nav discovery
→ EPUB 3 toc nav hierarchy
→ legacy NCX fallback
→ href / fragment resolution diagnostics
→ epub-navigation-map/v1 in Document.metadata
```

The map is an input to the later nav/spine reconciliation increment. It is not yet exposed as a new MCP Tool or treated as canonical Section identity.

## Default persistent state

```text
File Raw Cache
File Parsed Cache
SQLite DocumentRepository
SQLite Paragraph TextUnitIndex
SQLite lexical-search-index/v2
```

## 真实 stdio / release acceptance

测试启动真正的 `reading-mcp` 子进程并覆盖：

- 7 Tool discovery；
- raw/normalized identity；
- TextUnit deterministic rebuild / cursor continuation；
- non-prose/eligible-only coverage；
- locator-driven context；
- exact read + continuation + truthful returned ranges；
- SearchHit Section/Paragraph/Sentence locator handoff；
- Sentence SearchHit → exact read；
- Sentence SearchHit → Sentence neighbor context；
- CJK substring lexical retrieval；
- technical identifier lexical retrieval；
- non-prose Paragraph search without fake Sentence；
- SQLite lexical reopen；
- missing derived-index rebuild from persisted canonical Document；
- historical SearchIndex adapter Section fallback；
- cursor/locator malformed/stale fail closed；
- telemetry stderr only and no query/body content logging。

Release gate：

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 架构约束

禁止：

```text
parser/retriever/index → MCP DTO
cursor offset → source identity
snippet/score/index row → source identity
lexical token → TextLocator identity
```

## v0.1 非目标

- browser rendering；
- OCR；
- OAuth/Cookie interactive login；
- public multi-tenant service；
- enterprise product APIs；
- AI summary/QA/note taking；
- general vector/semantic RAG。
