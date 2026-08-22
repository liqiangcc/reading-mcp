# ADR 0005: Block-Aware TextUnit Identity Migration

- Status: Accepted
- Date: 2026-08-22
- Reviewed branch: `design/block-aware-text-unit-identity`
- Reviewed against main: `1c95e430819f5e6ae422f3276cbf64ec28b18787`
- Implementation status: pending `feat/block-aware-text-unit-identity`
- Related: ADR 0002, ADR 0003, ADR 0004, `docs/block-aware-text-unit-identity-migration.md`, `docs/normalized-block-model-contract.md`, `docs/epub-structure-validator-contract.md`

## Context

Reading MCP already persists exact HTML/XHTML native block evidence through `normalized-block-model/v1`, and EPUB validation now measures block/TextUnit agreement and non-prose overlap from persisted facts.

Current TextUnit identity still ignores that evidence:

```text
normalized-document-hash/v1
+ text-segmentation/v1
→ Paragraph / Sentence / TextLocator
```

`text-segmentation/v1` uses blank-line Paragraph splitting and persisted-text heuristics for obvious code/table content. If native block facts begin changing Paragraph/Sentence boundaries without an identity migration, old locators/cursors could be silently reinterpreted and persisted lexical rows could point at a different logical stream.

## Decision

### 1. Advance TextUnit segmentation to v2

Accepted current implementation target:

```text
text-segmentation/v2
```

Segmentation v2 consumes only persisted canonical facts:

```text
Document / Section.content
+ optional valid normalized-block-model/v1
```

No transient DOM/ZIP state, FTS row, snippet similarity, LLM segmentation, or fuzzy repair is allowed.

### 2. Native block evidence defines primary Paragraph boundaries

Mapping:

```text
paragraph    → one sentence-eligible Paragraph
blockquote   → one sentence-eligible Paragraph
list_item    → one sentence-eligible Paragraph
preformatted → one coarse Paragraph; no Sentence children
table        → one coarse Paragraph; no Sentence children
```

Each native candidate uses the exact persisted block range.

Because block model v1 is flat/maximal, v2 does not invent nested Paragraph identity inside `blockquote` or other outer blocks. Richer nested identity requires a future versioned block model and another explicit migration.

### 3. Preserve uncovered source through deterministic fallback

For gaps between native block ranges:

```text
whitespace-only
→ separator coverage

non-whitespace
→ existing v1-style Paragraph fallback scoped to the gap
```

Current strong text heuristics for fenced/indented code and Markdown tables apply only where native block evidence is absent. Native evidence outranks text heuristics.

Absent block map is supported fallback. A declared but invalid block map is an integrity failure and must not silently degrade to fallback.

### 4. Native semantics remain evidence, not a new locator kind

`blockquote`, `list_item`, `preformatted`, and `table` do not create new MCP locator kinds or Tools.

They remain canonical block evidence that can be projected into TextUnit content-class/detail and provenance. Paragraph/Sentence source addresses remain the existing TextLocator shapes.

### 5. Advance normalized document identity to v2

Accepted target:

```text
normalized-document-hash/v2
```

In addition to v1 Section identity inputs, v2 binds the identity-bearing block projection:

```text
block-map presence/absence
block schema version
ordered owner_section_id
block_index
source_order
kind
normalized_range
```

It excludes provenance/diagnostics that do not define normalized source addressing, including native location/anchor, validator report, coverage counters, and lexical state.

Rationale: changing block kind/range/order under identical Section text can change Paragraph/Sentence boundaries and cursor streams. That change must alter normalized identity.

### 6. Keep TextUnit ID derivation namespace unless its algorithm changes

Current `text-unit-id/v1` already includes normalized identity, exact range, ordinal/kind, and segmentation version. New v2 normalized/segmentation inputs therefore generate new TextUnit IDs without requiring a new ID-derivation namespace solely for boundary-policy migration.

### 7. Fail old locator/cursor state closed

After migration:

```text
v1 Paragraph/Sentence TextLocator → STALE_LOCATOR
v1 TextUnitCursor                 → STALE_CURSOR
old ReadCursor                    → stale via normalized identity mismatch
```

No old locator/cursor is rebound to a v2 range even if text happens to match.

