# ADR 0002: Text Index, Locator Identity, and Precise Reading

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/text-index-locator`
- Reviewed against main: `e4ec0ee5a39f6c549afcf17b68d6dfa7ebfe6198`
- Related design: `docs/text-index-and-locator-design.md`
- Amended by: `docs/adr/0004-use-case-first-tool-contracts.md` (Tool-count and ordered TextUnit-enumeration decisions only)
- Implementation status: normalized identity/range, Paragraph TextUnits, Sentence locator/coverage, `get_text_units` + `TextUnitCursor`, locator-driven context, and exact TextLocator read are implemented; SearchHit locator handoff remains future.

## Context

Reading MCP treats `Document` / `Section` as normalized source facts and derived indexes as rebuildable state. Precise reading requires finer addressability than Section while preserving source/index/cursor separation.

The design review found four implementation-relevant ambiguities:

1. `read_document(document_id, section_id)` renders the selected Section plus descendants, so one Section-relative normalized range cannot describe continuation through the legacy response stream.
2. Paragraph boundaries cannot depend on transient parser-native state unless those boundaries are persisted in canonical Document facts; otherwise TextUnits cannot be deterministically rebuilt.
3. FTS preserves Section-title retrieval, including title-only Sections with empty body content; Paragraph/Sentence FTS must not remove this behavior or fake a Paragraph identity.
4. `content_hash` is SHA-256 of retrieved source bytes. The same bytes can produce different normalized `Section.content` after parser/normalization changes, so raw `content_hash + segmentation_version` is insufficient for stable Paragraph/Sentence identity.

This ADR records the accepted identity/range/index architecture. ADR 0004 later supplied use-case evidence for one generic ordered TextUnit enumeration Tool without changing the identity decisions below.

## Decision

### 1. Addressing levels

Reading MCP uses five logical source-addressing levels:

```text
L0 Document
L1 StructuralNode (current Section)
L2 Paragraph
L3 Sentence
L4 CharacterRange
```

`Document` and `Section` remain canonical normalized source facts. Paragraph and Sentence are deterministic, rebuildable TextUnits. Word and model Token are not stable source-addressing levels.

### 2. TextUnit persistence is derived state

Paragraph/Sentence TextUnits may be persisted for efficient reading, context expansion, validation, enumeration, and retrieval, but remain rebuildable derived state.

A TextUnit must be reproducible from:

```text
persisted canonical Document
+ segmentation_version
```

A rebuild must not depend on parser state discarded after `open_document`.

Current implementation persists Paragraph TextUnits in `TextUnitIndex`. Sentence locator/enumeration is deterministically materialized from the canonical Document and Paragraph/Sentence segmentation and is **not** persisted in SQLite. Repository-restart continuation proves Sentence persistence is not a correctness dependency.

Parser-native paragraph/block boundaries may influence future segmentation only when addressing-relevant boundary metadata has been materialized into persisted canonical Document facts. Until then, segmentation derives only from exact persisted `Section.content` plus deterministic rules.

### 3. Raw-source identity and normalized-document identity are separate

The existing `content_hash` keeps its meaning: hash of retrieved source bytes. It is source provenance and must not be silently redefined.

Precise addressing additionally uses deterministic `normalized_document_hash` computed from addressing-relevant persisted normalized facts. Current contract covers:

- Section identity, parentage, deterministic order;
- Section title and level;
- exact persisted `Section.content`.

Any future persisted block/boundary metadata that affects segmentation requires a new normalized-hash contract version. Presentation-only MCP rendering is excluded.

Parser/normalization behavior also carries `normalization_version` for diagnostics/cache migration. Locator identity is tied to actual normalized facts, so a policy version bump that yields identical facts need not change the normalized fingerprint.

Paragraph/Sentence identity is scoped by:

```text
document_id
+ normalized_document_hash
+ segmentation_version
```

Raw `content_hash` remains available in locators/results as source provenance.

### 4. Unified TextLocator

Precise enumeration/read/context share one canonical locator model:

```text
TextLocator
├── document_id
├── content_hash              # raw-source provenance
├── normalized_document_hash
├── owner_section_id
├── section_path
├── paragraph_index?
├── sentence_index?
├── normalized_range?
├── segmentation_version?
└── native_location / provenance?
```

Paragraph and Sentence ordinals are human-facing 1-based values.

Current `get_text_units` emits Section/Paragraph/Sentence locators. `get_context` consumes Section/Paragraph/Sentence anchors. Exact `read_document` consumes Section/Paragraph/Sentence plus CharacterRange locators. SearchHit still uses the legacy owning-Section/Location handoff and is the next locator migration.

CharacterRange is represented by a locator with `normalized_range` but no Paragraph/Sentence ordinal or segmentation version. It remains version-bound through document/raw/normalized identity and owner Section.

### 5. Normalized ranges use one coordinate system

`normalized_range` is always relative to exact persisted `Section.content` identified by `owner_section_id`.

It is:

- zero-based;
- half-open `[start, end)`;
- measured in Unicode scalar values (Rust `char` count).

Paragraph, Sentence, and CharacterRange do not switch to parent-local coordinate systems. Segmentation tracks exact slices of persisted `Section.content`; it does not trim/rewrite text and then claim reconstructed offsets as canonical ranges.

Existing `Location.char_start` / `char_end` keep parser-defined legacy meaning. Parser-native offsets, normalized ranges, and rendered-response offsets remain distinct coordinate spaces.

Exact read adds another explicitly stream-local coordinate system:

```text
exact-target-unicode-scalar/v1
```

Those positions are relative to the selected exact target and are continuation progress only. They are not source ranges. Exact read reports source coverage through a separate `returned_locator` CharacterRange.

### 6. Cursor state is not a TextLocator

Continuation is progress state, not source identity.

```text
TextLocator    = canonical source addressing
ReadCursor     = opaque progress through one versioned read stream
TextUnitCursor = opaque progress through one versioned enumeration stream
```

Legacy Section read continuation uses:

```text
SectionTreeReadStream/v1
```

A Section-tree `ReadCursor` binds at least:

- `document_id`;
- raw `content_hash`;
- `normalized_document_hash`;
- root `section_id`;
- `read_mode = section_tree`;
- `rendering_version = section-tree-markdown/v1`;
- next rendered-stream position;
- cursor schema version.

Rendered Section-tree offsets are never canonical source ranges/citations, and a legacy recursive response has `returned_locator=null` because it may contain multiple source regions and inserted heading rendering.

Exact TextLocator reads use:

```text
read_mode         = exact_target
rendering_version = exact-normalized-source/v1
```

They retain `read-cursor/v2` and add mode-specific optional bindings for target kind, Paragraph/Sentence ordinal, exact target range and segmentation version. Legacy Section-tree v2 cursors omit those fields and preserve their previous serialized claim shape. Exact cursors cannot resume a different target or mode.

Every exact response segment carries a source `returned_locator` satisfying:

```text
content == owner_section.normalized_text_slice(returned_locator.normalized_range)
```

Implemented TextUnit enumeration uses:

```text
text-unit-cursor/v1
```

and binds:

- document raw/normalized identity;
- owner Section target;
- `text-segmentation/v1`;
- requested TextUnit kind;
- direction;
- coverage policy;
- next enumeration index;
- total declared items;
- cursor schema version.

Current TextUnit v1 starts at a Section boundary and continues by cursor. Anchor policy is not yet part of the runtime request; future `before/after(locator)` start must extend cursor scope explicitly.

Every incomplete declared read/enumeration stream exposes actionable continuation. Repeated continuation consumes the declared stream without gaps/overlap when the returned cursor is used as-is. A cursor fails when required identity or stream contract no longer matches.

### 7. Search preserves structural title candidates

Lexical retrieval design supports three candidate kinds:

```text
section | paragraph | sentence
```

- Section candidates preserve current title search, including empty-body Sections.
- Sentence candidates improve precise terminology/evidence localization.
- Paragraph candidates preserve local explanatory context/recall.

A title-only hit resolves to a Section-level locator. It must not be represented by inventing fake Paragraph/Sentence identity.

Client-facing granularity may evolve additively as:

```text
auto | section | paragraph | sentence
```

Existing FTS/BM25-first behavior remains baseline until measured evidence justifies a change. The next precise SearchHit increment must carry a locator directly into the already-implemented read/context consumers; snippet text is preview, never identity.

### 8. Tokenization belongs only to the retrieval layer

Sentence is a reading/evidence unit. Token is a retrieval implementation detail.

Tokenizer policy is deterministic, non-LLM, independently versioned, and must support CJK/mixed technical text without whitespace-only assumptions.

```text
TextUnit identity:
normalized_document_hash + segmentation_version

