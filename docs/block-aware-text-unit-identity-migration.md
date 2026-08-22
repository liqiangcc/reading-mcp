# Block-Aware TextUnit Identity Migration

> Status: Accepted design; implementation pending
>
> Branch: `design/block-aware-text-unit-identity`
>
> Reviewed against: `main@1c95e430819f5e6ae422f3276cbf64ec28b18787`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0003-epub-first-structure-reliability.md`, `docs/adr/0005-block-aware-text-unit-identity.md`, `docs/normalized-block-model-contract.md`, `docs/epub-structure-validator-contract.md`

## 1. Goal

Use persisted native HTML/XHTML block evidence to improve Paragraph/Sentence boundaries without silently reinterpreting any existing precise locator, cursor, or persisted search row.

The migration follows the reading use case rather than a parser convenience:

```text
Agent needs stable Paragraph/Sentence reading
        ↓
native prose/non-prose boundaries must survive parser restart
        ↓
block-aware deterministic TextUnit materialization
        ↓
versioned source identity + explicit stale behavior
        ↓
existing get_text_units / search / read / context workflows continue
```

No new MCP Tool is required.

## 2. Why v1 is no longer sufficient

Current precise identity is:

```text
normalized-document-hash/v1
+ text-segmentation/v1
→ Paragraph / Sentence / TextLocator identity
```

`text-segmentation/v1` derives Paragraphs only from blank-line structure in `Section.content`, then classifies obvious code/table content heuristically from text.

The parser now persists stronger evidence:

```text
normalized-block-model/v1
├── paragraph
├── blockquote
├── list_item
├── preformatted
└── table
```

with exact Section-relative ranges. The EPUB validator also proves where native blocks agree with or differ from current v1 TextUnits.

Once TextUnit boundaries consume this block map, block facts become addressing-relevant normalized inputs. Keeping the old identity while changing boundaries would allow the same apparent identity to rebuild a different TextUnit stream, which is forbidden.

## 3. Source-of-truth boundary

The migration keeps the existing ownership model:

```text
Document / Section.content
= canonical normalized text truth

NormalizedBlockMap
= persisted canonical boundary/type evidence into Section.content

Paragraph / Sentence TextUnits
= deterministic rebuildable derived state

SearchIndex / TextUnitIndex
= rebuildable derived persistence
```

A block never owns copied text. Every TextUnit still resolves to an exact `Section.content` slice.

## 4. Segmentation v2 input contract

Accepted segmentation version:

```text
text-segmentation/v2
```

The deterministic inputs are:

```text
canonical persisted Document / Section.content
+ optional valid normalized-block-model/v1
+ text-segmentation/v2 policy
```

Rules:

- a declared block map must validate before it can drive TextUnits;
- an invalid/corrupt declared block map is an integrity failure, not permission to silently fall back;
- an absent block map is a supported degradation and uses deterministic fallback segmentation;
- no transient DOM, ZIP state, FTS row, snippet search, LLM judgment, or fuzzy repair may participate.

## 5. Paragraph projection

For each Section, v2 partitions the exact Section-relative coordinate space into native block ranges and gaps.

### 5.1 Native prose-like blocks

```text
paragraph
blockquote
list_item
```

Each becomes exactly one Paragraph TextUnit at the exact native block range.

This is intentional even for `blockquote`: `normalized-block-model/v1` is a flat maximal non-overlapping projection. If a blockquote contains nested `<p>` elements, v1 does not persist a nested block tree, so segmentation v2 must not invent nested Paragraph identity that the persisted evidence cannot prove.

`blockquote` and `list_item` remain sentence-eligible. Their native kind remains available as block evidence/content-class detail rather than being erased into an unqualified `prose_or_unknown` claim.

### 5.2 Native non-prose blocks

```text
preformatted
table
```

Each becomes exactly one coarse Paragraph TextUnit at its exact block range and is ineligible for Sentence children.

Punctuation inside code/preformatted/table content never creates fake Sentence identity merely because it resembles prose.

### 5.3 Gaps between native blocks

A gap is content in `Section.content` not covered by any persisted native block.

```text
whitespace-only gap
→ separator coverage only

