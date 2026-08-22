# ADR 0002: Text Index, Locator Identity, and Precise Reading

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/text-index-locator`
- Reviewed against main: `e4ec0ee5a39f6c549afcf17b68d6dfa7ebfe6198`
- Related design: `docs/text-index-and-locator-design.md`
- Amended by: `docs/adr/0004-use-case-first-tool-contracts.md` (Tool-count and ordered TextUnit-enumeration decisions only)
- Implementation status: normalized identity/range, Paragraph TextUnits, Sentence locator/coverage, `get_text_units` + `TextUnitCursor`, locator-driven context, exact TextLocator read, shared locator resolution, and SearchHit → Section TextLocator handoff are implemented; Paragraph/Sentence lexical candidate precision remains future.

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

Enumeration, read, context, and search handoff share one canonical locator model:

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

Current behavior:

- `get_text_units` emits Section/Paragraph/Sentence locators;
- `get_context` consumes Section/Paragraph/Sentence anchors;
- exact `read_document` consumes Section/Paragraph/Sentence plus CharacterRange;
- current `search_document` emits the strongest truthful locator supported by the existing SearchIndex: the canonical owning Section locator.

Search result enrichment does **not** interpret the current paragraph-like FTS/search-unit split as canonical Paragraph identity. Existing search rows do not carry the canonical normalized range + segmentation facts required to prove Paragraph/Sentence identity.

CharacterRange is represented by a locator with `normalized_range` but no Paragraph/Sentence ordinal or segmentation version. It remains version-bound through document/raw/normalized identity and owner Section.

### 5. Normalized ranges use one coordinate system

`normalized_range` is always relative to exact persisted `Section.content` identified by `owner_section_id`.

It is:

- zero-based;
- half-open `[start, end)`;
- measured in Unicode scalar values (Rust `char` count).

Paragraph, Sentence, and CharacterRange do not switch to parent-local coordinate systems. Segmentation tracks exact slices of persisted `Section.content`; it does not trim/rewrite text and then claim reconstructed offsets as canonical ranges.

Existing `Location.char_start` / `char_end` keep parser-defined legacy meaning. Parser-native offsets, normalized ranges, search-unit locations, and rendered-response offsets remain distinct coordinate spaces.

Exact read adds another explicitly stream-local coordinate system:

```text
exact-target-unicode-scalar/v1
```

Those positions are relative to the selected exact target and are continuation progress only. They are not source ranges. Exact read reports source coverage through a separate `returned_locator` CharacterRange.

### 6. Locator resolution is shared; capability acceptance remains explicit

All locator-consuming application use cases must share one identity/stale resolver for:

```text
document_id
content_hash
normalized_document_hash
owner Section
locator shape
Paragraph/Sentence ordinal
segmentation version
normalized range equality
```

The shared resolver returns a validated locator kind. Individual capabilities then decide which valid kinds they support.

For example:

- exact read supports CharacterRange;
- current context does not support CharacterRange as an anchor and returns an explicit request-level error;
- a valid CharacterRange must not be mislabeled `INVALID_LOCATOR` merely because one consumer does not implement that relation.

No consumer may implement a private fuzzy/stale-repair rule.

### 7. Cursor state is not a TextLocator

Continuation is progress state, not source identity.

```text
TextLocator    = canonical source addressing
ReadCursor     = opaque progress through one versioned read stream
TextUnitCursor = opaque progress through one versioned enumeration stream
```

Legacy Section read continuation uses `SectionTreeReadStream/v1`. A Section-tree `ReadCursor` binds document/raw/normalized identity, root Section, read mode/rendering version, next stream position, and cursor schema.

Exact TextLocator reads use:

```text
read_mode         = exact_target
rendering_version = exact-normalized-source/v1
```

They retain `read-cursor/v2` and add optional exact-target bindings. Legacy Section-tree v2 cursors omit those fields and preserve their previous serialized claim/checksum shape. Exact cursors cannot resume another target or read mode.

Every exact response segment carries a source `returned_locator` satisfying:

```text
content == owner_section.normalized_text_slice(returned_locator.normalized_range)
```

Implemented TextUnit enumeration uses `text-unit-cursor/v1` and binds raw/normalized identity, owner Section, segmentation, kind, direction, coverage policy, next index, stream length, and cursor schema.

Every incomplete declared read/enumeration stream exposes actionable continuation. Repeated continuation consumes the declared stream without gaps/overlap when the returned cursor is used as-is.

### 8. Search preserves structural title candidates and truthful precision

The accepted lexical candidate surface is:

```text
section | paragraph | sentence
```

A title-only hit resolves to a Section-level locator. It must not be represented by inventing Paragraph/Sentence identity.

Current SearchIndex implementations use Section-associated paragraph-like retrieval units for ranking/snippets. Their split/location facts are **not** the canonical Paragraph TextUnit contract. Therefore current `search_document` returns:

```text
candidate_kind = section
text_locator   = canonical owning Section locator
```

while retaining the old snippet/score/legacy Location preview fields.

The direct workflow is now implemented:

```text
SearchHit.text_locator ─┬→ read_document
                        └→ get_context
