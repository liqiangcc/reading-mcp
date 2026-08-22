# Normalized Document Identity and Text Range Contract

> Status: Implemented contract
>
> Branch: `feat/normalized-text-range`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/normalized-block-model-contract.md`, `docs/epub-structure-validator-contract.md`, `docs/tool-contract-use-case-design.md`

## 1. Goal

This contract establishes the identity/range foundation required by precise Paragraph/Sentence locators and canonical block facts.

It answers four separate questions:

```text
Did the retrieved source bytes change?
→ content_hash

Did addressing-relevant canonical normalized facts change?
→ normalized_document_hash

Which parser/normalization policy produced the persisted Document?
→ normalization_version

What does [start, end) mean for an exact normalized excerpt?
→ normalized text coordinate space
```

These identities and coordinate spaces must not be collapsed into one field.

## 2. Identity layers

### 2.1 Raw-source provenance

The existing `content_hash` remains SHA-256 over retrieved source bytes.

It answers whether the retrieved source representation changed. It does not prove that the persisted normalized `Document / Section` facts are unchanged across parser or normalization-policy revisions.

### 2.2 Normalized-document identity

Current contract version:

```text
normalized_document_hash_version = normalized-document-hash/v1
```

The hash is a deterministic fingerprint over addressing-relevant canonical Section facts, in stored/source-tree order:

```text
root Section count
Section id
Section parent id / absence
Section title
Section level
exact persisted Section.content
child count
children recursively in stored order
```

The current hash intentionally excludes:

```text
DocumentSource
raw content_hash
Document metadata
legacy Location fields
native page/anchor/spine provenance
MCP rendering and response offsets
search/index rows
```

`normalized-block-model/v1` and `epub-structure-validator/v1` are persisted in reserved Document metadata but are **not inputs to `normalized-document-hash/v1`**, because current `text-segmentation/v1` Paragraph/Sentence identity does not consume them. A future block-aware segmentation migration must explicitly version its identity inputs rather than silently redefining existing locators.

### 2.3 Normalization policy version

Current diagnostic/cache version:

```text
normalization_version = reading-mcp-normalization/v5
```

Relevant EPUB/HTML parser-policy history:

```text
v2 = EPUB navigation-map parser facts added
v3 = navigation/spine reconciliation may change canonical Section structure
v4 = normalized-block-model/v1 persisted + HTML inline text normalization correction
v5 = epub-structure-validator/v1 persisted report + factual coverage evidence
```

`normalization_version` scopes Parsed Cache policy. It is not a substitute for the actual normalized fingerprint.

A policy version may change while producing identical hash-v1 facts. Conversely, any canonical Section fact that is already part of hash v1 changes the normalized hash even when raw bytes remain identical.

## 3. Parsed-cache identity

Parsed cache identity is scoped by:

```text
final_source
+ raw_sha256
+ normalization_version
```

Consequences:

- unchanged raw bytes can still reuse Raw Cache;
- a parser/normalization-policy upgrade does not accidentally reuse an old Parsed Document;
- old parsed cache files become harmless misses;
- DocumentRepository and SearchIndex responsibilities remain unchanged.

Current tests explicitly prove v4 Parsed Cache keys miss under v5.

## 4. Normalized text range

A `NormalizedTextRange` is always relative to the exact persisted `Section.content` selected as owner.

Semantics:

```text
coordinate space = section-content-unicode-scalar/v1
base             = zero
interval         = half-open [start, end)
unit             = Unicode scalar value / Rust char
owner            = exactly one Section.content
```

Example:

```text
Section.content = "A中🙂Z"
range            = [1, 3)
result           = "中🙂"
```

A valid range satisfies:

```text
0 <= start <= end <= Section.content.chars().count()
```

Valid empty ranges are allowed for the generic range type, including `[len,len)`. Specific higher-level contracts such as `NormalizedBlock` may impose stricter non-empty requirements.

The range implementation does not trim, rewrite punctuation, normalize whitespace, or reconstruct text from a second representation.

## 5. Validation behavior

The domain exposes:

```text
NormalizedTextRange::new(start, end)
NormalizedTextRange::validate_for_text(owner_text)
NormalizedTextRange::slice(owner_text)
Section::normalized_text_len()
Section::validate_normalized_range(range)
Section::normalized_text_slice(range)
```

Construction rejects `start > end`; owner validation rejects `end > owner length`. Validators report errors and do not clamp or repair ranges.

`NormalizedBlockMap` reuses this coordinate contract and additionally checks owner existence, non-empty ranges, per-owner order/non-overlap, block ordinals and global block source order.

`epub-structure-validator/v1` composes these persisted range facts with canonical Section and current deterministic TextUnit evidence. It does not introduce another coordinate space.

## 6. Coordinate spaces remain separate

### Parser/native/legacy coordinates

`Location.char_start/char_end` retain parser-defined historical meaning and are never silently reinterpreted as normalized ranges.

### Canonical normalized coordinates

```text
section-content-unicode-scalar/v1
```

This is the source coordinate space for Paragraph, Sentence, CharacterRange, exact TextLocator ranges, and NormalizedBlock ranges.

### Rendered read-stream coordinates

Legacy Section subtree continuation uses:

```text
section-tree-rendered-unicode-scalar/v1
```

These are continuation positions, not citations/source ranges.

## 7. MCP contract evolution

`open_document` returns:

```text
content_hash
normalized_document_hash
normalized_document_hash_version
normalization_version
normalized_text_coordinate_space
```

The runtime Tool count does not change when normalization policy or internal persisted evidence advances.

## 8. Cursor/locator integration

ReadCursor/TextLocator bind the normalized-document hash for stale detection. No cursor or locator is rebased by text similarity.

EPUB reconciliation can change canonical Section title/parent/order/level and therefore naturally changes hash-v1 values. Block/validation metadata does not silently stale locators solely because evidence/report fields were added; current segmentation/locator identity remains v1 until a separately reviewed migration.

## 9. Persistence and rebuildability

The normalized hash is computed from canonical `Document` facts rather than stored as a second source of truth.

Required invariant:

```text
hash(document before repository save)
==
hash(document restored from repository)
```

The normalized block map and EPUB validation report are also persisted through existing DocumentRepository metadata serialization, without new SQLite schemas. Persistence of these derived/canonical-evidence records does not itself make them hash-v1 identity inputs.

The EPUB validation report can be deterministically recomputed from the restored underlying facts without source reparse.

## 10. Acceptance evidence

Tests cover:

- Unicode-scalar, zero-based, half-open slicing;
- exact whitespace preservation;
- reversed and out-of-bounds rejection;
- hash changes for Section id, parentage, title, level, content, and order;
- hash stability across raw source/provenance and legacy Location changes;
- hash rebuild after SQLite persistence;
- Parsed Cache misses across normalization versions, now including v4 → v5;
- additive MCP schema fields;
- rendered read-stream/source-coordinate separation;
- NormalizedBlock exact-slice and SQLite reopen persistence;
- current Paragraph TextUnit IDs unchanged when only block metadata is removed;
- EPUB validator report/revalidation surviving SQLite reopen without source reparse.

## 11. Migration rule

Persisting new evidence and using it as identity are deliberately separate changes.

Current state:

```text
normalized-block-model/v1      = persisted canonical normalization evidence
epub-structure-validator/v1    = persisted/rebuildable validation evidence
text-segmentation/v1           = current Paragraph/Sentence identity policy
normalized-document-hash/v1    = current source-address fingerprint
```

Before native blocks affect Paragraph/Sentence identity, the new increment must explicitly version segmentation and any required normalized-hash inputs, add stale/migration tests, and must not reinterpret existing locators under the old version names.
