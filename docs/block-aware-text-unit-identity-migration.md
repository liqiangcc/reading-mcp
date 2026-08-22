# Block-Aware TextUnit Identity Migration

> Status: Implemented
>
> Design branch: `design/block-aware-text-unit-identity`
>
> Implementation branch: `feat/block-aware-text-unit-identity`
>
> Design baseline: `main@1c95e430819f5e6ae422f3276cbf64ec28b18787`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0003-epub-first-structure-reliability.md`, `docs/adr/0005-block-aware-text-unit-identity.md`, `docs/normalized-block-model-contract.md`, `docs/epub-structure-validator-contract.md`

## 1. Goal

Use persisted native HTML/XHTML block evidence to improve Paragraph/Sentence boundaries while preserving exact source identity and failing old locator/cursor state closed.

Implemented dependency chain:

```text
persisted Document / Section.content
+ optional valid normalized-block-model/v1
        ↓
normalized-document-hash/v2
+ text-segmentation/v2
        ↓
block-aware Paragraph / Sentence TextUnits
        ↓
TextLocator / TextUnitCursor
        ↓
get_text_units / search / read / context
        ↓
lexical-search-index/v3 derived persistence
```

No new MCP Tool was added.

## 2. Why the migration was required

The previous identity was:

```text
normalized-document-hash/v1
+ text-segmentation/v1
```

Paragraph v1 split only by blank lines and used text heuristics for obvious code/table regions. After Reading MCP began persisting native body-block facts, allowing those facts to change Paragraph/Sentence boundaries without changing identity would have silently reinterpreted existing locators/cursors/search rows.

The migration therefore advances both normalized identity and segmentation policy explicitly.

## 3. Source-of-truth boundary

```text
Document / Section.content
= canonical normalized text truth

NormalizedBlockMap
= persisted exact boundary/type evidence into Section.content

Paragraph / Sentence TextUnits
= deterministic rebuildable derived state

TextUnitIndex / SearchIndex
= rebuildable derived persistence
```

Blocks never copy source text. Every block and every TextUnit resolves to an exact owner-Section slice.

## 4. Segmentation v2 input contract

Current segmentation version:

```text
text-segmentation/v2
```

Inputs:

```text
canonical persisted Document / Section.content
+ optional valid normalized-block-model/v1
```

Rules:

- declared block evidence is validated before materialization;
- invalid/corrupt declared block evidence fails closed;
- absent block evidence uses deterministic fallback;
- no transient DOM/ZIP state enters TextUnit identity;
- no FTS row/snippet search/LLM/fuzzy repair enters segmentation.

The domain exposes fallible materialization for capability boundaries:

```text
try_paragraph_text_units()
try_sentence_text_units()
```

Application/search paths convert invalid persisted block evidence into explicit application errors instead of panics or silent fallback.

## 5. Native Paragraph-level projection

Implemented projection:

```text
normalized-block-model/v1 kind

paragraph    → exact sentence-eligible Paragraph
blockquote   → typed coarse Paragraph-level item; no Sentence
list_item    → typed coarse Paragraph-level item; no Sentence
preformatted → coarse Paragraph-level item; no Sentence
table        → coarse Paragraph-level item; no Sentence
```

### Why BlockQuote/ListItem are coarse

`normalized-block-model/v1` is a flat maximal non-overlapping projection. A persisted outer BlockQuote/ListItem may suppress nested `<p>`, nested list, `<pre>`, or `<table>` leaf boundaries.

A real regression fixture proves the limitation:

```html
<blockquote>
  <p>Quoted text.</p>
  <p>Second paragraph.</p>
</blockquote>
```

persists one BlockQuote range and can normalize to:

```text
Quoted text.Second paragraph.
```

The child Paragraph boundary is no longer a persisted fact. v2 therefore preserves the outer range coarsely rather than guessing nested Paragraph/Sentence identity.

This is evidence degradation, not a claim that quoted/list text is inherently non-prose.

## 6. Uncovered gaps and fallback

For each Section, native ranges and uncovered gaps are merged in exact Section source order.

```text
whitespace-only gap
→ separator coverage only

non-whitespace gap
→ deterministic blank-line Paragraph fallback scoped to that exact gap
```

Fallback range offsets are translated back into owner-Section Unicode-scalar coordinates.

Existing strong text heuristics remain only for fallback/no-native-evidence regions:

```text
fenced / fully indented code → coarse Paragraph
Markdown table               → coarse Paragraph
other text                   → prose_or_unknown
```

Native evidence outranks heuristic appearance. A native `<p>` remains an exact native Paragraph even if its punctuation resembles code/table text.

## 7. Ordering and coverage

After native and fallback candidates are merged:

```text
paragraph_index
= 1-based Section source-position order

