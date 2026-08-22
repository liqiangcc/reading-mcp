# Paragraph TextUnit Index Contract

> Status: Implemented block-aware Paragraph contract
>
> Foundation branch: `feat/text-unit-index`
>
> Current identity branch: `feat/block-aware-text-unit-identity`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0005-block-aware-text-unit-identity.md`, `docs/normalized-text-range-contract.md`, `docs/normalized-block-model-contract.md`

## 1. Goal

Paragraph TextUnits are deterministic, exact, rebuildable reading/addressing units derived only from persisted canonical facts.

Current dependency chain：

```text
persisted Document / Section.content
+ optional valid normalized-block-model/v1
        ↓
normalized-document-hash/v2
+ text-segmentation/v2
        ↓
Paragraph TextUnits
        ↓
exact NormalizedTextRange
        ↓
TextUnitIndex / TextLocator / search
```

`Document / Section` remain source truth. `TextUnitIndex` remains rebuildable derived state.

## 2. Current Paragraph boundary policy

Current segmentation：

```text
text-segmentation/v2
```

### Native block projection

When a valid block map is present：

```text
paragraph    → exact Paragraph
blockquote   → typed coarse Paragraph-level unit
list_item    → typed coarse Paragraph-level unit
preformatted → coarse Paragraph-level unit
table        → coarse Paragraph-level unit
```

Every native Paragraph-level unit uses the exact persisted block range.

BlockQuote/ListItem are intentionally coarse under flat `normalized-block-model/v1`; nested leaf boundaries may have been suppressed and therefore cannot be reconstructed as canonical Paragraphs.

### Uncovered gaps

For source text not covered by native ranges：

```text
whitespace-only gap → separator coverage
non-whitespace gap  → deterministic blank-line fallback scoped to exact gap
```

Fallback Paragraphs retain v1 blank-line semantics within the gap：

- blank lines separate fallback Paragraphs;
- internal non-blank line endings are preserved;
- exact first/last content is retained;
- terminal separator line endings are not fabricated into Paragraph text.

Fallback ranges are translated back into owner-Section Unicode-scalar coordinates.

If no block map exists, the entire Section uses deterministic fallback. A declared invalid block map fails closed.

## 3. Versioning and identity

Current versions：

```text
normalized-document-hash/v2
text-segmentation/v2
text-unit-id/v1
```

`text-unit-id/v1` remains valid because its derivation algorithm already includes：

```text
document_id
normalized_document_hash
owner_section_id
kind = paragraph
1-based paragraph_index
normalized_range [start,end)
segmentation_version
```

Hash v2 and segmentation v2 therefore produce new IDs without redefining the ID derivation namespace.

Raw `content_hash` remains source provenance rather than an independent Paragraph-boundary identity input.

## 4. Paragraph model

```text
TextUnit
├── id
├── document_id
├── content_hash                  # raw provenance
├── normalized_document_hash      # v2 identity
├── owner_section_id
├── kind = paragraph
├── paragraph_index               # 1-based within owner Section
├── source_order                  # deterministic document order
├── normalized_range              # Section.content-relative
├── text                          # exact slice
└── segmentation_version          # text-segmentation/v2
```

Paragraph indices are assigned after native and fallback candidates are merged by exact Section source position.

`NormalizedBlock.block_index` is not reused as `paragraph_index` because fallback ranges can occur before/between/after native blocks.

Document `source_order` remains deterministic root/child traversal with Paragraphs in each Section's source order.

## 5. Exact-text invariant

For every Paragraph：

```text
unit.text
==
owner_section.normalized_text_slice(unit.normalized_range)
```

The materializer never trims or rewrites TextUnit text after deciding a range.

## 6. Coverage

Current per-Section Paragraph coverage：

```text
owner_chars
paragraph_chars
separator_chars
paragraph_count
native_paragraph_chars
native_structural_container_chars
native_non_prose_chars
fallback_chars
```

Accounting invariant：

```text
owner_chars = paragraph_chars + separator_chars
```

Native structural containers currently mean BlockQuote/ListItem under flat block evidence. Native non-prose means Preformatted/Table. Fallback classification remains separate.

Whitespace separators are factual source coverage, not fake Paragraphs or unsupported gaps.

## 7. Sentence eligibility handoff

Paragraph content class is derived from persisted native evidence when available, otherwise fallback text heuristics：

```text
NativeParagraph  → Sentence eligible
BlockQuote       → coarse only
ListItem         → coarse only
Preformatted     → coarse only
Table            → coarse only
fallback prose   → eligible
fallback code    → coarse only
fallback table   → coarse only
```

A Paragraph is always addressable even when it is not Sentence-eligible.

## 8. Fallible materialization boundary

Domain exposes：

```text
try_paragraph_text_units()
```

A declared corrupt/invalid block map returns an error. Capability boundaries such as `open_document`, `get_text_units`, locator resolution and lexical indexing use the fallible path so invalid persisted evidence cannot silently become fallback identity.

The infallible convenience wrapper remains for internal call sites that already operate on validated canonical Documents.

## 9. TextUnitIndex persistence

Application port：

```text
TextUnitIndex
├── replace_document(document_id, units)
└── list_document(document_id)
```

Adapters：

```text
InMemoryTextUnitIndex
SqliteTextUnitIndex
```

SQLite persists unit ID, source order, document/raw/normalized identity, owner, kind, paragraph index, exact range/text and segmentation version.

`replace_document` atomically replaces one document's derived Paragraph rows.

`open_document` validates current block-aware Paragraph materialization before replacing TextUnitIndex state.

## 10. Rebuildability

For the same persisted canonical Document and current segmentation policy：

```text
build(document).units
==
build(repository_round_trip(document)).units
```

No TextUnitIndex row is required to reconstruct `Section.content` or `NormalizedBlockMap`.

## 11. Acceptance evidence

Tests cover：

- exact native Paragraph ranges;
- BlockQuote/ListItem coarse Paragraph projection;
- Preformatted/Table coarse projection;
- mixed native/fallback source ordering;
- exact fallback offset translation;
- separator accounting;
- 1-based merged Paragraph ordinals;
- Unicode-scalar exact slicing;
- deterministic IDs under hash-v2/segmentation-v2;
- block kind/presence changing identity;
- provenance-only native location not changing hash;
- invalid declared block evidence fail closed;
- SQLite TextUnit persistence/replacement/reopen;
- old v1 locator/cursor stale migration;
- existing read/context/search/EPUB suites.

CI #876 passed the implementation head before final documentation-only synchronization：

```text
Format  success
Clippy  success
Test    success
```

## 12. Current non-goals

```text
nested/leaf BlockQuote/ListItem Paragraph identity
Sentence SQLite persistence
heading-title body ranges
SVG/fixed-layout precise blocks
block-specific MCP Tools
fuzzy source rebasing
```
