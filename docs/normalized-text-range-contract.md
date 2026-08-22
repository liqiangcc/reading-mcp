# Normalized Document Identity and Text Range Contract

> Status: Implemented contract
>
> Foundation branch: `feat/normalized-text-range`
>
> Current identity consumer: `feat/block-aware-text-unit-identity`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0005-block-aware-text-unit-identity.md`, `docs/normalized-block-model-contract.md`, `docs/epub-structure-validator-contract.md`

## 1. Goal

This contract separates raw-source provenance, normalized source-address identity, parser/cache policy version, and exact normalized coordinates.

```text
Did retrieved source bytes change?
→ content_hash

Did addressing-relevant persisted normalized facts change?
→ normalized_document_hash

Which parser/normalization policy produced the Document?
→ normalization_version

What does [start,end) mean?
→ normalized text coordinate space
```

These layers must not be collapsed.

## 2. Raw-source provenance

`content_hash` remains SHA-256 over retrieved source bytes. It is provenance, not sufficient fine-grained TextUnit identity.

## 3. Current normalized-document identity

Current contract：

```text
normalized_document_hash_version = normalized-document-hash/v2
```

Hash v2 is deterministic over addressing-relevant persisted facts.

### Canonical Section projection

It retains the original Section inputs：

```text
root Section count
Section id
Section parent id / absence
Section title
Section level
exact Section.content
child count
children recursively in stored order
```

### Block identity projection

Because current `text-segmentation/v2` consumes persisted native block facts, hash v2 additionally binds：

```text
block-map presence/absence
normalized-block-model schema version
ordered owner_section_id
block_index
source_order
kind
normalized_range.start/end
```

A change to block kind/range/order can change Paragraph/Sentence boundaries or eligibility even when `Section.content` is identical; such a change must therefore change normalized identity.

### Excluded facts

The hash intentionally excludes facts that do not define normalized source addressing：

```text
DocumentSource
raw content_hash
non-identity Document metadata
legacy Location fields
native_anchor / native_location provenance
validator report / diagnostics / coverage
MCP rendering/response offsets
search/index rows
```

Tests prove native-location-only block changes do not change hash v2, while block presence/kind changes do.

## 4. Normalization policy version

Current parser/cache policy：

```text
normalization_version = reading-mcp-normalization/v6
```

Relevant history：

```text
v2 = EPUB navigation-map parser facts
v3 = navigation/spine reconciliation
v4 = normalized-block-model/v1 + HTML inline normalization correction
v5 = epub-structure-validator/v1 persisted report/coverage
v6 = block-aware TextUnit v2 changes persisted EPUB validator TextUnit coverage
```

The block-aware identity migration initially appeared to affect only derived addressing. Implementation review established that the persisted EPUB validation report is also parser output and depends on current Paragraph/Sentence materialization. A v5 Parsed Cache hit would bypass parser/validator execution and could retain v1-era TextUnit coverage beside current v2 TextUnits. v6 prevents that stale mixed state.

`normalization_version` scopes Parsed Cache policy; it is not a replacement for the actual normalized fingerprint.

## 5. Parsed-cache identity

Parsed cache identity remains：

```text
final_source
+ raw_sha256
+ normalization_version
```

Consequences：

- Raw Cache can survive parser-policy changes;
- normalization-v5 Parsed Cache entries miss under v6;
- current EPUB parser/validator output is regenerated under TextUnit v2 semantics;
- DocumentRepository/TextUnit/Search derived-state responsibilities remain separate.

## 6. Normalized text range

A `NormalizedTextRange` is always relative to exact persisted owner `Section.content`.

```text
coordinate space = section-content-unicode-scalar/v1
base             = zero
interval         = half-open [start,end)
unit             = Unicode scalar / Rust char
owner            = exactly one Section.content
```

Example：

```text
Section.content = "A中🙂Z"
range            = [1,3)
result           = "中🙂"
```

Valid range：

```text
0 <= start <= end <= Section.content.chars().count()
```

The generic range type permits empty ranges. Higher-level contracts such as `NormalizedBlock` may require non-empty ranges.

Range slicing never trims, rewrites, normalizes, or reconstructs from a second representation.

## 7. Validation APIs

Domain APIs：

```text
NormalizedTextRange::new(start,end)
NormalizedTextRange::validate_for_text(owner_text)
NormalizedTextRange::slice(owner_text)
Section::normalized_text_len()
Section::validate_normalized_range(range)
Section::normalized_text_slice(range)
```

`NormalizedBlockMap` composes the same coordinate contract with owner existence, block ordinal/source order, non-empty ranges, and per-owner non-overlap validation.

Current block-aware TextUnit capability boundaries use fallible block-map decoding/materialization. A declared invalid block map fails closed rather than being silently ignored.

## 8. Coordinate spaces remain separate

### Native / legacy parser coordinates

`Location.char_start/char_end` retain parser-defined historical meaning and are never reinterpreted as normalized ranges.

### Canonical normalized coordinates

```text
section-content-unicode-scalar/v1
```

Used by Paragraph, Sentence, CharacterRange, exact TextLocator, and NormalizedBlock ranges.

### Rendered read-stream coordinates

Legacy Section-tree continuation uses：

```text
section-tree-rendered-unicode-scalar/v1
```

These are stream progress positions, not canonical citations.

## 9. MCP identity exposure

`open_document` returns：

```text
content_hash
normalized_document_hash
normalized_document_hash_version
normalization_version
normalized_text_coordinate_space
```

Current response therefore advertises：

```text
normalized-document-hash/v2
reading-mcp-normalization/v6
section-content-unicode-scalar/v1
```

Tool count does not change when internal identity/persistence versions evolve.

## 10. Locator/cursor stale semantics

TextLocator and cursors bind normalized identity and, for Paragraph/Sentence streams, segmentation version.

Current segmentation：

```text
text-segmentation/v2
```

Historical v1 Paragraph/Sentence locators are rejected as `STALE_LOCATOR` even if their old exact range happens to match. Historical TextUnitCursor state is rejected as `STALE_CURSOR`. Old normalized-hash-bound state fails through normalized identity mismatch.

No text-similarity/fuzzy rebasing is allowed.

## 11. Persistence and rebuildability

Normalized hash is computed from persisted canonical facts rather than stored as an independent source of truth.

Invariant：

```text
hash(document before save)
==
hash(document restored from repository)
```

The block map and EPUB validation report persist through Document metadata. The block-map identity projection is now part of hash v2; validator report/diagnostics remain excluded from the normalized hash even though their generation participates in parser/cache policy v6.

Sentence facts remain deterministically rebuildable and do not require Sentence SQLite rows for correctness.

## 12. Acceptance evidence

Tests cover：

- Unicode-scalar half-open slicing;
- exact whitespace preservation;
- reversed/out-of-bounds rejection;
- hash changes for Section identity facts;
- hash stability across raw source and legacy/native provenance changes;
- block-map presence/kind sensitivity in hash v2;
- native-location-only exclusion from hash v2;
- hash rebuild after SQLite persistence;
- Parsed Cache v5→v6 invalidation;
- open-document schema advertises hash v2 + normalization v6;
- old v1 Paragraph locator stale rejection;
- old v1 TextUnitCursor stale rejection;
- normalized block exact slices/reopen persistence;
- EPUB validator deterministic reopen behavior.

CI #898 passed after the v6 cache-policy correction：

```text
Format  success
Clippy  success
Test    success
```

## 13. Current identity summary

```text
content_hash
= raw source provenance

reading-mcp-normalization/v6
= parser/cache policy

normalized-document-hash/v2
= canonical Section + identity-bearing block projection

text-segmentation/v2
= current block-aware Paragraph/Sentence identity policy

section-content-unicode-scalar/v1
= canonical exact-text coordinate space
```