Because normalized-document identity advances globally, previously issued Section/CharacterRange locators also fail closed when carrying the old hash. This conservative invalidation is accepted.

TextLocator and current cursor wire shapes do not need a version bump solely because their existing claims already carry the changing identity/version inputs.

### 8. Rebuild derived persistence

Paragraph TextUnit rows remain rebuildable and already store normalized hash + segmentation version. Existing rows must not be accepted as current v2 facts; replacement remains atomic per document.

Persistent lexical state must advance:

```text
lexical-search-index/v2
→ lexical-search-index/v3
```

because it stores Paragraph/Sentence TextLocators whose identity changes.

Tokenizer stays:

```text
lexical-tokenizer/v1
```

because query tokenization policy is unchanged.

The v3 lexical index is rebuilt from canonical persisted Document facts; no source fetch/reparse is required merely to rebuild derived search state.

### 9. Parser/cache normalization stays v5 unless parser output changes

This decision changes derived TextUnit/address identity, not the current parser output contract.

Therefore:

```text
reading-mcp-normalization/v5
```

remains valid unless implementation also modifies canonical parser output.

A persisted Document without a block map remains readable through explicit fallback; derived-state migration does not secretly fetch/reparse the source.

### 10. MCP Tool surface remains seven

No new Tool or caller-selected segmentation mode is accepted.

Current workflow remains:

```text
get_text_units → TextLocator → read_document / get_context
search_document → SearchHit.text_locator → read_document / get_context
```

The server exposes one current canonical segmentation policy and rejects stale historical identity.

## Consequences

Positive:

- EPUB/HTML Paragraph/Sentence identity can use persisted native structure instead of text-only guessing;
- `pre`/table punctuation can no longer fabricate Sentence identity when native evidence is available;
- list/quote semantics remain explicit evidence without Tool/locator proliferation;
- block-map changes cannot silently reuse old normalized identity;
- persistent search rows migrate coherently with precise locator identity;
- blockless formats retain deterministic fallback behavior.

Costs:

- normalized document identity changes globally;
- all existing locator/read-cursor state bound to the old normalized hash becomes stale;
- TextUnit materialization must handle native ranges plus uncovered gaps and expose richer coverage;
- lexical persistent state requires v3 rebuild;
- a malformed declared block map must surface as an integrity error rather than being ignored.

## Acceptance invariants

1. Native Paragraph/BlockQuote/ListItem ranges become exact sentence-eligible Paragraphs.
2. Native Preformatted/Table ranges become exact coarse Paragraphs with no fake Sentences.
3. Native evidence outranks fallback text heuristics.
4. Uncovered non-whitespace source remains represented through offset-correct deterministic fallback.
5. Whitespace gaps remain separators.
6. Mixed native/fallback Paragraph ordering is deterministic.
7. No Sentence crosses a v2 Paragraph boundary.
8. Absent block evidence is supported; invalid declared evidence fails closed.
9. Hash v2 binds identity-bearing block facts but excludes diagnostics/provenance-only changes.
10. Old locator/cursor identity fails stale and is never fuzzy-rebased.
11. lexical-search-index/v2 cannot survive as current precise state; v3 rebuild is required.
12. lexical-tokenizer/v1 remains independent from segmentation.
13. Runtime Tool count remains seven.
14. Reopen/restart reproduces identical v2 TextUnits from persisted facts.

## Explicit non-goals

- nested block-tree identity;
- heading-title CharacterRange coordinates;
- SVG/fixed-layout precise blocks;
- Sentence SQLite persistence;
- new MCP Tools;
- caller-selectable historical segmentation;
- fuzzy source rebasing;
- tokenizer/ranking changes.

## Implementation order

```text
normalized-document-hash/v2
→ text-segmentation/v2 Paragraph projection
→ native/fallback Sentence eligibility + coverage
→ locator/cursor stale gates
→ derived TextUnit replacement
→ lexical-search-index/v3 rebuild
→ stdio direct-handoff acceptance
```

## Review outcome

Accepted. The implementation must treat persisted native block evidence as a new identity-bearing input and migrate all dependent derived state explicitly. It must not hide the change behind `text-segmentation/v1`, reuse old precise locators, or add a new MCP Tool for block-specific reading.
