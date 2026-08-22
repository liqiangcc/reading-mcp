# ADR 0002: Text Index, Locator Identity, and Precise Reading

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/text-index-locator`
- Reviewed against main: `e4ec0ee5a39f6c549afcf17b68d6dfa7ebfe6198`
- Amended by: ADR 0004 for Tool-surface / ordered-enumeration decisions; ADR 0005 for block-aware identity migration.
- Current implementation status: normalized hash v2/ranges, block-aware Paragraph/Sentence TextUnits, TextUnit enumeration, locator-driven context/read, shared locator resolution, direct SearchHit handoff, and Section/Paragraph/eligible-Sentence lexical candidates are implemented.

## Context

Reading MCP treats `Document / Section` as canonical normalized source facts and TextUnit/Search structures as rebuildable derived state. Precise reading requires finer source addressing than Section without letting indexes, rendered streams, parser-transient objects, or retrieval rows redefine source identity.

## Decision

### 1. Addressing levels

```text
L0 Document
L1 StructuralNode / Section
L2 Paragraph
L3 Sentence
L4 CharacterRange
```

Word/model Token is not a stable source-addressing level.

### 2. TextUnits are deterministic derived state

Current runtime rebuild contract:

```text
persisted canonical Document / Section.content
+ optional valid normalized-block-model/v1
+ text-segmentation/v2
→ Paragraph / Sentence TextUnits
```

An absent block map uses deterministic fallback. A declared invalid block map fails closed rather than being silently ignored.

Current runtime persists Paragraph TextUnits; Sentence facts are deterministically materialized and do not require Sentence SQLite persistence for correctness.

### 3. Raw and normalized identity are separate

```text
content_hash
= retrieved source bytes provenance

normalized_document_hash
= addressing-relevant persisted normalized facts
```

Current normalized identity:

```text
normalized-document-hash/v2
```

Hash v2 includes canonical Section identity plus the block-map facts that can alter Paragraph/Sentence addressing:

```text
block-map presence/absence
block schema version
owner_section_id
block_index
source_order
kind
normalized_range
```

It excludes provenance/diagnostic-only facts such as native location/anchor, validator diagnostics/coverage and lexical state.

Paragraph/Sentence addressing is scoped by normalized identity + segmentation version. Raw `content_hash` remains provenance and is not silently redefined.

### 4. Unified TextLocator

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

Current producers/consumers:

```text
get_text_units
→ Section / Paragraph / Sentence locators

search_document
→ Section / Paragraph / Sentence candidate locators

read_document
← Section / Paragraph / Sentence / CharacterRange

get_context
← Section / Paragraph / Sentence
```

Title-only SearchHit remains Section-level. Coarse structural/non-prose content may remain Paragraph-level but never receives fake Sentence identity.

Native `blockquote/list_item/preformatted/table` do not become new locator kinds. They remain persisted block evidence projected into Paragraph/Sentence eligibility and coverage.

### 5. Normalized ranges

`normalized_range` is always relative to exact persisted owner `Section.content`:

```text
zero-based
half-open [start, end)
Unicode scalar positions
section-content-unicode-scalar/v1
```

Legacy parser/native offsets, legacy search-unit locations and rendered/read-stream offsets are separate coordinate spaces.

### 6. Block-aware Paragraph/Sentence policy

Current segmentation:

```text
text-segmentation/v2
```

Native mapping under flat `normalized-block-model/v1` evidence:

```text
paragraph    → exact sentence-eligible Paragraph
blockquote   → typed coarse Paragraph-level unit; no Sentence
list_item    → typed coarse Paragraph-level unit; no Sentence
preformatted → coarse Paragraph-level unit; no Sentence
table        → coarse Paragraph-level unit; no Sentence
```

BlockQuote/ListItem stay coarse because block-model/v1 is a flat maximal projection and may suppress nested leaf boundaries. This is evidence sufficiency, not a claim that quote/list text is inherently non-prose.

Uncovered gaps:

```text
whitespace-only → separator coverage
non-whitespace  → deterministic blank-line fallback scoped to exact gap
```

Fallback strong code/table heuristics apply only where native evidence is absent. No Sentence crosses a Paragraph boundary.

### 7. Shared locator resolver

Locator-consuming application paths share one resolver for:

```text
document_id
raw/normalized identity
owner Section
locator shape
Paragraph/Sentence ordinal
segmentation version
range equality
```

The resolver decides locator validity; each capability separately decides which valid locator kinds it accepts.

No fuzzy relocation or snippet-based identity repair is allowed.

The resolver uses fallible block-aware TextUnit materialization. Invalid persisted block evidence produces an application error instead of a process panic or fallback locator.

Historical v1 Paragraph/Sentence locators fail `STALE_LOCATOR` even when their old range still matches a current v2 range. Old normalized-hash-bound state also fails closed on normalized identity mismatch.

### 8. Cursor is not locator

```text
TextLocator    = source address
ReadCursor     = progress through a versioned read stream
TextUnitCursor = progress through a versioned enumeration stream
```

Cursor positions never become source ranges/citations.

Exact read uses:

```text
read_mode         = exact_target
rendering_version = exact-normalized-source/v1
```

and returns a separate `returned_locator` CharacterRange for each response segment.

TextUnitCursor already binds normalized hash + segmentation version; ReadCursor binds normalized identity. Historical v1 stream state therefore fails stale rather than resuming against a v2 stream.

### 9. Search candidate identity

Implemented candidate kinds:

```text
section | paragraph | sentence
```

Current precise lexical index derives them from canonical facts:

```text
Section title        → Section locator
Paragraph TextUnit   → Paragraph locator
eligible Sentence    → Sentence locator
```

Coarse BlockQuote/ListItem/Preformatted/Table regions are Paragraph candidates only.

SearchIndex row/snippet/score remains derived retrieval state. Every precise hit locator is revalidated against current canonical Document before handoff.

### 10. Segmentation and tokenization are independent

Current contracts:

```text
normalized-document-hash/v2 + text-segmentation/v2
→ TextUnit / TextLocator identity

