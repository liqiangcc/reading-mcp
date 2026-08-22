# ADR 0002: Text Index, Locator Identity, and Precise Reading

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/text-index-locator`
- Reviewed against main: `e4ec0ee5a39f6c549afcf17b68d6dfa7ebfe6198`
- Amended by: ADR 0004 for Tool-surface / ordered-enumeration decisions; ADR 0005 for the accepted block-aware identity migration.
- Current implementation status: normalized identity/range, Paragraph/Sentence TextUnits, TextUnit enumeration, locator-driven context/read, shared locator resolution, direct SearchHit handoff, and canonical Section/Paragraph/Sentence lexical candidates are implemented under v1 identity. ADR 0005 v2 identity is accepted but not yet implemented.

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

Paragraph/Sentence identity must rebuild from persisted canonical normalized facts plus the current segmentation policy.

Current runtime:

```text
persisted canonical Document
+ text-segmentation/v1
```

Accepted ADR 0005 target:

```text
persisted canonical Document
+ optional valid normalized-block-model/v1
+ text-segmentation/v2
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

Current runtime uses `normalized-document-hash/v1` over canonical Section facts. ADR 0005 accepts `normalized-document-hash/v2`, which additionally binds the identity-bearing native block projection because segmentation v2 depends on it.

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

ADR 0005 does not add block-specific locator kinds. Native `blockquote/list_item/preformatted/table` remain persisted block evidence projected into Paragraph/Sentence policy and coverage.

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

Under ADR 0005, v1 Paragraph/Sentence locators become `STALE_LOCATOR` after the v2 migration even when text/range happens to match. Because normalized-document identity itself changes, old Section/CharacterRange locators also fail closed on the old normalized hash.

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

ADR 0005 keeps the existing cursor claim shapes unless implementation needs new fields: current TextUnitCursor already binds normalized hash + segmentation version, and ReadCursor already binds normalized identity. Old state therefore fails stale rather than resuming against a v2 stream.

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

ADR 0005 requires persistent lexical state to advance from `lexical-search-index/v2` to `lexical-search-index/v3` when v2 TextUnit identity lands, because stored Paragraph/Sentence locators become stale. `lexical-tokenizer/v1` remains unchanged.

### 9. Segmentation and tokenization are independent

Current runtime:

```text
normalized-document-hash/v1 + text-segmentation/v1
→ TextUnit / TextLocator identity

lexical-tokenizer/v1
→ lexical projection / matching only
```

Accepted next identity migration:

```text
normalized-document-hash/v2 + text-segmentation/v2
→ block-aware TextUnit / TextLocator identity

lexical-tokenizer/v1
→ unchanged lexical projection / matching policy
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

ADR 0005 extends this rule: segmentation/normalized-identity migration also invalidates stored precise lexical rows, so v2 index state is rebuilt as v3 while tokenizer v1 remains independent.

### 11. Backward compatibility

- existing MCP Tool count remains seven;
- existing `search_document(document_id, query, limit)` request is unchanged;
- old SearchHit preview fields remain;
- `candidate_kind + text_locator` are additive;
- legacy `Location` semantics remain legacy;
- historical Rust SearchIndex adapters that only implement `search()` continue to work at Section precision;
- precise-capable adapters may return Paragraph/Sentence candidates.

Identity migrations are fail-closed rather than wire-mode compatible: callers cannot request old segmentation interpretation after the server advances to v2.

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

Accepted next migration (ADR 0005)            design ✓ / runtime pending
   - normalized-document-hash/v2
   - text-segmentation/v2
   - native paragraph/blockquote/list-item boundaries
   - native pre/table coarse Sentence policy
   - fallback gaps + coverage
   - old locator/cursor stale behavior
   - lexical-search-index/v3 rebuild
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
18. Once block facts drive segmentation, identity-bearing block kind/range/order must be bound by normalized identity.
19. Absent block evidence may degrade to deterministic fallback; invalid declared block evidence must not be silently ignored.
20. Identity migration never fuzzy-rebases old locators/cursors.

## Consequences

Positive:

- enumeration, search, read, and context share one source-addressing model;
- precise search can hand Paragraph/Sentence evidence directly into exact read/context;
- tokenizer/index changes are isolated from source identity;
- CJK and technical-token improvements remain rebuildable retrieval concerns;
- old adapter compatibility does not force fabricated fine-grained identity;
- the accepted v2 migration can consume persisted native EPUB/HTML structure without making transient parser state canonical.

Costs:

- normalized identity, segmentation version, tokenizer version, and lexical-index version must all be maintained explicitly;
- SearchDocumentUseCase validates derived hits against DocumentRepository;
- persistent lexical state requires migration/rebuild logic;
- ranking policy remains a separate evidence-driven concern;
- ADR 0005 intentionally stales old normalized-hash-bound locator/cursor state when v2 lands.

## Review outcome

Accepted. The implementation now realizes the originally accepted `section | paragraph | sentence` lexical candidate model without reusing historical paragraph-like search-unit boundaries as source identity. `lexical-tokenizer/v1` is explicitly independent of `text-segmentation/v1`, and legacy SearchIndex adapters retain a truthful Section-level compatibility path.

ADR 0005 additionally accepts the next block-aware identity migration: native block evidence becomes a versioned segmentation input, `normalized-document-hash/v2` binds the identity-bearing block projection, old precise state fails stale, and persistent lexical rows rebuild under `lexical-search-index/v3`. That migration remains pending implementation.
