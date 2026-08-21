# ADR 0002: Text Index, Locator Identity, and Precise Reading

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/text-index-locator`
- Reviewed against main: `e4ec0ee5a39f6c549afcf17b68d6dfa7ebfe6198`
- Related design: `docs/text-index-and-locator-design.md`
- Amended by: `docs/adr/0004-use-case-first-tool-contracts.md` (Tool-count and ordered TextUnit-enumeration decisions only)

## Context

Reading MCP currently treats `Document` / `Section` as normalized source facts and the SQLite FTS search index as rebuildable derived state. Precise reading requires finer addressability than Section while preserving the source/search separation.

The design review compared the proposal with the current domain, MCP contracts, read/context behavior, parser offsets, SQLite persistence, and FTS behavior. Four implementation-relevant ambiguities were found:

1. `read_document(document_id, section_id)` renders the selected Section plus all descendants, so one Section-relative normalized range cannot describe continuation through the legacy response stream.
2. Paragraph boundaries cannot depend on transient parser-native state unless those boundaries are persisted in the canonical Document; otherwise TextUnits cannot be deterministically rebuilt.
3. Current FTS preserves Section-title retrieval, including title-only Sections with empty body content; Paragraph/Sentence FTS must not remove this behavior or fake a Paragraph identity.
4. Current `content_hash` is SHA-256 of retrieved source bytes. The same bytes can produce different normalized `Section.content` after parser/normalization changes, so raw `content_hash + segmentation_version` is insufficient for stable Paragraph/Sentence identity.

This ADR records the accepted identity/range/index architecture and is normative where it clarifies or overrides the earlier draft design. ADR 0004 later supplied use-case evidence for one generic ordered TextUnit enumeration Tool without changing the identity decisions below.

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

Paragraph/Sentence TextUnits may be persisted in SQLite for efficient reading, context expansion, validation, enumeration, and retrieval, but they remain rebuildable derived state.

A TextUnit must be reproducible from:

```text
persisted canonical Document
+ segmentation_version
```

A rebuild must not depend on parser state that was discarded after `open_document`.

Parser-native paragraph/block boundaries may influence segmentation only when the addressing-relevant boundary metadata has been materialized into the persisted canonical Document. Until such metadata exists, Paragraph v1 must derive boundaries only from exact persisted `Section.content` plus deterministic segmentation rules.

### 3. Raw-source identity and normalized-document identity are separate

The existing `content_hash` keeps its current meaning: hash of retrieved source bytes. It is source provenance and must not be silently redefined.

Precise addressing additionally uses a deterministic `normalized_document_hash` computed from a canonical serialization of addressing-relevant persisted normalized facts. At minimum the fingerprint covers:

- Section identity, parentage, and deterministic order;
- Section title and level;
- exact persisted `Section.content`;
- any future persisted block/boundary metadata that affects segmentation.

Presentation-only MCP rendering is excluded.

Parser/normalization code also carries an auditable `normalization_version` (or equivalent policy version) for diagnostics and migration. Locator identity is tied to the actual normalized fingerprint, so a parser version bump that yields identical normalized facts does not have to invalidate locators.

Paragraph/Sentence identity is scoped by:

```text
document_id
+ normalized_document_hash
+ segmentation_version
```

Raw `content_hash` remains available in locators/results for source provenance.

### 4. Unified TextLocator

Fine-grained enumeration, read, context, search, and citation paths resolve through one logical locator contract:

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

### 5. Normalized ranges use one coordinate system

`normalized_range` is always relative to the exact persisted `Section.content` identified by `owner_section_id`.

It is:

- zero-based;
- half-open `[start, end)`;
- measured in Unicode scalar values (Rust `char` count).

Paragraph, Sentence, and CharacterRange do not switch to parent-local coordinate systems. Segmentation must track exact slices of persisted `Section.content`; it must not trim or rewrite text and then claim reconstructed offsets as canonical ranges.

Existing `Location.char_start` / `char_end` keep their parser-defined legacy meaning until an explicit migration. Parser-native offsets, normalized ranges, and rendered-response offsets are distinct coordinate spaces.

### 6. Cursor state is not a TextLocator

Continuation is transport/progress state, not source identity.

```text
TextLocator    = canonical source addressing
ReadCursor     = opaque progress through one versioned read stream
TextUnitCursor = opaque progress through one versioned enumeration stream
```

Current legacy Section reads recursively render a subtree. Their P0 continuation contract therefore uses a deterministic logical stream:

```text
SectionTreeReadStream/v1
```

A legacy `ReadCursor` binds at least:

- `document_id`;
- raw `content_hash`;
- `normalized_document_hash`;
- root `section_id`;
- `read_mode = section_tree`;
- `rendering_version`;
- next stream position;
- cursor schema version.

If the implementation uses a character offset internally, it is a read-stream coordinate scoped to `rendering_version`, never a canonical source range or citation locator.

A future `TextUnitCursor` separately binds at least:

- document raw/normalized identity;
- owner Section target;
- segmentation version;
- requested TextUnit kind;
- direction/anchor policy;
- non-prose/coverage policy;
- next enumeration position;
- cursor schema version.

Every incomplete stream must expose actionable continuation or explicit unsupported state. Repeated continuation must consume the complete declared stream without gaps or overlap. A cursor must fail rather than silently continue when any required identity or stream contract no longer matches.

### 7. Search preserves structural title candidates

Lexical retrieval supports three candidate kinds:

```text
section | paragraph | sentence
```

- Section/StructuralNode candidates preserve current title search, including empty-body Sections.
- Sentence candidates improve precise terminology/evidence localization.
- Paragraph candidates preserve local explanatory context and recall.

A title-only hit resolves to a Section-level locator. It must not be represented by inventing a fake Paragraph/Sentence TextUnit.

Client-facing granularity may evolve additively as:

```text
auto | section | paragraph | sentence
```

Existing FTS/BM25-first behavior remains the baseline unless measured evidence justifies a later change. Every precise SearchHit must carry a locator that flows directly into read/context; snippet text is preview, never identity.

### 8. Tokenization belongs only to the retrieval layer

Sentence is a reading/evidence unit. Token is a retrieval implementation detail.

Tokenizer policy is deterministic, non-LLM, independently versioned, and must support CJK/mixed technical text without relying on whitespace-only tokenization.

```text
TextUnit identity:
normalized_document_hash + segmentation_version

