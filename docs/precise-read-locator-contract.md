# Precise Read Locator Contract

> Status: Implemented P1 precise-read foundation
>
> Branch: `feat/precise-read-locator`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/text-unit-enumeration-contract.md`, `docs/context-granularity-contract.md`

## 1. Goal

This increment closes the direct source handoff from precise enumeration into canonical read without adding another MCP Tool:

```text
get_text_units
    ↓
TextLocator
    ↓
read_document(target_locator=...)
```

`read_document` now has two deliberately distinct read modes:

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

Exact Section read means **only the canonical `Section.content` of that Section**.

It does not recursively render descendants. This is intentionally different from the legacy `section_id` read.

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

Every exact read validates:

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

## 5. Exact read stream

The exact target has an independently versioned logical stream:

```text
read_mode         = exact_target
rendering_version = exact-normalized-source/v1
coordinate_space  = exact-target-unicode-scalar/v1
```

The logical stream is the target's canonical normalized text with no Markdown heading rendering or descendant insertion.

`stream.start_char/end_char` are zero-based Unicode-scalar positions **relative to that exact target stream**. They are continuation progress, not a source locator.

The distinction remains normative:

```text
TextLocator            = canonical source identity
returned_locator       = exact source range represented by this response segment
ReadCursor             = opaque progress through the versioned read stream
stream.start/end_char  = stream-local progress coordinates
```

## 6. `resolved_target_locator` and `returned_locator`

Every read response now carries:

```text
resolved_target_locator
```

For legacy Section-tree mode it is the resolved Section locator. Because rendered Section-tree output cannot be represented as one contiguous canonical source range, legacy responses use:

```text
returned_locator = null
```

For `exact_target`, every response segment carries:

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

Oversized exact targets are not rejected merely because one MCP response budget is smaller than the target. They use actionable `ReadCursor` continuation.

Current cursor schema remains:

```text
read-cursor/v2
```

For legacy Section-tree cursors, the previous v2 serialized claims remain unchanged.

Exact-target cursors add optional mode-specific bindings:

```text
target_kind
target_paragraph_index?
target_sentence_index?
target_range_start?
target_range_end?
target_segmentation_version?
```

along with the existing bindings:

```text
document_id
raw content_hash
normalized_document_hash
owner/root Section
read_mode
rendering_version
next stream character position
cursor schema version
```

A legacy Section-tree cursor cannot be used for exact read and an exact cursor cannot be used for Section-tree read. An exact cursor cannot be resumed with another locator.

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

As with legacy continuation, a continuation call with `max_chars=0` is rejected because it cannot advance the stream. Initial zero-budget behavior remains compatible and may return an actionable position-zero cursor for a non-empty target.

## 9. Acceptance evidence

Tests cover:

- exact Sentence read;
- exact Paragraph read;
- Unicode CharacterRange read;
- exact Section locator reads only its own `Section.content`;
- returned source locator slices exactly equal response content;
- multi-page exact target reconstruction with no gap/overlap;
- exact cursor target binding;
- stale normalized-document locator failure;
- malformed/out-of-bounds locator failure;
- real stdio `get_text_units → TextLocator → read_document` handoff;
- real exact-target ReadCursor continuation;
- legacy Section-tree stdio read compatibility.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 10. Explicit non-goals / next dependency

This increment does not implement:

```text
SearchHit → precise TextLocator
Paragraph/Sentence FTS
anchor-based get_text_units before/after(locator)
Sentence SQLite persistence
EPUB parser/navigation restructuring
```

With both direct consumers now available:

```text
TextLocator → read_document
TextLocator → get_context
```

the next dependency step is `feat/search-locator`: make SearchHit carry the strongest truthful version-bound TextLocator and hand it directly to both consumers without snippet re-search.