source_order
= deterministic document TextUnit traversal order
```

`NormalizedBlock.block_index` remains native-block identity and is not reused as `paragraph_index`.

Paragraph coverage now accounts for:

```text
native_paragraph_chars
native_structural_container_chars
native_non_prose_chars
fallback_chars
separator_chars
paragraph_count
```

Sentence enumeration additionally distinguishes:

```text
sentence_eligible_paragraphs
coarse_structural_paragraphs
non_prose_paragraphs
coarse_structural_items
coarse_non_prose_items
intentionally_skipped
```

Sentence-first `preserve_source` returns coarse structural/non-prose regions as Paragraph items rather than dropping them. `eligible_only` may skip coarse items and therefore never claims all-source completion.

BlockQuote/ListItem coarse degradation is explicit:

```text
flat_native_container_no_nested_textunit_evidence
```

## 8. Sentence policy

The deterministic punctuation/technical-protection algorithm remains unchanged; the identity version changes because Paragraph boundaries/eligibility changed.

Eligibility:

```text
native paragraph       → eligible
native blockquote      → coarse only
native list_item       → coarse only
native preformatted    → coarse only
native table           → coarse only
fallback prose/unknown → eligible
fallback code/table    → coarse only
```

Sentence ranges remain exact owner-Section Unicode-scalar slices. No Sentence crosses a v2 Paragraph boundary.

## 9. Normalized identity v2

Current normalized identity:

```text
normalized-document-hash/v2
```

It includes all previous canonical Section identity facts plus block-map identity projection:

```text
block-map presence/absence
normalized-block-model schema version
owner_section_id
block_index
source_order
kind
normalized_range
```

It excludes provenance/diagnostic-only fields:

```text
native_anchor
native_location
validator report / errors / degradations
coverage counters
lexical state
```

Tests prove:

- removing block identity changes normalized hash and TextUnit IDs;
- changing identity-bearing block kind changes normalized hash and Sentence eligibility;
- changing only native location leaves normalized hash unchanged.

The hash API remains deterministic even if persisted block metadata is malformed, but TextUnit consumers reject malformed declared evidence instead of treating it as valid segmentation input.

## 10. TextUnit identity and stale behavior

TextUnit ID derivation remains:

```text
text-unit-id/v1
```

because its algorithm already binds normalized hash, owner, ordinal/kind, exact range, and segmentation version.

Old precise state is fail-closed:

```text
v1 Paragraph/Sentence locator → STALE_LOCATOR
v1 TextUnitCursor             → STALE_CURSOR
old normalized-hash state     → stale via normalized identity mismatch
```

A historical v1 Paragraph locator is rejected even if its exact range still equals the current v2 range.

No fuzzy relocation or snippet-based rebase exists.

## 11. Derived persistence migration

### Paragraph TextUnitIndex

Paragraph rows remain rebuildable derived state. `open_document` validates block-aware Paragraph materialization before replacing the current derived rows.

### Lexical SearchIndex

Persistent semantic version advances:

```text
lexical-search-index/v2
→ lexical-search-index/v3
```

Tokenizer remains:

```text
lexical-tokenizer/v1
```

SQLite v2 lexical metadata invalidates prior derived rows. Search can rebuild v3 state from a persisted canonical Document without source retrieval/reparse.

Coarse BlockQuote/ListItem/Preformatted/Table regions remain Paragraph lexical candidates and never emit Sentence candidates under current block evidence.

## 12. Parser/cache boundary

Parser/cache normalization remains:

```text
reading-mcp-normalization/v5
```

The migration changes normalized address/TextUnit identity rather than parser-output policy. Blockless persisted Documents continue through deterministic fallback and are not forcibly re-fetched/reparsed merely to obtain native blocks.

## 13. MCP Tool impact

Runtime Tool count remains seven:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

No caller-selected segmentation version was added. The server exposes one current canonical policy and rejects stale historical state.

Direct handoff remains:

```text
get_text_units → TextLocator → read_document / get_context
search_document → SearchHit.text_locator → read_document / get_context
```

## 14. Acceptance evidence

New migration tests prove:

1. native Paragraph/BlockQuote/ListItem/Preformatted/Table projection;
2. native Paragraph only is sentence-eligible under flat block-model/v1 evidence;
3. nested BlockQuote child boundaries are not fabricated;
4. source-preserving Sentence enumeration returns coarse structural/non-prose Paragraphs explicitly;
5. eligible-only skips coarse items without claiming source completion;
6. native/fallback ranges merge in exact Section source order;
7. whitespace gaps remain separator coverage;
8. invalid declared block metadata fails TextUnit enumeration and lexical rebuild closed;
9. identity-bearing block kind changes hash/TextUnit ID;
10. provenance-only native-location changes do not affect hash v2;
11. old v1 Paragraph locators fail `STALE_LOCATOR`;
12. old v1 TextUnit cursors fail `STALE_CURSOR`;
13. lexical v2 derived state is invalidated and rebuilt under v3;
14. `lexical-tokenizer/v1` remains unchanged;
15. existing EPUB navigation/reconciliation/validator, precise read, context, search and stdio suites remain green.

CI #876 passed before final docs-only synchronization:

```text
Format  success
Clippy  success
Test    success
```

## 15. Explicit non-goals

```text
nested/leaf block-tree identity
heading-title CharacterRange coordinates
SVG/fixed-layout precise blocks
Sentence SQLite persistence
new MCP Tools
caller-selectable historical segmentation
fuzzy locator rebasing
semantic/vector retrieval
lexical tokenizer/ranking changes
```

A future nested/leaf block model may refine BlockQuote/ListItem internals only through another explicit versioned identity migration.