Lexical index identity additionally includes:
tokenizer_version
```

Changing tokenizer configuration rebuilds lexical indexes but must not renumber Paragraph/Sentence TextUnits. A separate relational token table is not required by default.

### 9. Backward compatibility and Tool-surface amendment

The first implementation remains additive:

- the current runtime's six MCP Tools remain valid: `list_documents`, `open_document`, `get_document_structure`, `search_document`, `get_context`, and `read_document`;
- current Section-based requests continue to work;
- current `content_hash` semantics remain unchanged;
- existing `Location` fields are not silently reinterpreted;
- new locator/cursor fields are additive;
- persisted migrations must be explicit and tested.

ADR 0004 accepts one future generic `get_text_units` Tool because Paragraph/Sentence ordered enumeration is an independent, use-case-proven responsibility that cannot be naturally expressed by read or context without conflating state machines. It does not authorize `get_sentences`, `get_paragraphs`, or format-specific Tools.

The distinction is normative:

```text
current implemented surface = six Tools
accepted future surface      = seven Tools including get_text_units
```

## Implementation order

After these design ADRs are merged, implementation proceeds on short-lived feature branches:

```text
P0 feat/read-continuation
   - SectionTreeReadStream/v1
   - actionable ReadCursor for truncated legacy reads
   - raw + normalized document binding

P0 feat/normalized-text-range
   - Section-relative normalized range semantics
   - deterministic normalized_document_hash
   - normalization-version diagnostics

P1 feat/text-unit-index
   - deterministic Paragraph TextUnits
   - persisted/rebuildable TextUnitIndex

P1 feat/sentence-locator
   - deterministic versioned Sentence segmentation
   - non-prose eligibility/coverage semantics

P1 feat/text-unit-enumeration-contract
   - generic get_text_units application/Tool contract
   - source-order guarantee
   - TextUnitCursor + complete/no-gap/no-overlap behavior

P1 feat/context-granularity
   - tagged Paragraph/Sentence neighbor, container, and structural context

P1 feat/search-locator
   - search hits resolve directly to TextLocator

P1 feat/lexical-text-unit-index
   - preserve Section-title candidates
   - Paragraph + Sentence FTS/BM25
   - independently versioned CJK-capable tokenizer policy
```

`feat/read-continuation` remains limited to P0 read-stream continuation. Sentence/TextUnit/FTS/enumeration work must not be pulled into that branch.

## Acceptance invariants

An implementation conforms only if all of these hold:

1. Document/Section are source truth; TextUnit and lexical indexes are derived state.
2. Paragraph/Sentence boundaries rebuild identically for the same normalized-document hash and segmentation version.
3. TextUnit rebuilds use only persisted canonical Document state plus declared segmentation policy/version.
4. Paragraph/Sentence ranges are exact slices of owner `Section.content`.
5. Raw `content_hash` alone is never treated as normalized TextUnit identity.
6. ReadCursor/TextUnitCursor progress is never exposed as source identity or citation location.
7. Every incomplete stream has deterministic, gap-free, overlap-free continuation or explicit unsupported state.
8. Search hits can flow into read/context without re-searching quoted text.
9. Existing Section-title retrieval is preserved without fabricated Paragraph identity.
10. Tokenizer changes cannot change TextUnit identity.
11. CJK/mixed technical text does not depend on whitespace-only tokenization.
12. Word/Token never becomes a stable source locator level.
13. Existing six-Tool clients remain valid.
14. Given a Section, the future enumeration contract can return the first/next Paragraph/Sentence-first reading item and explicit terminal completion.
15. Code/table/non-prose remains readable or explicitly accounted for without fabricated Sentence identity.
16. Stale locator/cursor identity fails closed and is never fuzzily remapped.
17. `get_document_structure` never becomes a Paragraph/Sentence tree.
18. `read_document` never hides TextUnit enumeration behind an ambiguous granularity parameter.

## Consequences

Positive:

- precise reading, sentence-level questioning, ordered traversal, context recovery, and evidence citation share one locator model;
- reindexing/search-engine changes cannot silently redefine source identity;
- parser upgrades that alter normalized text are detected even when raw source bytes are unchanged;
- legacy recursive Section reads can gain continuation without contaminating canonical source ranges;
- CJK lexical improvements remain isolated from source identity;
- the new enumeration responsibility has explicit Actor/use-case evidence rather than being added for convenience.

Costs:

- normalized-document fingerprinting becomes a required primitive;
- cursor/rendering versions and segmentation/tokenizer versions must be maintained explicitly;
- future schema migrations need to preserve legacy `Location` semantics;
- Paragraph/Sentence persistence adds storage and rebuild logic, intentionally accepted for deterministic AI reading workflows;
- the future surface grows from six to seven Tools and requires a distinct TextUnit enumeration state machine.

## Review outcome

Accepted. ADR 0004's use-case review amends only the earlier Tool-surface/deferment statements. Locator identity, normalized range, source/index separation, tokenizer separation, stale fail-closed behavior, and P0 read-continuation boundaries remain normative. The first implementation task is still `feat/read-continuation`; Sentence/TextUnit/FTS/get_text_units work must not be pulled into that P0 branch.
