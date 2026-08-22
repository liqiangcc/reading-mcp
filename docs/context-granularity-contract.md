# Context Granularity Contract

> Status: Implemented P1 precise-context foundation
>
> Branch: `feat/context-granularity`
>
> Follow-ups: `feat/precise-read-locator` and `feat/search-locator`
>
> Related: `docs/tool-contract-use-case-design.md`, `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/text-unit-enumeration-contract.md`, `docs/precise-read-locator-contract.md`, `docs/search-locator-contract.md`

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

```text
document_id
section_id
before
after
max_chars?
```

When `relation` is absent, this means exactly `neighbor(unit=section)` and keeps historical shallow Section rendering plus legacy response fields.

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

With an explicit relation, `section_id` and `target_locator` are mutually exclusive. A locator cannot be supplied without an explicit relation because that would make context semantics implicit again.

## 3. Locator validation

Context now delegates canonical identity/stale validation to the shared application-level TextLocator resolver also used by exact read.

The shared resolver covers:

```text
document_id
raw content_hash
normalized_document_hash
owner_section_id
Section / CharacterRange / Paragraph / Sentence shape
Paragraph/Sentence ordinal
text-segmentation/v1
exact Section-relative normalized range
```

Current valid **context anchor** shapes remain:

```text
Section
Paragraph
Sentence
```

CharacterRange is a valid canonical TextLocator kind in the shared resolver and exact read, but current context relations do not accept it. Context therefore returns a request-level unsupported semantic error for CharacterRange rather than misclassifying it as malformed identity.

Failure taxonomy for actual identity problems:

```text
INVALID_LOCATOR
STALE_LOCATOR
```

No title search, snippet comparison, nearest-text match, ordinal rebasing or fuzzy relocation is allowed.

## 4. Neighbor semantics

### Section neighbor

`neighbor(unit=section)` uses existing flattened structural source order and remains compatible with the legacy call.

### Paragraph neighbor

Requirements:

- anchor is a Paragraph locator;
- before/after are deterministic Paragraph TextUnits;
- traversal stays inside the same owner Section;
- child Sections are not crossed implicitly;
- every returned Paragraph has its exact TextLocator.

### Sentence neighbor

Requirements:

- anchor is a Sentence locator;
- response is canonical source order;
- semantics match `get_text_units(requested_kind=sentence, coverage_policy=preserve_source)`;
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

`container(kind=paragraph)` resolves a Sentence through deterministic Paragraph ownership. A Paragraph resolves to itself. A Section anchor cannot be reinterpreted as a Paragraph container.

### TextUnit / Section → owner Section

`container(kind=section)` returns the exact owner Section selected through locator ownership. Legacy Section content projection may be bounded/truncated while the structured item carries Section identity.

## 6. Structural semantics

```text
structural(owner_section)
structural(ancestors)
structural(siblings)
structural(children)
```

These use Section identity/parentage, never title search.

- `owner_section`: exactly the locator's owner Section;
- `ancestors`: root to immediate parent;
- `siblings`: same parent, excluding owner;
- `children`: direct children only.

Structural items are metadata-oriented (`title + TextLocator`) and do not become an implicit body-read stream. Response is bounded to 100 structural items.

## 7. Response shape and token policy

Legacy fields remain:

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

For locator-driven Paragraph/Sentence/structural context, canonical content lives in `items[]`; top-level legacy `content` stays empty to avoid duplicate body tokens. Legacy Section-neighbor and Section-container preserve their old projection.

## 8. Response budgets

Section legacy projections preserve existing bounded/truncating semantics.

Precise Paragraph/Sentence items are atomic:

```text
exact TextUnit > max_chars
→ RESOURCE_LIMIT_EXCEEDED
```

A precise item is never split and returned under its original TextLocator. A context window exceeding the budget fails explicitly because context has no traversal cursor; it is a bounded expansion around a known anchor.

## 9. Relationship to enumeration, exact read, and search

```text
get_text_units
= discover/enumerate reading items

get_context
= bounded expansion around one known locator

read_document(target_locator)
= canonical exact source read

search_document
= bounded retrieval candidate + direct locator handoff
```

`TextUnitCursor` is not accepted by context, and context does not redefine enumeration progress.

Sentence neighbor source order/coarse semantics are regression-tested against `get_text_units(... preserve_source)`.

A single locator can now flow from either enumeration or search into context/read:

```text
get_text_units ─→ TextLocator ─┬→ get_context
                               └→ read_document

search_document → SearchHit.text_locator ─┬→ get_context
                                         └→ read_document
```

Current SearchHit locator is Section-level because the existing SearchIndex cannot yet prove canonical Paragraph/Sentence identity.

## 10. MCP surface

No new Tool is added. Runtime remains seven Tools.

## 11. Acceptance evidence

Tests cover:

- Sentence ±N from exact TextLocator;
- Paragraph ±N inside one owner Section;
- Sentence → containing Paragraph;
- owner Section / ancestors / siblings / children;
- deterministic order/roles;
- non-prose coarse Sentence context;
- parity with `get_text_units(preserve_source)`;
- stale/malformed locator failures;
- precise TextUnit budget atomicity;
- legacy Section-neighbor compatibility;
- real stdio `get_text_units → TextLocator → get_context`;
- exact-read/context parity through the shared resolver;
- real stdio `search_document → Section TextLocator → get_context`.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 12. Current non-goals / next dependency

Now implemented by follow-ups:

```text
TextLocator → exact read_document
SearchHit → truthful Section TextLocator → context/read
shared locator identity resolver
```

Still deferred:

```text
canonical Paragraph/Sentence search candidates
CJK/mixed technical tokenizer versioning
anchor-based get_text_units before/after(locator)
Sentence SQLite persistence
EPUB parser/navigation restructuring
```

The next search-precision dependency is `feat/lexical-text-unit-index`. It must preserve title-only Section candidates while adding canonical Paragraph/Sentence candidates whose locator identity is actually stored/provable.
