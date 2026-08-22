# TextUnit Enumeration Contract

> Status: Implemented P1 enumeration foundation
>
> Branch: `feat/text-unit-enumeration-contract`
>
> Related: `docs/tool-contract-use-case-design.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/paragraph-text-unit-index.md`, `docs/sentence-locator-contract.md`

## 1. Goal

This increment realizes the use-case-approved `OrderedTextUnitEnumeration` capability as the seventh runtime MCP Tool:

```text
get_text_units
```

It answers a different question from `read_document`:

```text
read_document
= read an already-known target / continue one read stream

get_text_units
= discover and enumerate first/next Paragraph or Sentence-first reading items
```

The runtime Tool surface is now:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

## 2. Current v1 request

```text
document_id
section_id
requested_kind: paragraph | sentence       # default sentence
direction: forward | backward              # default forward
coverage_policy: preserve_source | eligible_only
max_items                                   # default 32, max 256
max_chars?                                  # default 32768, max 65536
cursor?
```

The initial runtime starts from the selected Section boundary. Cursor continuation is implemented in both directions.

The accepted design also allows future anchor starts such as `after(locator)` / `before(locator)`. Those are intentionally deferred until locator-input/context handoff is implemented; they are not required for the core complete-Section state machine.

A cursor request may change page budgets (`max_items` / `max_chars`) but cannot redefine document, Section, requested kind, direction, coverage policy, or segmentation semantics.

## 3. Target scope

`section_id` owns exactly one Section's `Section.content` stream.

```text
get_text_units(section=A)
→ A's Paragraph/Sentence units only
```

It does not recursively include child Sections. Structural traversal remains the responsibility of `get_document_structure` and the Agent's selected workflow.

This avoids conflating:

```text
Section tree traversal
≠
TextUnit enumeration inside one Section
```

## 4. TextLocator output

Enumeration introduces the canonical `TextLocator` domain/wire output foundation:

```text
TextLocator
├── document_id
├── content_hash                    # raw-source provenance
├── normalized_document_hash
├── owner_section_id
├── section_path
├── paragraph_index?
├── sentence_index?
├── normalized_range?
├── segmentation_version?
└── native_location?
```

Paragraph/Sentence ranges use:

```text
section-content-unicode-scalar/v1
zero-based
half-open [start, end)
```

For every precise item:

```text
item.text
==
owner_section.normalized_text_slice(item.locator.normalized_range)
```

The locator is source identity. It is not a cursor and does not contain pagination progress.

The current increment emits locators but does not yet accept them as `read_document` / `get_context` input; direct locator handoff is a later short-lived increment.

## 5. Declared stream

### Paragraph request

```text
Section
  ↓
¶1
  ↓
¶2
  ↓
...
  ↓
¶N
```

### Sentence request with `preserve_source`

```text
Section
  ↓
¶1 S1
  ↓
¶1 S2
  ↓
...
  ↓
recognized non-prose ¶ as coarse Paragraph item
  ↓
...
  ↓
¶N SM
```

A Sentence request never invents Sentence identity for code/table content. Strong non-prose Paragraphs use:

```text
effective_kind = paragraph
content_class = non_prose
degradation = requested_sentence_but_non_prose_is_paragraph_only
```

## 6. Coverage policies

### `preserve_source`

Default complete-reading policy.

Recognized non-prose remains in the declared stream as a coarse Paragraph item. A terminal response may claim:

```text
complete = true
source_complete = true
section_complete = true
```

only when the declared stream is exhausted and there are no intentionally skipped regions/unsupported gaps.

### `eligible_only`

This policy intentionally narrows the stream to eligible fine-grained content.

Even when a particular Section happens to contain only prose, `eligible_only` does **not** claim all-source completion by contract:

```text
complete = true             # eligible stream consumed
source_complete = false     # all-source guarantee not provided
section_complete = false
```

When recognized non-prose exists, `intentionally_skipped` reports the number of skipped coarse Paragraph regions.

## 7. Coverage result

Current factual coverage:

```text
owner_chars
section_separator_chars
sentence_separator_chars
paragraph_count
sentence_eligible_paragraphs
non_prose_paragraphs
represented_paragraphs
represented_sentences
coarse_non_prose_items
intentionally_skipped
unsupported_gaps
source_complete
```