Lexical index identity additionally includes:
tokenizer_version
```

Changing tokenizer configuration rebuilds lexical indexes but must not renumber Paragraph/Sentence TextUnits. A relational token table is not required by default.

### 9. Backward compatibility and Tool-surface amendment

Precise evolution remains additive:

- the six pre-existing MCP Tools remain valid;
- legacy Section-based `read_document` and `get_context` requests continue to work;
- current `content_hash` semantics remain unchanged;
- existing `Location` fields are not silently reinterpreted;
- new locator/cursor fields are additive;
- persisted migrations are explicit/tested.

ADR 0004 accepted one generic `get_text_units` Tool because Paragraph/Sentence ordered enumeration is an independent, use-case-proven responsibility. That Tool is now implemented.

Current distinction:

```text
pre-enumeration compatible surface = six Tools
current implemented surface        = seven Tools including get_text_units
```

No `get_sentences`, `get_paragraphs`, or format-specific TextUnit Tool is authorized.

## Implementation order / status

```text
P0 feat/read-continuation                         ✓
   - SectionTreeReadStream/v1
   - actionable ReadCursor
   - raw + normalized document binding

P0 feat/normalized-text-range                    ✓
   - Section-relative normalized range
   - deterministic normalized_document_hash
   - normalization-version diagnostics

