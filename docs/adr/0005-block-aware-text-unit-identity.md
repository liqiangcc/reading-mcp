# ADR 0005: Block-Aware TextUnit Identity Migration

- Status: Accepted / Implemented
- Date: 2026-08-22
- Design branch: `design/block-aware-text-unit-identity`
- Implementation branch: `feat/block-aware-text-unit-identity`
- Design reviewed against main: `1c95e430819f5e6ae422f3276cbf64ec28b18787`
- Implementation status: complete; final gate includes cache-policy correction to `reading-mcp-normalization/v6`
- Related: ADR 0002, ADR 0003, ADR 0004, `docs/block-aware-text-unit-identity-migration.md`, `docs/normalized-block-model-contract.md`, `docs/epub-structure-validator-contract.md`

## Context

Reading MCP persists exact HTML/XHTML native block evidence through `normalized-block-model/v1`. The previous precise identity used blank-line Paragraph boundaries plus persisted-text heuristics:

```text
normalized-document-hash/v1
+ text-segmentation/v1
→ Paragraph / Sentence / TextLocator
```

That identity could not begin consuming native block kind/range/order without an explicit migration: otherwise the same apparent identity could rebuild a different Paragraph/Sentence stream.

Independent source-first review also exposed an evidence limit in `normalized-block-model/v1`: it is a flat maximal non-overlapping projection. A blockquote containing multiple nested `<p>` elements may persist as one BlockQuote range, and normalized text can lose the inner Paragraph separator. Therefore the implementation preserves such regions coarsely rather than fabricating nested precision.

A later implementation review found one additional cache boundary that the original design underestimated: `epub-structure-validator/v1` is persisted inside parser output and its TextUnit coverage is computed from the current Paragraph/Sentence materialization. Advancing segmentation to v2 therefore changes persisted parser output for EPUB even when ZIP/DOM parsing itself is unchanged. Parsed Cache entries produced under normalization v5 must not survive this migration.

## Decision and implemented behavior

### 1. TextUnit segmentation is v2

Current runtime:

```text
text-segmentation/v2
```

Segmentation consumes only persisted canonical facts:

```text
Document / Section.content
+ optional valid normalized-block-model/v1
```

No transient DOM/ZIP state, FTS row, snippet similarity, LLM segmentation, or fuzzy repair participates.

A declared block map is validated before use. An absent block map is supported deterministic fallback; a declared invalid/corrupt block map fails closed.

### 2. Native block evidence defines Paragraph-level boundaries

Implemented projection under flat block-model/v1 evidence:

```text
paragraph    → exact sentence-eligible Paragraph
blockquote   → typed coarse Paragraph-level unit; no Sentence children
list_item    → typed coarse Paragraph-level unit; no Sentence children
preformatted → coarse Paragraph-level unit; no Sentence children
table        → coarse Paragraph-level unit; no Sentence children
```

The BlockQuote/ListItem rule is about evidence sufficiency, not about their semantic prose quality. Their persisted outer range may hide suppressed nested Paragraph/List/Preformatted/Table boundaries.

Coarse structural and non-prose regions remain Paragraph-addressable/searchable but do not emit fabricated Sentence locators.

### 3. Uncovered source is preserved through deterministic fallback

For gaps between native ranges:

```text
whitespace-only gap
→ separator coverage

non-whitespace gap
→ v1-style deterministic Paragraph fallback scoped to that exact gap
```

Fallback ranges are translated back into owner-Section Unicode-scalar coordinates before TextUnits are created.

Existing fenced/indented-code and Markdown-table heuristics apply only to fallback/no-native-evidence regions. Native evidence outranks text heuristics.

Paragraph ordinals are assigned by exact Section source position after native and fallback candidates are merged. Native `block_index` is not reused as `paragraph_index`.

### 4. Normalized document identity is v2

Current runtime:

```text
normalized-document-hash/v2
```

Hash v2 keeps the v1 Section identity inputs and additionally binds identity-bearing native block facts:

```text
block-map presence/absence
block schema version
owner_section_id
block_index
source_order
kind
normalized_range
```

It excludes provenance/diagnostic facts that do not define normalized source addressing:

```text
native_anchor
native_location
validator report / diagnostics
coverage counters
lexical state
```

Tests prove block-map presence and block kind affect normalized identity while native-location-only changes do not.

### 5. TextUnit ID derivation remains v1

```text
text-unit-id/v1
```

The derivation algorithm already includes normalized document identity, exact range, ordinal/kind, and segmentation version. Hash v2 + segmentation v2 therefore generate new IDs without changing the ID-derivation namespace itself.

### 6. Old precise state fails closed

After the migration:

```text
v1 Paragraph/Sentence TextLocator → STALE_LOCATOR
v1 TextUnitCursor                 → STALE_CURSOR
old normalized-hash-bound state   → stale via normalized identity mismatch
```

A v1 locator is rejected even when its historical exact range happens to equal the current v2 range. No fuzzy or snippet-based rebasing is performed.

The existing locator/cursor wire shapes remain sufficient because they already bind normalized identity and/or segmentation version.

### 7. Derived TextUnit/search state is rebuilt coherently

Paragraph TextUnit rows remain rebuildable derived state. `open_document` validates block-aware Paragraph materialization before replacing the derived TextUnitIndex.

