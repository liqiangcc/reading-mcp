# EPUB Structure Validator Contract

> Status: Implemented P1 validator foundation; current TextUnit consumer is v2
>
> Validator foundation branch: `feat/epub-structure-validator`
>
> Current identity branch: `feat/block-aware-text-unit-identity`
>
> Related: `docs/adr/0003-epub-first-structure-reliability.md`, `docs/adr/0005-block-aware-text-unit-identity.md`, `docs/epub-structure-reconciliation-contract.md`, `docs/normalized-block-model-contract.md`

## 1. Goal

`epub-structure-validator/v1` turns the persisted EPUB structure pipeline into auditable evidence. It now validates the current block-aware TextUnit v2 materialization from persisted facts.

The validator does **not** reopen the EPUB archive or reparse DOM state. It validates only facts that survive persistence:

```text
canonical Document / Sections
+ epub-navigation-map/v1
+ epub-structure-reconciliation/v1
+ normalized-block-model/v1
+ deterministic text-segmentation/v2 Paragraph/Sentence materialization
→ epub-structure-validator/v1
```

This keeps validation reproducible after `DocumentRepository` close/reopen and prevents a second parser from becoming a competing source of truth.

## 2. Error vs degradation

Findings are explicitly classified:

```text
error
= persisted facts contradict each other or violate a claimed invariant
→ integrity = invalid
→ EpubParser fails closed

degradation
= source capability/coverage is incomplete but persisted facts remain internally truthful
→ integrity can remain valid
→ readable Document survives
```

Examples of errors:

```text
duplicate / cyclic / inconsistent Section identity
structure map contradicts canonical Section title/parent/level
spine/source order contradiction
claimed resolved navigation without required evidence
block map invalid owner/range/order
Paragraph/Sentence exact-slice or ownership invariant failure
summary count contradicts persisted detailed facts
```

Examples of degradations:

```text
missing navigation fragment
missing navigation resource
unsupported navigation resource
unsupported/missing-manifest spine item
reconciliation fallback diagnostic
nonempty Section with no normalized native block
native block/current TextUnit coverage mismatch
```

The validator reports degradation; it never changes source text, reorders structure, searches for replacement targets, or fuzzy-rebases locators.

## 3. Persisted report

Current report schema:

```text
epub-structure-validator/v1
```

The parser stores:

```text
epub_validation_report_version
epub_validation_integrity
epub_validation_errors
epub_validation_degradations
epub_validation_report
```

`epub_validation_report` contains:

```text
schema_version
integrity = valid | invalid
error_count
degradation_count
findings[]
coverage
```

The report is derived evidence. Revalidation is performed from the underlying navigation/structure/block/canonical facts rather than trusting an older report recursively.

## 4. Package / spine coverage

Coverage records factual denominators and counts:

```text
manifest_items_total
spine_items_total
spine_items_parsed
spine_items_missing_manifest
spine_items_unsupported_media
linear_spine_items
non_linear_spine_items
```

Validator invariants include:

- contiguous 1-based spine indices;
- parsed spine rows have manifest href, resolved path, and media type;
- structure summary linear/non-linear counts equal detailed spine rows;
- each Section structure fact references an existing spine index;
- each Section fact's `linear` value equals that spine row.

Unsupported or missing-manifest spine entries are explicit coverage degradation, not silently removed from the denominator.

## 5. Navigation coverage

Navigation coverage records:

```text
nodes_total
resolved_nodes
resolved_document
resolved_fragment
missing_fragment
missing_resource
unsupported_resource
invalid_path
malformed_resource
unlinked
fragment_targets_total
fragment_targets_resolved
diagnostics
```

Validator invariants include:

- `epub-navigation-map/v1` schema;
- deterministic pre-order `source_order`;
- stored depth matches hierarchy;
- node provenance matches the selected navigation plane;
- `resolved_document` has a resolved entry path;
- `resolved_fragment` has both path and fragment evidence;
- persisted summary counts agree with the detailed map.

Non-resolved target states remain degradations unless the persisted map contradicts its own claim.

## 6. Canonical structure validation

Canonical Section validation checks:

```text
unique Section IDs
nested tree parent == parent_id
parent exists
no parent-reference cycle
```

`epub-structure-reconciliation/v1` is cross-checked against the actual canonical tree:

```text
Section count
source_order
spine_index monotonicity
linear status
entry/native-location provenance
canonical title
canonical level
canonical parent
navigation provenance + source_order + resolution status
applied navigation count
root/sibling source order
parent source order < child source order
```

Reconciliation diagnostics are retained as degradation findings rather than hidden.

## 7. Normalized block validation / coverage

The existing `Document::normalized_block_map()` domain validator remains authoritative for block-map shape/ranges.

Validator coverage adds:

