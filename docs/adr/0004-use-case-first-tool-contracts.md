# ADR 0004: Use-Case-First MCP Tool Contracts

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/tool-contract-use-cases`
- Reviewed against main: `e55fc203aa4c6e184b71125a2022140c31a4b762`
- Implementation status: runtime surface is seven Tools; `get_text_units`, locator-driven context, and exact TextLocator read are implemented; SearchHit locator handoff remains future.
- Related design: `docs/tool-contract-use-case-design.md`
- Related implementation contracts: `docs/text-unit-enumeration-contract.md`, `docs/context-granularity-contract.md`, `docs/precise-read-locator-contract.md`
- Related identity architecture: `docs/adr/0002-text-index-locator-identity.md`
- Related reliability architecture: `docs/adr/0003-epub-first-structure-reliability.md`

## Context

At design-review time, Reading MCP exposed six Tools:

```text
list_documents
open_document
get_document_structure
search_document
get_context
read_document
```

Those Tools supported the existing coarse workflow, but source-first review of implementation/tests found contract gaps:

1. a truncated Section read had no actionable continuation;
2. a bounded structure response had no expansion continuation;
3. SearchHit could identify an owning Section but not hand an exact Paragraph/Sentence locator directly to read/context;
4. context only meant neighboring Sections;
5. no contract could discover the first Paragraph/Sentence, enumerate subsequent TextUnits in source order, or prove that a Section-level TextUnit stream was complete;
6. open/structure responses did not expose all capability, reliability, provenance, and coverage facts needed to qualify precise-reading claims;
7. old documentation still contained five-Tool assumptions after `list_documents` was implemented.

The design review started from Actor goals and detailed use cases, derived independent capabilities, modeled their state machines, and only then evaluated Tool candidates. It compared three alternatives for ordered Paragraph/Sentence enumeration:

```text
A. overload read_document with granularity/view
B. add one generic get_text_units Tool
C. overload get_context
```

## Decision

### 1. Use-case-first is the Tool admission rule

A Tool or contract mode is accepted only when an Actor goal and use case establish:

- task-level success conditions;
- alternative, failure, and degradation behavior;
- required information/state;
- legal next operations;
- acceptance evidence.

“Keep the Tool count small” and “adding a Tool is convenient” are both insufficient reasons.

### 2. Six-Tool reviewed baseline; seven-Tool implemented runtime

The reviewed baseline exposed six Tools. ADR 0004 accepted exactly one additional independent responsibility. Existing valid calls and response fields remain supported.

Current runtime fact:

```text
implemented surface = seven Tools
```

The migration was additive: the six previous Tool calls remain valid and the seventh Tool is advertised only because its supporting capability and invariants exist.

### 3. One generic ordered TextUnit enumeration responsibility

The accepted and implemented Tool is:

```text
get_text_units
```

Its responsibility is:

> Enumerate bounded Paragraph or Sentence-first reading items within one structural target in canonical source order, with per-item TextLocator, actionable pagination, explicit completion, and truthful non-prose/coverage semantics.

Use-case evidence includes:

- list Paragraphs in a Section;
- find the first Sentence in a Section;
- advance Sentence by Sentence in source order;
- backward traversal;
- continue until declared stream completion;
- account for code/table/non-prose without fake Sentence identity;
- verify no gap/overlap under response budgets.

Current v1 starts at a Section boundary and continues by TextUnitCursor. Anchor-based `before/after(locator)` starts remain a compatible future extension.

No separate `get_sentences`, `get_paragraphs`, or format-specific Tool is accepted.

### 4. Do not overload `read_document` with enumeration

`read_document` remains responsible for canonical reading of an already-known source target or continuation of one deterministic read stream.

`get_text_units` discovers/enumerates unknown child reading items. Combining both would conflate:

```text
ReadCursor     = progress through one read/render stream
TextUnitCursor = progress through one ordered enumeration stream
```

and create a parameter cross-product across target, read mode, granularity, anchor, direction, pagination, rendering, and non-prose policy.

Exact TextLocator read does **not** violate this boundary: the target is already known, and `read_document` returns only that canonical target/stream rather than discovering child TextUnits.

### 5. Current Tool surface

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

Responsibilities remain:

- discovery does not open/parse;
- structure exposes StructuralNodes, not every TextUnit;
- enumeration exposes ordered Paragraph/Sentence-first items;
- search answers “where?”;
- context expands around a known anchor;
- read returns canonical content for a known target/stream.

### 6. Context has three explicit semantic variants

The following are implemented independent variants under the existing `get_context` Tool:

```text
neighbor context
container context
structural context
```

The runtime uses an explicit tagged relation contract. Legacy `section_id + before + after` maps only to Section-neighbor context. Structured context consumes Section/Paragraph/Sentence TextLocator anchors and preserves exact locator-bearing items.

A bag of interacting optional parameters that changes meaning implicitly remains rejected.

### 7. SearchHit must carry direct source handoff

Future search candidates are:

```text
section | paragraph | sentence
```

Each hit carries the strongest truthful `TextLocator`. A title-only structural hit remains Section-level and must not receive fake Paragraph/Sentence identity.

The accepted interaction is:

```text
search → SearchHit.text_locator ─┬→ read
                                └→ context