lexical-tokenizer/v1
→ lexical projection / matching only
```

Changing tokenizer policy rebuilds lexical state but must not renumber TextUnits or alter TextLocator identity.

Current tokenizer is deterministic/non-LLM and supports CJK/mixed technical text without whitespace-only assumptions.

### 11. Lexical index is versioned derived state

Current persistent search contract:

```text
lexical-search-index/v3
lexical-tokenizer/v1
```

SQLite rows store candidate kind + canonical TextLocator + tokenizer version + source order + encoded lexemes. A semantic index-version mismatch invalidates only rebuildable lexical state.

The v2→v3 migration discards old precise locator rows and rebuilds from canonical persisted Document facts without source retrieval/reparse. Tokenizer remains v1.

If canonical persisted Document exists but lexical rows are absent, `SearchDocumentUseCase` can rebuild the derived index and retry.

Historical SearchIndex adapters remain runtime-compatible through truthful Section-level fallback when they do not advertise the precise lexical contract.

### 12. TextUnit persistence remains derived

`open_document` validates block-aware Paragraph materialization before replacing the derived TextUnitIndex.

Sentence SQLite persistence remains optional performance optimization rather than correctness/source truth. Sentence enumeration, read/context resolution and lexical candidate materialization can rebuild from canonical persisted facts.

### 13. Backward compatibility

- runtime Tool count remains seven;
- `search_document(document_id, query, limit)` request remains unchanged;
- existing SearchHit preview fields remain;
- `candidate_kind + text_locator` remain additive precise fields;
- legacy `Location` remains provenance/preview rather than canonical identity;
- historical Rust SearchIndex adapters remain usable at Section precision;
- identity migrations are fail-closed rather than caller-selectable historical modes.

## Implementation status

```text
P0 read-continuation                         ✓
P0 normalized-text-range                    ✓
P1 paragraph TextUnit index                 ✓
P1 sentence locator/coverage                ✓
P1 get_text_units + TextUnitCursor          ✓
P1 context granularity                      ✓
P1 exact TextLocator read                   ✓
P1 shared locator resolver                  ✓
P1 SearchHit → TextLocator                  ✓
P1 lexical-text-unit-index                  ✓
P1 block-aware TextUnit identity            ✓
   - normalized-document-hash/v2
   - text-segmentation/v2
   - native paragraph exact boundaries
   - blockquote/list_item coarse structural handling
   - pre/table coarse non-prose handling
   - native/fallback gap projection
   - old locator/cursor stale behavior
   - lexical-search-index/v3 rebuild
```

## Acceptance invariants

1. Document/Section remain source truth.
2. Paragraph/Sentence identity rebuilds deterministically from persisted canonical facts + segmentation version.
3. Paragraph/Sentence ranges are exact Section.content slices.
4. Raw hash alone is not normalized TextUnit identity.
5. Identity-bearing block facts are bound by normalized hash v2.
6. Provenance-only block fields do not redefine normalized identity.
7. Absent block evidence may use deterministic fallback; invalid declared evidence fails closed.
8. Cursor progress is not source identity.
9. Exact-read returned ranges reproduce response content exactly.
10. SearchHit locator flows directly to read/context.
11. Title-only Section search remains available without fake Paragraph identity.
12. Coarse structural/non-prose content never gains fake Sentence identity.
13. Tokenizer changes cannot change TextUnit identity.
14. CJK/mixed technical search does not depend on whitespace-only splitting.
15. Search rows/snippets/legacy search-unit offsets never become canonical identity.
16. Search candidate kind must equal the resolved canonical locator kind.
17. Precise search locators are fail-closed validated against current Document.
18. Missing/incompatible derived lexical state can rebuild without source retrieval/reparse.
19. Historical SearchIndex adapters remain usable through Section-level fallback.
20. Sentence persistence remains optional optimization, not source truth.
21. Identity migration never fuzzy-rebases old locators/cursors.

## Consequences

Positive:

- enumeration, search, read and context share one source-addressing model;
- EPUB/HTML native Paragraph evidence can improve precision without transient parser state becoming canonical;
- flat composite containers are preserved truthfully rather than over-claimed;
- precise search can hand canonical Paragraph/Sentence evidence directly into exact read/context;
- tokenizer/index changes remain isolated rebuildable concerns;
- old adapter compatibility does not force fabricated fine-grained identity.

Costs:

- normalized identity, segmentation version, tokenizer version and lexical-index version must all be maintained explicitly;
- v2 normalized identity intentionally stales old normalized-hash-bound state;
- persistent lexical state requires migration/rebuild semantics;
- quote/list Sentence precision remains deferred until stronger nested/leaf evidence exists;
- ranking remains a separate evidence-driven optimization problem.

## Review outcome

Accepted and current. ADR 0005 has now been implemented: persisted native block evidence is an identity-bearing segmentation input; `normalized-document-hash/v2` binds the addressing-relevant block projection; `text-segmentation/v2` materializes native/fallback Paragraphs conservatively; historical precise state fails stale; and persistent lexical state rebuilds under `lexical-search-index/v3` while `lexical-tokenizer/v1` remains independent.
