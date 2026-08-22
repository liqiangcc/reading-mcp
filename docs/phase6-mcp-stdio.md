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

格式扩展、EPUB reconciliation 和 normalized block persistence 都不增加格式专属 Tool。

## Precise-reading foundation

`open_document` additive 返回 raw/normalized identity：

```text
content_hash
normalized_document_hash
normalized-document-hash/v1
reading-mcp-normalization/v4
section-content-unicode-scalar/v1
```

Normalization version history relevant to EPUB/HTML：

```text
v2 → navigation-map parser facts added
v3 → navigation/spine reconciliation can change canonical Section facts
v4 → normalized-block-model/v1 persisted + inline HTML text normalization correction
```

`normalized-document-hash/v1` remains the addressing hash algorithm. Reconciliation changes its value naturally when existing Section facts change. Persisted block metadata does not silently become an input to current hash/TextUnit identity while `text-segmentation/v1` remains active.

Paragraph/Sentence：

```text
Section.content
→ text-segmentation/v1
→ Paragraph exact ranges
→ deterministic Sentence ranges / ownership
```

Sentence persistence is not enumeration/context/read/search correctness dependency.

## Persisted normalized blocks

HTML/XHTML now also persists:

```text
normalized-block-model/v1
```

Current body kinds:

```text
paragraph
blockquote
list_item
preformatted
table
```

Every block carries owner Section, 1-based block index, parser/source order, exact Section-relative normalized range, native anchor/location and `xhtml_native_block` provenance.

Ranges are generated while rendering the exact `Section.content`; block text is never stored as a competing source copy. Heading remains canonical Section structure because heading label text is not in current Section body content.

The flat v1 projection emits maximal non-overlapping selected body blocks so nested `<p>` inside `<blockquote>` etc. does not duplicate source text. Table cells receive semantic separation; inline DOM text is concatenated without adding synthetic spaces before punctuation.

For EPUB, HTML block owner IDs/native locations are remapped through the same spine Section-ID scheme before the final reconciled Document is validated and persisted.

Current identity boundary:

```text
normalized-block-model/v1 = persisted evidence
text-segmentation/v1      = current TextUnit identity policy
normalized-hash/v1        = current source-address hash contract
```

Block-aware Paragraph/Sentence identity is a future separately versioned migration.

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

Tokenizer version is independent of segmentation identity. SQLite v2 persists candidate kind + canonical TextLocator + tokenizer/index version + source order + encoded lexemes. Missing/incompatible lexical state can be rebuilt from persisted canonical Document without source retrieval/reparse.

Historical SearchIndex adapters remain compatible through Section-level fallback.

## Direct SearchHit handoff

```text
search_document
→ SearchHit(candidate_kind + text_locator)
        ├→ read_document(target_locator)
        └→ get_context(target_locator, relation)
```

SearchDocumentUseCase revalidates candidate identity against canonical Document before returning a precise hit.

## EPUB navigation + structure foundation

```text
EPUB package / manifest
→ properties=nav discovery
→ EPUB 3 toc nav hierarchy
→ legacy NCX fallback
→ href / fragment resolution diagnostics
→ epub-navigation-map/v1
        ↓
spine-authoritative source order
+ publisher nav/NCX labels/hierarchy
+ XHTML heading fallback
+ spine-item fallback
→ epub-structure-reconciliation/v1
→ canonical Section tree
        ↓
HTML/XHTML native body blocks
→ normalized-block-model/v1 exact ranges
```

Reconciliation rules remain:

- only navigation targets that map to real canonical Section boundaries may override title/parentage;
- non-heading DOM fragments do not fabricate Sections;
- navigation order never reverses canonical sibling/root order against the spine;
- multiple TOC aliases never duplicate canonical text;
- `linear=no` supported XHTML remains addressable and is tagged auxiliary;
- structural provenance remains explicit;
- missing/unsupported spine entries remain visible facts.

The next EPUB increment is `feat/epub-structure-validator`, which can now validate stable package/navigation/reconciliation/block facts and produce coverage evidence.

## Default persistent state

```text
File Raw Cache
File Parsed Cache
SQLite DocumentRepository
  └── includes reserved normalized-block-model/v1 metadata
SQLite Paragraph TextUnitIndex
SQLite lexical-search-index/v2
```

No dedicated SQLite block table is required in v1 because the full canonical block map is serialized with persisted Document metadata and revalidated after repository reopen.

## 真实 stdio / release acceptance

测试覆盖：

- 7 Tool discovery；
- raw/normalized identity；
- TextUnit deterministic rebuild / cursor continuation；
- non-prose/eligible-only coverage；
- locator-driven context；
- exact read + truthful returned ranges；
- SearchHit Section/Paragraph/Sentence locator handoff；
- CJK/technical lexical retrieval；
- SQLite lexical reopen/rebuild；
- EPUB nav/NCX resolution/degradation；
- EPUB canonical reconciliation and spine-order conflict handling；
- `linear=no` preservation；
- non-heading-fragment no-fabrication；
- normalized body-block kind/exact range generation；
- inline punctuation and table text normalization；
- nested-block de-duplication；
- normalized block SQLite reopen persistence；
- EPUB block owner/native-location remap；
- current Paragraph IDs/hash unchanged for block-metadata-only removal；
- normalization-version Parsed Cache invalidation；
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
publisher navigation order → implicit spine source reorder
transient DOM block boundary → silent TextUnit identity change
normalized block text copy → second source of truth
```

## v0.1 非目标

- browser rendering；
- OCR；
- OAuth/Cookie interactive login；
- public multi-tenant service；
- enterprise product APIs；
- AI summary/QA/note taking；
- general vector/semantic RAG。