```

The read/context consumers are now implemented. Copying a snippet and searching again is not a conforming precise-reading workflow. SearchHit locator production remains the next increment.

### 8. Locator and cursor identities remain separate

ADR 0002 remains normative:

```text
TextLocator = canonical source address
Cursor      = opaque progress through one versioned stream
```

Fine-grained identity remains bound to normalized document facts and segmentation policy. Raw `content_hash` remains source provenance and is not silently redefined.

A stream cursor is never a citation and never becomes a normalized source range.

Current TextUnitCursor binds raw/normalized document identity, target Section, segmentation version, requested kind, direction, coverage policy, next index, stream length and cursor schema.

`read_document` now has two explicit modes:

```text
legacy section_id
→ section_tree / section-tree-markdown/v1

TextLocator
→ exact_target / exact-normalized-source/v1
```

Both use version-bound ReadCursor continuation when needed. Exact-target stream offsets are target-local progress only. Each exact response separately returns a CharacterRange `returned_locator` identifying the actual canonical source slice represented by that response.

### 9. Stale state fails closed

For a locator/cursor whose required identity no longer matches:

- resolve the exact retained historical version only when explicitly available and selected; otherwise
- return an explicit stale failure.

Forbidden:

- map to the nearest-looking Sentence;
- reuse the same Paragraph/Sentence ordinal in changed content;
- rebase a cursor onto a new stream;
- use snippet text as identity.

Index rebuild alone does not invalidate source identity when canonical normalized/segmentation identity is unchanged.

Current locator consumers expose `INVALID_LOCATOR` and `STALE_LOCATOR`; exact read and context have cross-consumer parity tests for overlapping Sentence locator identity/stale behavior.

### 10. Non-prose remains readable without fabricated Sentences

A Sentence-first request may encounter code, tables, formulas, or other non-prose.

Under `preserve_source`, `get_text_units` emits an explicitly coarser exact Paragraph reading item for recognized code/table content with:

```text
requested kind
effective kind
content class
locator/range
degradation reason
```

It does not create fake Sentence ordinals.

`eligible_only` is deliberately a narrower stream and must not claim all-source completion, even if a particular Section happens to contain only currently eligible prose.

Sentence neighbor context shares the source-preserving order/coarse semantics through regression tests. A coarse Paragraph locator can then be read exactly through `read_document` without fabricating Sentence identity.

### 11. Reliability and coverage are returned at decision points

No dedicated reliability/inspection Tool is accepted in this ADR.

Capability, provenance, degradation, and coverage information belongs additively in workflows where the Agent must choose what to do:

```text
open_document
get_document_structure
get_text_units
read/context results when relevant
```

A dedicated inspection Tool requires a later independent use-case decision.

### 12. Contract evolution is additive first

Current valid calls include the six legacy Tools plus `get_text_units` and additive precise forms:

```text
list_documents(path?, recursive, max_results)
open_document(source, auth_profile?, force_refresh)
get_document_structure(document_id, max_depth?)
get_text_units(document_id, section_id, requested_kind, direction, coverage_policy, max_items, max_chars?, cursor?)
search_document(document_id, query, limit)

