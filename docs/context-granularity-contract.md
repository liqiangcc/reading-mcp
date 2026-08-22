# Context Granularity Contract

> Status: Implemented P1 precise-context foundation
>
> Branch: `feat/context-granularity`
>
> Related: `docs/tool-contract-use-case-design.md`, `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/text-unit-enumeration-contract.md`

## 1. Goal

This increment makes the existing `get_context` Tool consume canonical `TextLocator` anchors without adding another Tool or changing the meaning of the legacy Section-neighbor call.

The responsibilities are explicitly separated by a tagged relation:

```text
neighbor
container
structural
```

The current seven-Tool surface remains unchanged.

## 2. Backward-compatible request paths

### Legacy Section-neighbor request

The existing wire request remains valid:

```text
document_id
section_id
before
after
max_chars?
```

When `relation` is absent, this means exactly:

```text
neighbor(unit=section)
```

It keeps the historical shallow Section rendering and legacy `content / location / owner_section_id / truncated` behavior.

### Structured request

```text
document_id
section_id?        # Section anchor
or
target_locator?   # precise Section/Paragraph/Sentence anchor

relation:
  neighbor {
    unit: section | paragraph | sentence
    before
    after
  }
  | container {
    kind: paragraph | section
  }
  | structural {
    kind: owner_section | ancestors | siblings | children
  }

max_chars?
```

With an explicit relation, `section_id` and `target_locator` are mutually exclusive.

A locator cannot be supplied without an explicit relation because that would make context semantics implicit again.

## 3. Locator validation

A locator-consuming context request fails closed before expanding context.

Validation covers:

```text
document_id
raw content_hash provenance
normalized_document_hash
owner_section_id
Paragraph/Sentence shape
1-based paragraph_index / sentence_index
text-segmentation/v1
exact Section-relative normalized range
```

Valid shapes are:

```text
Section:
  no paragraph/sentence/range/segmentation fields

Paragraph:
  paragraph_index
  normalized_range
  segmentation_version

Sentence:
  paragraph_index
  sentence_index
  normalized_range
  segmentation_version
```

The implementation deterministically rebuilds the referenced Paragraph/Sentence from canonical persisted `Document / Section.content` and verifies that the current exact range matches the locator.

Failure taxonomy:

```text
INVALID_LOCATOR
STALE_LOCATOR
```

No title search, snippet comparison, nearest-text match, ordinal rebasing or fuzzy relocation is allowed.

## 4. Neighbor semantics

### Section neighbor

```text
neighbor(unit=section)
```

Uses the existing flattened structural source order and remains compatible with the legacy call.

### Paragraph neighbor

```text
neighbor(unit=paragraph)
```

Requirements:

- anchor must be a Paragraph locator;
- before/after items are deterministic Paragraph TextUnits;
- traversal stays inside the same owner Section;
- child Sections are not crossed implicitly;
- every returned Paragraph has its exact TextLocator.

### Sentence neighbor

```text
neighbor(unit=sentence)
```

Requirements:

- anchor must be a Sentence locator;
- response order is canonical source order;
- the stream follows the same source-preserving semantics as `get_text_units(requested_kind=sentence, coverage_policy=preserve_source)`;
- recognized non-prose code/table regions may appear as an explicitly coarser Paragraph item;
- no fake Sentence identity is generated.

For a coarse item:

```text
effective_kind = paragraph
degradation = requested_sentence_context_but_non_prose_is_paragraph_only
```

The context window is bounded to 20 items on either side.

## 5. Container semantics

### Sentence / Paragraph → containing Paragraph

```text
container(kind=paragraph)
```

A Sentence resolves through its deterministic Paragraph ownership. A Paragraph resolves to itself as the textual container.

The result is exactly one Paragraph item with an exact locator. A Section anchor cannot be reinterpreted as a Paragraph container.

### TextUnit / Section → owner Section

```text
container(kind=section)
```

Returns the exact owner Section selected through locator ownership. The legacy Section content projection may be bounded/truncated, while the structured item carries Section identity.

## 6. Structural semantics

```text
structural(owner_section)
structural(ancestors)
structural(siblings)
structural(children)
```

These relations use Section identity/parentage, never title search.

Semantics:

- `owner_section`: exactly the locator's owner Section;
- `ancestors`: root to immediate parent;
- `siblings`: same parent, excluding owner;
- `children`: direct children only.

Structural items are metadata-oriented (`title + TextLocator`) and do not become an implicit body-read stream. The response is bounded to 100 structural items.

## 7. Response shape and token policy

The legacy response fields remain:

```text
document_id
source
owner_section_id
content
location
truncated
```

Structured fields are additive:

```text
complete
anchor_locator
relation
items[]:
  title?
  content?
  locator
  role: before | anchor | after | container | structural
  effective_kind: section | paragraph | sentence
  content_class?
  degradation?
```

For locator-driven Paragraph/Sentence/structural context, canonical content lives in `items[]`; the top-level legacy `content` projection is intentionally empty so the same body is not duplicated in one MCP response.

Legacy Section-neighbor requests and Section-container responses keep their legacy `content` projection because old clients depend on it.

## 8. Response budgets

Section legacy projections preserve the existing bounded/truncating semantics.

Precise Paragraph/Sentence items are atomic:

```text
exact TextUnit > max_chars
→ RESOURCE_LIMIT_EXCEEDED
```

A precise item is never split and then returned under the original TextLocator.

A requested precise context window that exceeds the effective text budget also fails explicitly rather than returning a partial context window without a context cursor.

Context itself has no continuation cursor in this increment; it is a bounded expansion around an already-known anchor, not an ordered complete-reading stream.

## 9. Relationship to TextUnit enumeration

```text
get_text_units
= discover/enumerate reading items

get_context
= expand around one already-known locator
```

`TextUnitCursor` is not accepted by `get_context`, and context does not redefine enumeration progress.

Sentence neighbor source order/coarse semantics are regression-tested against `get_text_units(... preserve_source)` so these two capabilities cannot silently drift into different definitions of the same Sentence-first reading stream.

## 10. MCP surface

No new Tool is added.

Current runtime remains:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

## 11. Acceptance evidence

Tests cover:

- Sentence ±N from an exact TextLocator;
- Paragraph ±N stays inside one owner Section;
- Sentence → containing Paragraph;
- owner Section / ancestors / siblings / direct children;
- deterministic source order and role marking;
- recognized non-prose coarse items in Sentence context;
- parity with `get_text_units(preserve_source)`;
- normalized document changes produce `STALE_LOCATOR`;
- malformed locator shape produces `INVALID_LOCATOR`;
- precise TextUnit context is atomic under `max_chars`;
- legacy Section-neighbor semantics remain valid;
- real stdio `get_text_units → TextLocator → get_context` handoff.

Repository release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 12. Explicit non-goals / next dependency

Not implemented here:

```text
TextLocator input to read_document
SearchHit → TextLocator
Paragraph/Sentence FTS
anchor-based get_text_units before/after(locator) start
Sentence SQLite persistence
EPUB parser/navigation restructuring
```

The next dependency should be `feat/precise-read-locator`: make `read_document` consume the same canonical TextLocator in an exact-target mode. After both read and context consume locators, `feat/search-locator` can hand a SearchHit directly into either operation without an asymmetric precise-reading path.