Coverage describes the full declared Section stream, while `stream.start_index/end_index` describes only the current response page.

`unsupported_gaps` is currently zero for the canonical normalized Section stream implemented here. Publication/resource coverage gaps remain a separate parser/reliability concern.

## 8. TextUnitCursor

Current schema:

```text
text-unit-cursor/v1
```

Cursor claims bind:

```text
document_id
raw content_hash
normalized_document_hash
section_id
text-segmentation/v1
requested_kind
direction
coverage_policy
next_index
total_items
cursor schema version
```

The cursor uses the same versioned opaque-envelope pattern as `ReadCursor`, with bounded encoding and checksum validation.

Rules:

- wrong document/Section/kind/direction/policy → `CURSOR_TARGET_MISMATCH`;
- changed raw/normalized identity or segmentation contract → stale/fail closed;
- invalid/tampered/terminal position → invalid cursor;
- no fuzzy rebasing;
- cursor is never citation identity.

## 9. Pagination invariants

For forward pages:

```text
page[n].end_index == page[n+1].start_index
```

For backward continuation:

```text
page[n+1].end_index == page[n].start_index
```

Each individual response always returns its items in canonical source order, including backward traversal.

Every incomplete response has `next_cursor`.

Terminal response:

```text
complete = true
next_cursor = null
```

Repeated pages consume the declared stream without gap or overlap when the returned cursor is used as-is.

## 10. Response budgets

Server limits:

```text
max_items default = 32
max_items max     = 256

max_chars default = 32768
max_chars max     = 65536
```

`max_chars` budgets canonical item text. One TextUnit is atomic for enumeration:

```text
one item > max_chars
→ RESOURCE_LIMIT_EXCEEDED
```

The implementation does not split a Sentence/Paragraph and then pretend the fragment is the same TextUnit locator.

Continuation may choose a different page budget without changing stream identity.

## 11. Sentence persistence decision

Sentence rows are not added to SQLite in this increment.

Correctness evidence shows the enumeration stream can be rebuilt from:

```text
persisted canonical Document
+ deterministic Paragraph segmentation
+ deterministic Sentence segmentation
+ text-segmentation/v1
```

and `TextUnitCursor` successfully continues after `SqliteDocumentRepository` reopen without Sentence persistence.

Therefore:

```text
Sentence persistence = performance optimization candidate
not source truth
not current correctness dependency
```

The existing Paragraph `TextUnitIndex` remains available as independent derived state. SearchIndex/FTS remains unchanged.

A future Sentence persistence migration requires measured performance evidence and must remain rebuildable.

## 12. MCP response

Conceptual shape:

```text
document_id
target_section_locator
requested_kind
direction
coverage_policy
items[]:
  text
  locator
  effective_kind
  content_class
  content_class_detail
  degradation?
complete
section_complete
next_cursor?
coverage
stream:
  direction
  start_index
  end_index
  total_items
```

This is additive relative to the previous six-Tool runtime.

## 13. Acceptance evidence

Tests cover:

- seven-Tool MCP discovery;
- real stdio Sentence enumeration and cursor continuation;
- forward gap-free/overlap-free pages;
- backward traversal with source-ordered response pages;
- exact Paragraph/Sentence locators;
- Section scope does not absorb child Sections;
- source-preserving non-prose coarse items;
- eligible-only no all-source completion claim;
- raw/normalized stale cursor validation;
- cursor stream-contract mismatch;
- bounded response behavior without TextUnit splitting;
- repository restart continuation without Sentence persistence;
- existing Search/Context/Read contracts remain operational.

Release gate:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 14. Explicit non-goals / next steps

Not implemented here:

```text
anchor-based before/after TextUnit start
TextLocator input to read_document
Paragraph/Sentence context relations
SearchHit → TextLocator
Paragraph/Sentence FTS
Sentence SQLite persistence
EPUB block/structure redesign
```

The next dependency step is `feat/context-granularity`: consume TextLocator as an anchor and implement explicit tagged neighbor/container/structural context without changing enumeration cursor semantics.