legacy context:
get_context(document_id, section_id, before, after, max_chars?)

structured context:
get_context(document_id, section_id|target_locator, relation, max_chars?)

legacy read:
read_document(document_id, section_id, max_chars?, cursor?)

exact read:
read_document(document_id, target_locator, max_chars?, cursor?)
```

Rules:

1. existing fields retain meaning;
2. legacy `Location.char_start/end` are not silently reinterpreted;
3. legacy Section reads retain recursive subtree rendering;
4. a Section `target_locator` means exact canonical `Section.content`, not the legacy recursive stream;
5. legacy Section context retains current neighbor semantics;
6. precise locator/cursor fields are additive;
7. persisted migrations are explicit and tested;
8. TextUnit enumeration remains independent of read/context/search state machines.

## Tool Contract Status

### `list_documents`

Current discovery semantics are implemented. Explicit bounded discovery cursor/completion remains future.

### `open_document`

Raw/normalized identity and normalization diagnostics are implemented. Richer open outcome/capability grading remains future.

### `get_document_structure`

Structural Section hierarchy is implemented and continues to exclude Paragraph/Sentence enumeration. Actionable subtree pagination remains future.

### `get_text_units`

Implemented v1:

```text
Section-boundary start
paragraph | sentence
forward | backward
preserve_source | eligible_only
max_items / max_chars
TextLocator output
TextUnitCursor continuation
complete / section_complete
coverage
```

Every response page is source ordered. TextUnit is atomic under response budgeting. Sentence persistence is not a correctness dependency; stream materialization is deterministic from canonical Document + segmentation version.

Future compatible extension: anchor-based `before/after(locator)` starts.

### `search_document`

Current Section handoff remains. Next: `auto | section | paragraph | sentence` granularity and locator-bearing candidate kinds. Search remains bounded retrieval, not body reading.

### `get_context`

Implemented additive structured relations:

```text
neighbor(section | paragraph | sentence)
container(paragraph | section)
structural(owner_section | ancestors | siblings | children)
```

Section/Paragraph/Sentence TextLocator anchors are validated fail-closed. Legacy Section-neighbor calls retain their historical meaning.

### `read_document`

Implemented modes:

```text
section_id
→ SectionTreeReadStream/v1 + ReadCursor

