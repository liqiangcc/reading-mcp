# Phase 6：MCP stdio Server 与真实调用验证

## 目标

通过真实 MCP stdio transport 暴露 Application Use Cases，同时保持：

```text
rmcp only in MCP adapter/binary
mcp → application → domain
infrastructure/retrieval/parsing/security → ports
```

## 8 个 Tool

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
get_source_view
```

格式扩展、EPUB reconciliation、normalized block persistence、validator 和 block-aware identity migration 都不增加格式专属 Tool。

## Precise-reading foundation

`open_document` additive 返回 raw/normalized identity：

```text
content_hash
normalized_document_hash
normalized-document-hash/v2
reading-mcp-normalization/v6
section-content-unicode-scalar/v1
```

Normalization version history relevant to EPUB/HTML：

```text
v2 → navigation-map parser facts added
v3 → navigation/spine reconciliation can change canonical Section facts
v4 → normalized-block-model/v1 persisted + inline HTML text normalization correction
v5 → epub-structure-validator/v1 persisted report + coverage evidence
v6 → block-aware segmentation v2 changes persisted validator TextUnit coverage
```

`CachingParser` returns Parsed Cache hits without rerunning parser/validator work. Because the persisted EPUB validation report contains current Paragraph/Sentence coverage, v5 cache entries must miss once TextUnit semantics advance to v2. Raw Resource Cache remains independently reusable.

Current precise identity：

```text
normalized-document-hash/v2
+ text-segmentation/v2
+ identity-bearing normalized-block-model/v1 projection
→ Paragraph / Sentence / TextLocator identity
```

Hash v2 binds canonical Section facts plus block-map presence/schema/owner/index/source-order/kind/range. It excludes native location/anchor, validator diagnostics/coverage and lexical state.

Paragraph/Sentence materialization：

```text
Section.content
+ optional valid normalized-block-model/v1
→ text-segmentation/v2
→ block-aware Paragraph exact ranges
→ deterministic eligible Sentence ranges / ownership
```

Sentence persistence is not enumeration/context/read/search correctness dependency.

## Persisted normalized blocks and v2 projection

HTML/XHTML persists：

```text
normalized-block-model/v1
```

Current body kinds：

```text
paragraph
blockquote
list_item
preformatted
table
```

Every block carries owner Section, 1-based block index, parser/source order, exact Section-relative normalized range, native anchor/location and `xhtml_native_block` provenance.

Ranges are generated while rendering exact `Section.content`; block text is never stored as a competing source copy. Heading remains canonical Section structure because heading label text is not in Section body content.

The flat v1 projection emits maximal non-overlapping selected body blocks. Therefore nested leaf boundaries may be suppressed inside BlockQuote/ListItem.

Current v2 projection：

```text
paragraph    → exact sentence-eligible Paragraph
blockquote   → typed coarse Paragraph-level item; no Sentence
list_item    → typed coarse Paragraph-level item; no Sentence
preformatted → coarse Paragraph-level item; no Sentence
table        → coarse Paragraph-level item; no Sentence
```

BlockQuote/ListItem stay coarse because flat v1 evidence may hide mixed nested `<p>/<pre>/<table>` boundaries; this is evidence sufficiency rather than a semantic non-prose claim.

Uncovered Section gaps：

```text
whitespace-only → separator coverage
non-whitespace  → deterministic fallback Paragraph segmentation scoped to gap
```

Fallback fenced/indented-code and Markdown-table heuristics apply only where native evidence is absent.

Declared invalid block metadata fails TextUnit/search materialization closed. An absent block map remains a supported deterministic fallback.

For EPUB, HTML block owner IDs/native locations are remapped through the spine Section-ID scheme before final reconciled Document validation/persistence.

## EPUB persisted-fact validator

Current validator/report contract：

```text
epub-structure-validator/v1
```

It consumes persisted facts only：

```text
epub-navigation-map/v1
epub-structure-reconciliation/v1
normalized-block-model/v1
canonical Document / Sections
current deterministic text-segmentation/v2 Paragraph/Sentence materialization
```

It does not reopen ZIP/DOM state.

Findings distinguish：

```text
error
→ internal persisted-fact contradiction
→ integrity=invalid
→ EpubParser fails closed

