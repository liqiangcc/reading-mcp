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

Independent review also exposed an important evidence limit: `normalized-block-model/v1` is a flat maximal non-overlapping projection. A blockquote containing multiple nested `<p>` elements is persisted as one BlockQuote range, and the normalized body can lose the inner Paragraph separator. Therefore the migration must preserve such content coarsely rather than fabricate nested precision.

## Decision

### 1. Advance TextUnit segmentation to v2

Accepted implementation target:

```text
text-segmentation/v2
```

Segmentation v2 consumes only persisted canonical facts:

```text
Document / Section.content
+ optional valid normalized-block-model/v1
```

No transient DOM/ZIP state, FTS row, snippet similarity, LLM segmentation, or fuzzy repair is allowed.

### 2. Native block evidence defines primary Paragraph-level boundaries

Accepted mapping under flat block-model/v1 evidence:

```text
paragraph    → one exact sentence-eligible Paragraph
blockquote   → one typed coarse Paragraph-level unit; no Sentence children
list_item    → one typed coarse Paragraph-level unit; no Sentence children
preformatted → one coarse Paragraph-level unit; no Sentence children
table        → one coarse Paragraph-level unit; no Sentence children
```

Every native candidate uses its exact persisted block range.

The conservative BlockQuote/ListItem rule is about evidence sufficiency, not a claim that quotes or list items are inherently non-prose. Their persisted outer range may hide nested Paragraph/List/Preformatted/Table structure suppressed by the flat maximal projection. Sentence eligibility would therefore overstate what the canonical persisted facts prove.

Precise Paragraph/Sentence identity inside composite BlockQuote/ListItem content requires stronger persisted nested/leaf block evidence and a later explicit identity migration.

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

### 4. Native semantics remain evidence, not new locator kinds

`blockquote`, `list_item`, `preformatted`, and `table` do not create new MCP locator kinds or Tools.

They remain canonical block evidence projected into Paragraph-level content-class/detail and provenance. Paragraph/Sentence source addresses retain the existing TextLocator shapes.

Coarse BlockQuote/ListItem/Preformatted/Table units may remain Paragraph search candidates, but they must never receive fabricated Sentence candidates under block-model/v1 evidence.

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

Rationale: changing block kind/range/order under identical Section text can change Paragraph/Sentence boundaries, eligibility, ordinals, and cursor streams. That change must alter normalized identity.

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

- native `<p>` boundaries can replace blank-line guessing where persisted evidence is exact;
- `pre`/table punctuation can no longer fabricate Sentence identity when native evidence is available;
- composite quote/list structure remains truthfully addressable at coarse Paragraph level instead of receiving invented nested precision;
- block-map changes cannot silently reuse old normalized identity;
- persistent search rows migrate coherently with precise locator identity;
- blockless formats retain deterministic fallback behavior.

Costs:

- normalized document identity changes globally;
- all existing locator/read-cursor state bound to the old normalized hash becomes stale;
- TextUnit materialization must handle native ranges plus uncovered gaps and richer coverage;
- quote/list Sentence precision remains deferred until nested/leaf evidence exists;
- lexical persistent state requires v3 rebuild;
- a malformed declared block map must surface as an integrity error rather than being ignored.

## Acceptance invariants

1. Native Paragraph ranges become exact sentence-eligible Paragraphs.
2. Native BlockQuote/ListItem ranges become typed coarse Paragraph-level units with zero fabricated Sentences under flat block-model/v1 evidence.
3. Nested BlockQuote/ListItem fixtures do not recover or invent suppressed child Paragraph/Sentence boundaries.
4. Native Preformatted/Table ranges become exact coarse Paragraph-level units with no fake Sentences.
5. Native evidence outranks fallback text heuristics.
6. Uncovered non-whitespace source remains represented through offset-correct deterministic fallback.
7. Whitespace gaps remain separators.
8. Mixed native/fallback Paragraph ordering is deterministic.
9. No Sentence crosses a v2 Paragraph boundary.
10. Absent block evidence is supported; invalid declared evidence fails closed.
11. Hash v2 binds identity-bearing block facts but excludes diagnostics/provenance-only changes.
12. Old locator/cursor identity fails stale and is never fuzzy-rebased.
13. lexical-search-index/v2 cannot survive as current precise state; v3 rebuild is required.
14. lexical-tokenizer/v1 remains independent from segmentation.
15. Coarse structural/non-prose units are Paragraph search candidates only, never fake Sentence candidates.
16. Runtime Tool count remains seven.
17. Reopen/restart reproduces identical v2 TextUnits from persisted facts.

## Explicit non-goals

- nested/leaf block-tree identity;
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
→ text-segmentation/v2 native/fallback Paragraph-level projection
→ native/fallback Sentence eligibility + coverage
→ locator/cursor stale gates
→ derived TextUnit replacement
→ lexical-search-index/v3 rebuild
→ stdio direct-handoff acceptance
```

## Review outcome

Accepted after source-first correction. The implementation must treat persisted native block evidence as a new identity-bearing input, but it may claim only the precision actually preserved by `normalized-block-model/v1`. Native Paragraphs may be sentence-eligible; flat composite BlockQuote/ListItem and native Preformatted/Table regions remain coarse until stronger nested/leaf evidence exists. All dependent derived state migrates explicitly, old precise state fails stale, and no new MCP Tool is introduced.
