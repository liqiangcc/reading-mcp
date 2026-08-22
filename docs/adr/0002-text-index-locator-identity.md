# ADR 0002: Text Index, Locator Identity, and Precise Reading

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/text-index-locator`
- Reviewed against main: `e4ec0ee5a39f6c549afcf17b68d6dfa7ebfe6198`
- Amended by: ADR 0004 for Tool-surface / ordered-enumeration decisions only
- Current implementation status: normalized identity/range, Paragraph/Sentence TextUnits, TextUnit enumeration, locator-driven context/read, shared locator resolution, direct SearchHit handoff, and canonical Section/Paragraph/Sentence lexical candidates are implemented.

## Context

Reading MCP treats `Document / Section` as canonical normalized source facts and TextUnit/Search structures as rebuildable derived state. Precise reading requires finer source addressing than Section without letting indexes, rendered streams, or retrieval rows redefine source identity.

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

Paragraph/Sentence identity must rebuild from:

```text
persisted canonical Document
+ segmentation_version
```

Current runtime persists Paragraph TextUnits; Sentence facts are deterministically materialized and do not require Sentence SQLite persistence for correctness.

### 3. Raw and normalized identity are separate

```text
content_hash
= retrieved source bytes provenance

normalized_document_hash
= addressing-relevant persisted normalized facts
```

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

Title-only SearchHit remains Section-level. Non-prose Paragraph may be a Paragraph candidate but never receives fake Sentence identity.

### 5. Normalized ranges

`normalized_range` is always relative to exact persisted owner `Section.content`:

```text
zero-based
half-open [start, end)
Unicode scalar positions
section-content-unicode-scalar/v1
```

Legacy parser/native offsets, legacy search-unit locations, and rendered/read-stream offsets are separate coordinate spaces.

### 6. Shared locator resolver

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

### 7. Cursor is not locator

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

### 8. Search candidate identity

Accepted and implemented candidate kinds:

```text
section | paragraph | sentence
```

Current precise lexical index derives them from canonical facts:

```text
Section title        → Section locator
Paragraph TextUnit   → Paragraph locator
eligible Sentence    → Sentence locator
```

SearchIndex row/snippet/score remains derived retrieval state. Every precise hit locator is revalidated against current canonical Document before handoff.

### 9. Segmentation and tokenization are independent

```text
normalized_document_hash + text-segmentation/v1
→ TextUnit / TextLocator identity

lexical-tokenizer/v1
→ lexical projection / matching only
```

Changing tokenizer policy rebuilds lexical state but must not renumber TextUnits or alter TextLocator identity.

Current tokenizer is deterministic/non-LLM and supports CJK/mixed technical text without whitespace-only assumptions.

### 10. Lexical index is versioned derived state

Current persistent search contract:

```text
lexical-search-index/v2
lexical-tokenizer/v1
```

SQLite rows store candidate kind + canonical TextLocator + tokenizer version + source order + encoded lexemes. Incompatible index/tokenizer versions invalidate only rebuildable lexical state.

If a canonical persisted Document exists but lexical rows are absent, `SearchDocumentUseCase` can rebuild the derived index from that Document and retry without retrieve/reparse.

Historical SearchIndex adapters remain runtime-compatible through Section-level fallback. An adapter that does not advertise an independently versioned precise lexical contract continues to use legacy `search()` plus canonical Section enrichment.

### 11. Backward compatibility

- existing MCP Tool count remains seven;
- existing `search_document(document_id, query, limit)` request is unchanged;
- old SearchHit preview fields remain;
- `candidate_kind + text_locator` are additive;
- legacy `Location` semantics remain legacy;
- historical Rust SearchIndex adapters that only implement `search()` continue to work at Section precision;
- precise-capable adapters may return Paragraph/Sentence candidates.

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
   - Section title candidates
   - canonical Paragraph candidates
   - canonical Sentence candidates
   - lexical-tokenizer/v1
   - CJK/mixed technical lexical terms
   - lexical-search-index/v2
   - persistent rebuild/migration semantics
```

## Acceptance invariants

1. Document/Section remain source truth.
2. Paragraph/Sentence identity rebuilds deterministically from persisted canonical facts + segmentation version.
3. Paragraph/Sentence ranges are exact Section.content slices.
4. Raw hash alone is not normalized TextUnit identity.
5. Cursor progress is not source identity.
6. Exact-read returned ranges reproduce response content exactly.
7. SearchHit locator flows directly to read/context.
8. Title-only Section search remains available without fake Paragraph identity.
9. Non-prose never gains fake Sentence identity.
10. Tokenizer changes cannot change TextUnit identity.
11. CJK/mixed technical search does not depend on whitespace-only splitting.
12. Search rows/snippets/legacy search-unit offsets never become canonical identity.
13. Search candidate kind must equal the resolved canonical locator kind.
14. Precise search locators are fail-closed validated against current Document.
15. Missing/incompatible derived lexical state can be rebuilt without source retrieval or reparsing.
16. Historical SearchIndex adapters remain usable through Section-level fallback.
17. Sentence persistence remains optional optimization, not source truth.

## Consequences

Positive:

- enumeration, search, read, and context share one source-addressing model;
- precise search can hand Paragraph/Sentence evidence directly into exact read/context;
- tokenizer/index changes are isolated from source identity;
- CJK and technical-token improvements remain rebuildable retrieval concerns;
- old adapter compatibility does not force fabricated fine-grained identity.

Costs:

- normalized identity, segmentation version, tokenizer version, and lexical-index version must all be maintained explicitly;
- SearchDocumentUseCase validates derived hits against DocumentRepository;
- persistent lexical state requires migration/rebuild logic;
- ranking policy remains a separate evidence-driven concern.

## Review outcome

Accepted. The implementation now realizes the originally accepted `section | paragraph | sentence` lexical candidate model without reusing historical paragraph-like search-unit boundaries as source identity. `lexical-tokenizer/v1` is explicitly independent of `text-segmentation/v1`, and legacy SearchIndex adapters retain a truthful Section-level compatibility path.
