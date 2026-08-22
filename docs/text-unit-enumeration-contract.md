# TextUnit Enumeration Contract

> Status: Implemented precise enumeration with anchor continuation
>
> Foundation branch: `feat/text-unit-enumeration-contract`
>
> Current anchor extension: `feat/text-unit-anchor-enumeration`
>
> Related: `docs/tool-contract-use-case-design.md`, `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/paragraph-text-unit-index.md`, `docs/sentence-locator-contract.md`, `docs/context-granularity-contract.md`

## 1. Goal

`get_text_units` is the ordered discovery/continuation capability for canonical Paragraph/Sentence reading items.

```text
read_document
= read an already-known exact target / continue one exact read

get_context
= bounded expansion around one known target

get_text_units
= enumerate a Section stream from a boundary or continue before/after one known TextLocator
```

Runtime Tool count remains seven.

## 2. Current request

```text
document_id
section_id
anchor_locator?                             # initial anchored start only
requested_kind: paragraph | sentence       # default sentence
direction: forward | backward              # default forward
coverage_policy: preserve_source | eligible_only
max_items                                   # default 32, max 256
max_chars?                                  # default 32768, max 65536
cursor?
```

`anchor_locator` and `cursor` are mutually exclusive.

A continuation request uses the returned cursor alone; it does not repeat the anchor.

## 3. Section scope

The declared stream belongs to exactly one `section_id`:

```text
get_text_units(section=A)
→ A's Paragraph/Sentence-first items only
```

Child Sections are not absorbed implicitly. Structural traversal remains a separate capability.

An anchor must resolve to the same owner Section as `section_id`.

## 4. Current TextUnit identity

Current identity inputs:

```text
normalized-document-hash/v2
+ text-segmentation/v2
+ optional valid normalized-block-model/v1 evidence
```

Every precise item contains the canonical current `TextLocator`.

Paragraph/Sentence ranges use:

```text
section-content-unicode-scalar/v1
zero-based
half-open [start,end)
```

Invariant:

```text
item.text
== owner_section.normalized_text_slice(item.locator.normalized_range)
```

No snippet search, nearest-text match, ordinal repair, or fuzzy rebasing participates in enumeration.

## 5. Declared stream

### Paragraph request

The stream contains current Paragraph-level reading units in exact Section source order.

Under block-aware v2 this can include:

```text
native paragraph
coarse blockquote
coarse list_item
coarse preformatted
table
fallback Paragraph
```

`eligible_only` may remove coarse Paragraphs according to the current eligibility policy.

### Sentence request with `preserve_source`

The declared stream interleaves:

```text
eligible Sentence items
+ coarse Paragraph items where Sentence identity is not justified
```

Current coarse behavior:

```text
blockquote / list_item
→ effective_kind = paragraph
→ flat_native_container_no_nested_textunit_evidence

preformatted / table / fallback code-table
→ effective_kind = paragraph
→ requested_sentence_but_non_prose_is_paragraph_only
```

No fake Sentence identity is generated.

## 6. Anchor semantics

An `anchor_locator` is a canonical source address and is validated through the shared TextLocator resolver used by read/context.

The resolved locator must also be an **actual member of the requested declared stream**.

This means stream policy matters:

```text
Paragraph anchor + requested_kind=paragraph
→ valid when that Paragraph is present

Sentence anchor + requested_kind=sentence
→ valid when that Sentence is present

coarse Paragraph anchor + sentence/preserve_source
→ valid because the coarse Paragraph is a real declared stream item

coarse Paragraph anchor + sentence/eligible_only
→ invalid request because that item is intentionally absent from the declared stream

Section / CharacterRange locator
→ valid locator kinds elsewhere, but not TextUnit stream anchors
```

The anchor is **exclusive**:

```text
direction = forward
→ enumerate strictly after anchor

direction = backward
→ enumerate strictly before anchor
```

Backward pages are still returned in canonical source order.

## 7. Anchor identity failures

Anchor validation distinguishes locator identity failure from stream-membership mismatch:

```text
malformed locator        → INVALID_LOCATOR
stale hash/version/range → STALE_LOCATOR
wrong owner Section      → INVALID_REQUEST
valid locator not in requested declared stream
                         → INVALID_REQUEST
```