degradation
→ source/capability coverage incomplete but facts truthful
→ readable Document survives
```

Persisted report metadata：

```text
epub_validation_report_version
epub_validation_integrity
epub_validation_errors
epub_validation_degradations
epub_validation_report
```

Coverage keeps separate denominators for package/spine, navigation resolution, canonical structure provenance, normalized blocks and current TextUnits. The report remains reproducible after SQLite DocumentRepository reopen without source reparse.

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

`preserve_source` 返回 coarse structural/non-prose Paragraph，不伪造 Sentence。当前 coverage 区分：

```text
sentence_eligible_paragraphs
coarse_structural_paragraphs
non_prose_paragraphs
coarse_structural_items
coarse_non_prose_items
intentionally_skipped
```

BlockQuote/ListItem Sentence-first degradation：

```text
flat_native_container_no_nested_textunit_evidence
```

`eligible_only` 可跳过 coarse items，因此不宣称 all-source completion。

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

Paragraph/Sentence resolution通过 fallible block-aware materialization；损坏 persisted block evidence 返回应用错误而不是 panic。

Historical v1 Paragraph/Sentence locator 即使 range 仍相同也返回 `STALE_LOCATOR`。旧 TextUnitCursor 返回 `STALE_CURSOR`；旧 normalized-hash-bound state 通过 identity mismatch stale。

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

Response separates logical target, source range and stream progress：

```text
resolved_target_locator
returned_locator
stream.start_char / end_char
```

## Canonical lexical search

Current precise retrieval contract：

```text
lexical-search-index/v3
lexical-tokenizer/v1
```

Candidate builder is shared by InMemory and SQLite and uses fallible current TextUnit materialization：

```text
Section title            → Section TextLocator
Paragraph TextUnit       → Paragraph TextLocator
eligible Sentence        → Sentence TextLocator
coarse structural/nonprose → Paragraph candidate only
```

Tokenizer version remains independent of segmentation identity.

SQLite semantic v3 persists candidate kind + canonical TextLocator + tokenizer/index version + source order + encoded lexemes. Old semantic v2 metadata invalidates derived rows; v3 rebuilds from persisted canonical Document without source retrieval/reparse. Physical table shape need not change solely because semantic locator identity changed.

Historical SearchIndex adapters remain compatible through truthful Section-level fallback.

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
normalized-document-hash/v2 + text-segmentation/v2
→ current block-aware TextUnits
        ↓
persisted-fact validation + coverage
→ epub-structure-validator/v1
```

Reconciliation rules remain：

- only navigation targets that map to real canonical Section boundaries may override title/parentage;
- non-heading DOM fragments do not fabricate Sections;
- navigation order never reverses canonical sibling/root order against the spine;
- multiple TOC aliases never duplicate canonical text;
- `linear=no` supported XHTML remains addressable and tagged auxiliary;
- structural provenance remains explicit;
- missing/unsupported spine entries remain visible facts.

Validator rules include：

- Section IDs/parentage/order internally valid;
- structure facts match canonical Section title/level/parent/native/spine evidence;
- Section `linear` matches spine row;
- navigation resolution claims require matching evidence;
- normalized block and current TextUnit range/ownership partitions are verified;
- source/capability gaps remain degradations rather than erased;
- validator never repairs or fuzzy-rebases persisted facts.

## Default persistent state

```text
File Raw Cache
File Parsed Cache (reading-mcp-normalization/v6)
SQLite DocumentRepository
  ├── normalized-block-model/v1 metadata
  └── epub-structure-validator/v1 report metadata
SQLite Paragraph TextUnitIndex
SQLite lexical-search-index/v3
```

No dedicated SQLite block/validator table is required: map/report serialize with persisted Document metadata and can be revalidated after repository reopen.

## 真实 stdio / release acceptance

测试覆盖：

- 7 Tool discovery；
- raw/normalized hash-v2 identity；
- normalization-v5 Parsed Cache miss under v6；
- block-map identity/provenance separation；
- block-aware Paragraph materialization；
- native Paragraph exact Sentence eligibility；
- BlockQuote/ListItem flat-container coarse preservation；
- native pre/table zero fake Sentences；
- native + fallback gap offset/order accounting；
- invalid declared block evidence fail closed；
- TextUnit deterministic rebuild / cursor continuation；
- old v1 locator → `STALE_LOCATOR`；
- old v1 TextUnitCursor → `STALE_CURSOR`；
- locator-driven context；
- exact read + truthful returned ranges；
- SearchHit Section/Paragraph/Sentence locator handoff；
- CJK/technical lexical retrieval；
- lexical v2 → v3 SQLite invalidation/rebuild；
- EPUB nav/NCX resolution/degradation；
- EPUB canonical reconciliation and spine-order conflict handling；
- `linear=no` preservation；
- non-heading-fragment no-fabrication；
- normalized block SQLite reopen persistence；
- EPUB block owner/native-location remap；
- validator clean/degradation/tamper/reopen evidence；
- cursor/locator malformed/stale fail closed；
- telemetry stderr only and no query/body content logging。

Implementation CI #898：

```text
Format  success
Clippy  success
Test    success
```

Final release gate remains：

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
transient DOM block boundary → TextUnit identity
normalized block text copy → second source of truth
invalid declared block evidence → silent fallback
flat BlockQuote/ListItem → fabricated nested Sentence precision
old locator/cursor → fuzzy rebase
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