Persistent lexical state advances:

```text
lexical-search-index/v2
→ lexical-search-index/v3
```

Tokenizer remains:

```text
lexical-tokenizer/v1
```

Opening SQLite state with v2 lexical metadata invalidates the old derived rows. The v3 index rebuilds from the persisted canonical Document without source retrieval/reparse. Section-title candidates are rebuilt together with Paragraph/Sentence candidates so one index version never mixes old and new locator identity.

Invalid persisted block evidence causes lexical rebuild to fail closed rather than panic or silently create fallback locators.

### 8. Parser/cache normalization advances to v6

Final runtime policy:

```text
reading-mcp-normalization/v5
→ reading-mcp-normalization/v6
```

The original design expected v5 to remain valid because the native block parser output itself did not change. Implementation review showed that boundary was too narrow.

`CachingParser` keys Parsed Cache entries by:

```text
final_source
+ raw_sha256
+ normalization_version
```

and returns a cache hit without reparsing or revalidating the EPUB structure report. Meanwhile `epub-structure-validator/v1` persists Paragraph/Sentence coverage and degradations inside `Document.metadata`. `text-segmentation/v2` changes those persisted facts. Reusing a v5 cached EPUB could therefore expose a v1-era validator report beside current v2 TextUnits.

Normalization v6 makes all v5 Parsed Cache entries miss and forces current parser/validator output to be regenerated. Raw Resource Cache remains reusable; this is a parsed-output invalidation, not an unnecessary source refetch requirement.

A regression test explicitly proves a v5 Parsed Cache key is a miss under current v6 policy.

### 9. MCP Tool surface remains seven

No new Tool, block-specific locator kind, or caller-selected historical segmentation mode was added.

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

Direct handoff remains:

```text
get_text_units → TextLocator → read_document / get_context
search_document → SearchHit.text_locator → read_document / get_context
```

`get_text_units` now exposes separate coarse structural/non-prose coverage. Sentence-first source-preserving enumeration returns BlockQuote/ListItem as coarse Paragraphs with explicit degradation:

```text
flat_native_container_no_nested_textunit_evidence
```

Native/fallback non-prose coarse regions continue to use explicit Paragraph-only degradation rather than fake Sentence identity.

## Acceptance evidence

The implementation gate proves:

1. native Paragraph ranges become exact sentence-eligible Paragraphs;
2. native BlockQuote/ListItem remain typed coarse Paragraph-level units with zero fabricated Sentences;
3. the real nested BlockQuote fixture does not recover suppressed child boundaries;
4. native Preformatted/Table remain coarse with zero fake Sentences;
5. native evidence outranks fallback heuristics;
6. native + uncovered fallback ranges merge in exact Section source order;
7. whitespace gaps remain separator coverage;
8. absent block evidence remains deterministic fallback;
9. malformed declared block evidence fails TextUnit enumeration and lexical rebuild closed;
10. block kind/presence changes hash v2 and TextUnit IDs, while native location does not;
11. v1 Paragraph locators fail `STALE_LOCATOR`;
12. v1 TextUnit cursors fail `STALE_CURSOR`;
13. lexical v2 state is invalidated and rebuilt as v3 while tokenizer stays v1;
14. normalization v5 Parsed Cache entries miss under v6 so persisted EPUB validator evidence cannot remain on v1 TextUnit semantics;
15. restart/reopen and existing EPUB validator tests remain green;
16. existing read/context/search direct handoff remains green;
17. runtime Tool count remains seven.

CI #898 passed the implementation and v6 cache-policy correction:

```text
Format  success
Clippy  success
Test    success
```

## Consequences

Positive:

- native `<p>` boundaries replace blank-line guessing where persisted evidence is exact;
- native pre/table punctuation cannot fabricate Sentences;
- composite quote/list structure is preserved truthfully at coarse precision;
- block-map changes cannot silently reuse old normalized identity;
- locator/cursor migration is explicitly fail-closed;
- lexical derived state stays coherent with current TextUnit identity;
- stale v5 parsed EPUB reports cannot survive the segmentation-v2 migration;
- blockless formats retain deterministic behavior.

Costs:

- normalized document identity changed globally;
- parser/cache normalization advances to v6 and old parsed documents are regenerated;
- old normalized-hash-bound locator/cursor state becomes stale;
- quote/list Sentence precision remains deferred until nested/leaf block evidence exists;
- TextUnit materialization must account for native ranges plus uncovered gaps;
- persistent lexical state required a v3 rebuild.

## Explicit non-goals

- nested/leaf block-tree identity;
- heading-title CharacterRange coordinates;
- SVG/fixed-layout precise blocks;
- Sentence SQLite persistence;
- new MCP Tools;
- caller-selectable historical segmentation;
- fuzzy source rebasing;
- tokenizer/ranking changes.

## Review outcome

Implemented as accepted after two source-first corrections. Reading MCP now claims only the precision persisted by `normalized-block-model/v1`: native Paragraph is sentence-eligible; flat BlockQuote/ListItem and Preformatted/Table regions remain coarse; uncovered content uses deterministic fallback; all identity-bearing block facts participate in normalized hash v2; old precise state fails stale; lexical state rebuilds under v3; Parsed Cache advances to normalization v6 because persisted EPUB validator output depends on current TextUnit coverage; and no new Tool was introduced.
