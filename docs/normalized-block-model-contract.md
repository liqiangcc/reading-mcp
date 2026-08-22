# Normalized Block Model Contract

> Status: Implemented canonical block foundation; consumed by block-aware TextUnit identity
>
> Block foundation branch: `feat/normalized-block-model`
>
> Identity consumer branch: `feat/block-aware-text-unit-identity`
>
> Related: `docs/adr/0003-epub-first-structure-reliability.md`, `docs/adr/0005-block-aware-text-unit-identity.md`, `docs/epub-structure-reconciliation-contract.md`, `docs/normalized-text-range-contract.md`, `docs/epub-structure-validator-contract.md`, `docs/block-aware-text-unit-identity-migration.md`

## 1. Goal

`normalized-block-model/v1` preserves native HTML/XHTML body-block evidence after parsing so Paragraph/Sentence policy can rebuild from persisted canonical facts rather than transient DOM state.

The contract separates：

```text
Section hierarchy
= canonical heading/chapter structure

Section.content
= canonical normalized body text owned by one Section

NormalizedBlockMap
= persisted typed exact ranges into Section.content
```

A block row never becomes a competing source copy. Its text is always obtained by slicing owner `Section.content` with `normalized_range`.

## 2. Current versions

```text
normalized-block-model/v1
reading-mcp-normalization/v5
normalized-document-hash/v2
text-segmentation/v2
```

History：

```text
normalization v4
→ block-map persistence introduced

normalization v5
→ EPUB persisted-fact validator/coverage added

hash v2 + segmentation v2
→ identity-bearing block evidence becomes current TextUnit input
```

Parser/cache normalization remains v5 because the later TextUnit migration changes normalized address/derived identity rather than parser-output policy.

## 3. Persisted shape

Each block contains：

```text
owner_section_id
block_index          # 1-based within owner Section
source_order         # global parser/source order
kind
normalized_range     # exact Section-relative Unicode-scalar [start,end)
native_anchor?
native_location?
provenance
```

Current provenance：

```text
xhtml_native_block
```

Current body-block kinds：

```text
paragraph
blockquote
list_item
preformatted
table
```

## 4. Heading boundary

`heading` is not emitted as a body `NormalizedBlock` in v1.

Heading evidence remains canonical Section structure：

```text
Section.title
Section.id / parent_id / level
Section.location.anchor / native_location
```

`Section.content` owns body text rather than heading label text, so creating a heading body range would fabricate a false exact slice.

## 5. Exact-range generation

Ranges are generated while rendering the same string that becomes `Section.content`：

```text
native block
  ↓ normalize block text
append to Section.content
  ↓
record scalar start/end
  ↓
NormalizedBlock.normalized_range
```

The parser never renders all content and then substring-searches to guess offsets.

Invariant：

```text
Section.normalized_text_slice(block.normalized_range)
== exact persisted normalized block text
```

## 6. HTML/XHTML normalization

Ordinary inline descendant text is concatenated in DOM text order before whitespace collapse; the normalizer does not inject a synthetic space at every inline element boundary.

`pre` retains internal line breaks under the current normalization boundary.

`table` emits one coarse Table range and inserts semantic whitespace between normalized cells.

## 7. Flat nested-block boundary

`normalized-block-model/v1` is flat and maximal non-overlapping：

```text
outer selected block
└── nested selected block

→ emit outer block once
→ suppress nested overlapping rows
```

A concrete fixture：

```html
<blockquote>
  <p>Quoted text.</p>
  <p>Second paragraph.</p>
</blockquote>
```

persists one BlockQuote range and may normalize to：

```text
Quoted text.Second paragraph.
```

The suppressed inner Paragraph boundary is no longer a persisted fact. `text-segmentation/v2` therefore treats flat BlockQuote/ListItem evidence conservatively as one typed coarse Paragraph-level unit with no Sentence children.

A future nested/leaf block contract is required before quote/list internals can gain finer source identity.

## 8. Source order vs reconciled hierarchy

`NormalizedBlock.source_order` is parser/source order.

For EPUB：

```text
spine order
→ XHTML document order
→ emitted block order
```

It remains independent from reconciled Section-tree DFS because publisher navigation may reparent Sections while spine remains authoritative for source order.

## 9. EPUB remapping