```

Snippet text is preview, never identity. SearchDocumentUseCase constructs the locator from canonical `DocumentRepository` facts after ranking; if the index refers to a missing Section or inconsistent source, it fails instead of fabricating a handoff.

Paragraph/Sentence candidate kinds remain future until the lexical index stores/proves canonical TextUnit identity.

### 9. Tokenization belongs only to the retrieval layer

Sentence is a reading/evidence unit. Token is a retrieval implementation detail.

Tokenizer policy is deterministic, non-LLM, independently versioned, and must support CJK/mixed technical text without whitespace-only assumptions.

```text
TextUnit identity:
normalized_document_hash + segmentation_version

Lexical index identity additionally includes:
tokenizer_version
```

Changing tokenizer configuration rebuilds lexical indexes but must not renumber Paragraph/Sentence TextUnits. A relational token table is not required by default.

### 10. Backward compatibility and Tool-surface amendment

Precise evolution remains additive:

- the six pre-existing MCP Tools remain valid;
- legacy Section-based `read_document` and `get_context` requests continue to work;
- current `search_document(document_id, query, limit)` request remains valid;
- old SearchHit fields remain present; `candidate_kind + text_locator` are additive;
- `content_hash` semantics remain unchanged;
- existing `Location` fields are not silently reinterpreted;
- new locator/cursor fields are additive;
- persisted migrations are explicit/tested.

ADR 0004 accepted one generic `get_text_units` Tool. Current implemented surface remains seven Tools.

No `get_sentences`, `get_paragraphs`, or format-specific TextUnit Tool is authorized.

## Implementation order / status

```text
P0 feat/read-continuation                         ✓
P0 feat/normalized-text-range                    ✓
P1 feat/text-unit-index                          ✓
P1 feat/sentence-locator                         ✓
P1 feat/text-unit-enumeration-contract           ✓
P1 feat/context-granularity                      ✓
P1 feat/precise-read-locator                     ✓
P1 feat/search-locator                           ✓
   - shared TextLocator resolver
   - SearchHit candidate_kind + TextLocator
   - current strongest truthful precision = Section
   - direct search → read/context handoff

P1 feat/lexical-text-unit-index                  next
   - preserve Section-title candidates
   - canonical Paragraph + Sentence candidates
   - independently versioned CJK-capable tokenizer policy
   - index migration/rebuild semantics
```

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
9. Search hits flow into read/context without re-searching quoted text.
10. Current search retrieval rows never receive fabricated Paragraph/Sentence identity.
11. Existing Section-title retrieval is preserved without fabricated Paragraph identity.
12. Tokenizer changes cannot change TextUnit identity.
13. CJK/mixed technical text does not depend on whitespace-only tokenization.
14. Word/Token never becomes a stable source locator level.
15. Existing six-Tool clients remain valid while current runtime exposes seven Tools.
16. Given a Section, `get_text_units` returns first/next Paragraph/Sentence-first items and explicit terminal completion.
17. Code/table/non-prose remains readable or explicitly accounted for without fabricated Sentence identity.
18. `eligible_only` never claims all-source completion.
19. Stale locator/cursor identity fails closed and is never fuzzily remapped.
20. `get_document_structure` never becomes a Paragraph/Sentence tree.
21. `read_document` never hides TextUnit enumeration behind an ambiguous granularity parameter.
22. Legacy `section_id` read remains Section-tree mode while a Section `target_locator` reads only that Section's canonical `Section.content`.
23. Sentence persistence is optional derived optimization, not a source/cursor correctness dependency.
24. Read/context share the same locator identity resolver; capability-specific unsupported kinds are not misclassified as malformed locators.
25. SearchIndex row IDs/snippets/legacy search-unit offsets never become canonical source identity.

## Consequences

Positive:

- enumeration, exact read, context, and search handoff share one locator identity model;
- SearchHit can hand evidence candidates directly into canonical consumers without snippet copying;
- reindexing/search-engine changes cannot silently redefine source identity;
- current search precision is truthful even though retrieval rows are smaller than Sections;
- parser upgrades that alter normalized text are detected even when raw source bytes are unchanged;
- exact-target continuation can page a large source target while reporting truthful source coverage per segment;
- CJK lexical improvements remain isolated from source identity;
- deterministic reconstruction avoids premature Sentence storage coupling.

Costs:

- normalized-document fingerprinting is a required primitive;
- cursor/rendering/segmentation/tokenizer versions require explicit maintenance;
- SearchDocumentUseCase now needs canonical DocumentRepository facts in addition to SearchIndex ranking;
- Paragraph/Sentence search precision requires a separate lexical TextUnit index migration rather than reusing legacy search-unit boundaries;
- future schema migrations must preserve legacy `Location` semantics.

## Review outcome

Accepted. ADR 0004 amends only Tool-surface/deferment decisions. Locator identity, normalized ranges, source/index separation, tokenizer separation, stale fail-closed behavior, and cursor/source separation remain normative.

Subsequent implementation evidence confirmed that a direct search handoff does not require pretending the existing FTS row is a canonical Paragraph. The safe bridge is ranking in derived SearchIndex, then canonical Section locator enrichment from DocumentRepository; finer candidate identity remains evidence-gated.