P1 feat/text-unit-index                          ✓
   - deterministic Paragraph TextUnits
   - persisted/rebuildable Paragraph TextUnitIndex

P1 feat/sentence-locator                         ✓
   - deterministic Sentence segmentation
   - non-prose eligibility/coverage

P1 feat/text-unit-enumeration-contract           ✓
   - generic get_text_units
   - source-order pagination
   - TextLocator output
   - TextUnitCursor + complete/no-gap/no-overlap
   - preserve_source / eligible_only semantics

P1 feat/context-granularity                      ✓
   - tagged Paragraph/Sentence neighbor/container/structural context
   - TextLocator input/validation

P1 feat/precise-read-locator                     ✓
   - exact Section/Paragraph/Sentence/CharacterRange read
   - exact-target ReadCursor continuation
   - resolved target versus returned source locator

P1 feat/search-locator                           next
   - first consolidate shared locator resolution before a third consumer
   - search hits resolve directly to strongest truthful TextLocator

P1 feat/lexical-text-unit-index                  later
   - preserve Section-title candidates
   - Paragraph + Sentence FTS/BM25
   - independently versioned CJK-capable tokenizer policy
```

Read continuation remains isolated from TextUnit enumeration state. Locator consumers must share the same identity/stale rules; until shared resolution is extracted, cross-consumer parity tests guard the overlapping read/context semantics.

## Acceptance invariants

An implementation conforms only if all of these hold:

1. Document/Section are source truth; TextUnit and lexical indexes are derived state.
2. Paragraph/Sentence boundaries rebuild identically for the same normalized-document hash and segmentation version.
3. TextUnit rebuilds use only persisted canonical Document state plus declared segmentation policy/version.
4. Paragraph/Sentence ranges are exact slices of owner `Section.content`.
5. Raw `content_hash` alone is never treated as normalized TextUnit identity.
6. ReadCursor/TextUnitCursor progress is never exposed as source identity/citation location.
7. Every incomplete declared stream has deterministic, gap-free, overlap-free continuation or explicit unsupported state.
8. Exact-read `returned_locator` source ranges reproduce response content exactly and concatenate without gap/overlap across continuation.
9. Search hits must eventually flow into read/context without re-searching quoted text.
10. Existing Section-title retrieval is preserved without fabricated Paragraph identity.
11. Tokenizer changes cannot change TextUnit identity.
12. CJK/mixed technical text does not depend on whitespace-only tokenization.
13. Word/Token never becomes a stable source locator level.
14. Existing six-Tool clients remain valid while current runtime exposes seven Tools.
15. Given a Section, `get_text_units` returns first/next Paragraph/Sentence-first items and explicit terminal completion.
16. Code/table/non-prose remains readable or explicitly accounted for without fabricated Sentence identity.
17. `eligible_only` never claims all-source completion.
18. Stale locator/cursor identity fails closed and is never fuzzily remapped.
19. `get_document_structure` never becomes a Paragraph/Sentence tree.
20. `read_document` never hides TextUnit enumeration behind an ambiguous granularity parameter.
21. Legacy `section_id` read remains Section-tree mode while a Section `target_locator` reads only that Section's canonical `Section.content`.
22. Sentence persistence is optional derived optimization, not a source/cursor correctness dependency.

## Consequences

Positive:

- precise Sentence-first enumeration, exact read, context recovery and evidence location share one locator model;
- reindexing/search-engine changes cannot silently redefine source identity;
- parser upgrades that alter normalized text are detected even when raw source bytes are unchanged;
- legacy recursive Section reads retain continuation without contaminating canonical source ranges;
- exact-target continuation can page a large source target while reporting truthful source coverage per segment;
- CJK lexical improvements remain isolated from source identity;
- deterministic reconstruction avoids premature Sentence storage coupling.

Costs:

- normalized-document fingerprinting is a required primitive;
- cursor/rendering/segmentation/tokenizer versions require explicit maintenance;
- exact read adds a second explicit ReadCursor mode and source-range reporting contract;
- current read/context locator resolution overlaps and must be consolidated before a third consumer is added;
- future schema migrations must preserve legacy `Location` semantics;
- Paragraph persistence and runtime Sentence materialization add derived-state/rebuild work;
- the runtime surface has seven Tools and distinct read/enumeration/context state semantics.

## Review outcome

Accepted. ADR 0004 amends only Tool-surface/deferment decisions. Locator identity, normalized ranges, source/index separation, tokenizer separation, stale fail-closed behavior, and cursor/source separation remain normative.

Subsequent implementation evidence confirmed the core identity decisions and added operational clarifications: `eligible_only` cannot claim all-source completion; Sentence persistence is unnecessary for restart-safe TextUnit continuation; precise context and precise read can consume the same canonical locator; and exact read must distinguish stream-local progress from the exact source range returned in each response.
