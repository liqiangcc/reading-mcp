# Normalized Block Model Contract

> Status: Implemented P1 canonical block foundation
>
> Branch: `feat/normalized-block-model`
>
> Related: `docs/adr/0003-epub-first-structure-reliability.md`, `docs/epub-structure-reconciliation-contract.md`, `docs/normalized-text-range-contract.md`, `docs/epub-structure-validator-contract.md`, `docs/block-aware-text-unit-identity-migration.md`

## 1. Goal

`normalized-block-model/v1` preserves native HTML/XHTML body-block evidence after parsing so later Paragraph/Sentence policy does not depend on transient DOM state.

The contract separates three facts:

```text
Section hierarchy
= canonical heading/chapter structure

Section.content
= canonical normalized body text owned by one Section

NormalizedBlockMap
= persisted typed exact ranges into Section.content
```

A block row never becomes a second copy of source text. Its text is always obtained by slicing its owner `Section.content` with `normalized_range`.

## 2. Version

Current block schema:

```text
normalized-block-model/v1
```

The block model was introduced under parser/cache policy:

```text
reading-mcp-normalization/v4
```

Current parser/cache policy after the subsequent EPUB validator increment is:

```text
reading-mcp-normalization/v5
```

Current implemented source-address identity remains:

```text
normalized-document-hash/v1
text-segmentation/v1
```

The v4 normalization bump invalidated Parsed Cache entries created before persisted block facts existed. The subsequent v5 bump adds persisted validator/coverage output without changing the block schema or current TextUnit identity.

ADR 0005 has accepted the next migration target:

```text
normalized-document-hash/v2
text-segmentation/v2
```

That target is designed but not yet implemented on `main`.

## 3. Persisted shape

Each block contains:

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

Current provenance:

```text
xhtml_native_block
```

Current body-block kinds:

```text
paragraph
blockquote
list_item
preformatted
table
```

## 4. Heading boundary

`heading` is deliberately not emitted as a body `NormalizedBlock` in v1.

The current canonical model stores heading evidence as:

```text
Section.title
Section.id / parent_id / level
Section.location.anchor / native_location
```

and `Section.content` contains the body owned by that heading rather than the heading label itself. Creating a heading block with a range into body text would therefore fabricate a false exact slice.

A future canonical representation may add a separate heading-title coordinate contract, but v1 does not pretend one exists.

## 5. Exact-range generation

Ranges are generated while the parser renders the same text that becomes `Section.content`:

```text
native block
  ↓ normalize block text
append to Section.content
  ↓
record scalar start/end
  ↓
NormalizedBlock.normalized_range
```

The parser does not:

```text
render Section.content
→ search for block text afterward
→ guess offsets
```

This matters for repeated text, Unicode, punctuation and whitespace.

Required invariant for every block:

```text
Section.normalized_text_slice(block.normalized_range)
== exact persisted normalized block text
```

## 6. HTML/XHTML text policy

For ordinary inline content, descendant text nodes are concatenated according to their DOM text sequence before whitespace collapse. The normalizer does not insert a synthetic space at every inline element boundary; this avoids producing text such as:

```text
paragraph .
```

from:

```html
<p>paragraph<b></b>.</p>
```

`pre` retains internal line breaks while trimming only the existing outer normalization boundary.

`table` v1 emits one coarse Table block and joins normalized `th` / `td` cell values with spaces rather than collapsing adjacent cells into an ambiguous token.

## 7. Nested blocks

The current block map is flat and does not claim a nested block tree.

Therefore HTML/XHTML v1 uses a maximal non-overlapping body-block projection:

```text
outer selected block
└── nested selected block

→ emit outer block once
→ do not emit nested block as a second overlapping/copying block
```

For example, a `<blockquote>` containing `<p>` elements is represented as one `blockquote` block rather than a BlockQuote plus duplicated Paragraph rows containing the same source text.