Each spine XHTML document is parsed with the same HTML block contract. EPUB remaps block owners through canonical spine-qualified Section IDs：

```text
section://<html-id>
→ section://epub-<spine-index>/<html-id>
```

Native locations become EPUB-qualified provenance such as：

```text
epub:OPS/chapter.xhtml#p1
```

Reconciliation may change title/parentage/level/path, but not the Section ID/body content that owns block ranges.

## 10. Persistence and validation

Reserved Document metadata keys：

```text
normalized_block_map_version
normalized_blocks
normalized_block_map
```

`Document::normalized_block_map()` decodes and validates canonical interpretation.

Validation checks：

- schema version;
- contiguous global source order;
- owner Section existence;
- contiguous 1-based block index per owner;
- exact range bounds;
- non-empty ranges;
- no overlap/reorder within one owner Section.

No separate SQLite table is required because DocumentRepository already persists complete Document metadata. Reopen tests prove block facts survive and revalidate.

## 11. Current TextUnit projection

`text-segmentation/v2` consumes valid block-model/v1 evidence：

```text
paragraph    → exact sentence-eligible Paragraph
blockquote   → typed coarse Paragraph-level item, no Sentence
list_item    → typed coarse Paragraph-level item, no Sentence
preformatted → coarse Paragraph-level item, no Sentence
table        → coarse Paragraph-level item, no Sentence
```

Uncovered ranges：

```text
whitespace-only → separator coverage
non-whitespace  → deterministic fallback Paragraph segmentation scoped to exact gap
```

Native evidence outranks fallback text heuristics.

Paragraph ordinals are assigned after native + fallback candidates are merged by exact Section source position. Native `block_index` is not reused as `paragraph_index`.

## 12. Current normalized identity projection

`normalized-document-hash/v2` binds block facts that can change TextUnit addressing：

```text
block-map presence/absence
schema version
owner_section_id
block_index
source_order
kind
normalized_range
```

It excludes provenance/diagnostic-only facts：

```text
native_anchor
native_location
validator report / diagnostics / coverage
lexical state
```

Tests prove：

- block-map presence affects hash/TextUnit identity;
- block kind changes affect hash and eligibility;
- native-location-only changes do not affect normalized hash.

## 13. Invalid/absent evidence semantics

```text
absent block map
→ supported deterministic fallback

declared invalid/corrupt block map
→ fail closed for block-aware TextUnit materialization
```

The application exposes fallible TextUnit materialization at capability boundaries so malformed persisted block evidence does not become a panic or silent fallback locator/search row.

## 14. EPUB validator interaction

`epub-structure-validator/v1` consumes persisted block facts plus current deterministic TextUnit facts and records factual coverage such as：

```text
blocks by kind
Sections with / without native blocks
Section content chars
block chars
separator-or-unmodeled chars
block ↔ current Paragraph agreement
native non-prose/current Sentence overlap
```

Integrity contradictions are errors; source/capability gaps are degradations. The validator never clamps, searches replacement text, or fuzzy-rebases source facts.

## 15. Acceptance evidence

Coverage includes：

- Paragraph / BlockQuote / ListItem / Preformatted / Table kinds;
- exact Unicode-scalar Section-relative slices;
- inline punctuation without synthetic spaces;
- semantic table cell separation;
- headingless multi-block source order;
- nested block de-duplication;
- nested BlockQuote no-fabricated child Paragraph/Sentence identity;
- native/fallback mixed source-order projection;
- pre/table zero fake Sentences;
- invalid declared block evidence fail-closed behavior;
- SQLite DocumentRepository reopen persistence;
- EPUB owner/native-location remapping;
- hash-v2 block sensitivity/provenance exclusion;
- old locator/cursor stale migration;
- lexical-index/v3 rebuild;
- validator deterministic reopen coverage.

CI #876 passed the implementation head before docs-only synchronization：

```text
Format  success
Clippy  success
Test    success
```

## 16. Explicit non-goals

```text
nested/leaf block-tree identity
heading-title CharacterRange coordinates
SVG/fixed-layout precise blocks
Sentence SQLite persistence
block-specific MCP Tools
fuzzy source rebasing
semantic/vector retrieval
```

Future nested/leaf precision requires stronger persisted block evidence and another explicit identity migration.
