# Paragraph TextUnit Index Contract

> Status: Implemented P1 foundation
>
> Branch: `feat/text-unit-index`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/normalized-text-range-contract.md`, `docs/tool-contract-use-case-design.md`

## 1. Goal

This increment establishes deterministic, rebuildable Paragraph TextUnits without changing the MCP Tool surface or the current SearchIndex behavior.

The dependency chain is:

```text
persisted canonical Document / Section.content
        ↓
text-segmentation/v1
        ↓
Paragraph TextUnits
        ↓
exact NormalizedTextRange
        ↓
TextUnitIndex (derived, rebuildable)
```

`Document / Section` remain source truth. `TextUnitIndex` is never a replacement canonical store.

## 2. Paragraph boundary policy

Paragraph v1 derives only from exact persisted `Section.content`. It does not depend on parser-native block objects, transient parser state, FTS rows, or rendered MCP responses.

A Paragraph is a maximal contiguous run of non-blank lines. A blank line is a line whose content, excluding its line ending, is whitespace-only.

Rules:

```text
blank line              → paragraph separator
leading blank lines     → separators, not Paragraphs
trailing blank lines    → separators, not Paragraphs
multiple non-blank lines→ one Paragraph, preserving internal line endings
first/last line content → preserved exactly, including indentation/trailing spaces
terminal line ending    → separator, not part of Paragraph text
```

The implementation computes boundaries in Unicode-scalar coordinates and then resolves the exact slice through `NormalizedTextRange`.

It never trims or rewrites the TextUnit text after deciding the range.

## 3. Versioning

Current segmentation version:

```text
text-segmentation/v1
```

Any change that can alter Paragraph ordinals or ranges requires a new segmentation version.

Current TextUnit ID contract:

```text
text-unit-id/v1
```

The deterministic unit ID is derived from:

```text
document_id
+ normalized_document_hash
+ owner_section_id
+ kind = paragraph
+ 1-based paragraph_index
+ normalized_range [start, end)
+ segmentation_version
```

Raw `content_hash` is retained on the TextUnit as source provenance but is not an independent Paragraph-boundary identity input. This follows ADR 0002: actual normalized facts plus segmentation policy define fine-grained identity.

## 4. TextUnit model

Implemented Paragraph TextUnit fields:

```text
TextUnit
├── id: TextUnitId
├── document_id
├── content_hash                  # raw provenance
├── normalized_document_hash
├── owner_section_id
├── kind = paragraph
├── paragraph_index               # human-facing, 1-based within Section
├── source_order                  # deterministic document traversal order
├── normalized_range              # Section.content-relative
├── text                          # exact normalized slice
└── segmentation_version
```

The current source-order traversal is:

```text
root Sections in persisted order
  ↓
Paragraphs in owner Section order
  ↓
child Sections depth-first in persisted order
```

Because Section order is part of `normalized_document_hash`, a structural reorder also changes the normalized identity.

## 5. Coverage

Paragraph segmentation reports per-Section factual coverage:

```text
owner_chars
paragraph_chars
separator_chars
paragraph_count
```

Invariant:

```text
owner_chars = paragraph_chars + separator_chars
```

Whitespace separators are intentionally not fabricated as Paragraph TextUnits. They are known structural separators, not unsupported gaps.

The subsequent `feat/sentence-locator` increment now implements deterministic Sentence identity plus conservative persisted-text classification for obvious fenced/indented code and Markdown tables. Those non-prose Paragraphs remain Paragraph-addressable and are reported as coarse-only rather than receiving fabricated Sentence children. See [Sentence Locator and Coverage Contract](sentence-locator-contract.md).

The persisted Paragraph TextUnitIndex itself remains Paragraph-only; Sentence persistence and pagination are intentionally left to the enumeration contract.

## 6. TextUnitIndex boundary

A separate application port now stores derived units:

```text
TextUnitIndex
├── replace_document(document_id, units)
└── list_document(document_id)
```

`replace_document` means an atomic derived-state rebuild for one document version.

Two adapters exist:

```text
InMemoryTextUnitIndex
SqliteTextUnitIndex
```

The SQLite adapter persists:

```text
unit_id
source_order
document_id
raw content_hash
normalized_document_hash
owner_section_id
kind
paragraph_index
normalized range
exact text
segmentation_version
```

The table is separate from FTS `search_units`. TextUnitIndex and SearchIndex may share one physical SQLite file, but they remain separate logical stores and ports.

## 7. Open workflow integration

The production runtime builds Paragraph TextUnits during `open_document` after parsing canonical Document facts:

```text
retrieve
  ↓
parse canonical Document
  ↓
derive Paragraph TextUnits from Section.content
  ↓
save DocumentRepository
  ↓
replace TextUnitIndex
  ↓
update existing SearchIndex
```

The existing SearchIndex implementation is intentionally unchanged in this increment. It does not consume TextUnitIndex yet.

A compatibility constructor for direct application tests can still instantiate `OpenDocumentUseCase` without a TextUnitIndex. The production runtime uses the TextUnit-indexed constructor.

## 8. Rebuildability invariant

For the same canonical persisted Document and segmentation version:

```text
build(document).units
==
build(repository_round_trip(document)).units
```

For every Paragraph TextUnit:

```text
unit.text
==
owner_section.normalized_text_slice(unit.normalized_range)
```

No derived row may be required to reconstruct canonical `Section.content`.

## 9. Persistence semantics

`SqliteTextUnitIndex::replace_document` deletes the prior derived units for that document id and inserts the new deterministic set in one transaction.

It validates:

```text
every unit belongs to the requested document_id
source_order is contiguous and matches input order
paragraph ordinal is non-zero
persisted numeric fields are representable
persisted normalized ranges are ordered
persisted range length equals exact Unicode-scalar text length
persisted kind is supported
```

Reopening the SQLite adapter must reproduce the same ordered Paragraph TextUnit values.

## 10. Acceptance evidence

Tests cover:

- 1-based Paragraph ordinals;
- Unicode-scalar normalized ranges;
- exact text slicing;
- internal multiline Paragraph preservation;
- leading/trailing whitespace preservation inside Paragraph content;
- blank-line separator accounting;
- whitespace-only Section behavior;
- deterministic TextUnit IDs;
- raw provenance changes not redefining identical normalized Paragraph identity;
- normalized text changes invalidating TextUnit IDs;
- deterministic cross-Section source order;
- rebuild equality after canonical Document SQLite round-trip;
- InMemory/OpenDocument integration;
- SQLite TextUnitIndex persistence and replacement;
- rejection of malformed derived Paragraph rows.

The normal repository release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 11. Current boundary and next step

Paragraph indexing is complete for this foundation. Sentence locator identity and non-prose coverage are now implemented separately, but the following remain intentionally unimplemented:

```text
Sentence persistence migration
TextLocator wire DTOs
get_text_units
TextUnitCursor
Paragraph/Sentence MCP context
SearchHit → TextLocator
Paragraph/Sentence FTS
EPUB structure/parser changes
```

The next dependency step is `feat/text-unit-enumeration-contract`.
