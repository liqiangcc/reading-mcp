# ADR 0002: Text Index, Locator Identity, and Precise Reading

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/text-index-locator`
- Reviewed against main: `e4ec0ee5a39f6c549afcf17b68d6dfa7ebfe6198`
- Related design: `docs/text-index-and-locator-design.md`
- Amended by: `docs/adr/0004-use-case-first-tool-contracts.md` (Tool-count and ordered TextUnit-enumeration decisions only)
- Implementation status: normalized identity/range, Paragraph TextUnits, Sentence locator/coverage and `get_text_units` + `TextUnitCursor` are implemented; context/search locator handoff remains future.

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

Fine-grained enumeration now emits one canonical locator model:

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

Current `get_text_units` returns this locator for Section target and Paragraph/Sentence items. `read_document` / `get_context` / `search_document` do not yet consume the fine-grained locator; that handoff remains a later additive increment.

### 5. Normalized ranges use one coordinate system

`normalized_range` is always relative to exact persisted `Section.content` identified by `owner_section_id`.

It is:

- zero-based;
- half-open `[start, end)`;
- measured in Unicode scalar values (Rust `char` count).

Paragraph, Sentence, and CharacterRange do not switch to parent-local coordinate systems. Segmentation tracks exact slices of persisted `Section.content`; it does not trim/rewrite text and then claim reconstructed offsets as canonical ranges.

Existing `Location.char_start` / `char_end` keep parser-defined legacy meaning. Parser-native offsets, normalized ranges, and rendered-response offsets remain distinct coordinate spaces.

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

`ReadCursor` binds at least:

- `document_id`;
- raw `content_hash`;
- `normalized_document_hash`;
- root `section_id`;
- `read_mode = section_tree`;
- `rendering_version`;
- next rendered-stream position;
- cursor schema version.

Rendered stream offsets are never canonical source ranges/citations.

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

Current v1 starts at a Section boundary and continues by cursor. Anchor policy is not yet part of the runtime request; future `before/after(locator)` start must extend cursor scope explicitly.

Every incomplete stream exposes actionable continuation. Repeated continuation consumes the declared stream without gaps/overlap when the returned cursor is used as-is. A cursor fails when required identity/stream contract no longer matches.

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

Existing FTS/BM25-first behavior remains baseline until measured evidence justifies a change. Future precise SearchHit carries a locator directly into read/context; snippet text is preview, never identity.

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
- current Section-based requests continue to work;
- `content_hash` semantics remain unchanged;
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

P1 feat/context-granularity                      next
   - tagged Paragraph/Sentence neighbor/container/structural context
   - TextLocator input/validation

P1 feat/search-locator                           later
   - search hits resolve directly to TextLocator

P1 feat/lexical-text-unit-index                  later
   - preserve Section-title candidates
   - Paragraph + Sentence FTS/BM25
   - independently versioned CJK-capable tokenizer policy
```

Read continuation remains isolated from TextUnit/FTS enumeration state machines.

## Acceptance invariants

An implementation conforms only if all of these hold:

1. Document/Section are source truth; TextUnit and lexical indexes are derived state.
2. Paragraph/Sentence boundaries rebuild identically for the same normalized-document hash and segmentation version.
3. TextUnit rebuilds use only persisted canonical Document state plus declared segmentation policy/version.
4. Paragraph/Sentence ranges are exact slices of owner `Section.content`.
5. Raw `content_hash` alone is never treated as normalized TextUnit identity.
6. ReadCursor/TextUnitCursor progress is never exposed as source identity/citation location.
7. Every incomplete stream has deterministic, gap-free, overlap-free continuation or explicit unsupported state.
8. Search hits must eventually flow into read/context without re-searching quoted text.
9. Existing Section-title retrieval is preserved without fabricated Paragraph identity.
10. Tokenizer changes cannot change TextUnit identity.
11. CJK/mixed technical text does not depend on whitespace-only tokenization.
12. Word/Token never becomes a stable source locator level.
13. Existing six-Tool clients remain valid while current runtime exposes seven Tools.
14. Given a Section, `get_text_units` returns first/next Paragraph/Sentence-first items and explicit terminal completion.
15. Code/table/non-prose remains readable or explicitly accounted for without fabricated Sentence identity.
16. `eligible_only` never claims all-source completion.
17. Stale locator/cursor identity fails closed and is never fuzzily remapped.
18. `get_document_structure` never becomes a Paragraph/Sentence tree.
19. `read_document` never hides TextUnit enumeration behind an ambiguous granularity parameter.
20. Sentence persistence is optional derived optimization, not a source/cursor correctness dependency.

## Consequences

Positive:

- precise Sentence-first enumeration and evidence location share one locator model;
- reindexing/search-engine changes cannot silently redefine source identity;
- parser upgrades that alter normalized text are detected even when raw source bytes are unchanged;
- legacy recursive Section reads have continuation without contaminating canonical source ranges;
- CJK lexical improvements remain isolated from source identity;
- `get_text_units` exists because Actor/use-case evidence proved an independent state machine;
- deterministic reconstruction avoids premature Sentence storage coupling.

Costs:

- normalized-document fingerprinting is a required primitive;
- cursor/rendering/segmentation/tokenizer versions require explicit maintenance;
- future schema migrations must preserve legacy `Location` semantics;
- Paragraph persistence and runtime Sentence materialization add derived-state/rebuild work;
- the runtime surface now has seven Tools and a distinct TextUnit enumeration cursor/state machine.

## Review outcome

Accepted. ADR 0004 amends only Tool-surface/deferment decisions. Locator identity, normalized ranges, source/index separation, tokenizer separation, stale fail-closed behavior, and read-continuation boundaries remain normative.

Subsequent implementation evidence confirmed the core identity decisions and added two operational clarifications: `eligible_only` cannot claim all-source completion, and Sentence persistence is unnecessary for correct restart-safe enumeration continuation.
