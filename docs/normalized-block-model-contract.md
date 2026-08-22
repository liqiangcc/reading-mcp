# Normalized Block Model Contract

> Status: Implemented P1 canonical block foundation
>
> Branch: `feat/normalized-block-model`
>
> Related: `docs/adr/0003-epub-first-structure-reliability.md`, `docs/epub-structure-reconciliation-contract.md`, `docs/normalized-text-range-contract.md`, `docs/epub-structure-validator-contract.md`

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

Current source-address identity remains:

```text
normalized-document-hash/v1
text-segmentation/v1
```

The v4 normalization bump invalidated Parsed Cache entries created before persisted block facts existed. The subsequent v5 bump adds persisted validator/coverage output without changing the block schema or current TextUnit identity.

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

A future nested block-tree contract would require explicit parent/child identity and overlap semantics; it is not inferred in v1.

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

## 12. Identity boundary in v1

The block map is persisted canonical normalization evidence, but existing precise TextUnit identity deliberately remains unchanged:

```text
normalized-document-hash/v1
+ text-segmentation/v1
→ current Paragraph / Sentence / TextLocator identity
```

Consequently:

- adding/removing only block/validation metadata does not alter the current normalized hash;
- current Paragraph TextUnit IDs do not change solely because block rows are present;
- current search/read/context/enumeration behavior is unchanged.

This is an intentional migration boundary, not permission to ignore the block map forever.

Before a future block-aware Paragraph/Sentence policy can become identity-bearing, that increment must explicitly version the new identity inputs, for example through a new segmentation contract and any required normalized-hash contract revision. It must not silently reinterpret existing `text-segmentation/v1` locators.

## 13. Acceptance evidence

Block-model tests cover:

- Paragraph / BlockQuote / ListItem / Preformatted / Table kinds;
- exact Unicode-scalar Section-relative slices;
- inline punctuation without synthetic spaces;
- semantic table cell separation;
- headingless multi-block source order;
- nested block de-duplication;
- validator rejection of bad source order and overlap;
- SQLite DocumentRepository close/reopen persistence;
- EPUB Section-ID/native-location remapping after structure reconciliation;
- unchanged current normalized hash and Paragraph TextUnit IDs when only block metadata is removed.

The subsequent EPUB validator additionally proves that the persisted block map can participate in deterministic coverage/revalidation after repository reopen without reparsing source.

## 14. Explicit non-goals

The block-model increment does not itself implement:

```text
block-aware Paragraph segmentation
block-aware Sentence eligibility/segmentation
text-segmentation/v2
normalized-document-hash/v2
nested block-tree identity
block-aware SearchIndex ranking
SVG/fixed-layout precise blocks
new MCP Tools
```

The EPUB validator is now implemented separately rather than hidden inside the block model.

## 15. Next decision

With persisted block facts and validator coverage now available, the next independent unit is an explicit block-aware TextUnit identity migration decision.

Before implementation it must answer:

```text
Which block kinds become Paragraph candidates?
How do blockquote/list_item preserve source semantics?
How do pre/table become coarse non-prose?
Does segmentation advance to text-segmentation/v2?
Does normalized-document-hash require v2 block inputs?
How do existing v1 locators/cursors fail stale instead of being silently reinterpreted?
```

No current v1 identity changes until those decisions are versioned and tested.
