# Precise Read Locator Contract

> Status: Implemented P1 precise-read foundation
>
> Branch: `feat/precise-read-locator`
>
> Follow-up: `feat/search-locator` now hands SearchHit Section locators directly into this consumer.
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/text-unit-enumeration-contract.md`, `docs/context-granularity-contract.md`, `docs/search-locator-contract.md`

## 1. Goal

This increment closes the direct source handoff from precise enumeration into canonical read without adding another MCP Tool:

```text
get_text_units
    ↓
TextLocator
    ↓
read_document(target_locator=...)
```

`read_document` has two deliberately distinct read modes:

```text
legacy section_id
→ SectionTreeReadStream/v1
→ selected Section + descendants rendered as the historical Section-tree stream

TextLocator target
→ exact_target
→ exact canonical source represented by that locator
```

The runtime Tool surface remains seven Tools.

## 2. Backward-compatible request

Legacy requests remain valid:

```text
read_document(
  document_id,
  section_id,
  max_chars?,
  cursor?
)
```

The additive precise form is:

```text
read_document(
  document_id,
  target_locator,
  max_chars?,
  cursor?
)
```

`section_id` and `target_locator` are mutually exclusive. A request must supply exactly one of them.

A continuation repeats the same structural target or exact locator plus the returned cursor. Page budget may change, but the cursor cannot redefine document, target, read mode or identity.

## 3. Supported exact target shapes

### Section locator

```text
paragraph_index      = null
sentence_index       = null
normalized_range     = null
segmentation_version = null
```

Exact Section read means only the canonical `Section.content` of that Section. It does not recursively render descendants.

### CharacterRange locator

```text
paragraph_index      = null
sentence_index       = null
normalized_range     = [start, end)
segmentation_version = null
```

The range must be a valid zero-based, half-open Unicode-scalar range in the exact owner `Section.content`.

### Paragraph locator

```text
paragraph_index      = 1-based
sentence_index       = null
normalized_range     = exact Paragraph range
segmentation_version = text-segmentation/v1
```

The current canonical Paragraph is rebuilt and both ordinal and range must agree.

### Sentence locator

```text
paragraph_index      = 1-based
sentence_index       = 1-based within Paragraph
normalized_range     = exact Sentence range
segmentation_version = text-segmentation/v1
```

The current canonical Sentence is rebuilt and ordinal/range identity must agree.

Any mixed or incomplete shape fails as `INVALID_LOCATOR`.

## 4. Identity validation

Exact read now delegates locator identity/stale validation to the shared application-level TextLocator resolver also used by structured context.

It validates:

```text
document_id
raw content_hash
normalized_document_hash
owner_section_id
normalized range when present
Paragraph/Sentence ordinal when present
segmentation_version when required
```

Rules:

- malformed shape or invalid/out-of-bounds source range → `INVALID_LOCATOR`;
- raw/normalized version mismatch → `STALE_LOCATOR`;
- Paragraph/Sentence no longer exists or its exact range changed → `STALE_LOCATOR`;
- no fuzzy matching;
- no snippet matching;
- no nearest ordinal/range rebasing.

The shared resolver determines whether a locator is valid; exact read then applies its own capability policy. Exact read accepts Section, CharacterRange, Paragraph, and Sentence.

## 5. Exact read stream

The exact target has an independently versioned logical stream:

```text
read_mode         = exact_target
rendering_version = exact-normalized-source/v1
coordinate_space  = exact-target-unicode-scalar/v1
```

The logical stream is the target's canonical normalized text with no Markdown heading rendering or descendant insertion.

`stream.start_char/end_char` are zero-based Unicode-scalar positions relative to that exact target stream. They are continuation progress, not a source locator.

```text
TextLocator            = canonical source identity
returned_locator       = exact source range represented by this response segment
ReadCursor             = opaque progress through the versioned read stream
stream.start/end_char  = stream-local progress coordinates
```

## 6. `resolved_target_locator` and `returned_locator`

Every read response carries `resolved_target_locator`.

For legacy Section-tree mode it is the resolved Section locator. Because rendered Section-tree output cannot be represented as one contiguous canonical source range:

```text
returned_locator = null
```

For `exact_target`, every segment carries:

```text
returned_locator = CharacterRange TextLocator
```

and must satisfy:

```text
response.content
==
owner_section.normalized_text_slice(returned_locator.normalized_range)
```

If an exact target is returned over multiple pages, adjacent source ranges are gap-free and overlap-free and concatenate to the full resolved target range.

## 7. Exact-target continuation

Oversized exact targets use actionable `ReadCursor` continuation.

Current cursor schema remains:

```text
read-cursor/v2
```

Legacy Section-tree v2 serialized claims remain compatible. Exact-target cursors add optional mode-specific bindings:

```text
target_kind
target_paragraph_index?
target_sentence_index?
target_range_start?
target_range_end?
target_segmentation_version?
```

along with document/raw/normalized identity, owner/root Section, read mode, rendering version, next stream position, and cursor schema.

A legacy Section-tree cursor cannot be used for exact read; an exact cursor cannot be used for Section-tree read or another exact locator.

## 8. Response-budget invariants

Initial reads use the existing body response budget:

```text
default max_chars = 32000
server max         = 64000
```

When more exact-target text remains:

```text
truncated = true
complete  = false
next_cursor != null
```

Terminal segment:

```text
truncated = false
complete  = true
next_cursor = null
```

A continuation call with `max_chars=0` is rejected because it cannot advance the stream. Initial zero-budget behavior remains compatible and may return an actionable position-zero cursor for a non-empty target.

## 9. Acceptance evidence

Tests cover exact Sentence/Paragraph/CharacterRange/Section reads, truthful returned source ranges, multi-page no-gap/no-overlap reconstruction, exact cursor target/mode binding, stale/malformed locator failure, real stdio `get_text_units → TextLocator → read_document`, and legacy Section-tree compatibility.

The later search-locator increment additionally proves:

```text
search_document → SearchHit.text_locator → read_document(target_locator)
```

using the current truthful Section-level SearchHit locator.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 10. Current non-goals / next dependency

SearchHit → TextLocator handoff is now implemented. Current SearchIndex can only prove owning Section identity, so SearchHit currently produces a Section locator.

Still deferred:

```text
canonical Paragraph/Sentence lexical candidates
independently versioned CJK/mixed technical tokenizer policy
anchor-based get_text_units before/after(locator)
Sentence SQLite persistence
EPUB parser/navigation restructuring
```

The next search-precision dependency is `feat/lexical-text-unit-index`: only after the lexical index stores/proves canonical Paragraph/Sentence identity may SearchHit emit those finer candidate kinds.
