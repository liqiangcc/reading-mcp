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

格式扩展、EPUB reconciliation、normalized block persistence 和 validator 都不增加格式专属 Tool。

## Precise-reading foundation

`open_document` additive 返回 raw/normalized identity：

```text
content_hash
normalized_document_hash
normalized-document-hash/v1
reading-mcp-normalization/v5
section-content-unicode-scalar/v1
```

Normalization version history relevant to EPUB/HTML：

```text
v2 → navigation-map parser facts added
v3 → navigation/spine reconciliation can change canonical Section facts
v4 → normalized-block-model/v1 persisted + inline HTML text normalization correction
v5 → epub-structure-validator/v1 persisted report + coverage evidence
```

`normalized-document-hash/v1` remains the addressing hash algorithm. Reconciliation changes its value naturally when existing Section facts change. Persisted block/validation metadata does not silently become an input to current hash/TextUnit identity while `text-segmentation/v1` remains active.

Paragraph/Sentence：

```text
Section.content
→ text-segmentation/v1
→ Paragraph exact ranges
→ deterministic Sentence ranges / ownership
```

Sentence persistence is not enumeration/context/read/search correctness dependency.

## Persisted normalized blocks

HTML/XHTML persists:

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

Ranges are generated while rendering exact `Section.content`; block text is never stored as a competing source copy. Heading remains canonical Section structure because heading label text is not in current Section body content.

The flat v1 projection emits maximal non-overlapping selected body blocks so nested selected descendants do not duplicate source text. Table cells receive semantic separation; inline DOM text is concatenated without adding synthetic spaces before punctuation.

For EPUB, HTML block owner IDs/native locations are remapped through the same spine Section-ID scheme before the final reconciled Document is validated and persisted.

Current identity boundary:

```text
normalized-block-model/v1 = persisted evidence
text-segmentation/v1      = current TextUnit identity policy
normalized-hash/v1        = current source-address hash contract
```

Block-aware Paragraph/Sentence identity remains a separately versioned migration.

## EPUB persisted-fact validator

Current validator/report contract:

```text
epub-structure-validator/v1
```

It consumes persisted facts only:

```text
epub-navigation-map/v1
epub-structure-reconciliation/v1
normalized-block-model/v1
canonical Document / Sections
current deterministic Paragraph/Sentence materialization
```

It does not reopen ZIP/DOM state.

Findings distinguish:

```text
error
→ internal persisted-fact contradiction
→ integrity=invalid
→ EpubParser fails closed

degradation
→ source/capability coverage incomplete but facts truthful
→ readable Document survives
```

Persisted report metadata:

```text
epub_validation_report_version
epub_validation_integrity
epub_validation_errors
epub_validation_degradations
epub_validation_report
```

Coverage keeps separate denominators for package/spine, navigation resolution, canonical structure provenance, normalized blocks, and current TextUnits. The report is reproducible after SQLite DocumentRepository reopen without source reparse.

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
        ↓
persisted-fact validation + coverage
→ epub-structure-validator/v1
```

Reconciliation rules remain:

- only navigation targets that map to real canonical Section boundaries may override title/parentage;
- non-heading DOM fragments do not fabricate Sections;
- navigation order never reverses canonical sibling/root order against the spine;
- multiple TOC aliases never duplicate canonical text;
- `linear=no` supported XHTML remains addressable and is tagged auxiliary;
- structural provenance remains explicit;
- missing/unsupported spine entries remain visible facts.

Validator rules add:

- Section IDs/parentage/order must be internally valid;
- structure facts must match canonical Section title/level/parent/native/spine evidence;
- Section `linear` must match its spine row;
- navigation resolution claims require matching evidence;
- normalized block and TextUnit range/ownership partitions are verified;
- source/capability gaps remain degradation findings rather than being erased;
- validator never repairs or fuzzy-rebases persisted facts.

## Default persistent state

```text
File Raw Cache
File Parsed Cache
SQLite DocumentRepository
  ├── normalized-block-model/v1 metadata
  └── epub-structure-validator/v1 report metadata
SQLite Paragraph TextUnitIndex
SQLite lexical-search-index/v2
```

No dedicated SQLite block/validator table is required: the map/report are serialized with persisted Document metadata and can be revalidated after repository reopen.

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
- inline punctuation/table normalization and nested-block de-duplication；
- normalized block SQLite reopen persistence；
- EPUB block owner/native-location remap；
- validator clean zero-error/degradation coverage fixture；
- unsupported spine/nav gaps remain readable degradations；
- persisted-fact tampering becomes validator integrity error；
- validator report + deterministic revalidation survive SQLite reopen；
- current Paragraph IDs/hash unchanged for block/validation metadata-only evidence；
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
validator → silent repair / fuzzy source rebase
validation report → source identity
```

## v0.1 非目标

- browser rendering；
- OCR；
- OAuth/Cookie interactive login；
- public multi-tenant service；
- enterprise product APIs；
- AI summary/QA/note taking；
- general vector/semantic RAG。