target_locator
→ exact Section | Paragraph | Sentence | CharacterRange
→ exact_target + ReadCursor when oversized
```

Responses expose `resolved_target_locator`. Exact-target segments also expose a truthful CharacterRange `returned_locator`; legacy rendered Section-tree output does not pretend to be one contiguous source range.

## Relationship to Earlier ADRs

ADR 0002 remains accepted and normative for:

- addressing levels;
- canonical Document/Section versus derived TextUnit/Search state;
- normalized-document identity;
- exact normalized ranges;
- TextLocator versus cursors;
- search candidate kinds;
- tokenizer/segmentation separation;
- backward-compatible precise contract evolution.

ADR 0003 remains accepted and normative for:

- EPUB spine/source order;
- nav/NCX/heading/spine provenance precedence;
- target resolution states;
- capability-graded EPUB support;
- non-prose behavior;
- validator and coverage evidence.

This ADR supersedes only earlier statements that described the primary surface as five Tools or left the need for a generic `get_text_units` Tool undecided. Implementation-status notes supersede historical “future” wording while preserving the design rationale.

## Implementation Order / Status

```text
P0 read continuation                                  ✓
P0 normalized source identity/range                  ✓
P1 canonical/rebuildable Paragraph TextUnits         ✓
P1 deterministic Sentence locator/coverage           ✓
P1 get_text_units + TextUnitCursor enumeration       ✓
P1 tagged locator-driven context                     ✓
P1 exact TextLocator read + continuation             ✓
P1 SearchHit → TextLocator handoff                    next
P1 Paragraph/Sentence lexical index                  later
P1 EPUB reliability/coverage increments              later/evidence-driven
```

Before SearchHit becomes a third locator consumer, consolidate the overlapping read/context locator resolution into one shared resolver; cross-consumer parity tests guard behavior until that extraction lands.

Short-lived implementation branches must continue to preserve locator/cursor identity and avoid collapsing independent state machines.

## Acceptance Invariants

A conforming runtime requires:

1. use-case success is defined at reading-task level, not Tool-call level;
2. the six pre-existing Tool calls remain valid and `get_text_units` is the only new Tool admitted by this decision;
3. structure does not expand into a Sentence-sized TOC;
4. read does not become an implicit TextUnit enumerator;
5. every incomplete declared read/TextUnit stream has actionable continuation or explicit unsupported state;
6. repeated Section/exact-target/TextUnit continuation is gap-free and overlap-free when returned cursors are used as-is;
7. every precise TextUnit is an exact normalized source slice;
8. exact-read `returned_locator` reproduces exactly the response source slice;
9. SearchHit will flow directly into read/context through a locator when the next handoff increment lands;
10. neighbor/container/structural context semantics are explicit;
11. TextLocator and every cursor type remain non-interchangeable;
12. stale cursor/locator identity fails closed;
13. non-prose remains readable without fake Sentences under source-preserving enumeration;
14. `eligible_only` never claims all-source completion;
15. reliability/degradation/coverage is visible and reproducible;
16. Document/Section remain source truth and indexes remain rebuildable derived state;
17. Sentence persistence is never required to reconstruct canonical content or identity;
18. legacy `section_id` read and exact Section-locator read keep their deliberately different recursive-vs-own-content semantics.

## Consequences

Positive:

- an Agent can perform complete Section-scoped Sentence-first reading with explicit end-of-stream state;
- a TextLocator from enumeration can now flow directly into exact read or context;
- Paragraph/Sentence traversal has one generic source-ordered enumeration contract;
- read, enumeration, search, and context retain clear responsibilities;
- exact read can page oversized canonical targets without turning cursor offsets into source identity;
- non-prose degradation and completion claims are explicit;
- TextUnitCursor can continue from persisted canonical Document without a Sentence source store;
- the surface grew only where multiple real use cases required it.

Costs:

- the runtime surface contains seven rather than six Tools;
- TextUnit and read continuation state machines must be maintained independently;
- capability/reliability/coverage metadata is part of contract design;
- read/context currently contain overlapping locator-resolution logic that must be consolidated before a third consumer;
- SearchHit locator production and lexical TextUnit indexing remain separate migrations;
- end-to-end acceptance must validate task completion and source coverage, not only schema success.

## Review Outcome

The design was accepted after review corrected three initial risks:

1. a pure Sentence stream would have hidden non-prose;
2. overloading `read_document` would have conflated read and enumeration cursors;
3. a generic context parameter bag would have hidden neighbor/container/structural semantics.

Implementation review later added further constraints:

1. `eligible_only` never claims all-source completion;
2. Sentence persistence is unnecessary for correctness because cursor continuation is version-bound and deterministically rebuildable from canonical persisted Document state;
3. locator-driven precise context must not duplicate the same body in both top-level legacy content and structured items;
4. exact read must distinguish target-local cursor progress from the source CharacterRange returned in each response;
5. read/context locator identity and stale rules must remain in parity, and should be consolidated before SearchHit adds a third locator consumer.

The resulting decision remains consistent with ADR 0002 and ADR 0003.
