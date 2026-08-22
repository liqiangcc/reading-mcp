# Normalized Document Identity and Text Range Contract

> Status: Implemented contract
>
> Branch: `feat/normalized-text-range`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/tool-contract-use-case-design.md`

## 1. Goal

This contract establishes the P0 foundation required before Paragraph/Sentence TextUnits can receive stable locators.

It answers four separate questions:

```text
Did the retrieved source bytes change?
→ content_hash

Did addressing-relevant canonical normalized facts change?
→ normalized_document_hash

Which normalization policy produced the canonical Document?
→ normalization_version

What does [start, end) mean for an exact normalized excerpt?
→ normalized text coordinate space
```

These identities and coordinate spaces must not be collapsed into one field.

## 2. Identity layers

### 2.1 Raw-source provenance

The existing `content_hash` remains SHA-256 over retrieved source bytes.

It answers whether the retrieved source representation changed. It does not prove that the persisted normalized `Document / Section` facts are unchanged across parser or normalization-policy revisions.

Its semantics are unchanged for backward compatibility.

### 2.2 Normalized-document identity

`normalized_document_hash` is a deterministic SHA-256 fingerprint over addressing-relevant canonical facts that can be rebuilt from the persisted `Document`.

Current contract version:

```text
normalized_document_hash_version = normalized-document-hash/v1
```

The canonical input includes, in deterministic tree/source order:

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

The encoding is domain-separated and length-prefixed before hashing, so concatenation ambiguity cannot change identity.

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

Those fields remain valuable provenance or derived state, but they do not define the exact normalized text owned by a Section. A future persisted block/boundary model that affects Paragraph/Sentence segmentation must be added through a new normalized-hash contract version.

### 2.3 Normalization policy version

Current diagnostic/cache version:

```text
normalization_version = reading-mcp-normalization/v3
```

`normalization_version` identifies parser/normalization policy for cache invalidation, diagnostics, and migration. It is not a substitute for the actual normalized fingerprint.

Relevant EPUB policy history:

```text
v2 = EPUB navigation-map parser facts added to Document metadata
v3 = navigation/spine reconciliation may change canonical Section structure
```

The v3 bump invalidates Parsed Cache entries from v2 so identical raw EPUB bytes are reparsed under the new structural policy.

`normalized-document-hash/v1` does not need a new algorithm/version for reconciliation: it already includes Section title, parentage, level and child order. When reconciliation changes those canonical facts, the hash value changes naturally. When reconciliation produces identical canonical facts, the hash may remain identical even though normalization policy advanced.

A policy version may therefore change while producing identical canonical facts; conversely, canonical facts changing must change the normalized hash even when raw bytes remain identical.

## 3. Parsed-cache identity

Parsed cache identity is scoped by:

```text
final_source
+ raw_sha256
+ normalization_version
```

Consequences:

- unchanged raw bytes can still reuse Raw Cache;
- a parser/normalization-policy upgrade does not reuse an old Parsed Document accidentally;
- old parsed cache files become harmless misses rather than being silently reinterpreted;
- DocumentRepository and SearchIndex responsibilities remain unchanged.

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

Examples:

```text
Section.content = "A中🙂Z"
range            = [1, 3)
result           = "中🙂"
```

Valid empty ranges are allowed, including `[len, len)` at the end of the owner text.

A valid range must satisfy:

```text
0 <= start <= end <= Section.content.chars().count()
```

The returned excerpt must be an exact slice of persisted owner text. The range implementation does not trim, normalize whitespace, rewrite punctuation, or reconstruct text from another representation.

## 5. Validation behavior

Range construction rejects:

```text
start > end
```

Owner validation rejects:

```text
end > owner Unicode-scalar length
```

The domain exposes:

```text
NormalizedTextRange::new(start, end)
NormalizedTextRange::validate_for_text(owner_text)
NormalizedTextRange::slice(owner_text)
Section::normalized_text_len()
Section::validate_normalized_range(range)
Section::normalized_text_slice(range)
```

The validator reports explicit reversed/out-of-bounds errors. It does not clamp or repair invalid ranges.

## 6. Three coordinate spaces remain separate

### 6.1 Parser/native or legacy source coordinates

Current `Location.char_start` / `char_end` retain their parser-defined historical meaning.

For example, Markdown currently derives them from positions in the source Markdown text, while `Section.content` may be trimmed before persistence. These fields are therefore not normalized ranges and are not silently reinterpreted.

### 6.2 Canonical normalized coordinates

```text
section-content-unicode-scalar/v1
```

This is the only general coordinate space accepted for Paragraph, Sentence, CharacterRange, and exact TextLocator ranges.

### 6.3 Rendered read-stream coordinates

Legacy Section subtree continuation uses:

```text
section-tree-rendered-unicode-scalar/v1
```

`ReadStreamSegment.start_char/end_char` are positions in `SectionTreeReadStream/v1`, whose rendering version is `section-tree-markdown/v1`.

They are continuation progress only. They are never canonical source ranges or citations.

## 7. MCP contract evolution

`open_document` additively returns:

```text
content_hash
normalized_document_hash
normalized_document_hash_version
normalization_version
normalized_text_coordinate_space
```

Existing fields retain their meanings.

`read_document.stream` additively returns:

```text
coordinate_space = section-tree-rendered-unicode-scalar/v1
```

This makes the rendered-stream/non-source nature of continuation positions explicit.

## 8. ReadCursor integration

`ReadCursor` binds the normalized-document hash in addition to raw source hash, Section target, read mode, rendering version, and next stream position.

A normalized identity mismatch fails closed. No cursor is rebased by text similarity. Because EPUB reconciliation can change canonical Section facts, a cursor/locator issued for a pre-reconciliation normalized hash correctly becomes stale when the reconciled normalized hash differs.

## 9. Persistence and rebuildability

The normalized hash is computed from the canonical `Document` rather than stored as a second source of truth.

Required invariant:

```text
hash(document before repository save)
==
hash(document restored from repository)
```

A future persisted block/boundary model that becomes addressing-relevant must be persisted first and then incorporated through an explicit normalized-hash contract-version decision.

## 10. Acceptance evidence

Tests cover:

- Unicode-scalar, zero-based, half-open slicing;
- exact whitespace preservation;
- valid empty terminal ranges;
- reversed and out-of-bounds rejection;
- hash changes for Section id, parentage, title, level, content, and order;
- hash stability across raw source/provenance and legacy Location changes;
- hash rebuild after SQLite persistence;
- parsed cache misses across normalization versions, including v2 → v3;
- additive MCP schema fields;
- explicit rendered read-stream coordinate space;
- existing continuation stale/no-gap/no-overlap behavior.

## 11. Non-goals of the original P0 contract

The original normalized-range increment did not itself define Paragraph/Sentence segmentation, TextUnit persistence, FTS, or EPUB structural policy. Those have evolved in separate implementation increments while preserving the identity/range rules above.