A current regression fixture proves the consequence: a BlockQuote containing two nested `<p>` elements is persisted as one BlockQuote range, and the normalized body can be `Quoted text.Second paragraph.` without a reliable inner Paragraph separator. The suppressed child boundaries are therefore not recoverable canonical facts.

The accepted segmentation-v2 migration respects this evidence boundary conservatively: a persisted BlockQuote or ListItem becomes one **typed coarse Paragraph-level unit with no Sentence children** under block-model/v1. It does not invent nested Paragraph/Sentence identity from transient DOM knowledge or punctuation guesses.

A future nested/leaf block-tree contract would require explicit parent/child identity and overlap semantics; only that stronger persisted evidence can justify a later finer-grained identity migration.

## 8. Source order vs structural hierarchy

`NormalizedBlock.source_order` is parser/source order.

For EPUB it is derived from:

```text
spine order
→ XHTML document order
→ emitted block order
```

It is intentionally independent from reconciled Section-tree DFS order because publisher navigation may reparent Sections while spine remains authoritative for source order.

The validator therefore does not equate:

```text
block source order
== reconciled Section traversal order
```

## 9. EPUB remapping

Each spine XHTML document is first parsed with the HTML block contract. EPUB then remaps the block owner through the same deterministic Section-ID mapping used for canonical EPUB Sections:

```text
section://<html-id>
→ section://epub-<spine-index>/<html-id>
```

Native locations become EPUB-native provenance:

```text
epub:<entry-path>#<anchor>
```

or an entry-path-qualified HTML block location when no native anchor exists.

Navigation/spine reconciliation may change Section title, parentage, level and path, but it does not rewrite Section ID or body content; therefore the exact block owner/range remains valid after reconciliation.

## 10. Persistence

The block map is stored under reserved canonical Document metadata keys:

```text
normalized_block_map_version
normalized_blocks
normalized_block_map
```

This choice preserves the existing public `Document` / `Section` struct shape while making the block map durable through the existing DocumentRepository serialization.

These keys are not arbitrary diagnostics. `Document::normalized_block_map()` is the domain-level decoder and validator for their canonical interpretation.

No SQLite schema migration is required because DocumentRepository already persists the complete `Document.metadata` map. Reopen tests prove the same block map is recovered and revalidated after SQLite adapter recreation.

## 11. Validation

`Document::validate_normalized_block_map()` checks block-local shape:

- schema version;
- contiguous global source order;
- owner Section existence;
- contiguous 1-based block index per owner;
- exact normalized range bounds;
- non-empty block ranges;
- no overlap or reorder among blocks sharing one owner Section.

`epub-structure-validator/v1` now consumes the persisted block map together with EPUB navigation/reconciliation and current TextUnit facts. It additionally records:

```text
blocks by kind
Sections with / without native blocks
Section content chars
block chars
separator-or-unmodeled chars
native block ↔ current Paragraph exact matches
native pre/table overlap with current Sentence units
```

Integrity violations are errors; source/model coverage gaps are degradations. Neither validator clamps, rebases or searches for replacement text.

The accepted v2 migration keeps the same distinction: an absent block map is supported fallback evidence, but a declared invalid block map must fail closed rather than silently becoming fallback segmentation.

## 12. Identity boundary in v1

The block map is persisted canonical normalization evidence, but the currently implemented precise TextUnit identity deliberately remains unchanged:

```text
normalized-document-hash/v1
+ text-segmentation/v1
→ current Paragraph / Sentence / TextLocator identity
```

Consequently on current `main`:

- adding/removing only block/validation metadata does not alter the current normalized hash;
- current Paragraph TextUnit IDs do not change solely because block rows are present;
- current search/read/context/enumeration behavior is unchanged.

This is an intentional migration boundary, not permission to ignore the block map forever.

ADR 0005 now defines the explicit follow-up identity migration. It advances both segmentation and normalized identity so old v1 locators/cursors fail stale rather than being reinterpreted.

