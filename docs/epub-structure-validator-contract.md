# EPUB Structure Validator Contract

> Status: Implemented P1 validator foundation
>
> Branch: `feat/epub-structure-validator`
>
> Related: `docs/adr/0003-epub-first-structure-reliability.md`, `docs/epub-structure-reconciliation-contract.md`, `docs/normalized-block-model-contract.md`

## 1. Goal

`epub-structure-validator/v1` turns the persisted EPUB structure pipeline into auditable evidence before a later block-aware TextUnit identity migration.

The validator does **not** reopen the EPUB archive or reparse DOM state. It validates only facts that survive persistence:

```text
canonical Document / Sections
+ epub-navigation-map/v1
+ epub-structure-reconciliation/v1
+ normalized-block-model/v1
+ deterministic text-segmentation/v1 Paragraph/Sentence materialization
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
native block != one current text-segmentation/v1 Paragraph
current Sentence units overlapping native pre/table evidence
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

The validator materializes the current deterministic `text-segmentation/v1` Paragraph/Sentence stream from the persisted canonical Document and checks:

Paragraph:

```text
contiguous source_order
1-based paragraph_index per owner
owner Section exists
text == exact owner Section slice
coverage owner_chars = paragraph_chars + separator_chars
```

Sentence:

```text
contiguous source_order
1-based sentence_index per parent Paragraph
owner Section exists
parent Paragraph exists
Sentence range inside parent Paragraph
text == exact owner Section slice
coverage paragraph_chars = sentence_chars + separator_chars + coarse_only_chars
```

This is validation of the **current** TextUnit contract; it does not make normalized blocks identity-bearing yet.

## 9. Evidence for the later block-aware migration

The validator intentionally compares native block facts with current `text-segmentation/v1` facts.

Examples:

```text
native block range == one current Paragraph
→ exact match evidence

native block range != one current Paragraph
→ degradation: native_blocks_not_exact_current_paragraphs

native pre/table range overlaps current Sentence units
→ degradation: current_sentences_overlap_native_non_prose_blocks
```

These are migration evidence, not a reason to silently reinterpret existing v1 locators.

A later block-aware segmentation increment must make an explicit identity-version decision.

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

Validator report metadata changes parser output, so normalization/cache policy advances:

```text
reading-mcp-normalization/v4
→ reading-mcp-normalization/v5
```

Current addressing identity remains:

```text
normalized-document-hash/v1
text-segmentation/v1
```

The validation report itself is derived metadata and is not added to the current normalized-document hash.

## 12. Persistence / reopen

Because all source-facing inputs and the report are inside the persisted `Document`, no new SQLite table is required.

Acceptance requires:

```text
parse EPUB
→ persist Document
→ close repository
→ reopen repository
→ persisted validation report unchanged
→ validate_epub_document(restored Document)
→ same report
```

No source retrieval/reparse is required for this revalidation.

## 13. Acceptance evidence

Tests cover:

- clean EPUB → `integrity=valid`, zero error/degradation and factual coverage counts;
- unsupported spine media / missing fragment / unsupported nav target remain readable degradations;
- tampered persisted summary facts become integrity errors without source reparse;
- report survives SQLite close/reopen and deterministic revalidation;
- existing EPUB navigation/reconciliation/block/TextUnit/precise-read/search suites remain subject to the release gate.

Release gate:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 14. Explicit non-goals

This increment does not implement:

```text
text-segmentation/v2
block-aware Paragraph/Sentence identity
normalized-document-hash/v2
nested block-tree identity
new reliability-inspection MCP Tool
SearchIndex/ranking changes
SVG/fixed-layout precision
silent repair / fuzzy rebase
```

## 15. Next decision

After validator/coverage evidence is stable, the next separate design/implementation unit can evaluate a block-aware TextUnit identity migration.

It must answer explicitly:

```text
Which normalized block kinds become Paragraph candidates?
How are blockquote/list_item represented?
How are pre/table made coarse non-prose?
Does segmentation become text-segmentation/v2?
Which block facts must enter normalized-document-hash/v2, if any?
How are old v1 locators/cursors made stale rather than reinterpreted?
```

Validator evidence informs that migration; it does not silently perform it.