A historical `text-segmentation/v1` TextLocator is never reinterpreted against the current v2 stream.

## 8. Coverage policies

### `preserve_source`

The full declared Section stream preserves every represented coarse/fine region.

### `eligible_only`

The stream intentionally omits coarse regions and therefore never claims all-source completion.

Current coverage includes:

```text
owner_chars
section_separator_chars
sentence_separator_chars
paragraph_count
sentence_eligible_paragraphs
non_prose_paragraphs
coarse_structural_paragraphs
represented_paragraphs
represented_sentences
coarse_non_prose_items
coarse_structural_items
intentionally_skipped
unsupported_gaps
source_complete
```

Coverage describes the whole declared Section policy, not only the current page or anchored suffix/prefix.

## 9. Cursor semantics

Current schema remains:

```text
text-unit-cursor/v1
```

Existing claims bind:

```text
document_id
raw content_hash
normalized_document_hash
section_id
text-segmentation/v2
requested_kind
direction
coverage_policy
next_index
total_items
```

Anchor continuation adds one backward-compatible optional claim:

```text
origin_anchor_index?
```

When absent, it is omitted from serialized JSON. Therefore previously issued unanchored `text-unit-cursor/v1` payloads retain their original serialized claim bytes/checksum behavior and remain decodable.

When present, the cursor proves that traversal originated from an internal anchor and preserves that fact across later pages.

Cursor rules remain fail-closed:

- changed raw/normalized identity → `STALE_CURSOR`;
- changed segmentation version → `STALE_CURSOR`;
- wrong document/Section/kind/direction/policy → `CURSOR_TARGET_MISMATCH`;
- impossible/tampered position or anchor origin → `INVALID_CURSOR`;
- no fuzzy rebasing.

## 10. Completion semantics

`complete` means the **requested traversal** is exhausted.

For a boundary-origin traversal:

```text
complete + preserve_source
→ section_complete may become true
```

For an anchor-origin traversal:

```text
complete
= reached the requested directional Section boundary

section_complete
= false
```

The latter is intentional: an exclusive anchor traversal did not enumerate the anchor itself or the opposite side of the Section, so it must not claim full-Section completion.

`start_anchor_locator` in the response is populated for every anchored page, including cursor continuation pages, so the origin remains auditable.

## 11. Pagination

The stream indexes remain indexes in the complete declared Section stream.

Forward:

```text
anchor index = A
initial position = A + 1
```

Backward:

```text
anchor index = A
initial exclusive end = A
```

Continuation invariants remain:

```text
forward page[n].end_index == page[n+1].start_index
backward page[n+1].end_index == page[n].start_index
```

Every response page is source ordered and one TextUnit remains atomic under `max_chars`.

## 12. MCP handoff

The same locator can now flow through all precise-reading capabilities:

```text
get_text_units ──┐
search_document ─┼→ TextLocator ─┬→ read_document
                 │               ├→ get_context
                 └───────────────└→ get_text_units(anchor_locator)
```

This removes the previous requirement to restart enumeration from a Section boundary after search/context/read has already identified an exact TextUnit.

No new Tool is introduced.

## 13. Acceptance evidence

Tests cover:

- forward exclusive anchor start;
- backward exclusive anchor start with source-ordered pages;
- anchored cursor continuation preserving origin;
- anchored terminal response never falsely claiming `section_complete`;
- anchor + cursor mutual exclusion;
- shared stale locator validation;
- valid locator rejected when absent from requested declared stream;
- existing forward/backward boundary pagination unchanged;
- old cursor behavior remains compatible when no origin anchor claim exists;
- real stdio `get_text_units → TextLocator → get_text_units(anchor_locator)` handoff;
- runtime Tool count remains seven.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 14. Explicit non-goals

```text
cross-Section TextUnit traversal
inclusive anchor mode
CharacterRange anchor enumeration
fuzzy anchor relocation
caller-selected historical segmentation
Sentence SQLite persistence
new MCP Tool
```

Cross-Section reading should remain an explicit composition of `get_document_structure` plus per-Section TextUnit enumeration rather than silently changing one Section-scoped stream into a document traversal API.
