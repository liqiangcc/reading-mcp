# Context Granularity Contract

> Status: Implemented precise-context foundation; block-aware evidence aligned
>
> Foundation branch: `feat/context-granularity`
>
> Current evidence alignment: `fix/block-aware-context-evidence`
>
> Related: `docs/tool-contract-use-case-design.md`, `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/text-unit-enumeration-contract.md`, `docs/precise-read-locator-contract.md`, `docs/search-locator-contract.md`

## 1. Goal

`get_context` expands around a known canonical `TextLocator` without creating another Tool or redefining enumeration/read semantics.

Tagged relations remain:

```text
neighbor
container
structural
```

Runtime Tool count remains seven.

## 2. Request paths

Legacy Section-neighbor request remains:

```text
document_id
section_id
before
after
max_chars?
```

When `relation` is absent this means `neighbor(unit=section)`.

Structured request:

```text
document_id
section_id?
or target_locator?

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
```

With an explicit relation, `section_id` and `target_locator` are mutually exclusive.

## 3. Locator validation

Context delegates canonical identity/stale validation to the shared TextLocator resolver.

Current resolver contract:

```text
document_id
raw content_hash
normalized-document-hash/v2
owner_section_id
Section / CharacterRange / Paragraph / Sentence shape
Paragraph/Sentence ordinal
text-segmentation/v2
exact Section-relative normalized range
```

Valid context anchors:

```text
Section
Paragraph
Sentence
```

CharacterRange remains valid for exact read but is not a supported context anchor.

Identity failures are fail-closed:

```text
INVALID_LOCATOR
STALE_LOCATOR
```

No title search, snippet comparison, nearest-text match, ordinal rebasing, or fuzzy relocation is allowed.

Paragraph/Sentence materialization uses the fallible block-aware path; invalid persisted block evidence returns `TEXT_UNIT_INDEX_FAILED` rather than panicking or silently falling back.

## 4. Neighbor semantics

### Section

`neighbor(unit=section)` keeps historical flattened structural source order.

### Paragraph

- anchor must be a Paragraph locator;
- before/after remain inside the same owner Section;
- every item carries its exact current Paragraph locator;
- `content_class` preserves current block-aware Paragraph evidence.

### Sentence

Sentence context must remain source-order/evidence compatible with:

```text
get_text_units(
  requested_kind = sentence,
  coverage_policy = preserve_source
)
```

Under `text-segmentation/v2`:

```text
native paragraph
→ Sentence items
→ content_class = native_paragraph

fallback prose/unknown
→ Sentence items
→ content_class = prose_or_unknown

native blockquote / list_item
→ coarse Paragraph item
→ degradation = flat_native_container_no_nested_textunit_evidence

native/fallback non-prose
→ coarse Paragraph item
→ degradation = requested_sentence_context_but_non_prose_is_paragraph_only
```

The BlockQuote/ListItem degradation is structural-evidence degradation, not a false non-prose label.

No Sentence is fabricated for a coarse region.

Context windows remain bounded to 20 items on either side.

## 5. Container semantics

### Sentence / Paragraph → containing Paragraph

`container(kind=paragraph)` resolves through current block-aware Paragraph ownership. Paragraph resolves to itself; Sentence resolves to its deterministic parent Paragraph.

The returned item preserves the current Paragraph `content_class`.

### TextUnit / Section → owner Section

`container(kind=section)` returns the exact owner Section selected through locator ownership.

## 6. Structural semantics

```text
structural(owner_section)
structural(ancestors)
structural(siblings)
structural(children)
```

These use Section identity/parentage, never title search.

Structural context stays metadata-oriented and does not become an implicit body-read stream. Response remains bounded to 100 structural items.

## 7. Response shape

Legacy fields remain:

```text
document_id
source
owner_section_id
content
location
truncated
```

Structured fields:

```text
complete
anchor_locator
relation
items[]:
  title?
  content?
  locator
  role
  effective_kind
  content_class?
  degradation?
```

For locator-driven Paragraph/Sentence/structural context, canonical content lives in `items[]`; top-level legacy `content` remains empty to avoid duplicate body tokens.

## 8. Response budgets

Precise Paragraph/Sentence items are atomic:

```text
exact TextUnit > max_chars
→ RESOURCE_LIMIT_EXCEEDED
```

Context never splits a TextUnit while retaining its original locator.

## 9. Relationship to enumeration/read/search

```text
get_text_units
= discover/enumerate source-ordered reading items

get_context
= bounded expansion around one known locator

read_document(target_locator)
= exact canonical source read

search_document
= bounded retrieval candidate + direct locator handoff
```

Direct handoff remains:

```text
get_text_units ─→ TextLocator ─┬→ get_context
                               └→ read_document

search_document → SearchHit.text_locator ─┬→ get_context
                                         └→ read_document
```

Context does not accept TextUnitCursor or redefine enumeration progress.

## 10. Current search precision

Current precise lexical index can emit:

```text
section | paragraph | sentence
```

Coarse structural/non-prose content is Paragraph-searchable only; no fake Sentence candidate is produced.

A search locator is revalidated against current canonical Document/TextUnit facts before context handoff.

## 11. Acceptance evidence

Tests cover:

- Sentence ±N from exact TextLocator;
- Paragraph ±N inside one owner Section;
- Sentence → containing Paragraph;
- owner Section / ancestors / siblings / children;
- deterministic order/roles;
- fallback non-prose coarse Sentence context;
- native BlockQuote/ListItem structural degradation;
- native Paragraph Sentence `content_class` preservation;
- context/enumeration locator + content + evidence parity;
- invalid block-aware materialization fails closed;
- stale/malformed locator failures;
- precise TextUnit budget atomicity;
- legacy Section-neighbor compatibility;
- stdio TextLocator handoff from enumeration/search to context/read.

Release gate:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 12. Non-goals

```text
new context Tool
CharacterRange context
nested/leaf block recovery
fuzzy locator rebase
context traversal cursor
Sentence SQLite persistence
```

Finer quote/list Sentence context requires stronger persisted nested/leaf block evidence and an explicit future identity migration.