## 13. Accepted block-aware projection

The next implementation consumes block-model/v1 as follows:

```text
paragraph    → one exact sentence-eligible Paragraph
blockquote   → one typed coarse Paragraph-level unit, no Sentence children
list_item    → one typed coarse Paragraph-level unit, no Sentence children
preformatted → one coarse Paragraph-level unit, no Sentence children
table        → one coarse Paragraph-level unit, no Sentence children
```

BlockQuote/ListItem are coarse because flat maximal projection may hide mixed or multiple nested leaf blocks. This is evidence degradation, not a semantic claim that quote/list content is inherently non-prose.

For uncovered Section ranges:

```text
whitespace-only → separator coverage
non-whitespace  → deterministic v1-style fallback scoped to the gap
```

Native evidence outranks text heuristics. Existing fenced/indented-code and Markdown-table heuristics remain only for fallback/no-block regions.

Paragraph-level ordinals are assigned by exact Section source position after native and fallback candidates are merged. `block_index` remains a native-block ordinal and is not reused as `paragraph_index`.

Coarse BlockQuote/ListItem/Preformatted/Table units remain Paragraph-addressable/searchable but do not emit Sentence identity under block-model/v1.

## 14. Accepted normalized identity projection

`normalized-document-hash/v2` will bind the block facts that can affect TextUnit addressing:

```text
block-map presence/absence
schema version
owner_section_id
block_index
source_order
kind
normalized_range
```

It will not bind provenance-only/diagnostic facts such as native anchor/location, validator report, coverage counters, or lexical state.

This is required because a block kind/range/order change can alter Paragraph/Sentence boundaries while `Section.content` itself remains byte-for-byte identical.

## 15. Acceptance evidence

Existing block-model tests cover:

- Paragraph / BlockQuote / ListItem / Preformatted / Table kinds;
- exact Unicode-scalar Section-relative slices;
- inline punctuation without synthetic spaces;
- semantic table cell separation;
- headingless multi-block source order;
- nested block de-duplication;
- the concrete nested BlockQuote fixture where two child `<p>` elements collapse into one persisted outer range;
- validator rejection of bad source order and overlap;
- SQLite DocumentRepository close/reopen persistence;
- EPUB Section-ID/native-location remapping after structure reconciliation;
- unchanged current normalized hash and Paragraph TextUnit IDs when only block metadata is removed.

The subsequent EPUB validator additionally proves that the persisted block map can participate in deterministic coverage/revalidation after repository reopen without reparsing source.

The v2 implementation must add migration evidence for exact native Paragraphs, coarse BlockQuote/ListItem/Preformatted/Table Sentence behavior, nested-fixture no-fabrication, native/fallback gap projection, hash-v2 block sensitivity, old locator/cursor staleness, lexical-index/v3 rebuild, and restart determinism.

## 16. Explicit non-goals

The block-model increment itself does not implement:

```text
block-aware Paragraph segmentation
block-aware Sentence eligibility/segmentation
text-segmentation/v2
normalized-document-hash/v2
nested/leaf block-tree identity
block-aware SearchIndex ranking
SVG/fixed-layout precise blocks
new MCP Tools
```

The first four items above are now accepted by ADR 0005 as the next implementation target; they remain unimplemented until `feat/block-aware-text-unit-identity` lands.

The EPUB validator is already implemented separately rather than hidden inside the block model.

## 17. Next implementation

The design decision is complete.

Next branch:

```text
feat/block-aware-text-unit-identity
```

Implementation order is:

```text
normalized-document-hash/v2
→ text-segmentation/v2 native/fallback Paragraph-level projection
→ Sentence eligibility/coverage
→ locator/cursor stale gates
→ derived TextUnit replacement
→ lexical-search-index/v3 rebuild
→ stdio direct-handoff regression
```

No new MCP Tool is introduced.