non-whitespace gap
→ text-segmentation/v1-style paragraph fallback scoped to that exact gap
```

Fallback ranges are offset back into the owner Section coordinate space before TextUnits are created.

The existing strong text heuristics for fenced/indented code and Markdown tables remain available only for fallback/no-native-evidence regions. Native block evidence outranks heuristic classification.

Examples:

```text
native <pre> containing "Done. Next."
→ coarse Paragraph only

native <p> containing table-like punctuation
→ prose-like Paragraph; native evidence wins

no block map + fenced code
→ fallback coarse Paragraph through current text heuristic
```

### 5.4 Paragraph ordinal and order

Paragraphs are ordered by exact owner-Section source position after native and fallback candidates are merged.

```text
paragraph_index
= 1-based order within owner Section

source_order
= deterministic document TextUnit traversal order
```

`NormalizedBlock.block_index` is native-block identity and is not reused as `paragraph_index`; uncovered fallback ranges may exist between native blocks.

## 6. Sentence policy

Sentence boundary punctuation/technical protections remain the current deterministic algorithm unless changed by a separately versioned future policy.

Eligibility under v2 is evidence-driven:

```text
native paragraph   → eligible
native blockquote  → eligible
native list_item   → eligible
native preformatted→ coarse Paragraph only
native table       → coarse Paragraph only
fallback prose/unknown → eligible
fallback code/table    → coarse Paragraph only
```

Sentence ranges remain exact Section-relative Unicode-scalar slices and remain owned by one Paragraph.

No Sentence crosses a Paragraph boundary.

## 7. Coverage and degradation

The v2 materializer must make the boundary source measurable. At minimum Paragraph/Sentence coverage must distinguish:

```text
native_prose_like_chars
native_non_prose_chars
fallback_chars
separator_chars
paragraph_count
sentence_eligible_paragraphs
coarse_non_prose_paragraphs
```

For source-preserving enumeration, every non-whitespace represented region is either a Paragraph/Sentence item or an explicit coarse Paragraph. Unsupported/invalid declared native evidence is not hidden as ordinary fallback.

`eligible_only` continues to be allowed to skip coarse non-prose and therefore cannot claim all-source completion.

## 8. Normalized identity v2

Accepted normalized identity version:

```text
normalized-document-hash/v2
```

Reason: segmentation v2 depends on persisted block facts. A block-map change under identical Section text can change Paragraph/Sentence boundaries, eligibility, ordinals, and cursor streams. Therefore the normalized identity must bind the identity-bearing block projection.

`normalized-document-hash/v2` contains:

```text
all existing v1 Section identity inputs
+ explicit block-map presence/absence
+ normalized-block-model schema version when present
+ ordered block identity projection:
    owner_section_id
    block_index
    source_order
    kind
    normalized_range