```text
blocks_total
paragraph_blocks
blockquote_blocks
list_item_blocks
preformatted_blocks
table_blocks
sections_with_nonempty_content
sections_with_blocks
nonempty_sections_without_blocks
section_content_chars
block_chars
separator_or_unmodeled_chars
blocks_with_exact_paragraph_match
blocks_without_exact_paragraph_match
native_non_prose_blocks_with_sentence_units
```

Every EPUB block must retain EPUB-qualified native provenance. Coverage does not pretend that separator characters, unmodeled content, or missing native blocks are block text.

## 8. Current TextUnit validation

The validator materializes current deterministic `text-segmentation/v2` Paragraph/Sentence facts from the persisted canonical Document.

Paragraph projection under block-model/v1:

```text
native paragraph    → exact sentence-eligible Paragraph
native blockquote   → typed coarse Paragraph-level unit
native list_item    → typed coarse Paragraph-level unit
native preformatted → coarse Paragraph-level unit
native table        → coarse Paragraph-level unit
uncovered content   → deterministic fallback Paragraphs
```

Paragraph checks include:

```text
contiguous source_order
1-based paragraph_index per owner
owner Section exists
text == exact owner Section slice
coverage owner_chars = paragraph_chars + separator_chars
```

Sentence checks include:

```text
contiguous source_order
1-based sentence_index per parent Paragraph
owner Section exists
parent Paragraph exists
Sentence range inside parent Paragraph
text == exact owner Section slice
coverage paragraph_chars = sentence_chars + separator_chars + coarse_only_chars
```

Only native Paragraph and fallback prose/unknown regions are Sentence-eligible. Flat BlockQuote/ListItem and native Preformatted/Table remain coarse and receive no fabricated Sentence children.

## 9. Migration history and evidence

Before ADR 0005, the validator compared native block facts against `text-segmentation/v1` and deliberately reported mismatches such as native pre/table overlaps as migration evidence.

ADR 0005 then advanced current identity to:

```text
normalized-document-hash/v2
text-segmentation/v2
```

The validator remains schema `epub-structure-validator/v1`; its job did not change. What changed is the current deterministic TextUnit contract it validates.

The report itself remains excluded from normalized-document hash identity. Identity-bearing block kind/range/order is bound directly by hash v2 instead.

## 10. Parser integration

`EpubParser` constructs all canonical/persisted facts, attaches the validator report, then applies:

```text
error_count > 0
→ ParseFailed

degradation_count > 0 && error_count == 0
→ readable Document + persisted coverage/degradation evidence
```

The validator does not repair failed facts.

## 11. Parsed Cache / identity boundary

Validator report metadata is persisted parser output.

Historical validator introduction advanced:

```text
reading-mcp-normalization/v4
→ reading-mcp-normalization/v5
```

The later block-aware TextUnit migration changes the Paragraph/Sentence coverage stored in that report, so Parsed Cache policy advances again:

```text
reading-mcp-normalization/v5
→ reading-mcp-normalization/v6
```

`CachingParser` returns a Parsed Cache hit without rerunning validation. Reusing a v5 cached EPUB could therefore expose a v1-era validation report beside current v2 TextUnits. A v5 key must miss under v6.

Current addressing identity:

```text
normalized-document-hash/v2
text-segmentation/v2
```

The validation report itself remains derived metadata and is not added to normalized-document hash inputs.

## 12. Persistence / reopen

Because all source-facing inputs and the report are inside the persisted `Document`, no new SQLite table is required.

Acceptance requires:

```text
parse EPUB under current policy
→ persist Document
→ close repository
→ reopen repository
→ persisted validation report unchanged
→ validate_epub_document(restored Document)
→ same report
```

No source retrieval/reparse is required for repository-level revalidation. Parsed Cache policy is separate: old v5 parser output is invalidated at cache-key level before it can become the current Document.

## 13. Acceptance evidence

Tests cover:

- clean EPUB → valid integrity and factual coverage;
- unsupported spine media / missing fragment / unsupported nav target remain readable degradations;
- tampered persisted summary facts become integrity errors without source reparse;
- block-aware TextUnit ranges/eligibility feed current validation deterministically;
- report survives SQLite close/reopen and deterministic revalidation;
- normalization-v5 Parsed Cache keys miss under v6;
- existing EPUB navigation/reconciliation/block/precise-read/search suites remain subject to the release gate.

Final gate:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 14. Explicit non-goals

The validator still does not implement:

```text
nested/leaf block-tree identity
new reliability-inspection MCP Tool
SearchIndex/ranking changes
SVG/fixed-layout precision
silent repair / fuzzy rebase
```

TextUnit v2/hash v2 are now implemented by their own identity migration rather than hidden inside the validator.

## 15. Current boundary

The validator is evidence, not source identity and not a repair engine.

Current pipeline:

```text
persisted package/navigation/reconciliation/block facts
+ canonical Document
+ current text-segmentation/v2 TextUnits
→ epub-structure-validator/v1
→ deterministic integrity + coverage report
```

Any future nested-block or segmentation change that alters persisted validator coverage must again review both normalized identity and Parsed Cache policy explicitly.