```

It excludes non-addressing provenance/diagnostics such as:

```text
native_anchor
native_location
validator report
validator errors/degradations
coverage counters
lexical/search state
```

Those facts may change without redefining the exact normalized source address model.

A malformed declared block map is an integrity error for v2 TextUnit materialization. Hash calculation must remain deterministic, but consumers must not treat malformed block evidence as valid segmentation input.

## 9. Identity and stale-state migration

The migration is intentionally fail-closed.

### 9.1 TextUnit ID

The existing derivation namespace may remain:

```text
text-unit-id/v1
```

because its algorithm/shape is unchanged and already includes normalized document identity, exact range, ordinal/kind, and segmentation version. The new normalized hash + segmentation version force new IDs without pretending the derivation algorithm itself changed.

### 9.2 TextLocator

Wire shape stays unchanged.

Existing v1 Paragraph/Sentence locators contain:

```text
segmentation_version = text-segmentation/v1
normalized_document_hash = old v1 hash
```

After migration they must return:

```text
STALE_LOCATOR
```

They are never reinterpreted against v2 even if their old range happens to equal a new range.

Because normalized-document identity itself advances to v2, previously issued Section/CharacterRange locators also fail closed when their old normalized hash no longer matches. This conservative invalidation is intentional: the document-level addressing identity contract changed.

### 9.3 TextUnitCursor

`text-unit-cursor/v1` claim shape may remain unchanged because it already binds:

```text
normalized_document_hash
segmentation_version
stream contract
position / total_items
```

A cursor carrying v1 segmentation/hash becomes `STALE_CURSOR`; it is not resumed against a v2 stream.

### 9.4 ReadCursor

Existing read cursors already bind normalized identity. A cursor created before the hash migration therefore becomes stale naturally; no cursor schema bump is required unless claim shape changes during implementation.

## 10. Derived persistence migration

### 10.1 Paragraph TextUnitIndex

Paragraph rows remain rebuildable derived state. The existing row shape already stores normalized hash and segmentation version.

No SQLite schema-shape change is required solely for v2 boundaries, but current-version reads must never accept mixed/stale v1 rows as current v2 TextUnits. `open_document` replacement/rebuild must replace prior Paragraph rows atomically.

### 10.2 Lexical SearchIndex

The lexical index stores canonical Paragraph/Sentence locators, so its persistent semantic version must advance:

```text
lexical-search-index/v2
→ lexical-search-index/v3
```

`lexical-tokenizer/v1` stays unchanged because tokenization policy is not changing.

An incompatible v2 lexical index is discarded/rebuilt from the current persisted canonical Document; source retrieval/reparse is not required merely to rebuild lexical rows.

Section-title candidates are rebuilt too so the index is one coherent version rather than a mixture of old/new locator identity.

## 11. Parser/cache boundary

This migration changes derived TextUnit/address identity, not the parser output contract itself.

Current parser/cache policy can remain:

```text
reading-mcp-normalization/v5
```

unless implementation also changes canonical parser output.

A persisted Document with no block map remains readable through explicit fallback semantics. The TextUnit migration must not secretly re-fetch/reparse a source just to obtain native blocks.

## 12. MCP Tool contract impact

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

No request needs a caller-selected segmentation version. The server exposes only its current canonical TextUnit policy; old locators/cursors are rejected as stale rather than letting callers choose historical interpretation modes.

Existing direct handoff remains:

```text
get_text_units → TextLocator → read_document / get_context
search_document → SearchHit.text_locator → read_document / get_context
```

## 13. Implementation order

The implementation should be one bounded dependency chain:

```text
1. normalized-document-hash/v2 block identity projection
2. text-segmentation/v2 block-aware Paragraph materialization
3. native-evidence Sentence eligibility + coverage
4. shared locator/cursor stale tests
5. TextUnitIndex v2-row replacement semantics
6. lexical-search-index/v3 rebuild
7. MCP stdio end-to-end handoff/regression tests
8. documentation/status synchronization
```

Do not start by changing SearchIndex or MCP DTOs before the canonical TextUnit builder is correct.

## 14. Acceptance invariants

Implementation is accepted only if tests prove all of the following:

1. native `paragraph` / `blockquote` / `list_item` ranges become exact Paragraph TextUnits;
2. native `preformatted` / `table` become coarse Paragraph-only units with zero fake Sentences;
3. native evidence outranks text heuristics;
4. uncovered non-whitespace gaps use deterministic offset-correct fallback segmentation;
5. whitespace gaps remain separator coverage rather than fake Paragraphs;
6. mixed native/fallback Paragraph ordinals are deterministic by Section source position;
7. no Sentence crosses a v2 Paragraph boundary;
8. absent block map remains a supported deterministic fallback;
9. malformed declared block map fails closed rather than silently falling back;
10. hash v2 changes when identity-bearing block kind/range/order facts change;
11. hash v2 does not change solely because native location/validator diagnostics change;
12. old v1 Paragraph/Sentence locators fail `STALE_LOCATOR`;
13. old v1 TextUnit cursors fail `STALE_CURSOR`;
14. old read cursors fail stale through normalized identity mismatch;
15. lexical-search-index/v2 is rebuilt as v3 while tokenizer remains v1;
16. restart/reopen reproduces the same v2 TextUnits and locators from persisted facts;
17. exact read/context/search direct handoff continues without snippet relocation;
18. runtime Tool count remains seven;
19. no transient parser state or LLM decision enters identity.

## 15. Explicit non-goals

This migration does not add:

```text
nested block-tree identity
heading-title CharacterRange coordinates
SVG/fixed-layout precise blocks
Sentence SQLite persistence
new MCP Tools
caller-selectable segmentation versions
fuzzy locator rebasing
semantic/vector retrieval
lexical tokenizer changes
```

A future nested block model may refine `blockquote` internals only through another explicit versioned identity migration.

## 16. Next implementation branch

After this design is merged, the next implementation branch is:

```text
feat/block-aware-text-unit-identity
```

It must implement the accepted v2 identity/segmentation boundary before any later ranking or richer block-tree work.
