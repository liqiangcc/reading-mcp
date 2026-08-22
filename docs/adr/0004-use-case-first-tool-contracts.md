# ADR 0004: Use-Case-First MCP Tool Contracts

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/tool-contract-use-cases`
- Reviewed against main: `e55fc203aa4c6e184b71125a2022140c31a4b762`
- Implementation status: the accepted seventh Tool `get_text_units` is now implemented by `feat/text-unit-enumeration-contract`; runtime surface is seven Tools.
- Related design: `docs/tool-contract-use-case-design.md`
- Related implementation contract: `docs/text-unit-enumeration-contract.md`
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

The reviewed baseline exposed six Tools. Existing valid calls and response fields remain supported.

ADR 0004 accepted exactly one additional independent responsibility. After the underlying normalized identity, Paragraph/Sentence and cursor invariants were implemented, that decision was realized.

Current runtime fact:

```text
implemented surface = seven Tools
```

The migration was additive: the six previous Tool calls remain valid and the seventh Tool is now advertised only because its supporting capability and invariants exist.

### 3. One generic ordered TextUnit enumeration responsibility

The accepted and now implemented Tool is:

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

The initial runtime starts at a Section boundary and continues by TextUnitCursor. Anchor-based `before/after(locator)` starts remain a compatible future extension after locator-input/context handoff is implemented.

No separate `get_sentences`, `get_paragraphs`, or format-specific Tool is accepted.

### 4. Do not overload `read_document` with enumeration

`read_document` remains responsible for canonical reading of an already-known source target or continuation of one deterministic read stream.

`get_text_units` discovers/enumerates unknown child reading items. Combining both would conflate:

```text
ReadCursor     = progress through one read/render stream
TextUnitCursor = progress through one ordered enumeration stream
```

and create a parameter cross-product across target, read mode, granularity, anchor, direction, pagination, rendering, and non-prose policy.

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

The following remain independent capabilities:

```text
neighbor context
container context
structural context
```

They may share `get_context`, but only through an explicit tagged relation contract. A bag of interacting optional parameters that changes meaning implicitly is rejected.

Legacy `section_id + before + after` maps only to Section-neighbor context. Paragraph/Sentence context is still a later increment.

### 7. SearchHit must carry direct source handoff

Future search candidates are:

```text
section | paragraph | sentence
```

Each hit carries the strongest truthful `TextLocator`. A title-only structural hit remains Section-level and must not receive fake Paragraph/Sentence identity.

The accepted interaction is:

```text
search → SearchHit.text_locator → read/context
```

Copying a snippet and searching again is not a conforming precise-reading workflow. This search handoff is still a later increment; enumeration now provides the canonical TextLocator output foundation.

### 8. Locator and cursor identities remain separate

ADR 0002 remains normative:

```text
TextLocator = canonical source address
Cursor      = opaque progress through one versioned stream
```

Fine-grained identity remains bound to the normalized document and segmentation policy. Raw `content_hash` remains source provenance and is not silently redefined.

A stream cursor is never a citation and never becomes a normalized source range.

Current TextUnitCursor binds raw/normalized document identity, target Section, segmentation version, requested kind, direction, coverage policy, next index, stream length and cursor schema.

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

Coverage distinguishes:

- eligible content represented by Sentences;
- coarser non-prose items;
- intentionally skipped regions;
- unsupported gaps.

Sentence coverage and all-content coverage do not share a misleading denominator.

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

Current valid calls include the six legacy Tools plus `get_text_units`:

```text
list_documents(path?, recursive, max_results)
open_document(source, auth_profile?, force_refresh)
get_document_structure(document_id, max_depth?)
get_text_units(document_id, section_id, requested_kind, direction, coverage_policy, max_items, max_chars?, cursor?)
search_document(document_id, query, limit)
get_context(document_id, section_id, before, after, max_chars?)
read_document(document_id, section_id, max_chars?, cursor?)
```

Rules:

1. existing fields retain meaning;
2. legacy `Location.char_start/end` are not silently reinterpreted;
3. legacy Section reads retain current recursive subtree rendering;
4. legacy Section context retains current neighbor semantics;
5. precise locator/cursor/capability fields are additive initially;
6. persisted migrations are explicit and tested;
7. TextUnit enumeration remains independent of read/context/search state machines.

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

Current Section handoff remains. Future: `auto | section | paragraph | sentence` granularity and locator-bearing candidate kinds.

### `get_context`

Current Section-neighbor contract remains. Future: tagged `neighbor | container | structural` relations with locator-bearing items.

### `read_document`

SectionTreeReadStream + ReadCursor continuation is implemented. Future: tagged exact TextLocator targets. Rendered stream positions remain distinct from canonical normalized ranges.

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

This ADR supersedes only earlier statements that:

- described the primary surface as five Tools; or
- left the need for a generic `get_text_units` Tool undecided.

The implementation-status notes in this ADR supersede its own historical “future seven Tool” wording; the design rationale remains unchanged.

## Implementation Order / Status

```text
P0 read continuation                                  ✓
P0 normalized source identity/range                  ✓
P1 canonical/rebuildable Paragraph TextUnits         ✓
P1 deterministic Sentence locator/coverage           ✓
P1 get_text_units + TextUnitCursor enumeration       ✓
P1 precise read/context/search locator handoff       next
P1 EPUB reliability/coverage increments              later
```

Short-lived implementation branches must continue to preserve locator/cursor identity and avoid collapsing independent state machines.

## Acceptance Invariants

A conforming runtime now requires:

1. use-case success is defined at reading-task level, not Tool-call level;
2. the six pre-existing Tool calls remain valid and `get_text_units` is the only new Tool admitted by this decision;
3. structure does not expand into a Sentence-sized TOC;
4. read does not become an implicit TextUnit enumerator;
5. every incomplete read/TextUnit stream has actionable continuation or explicit unsupported state;
6. repeated Section/TextUnit continuation is gap-free and overlap-free when returned cursors are used as-is;
7. every TextUnit is an exact normalized source slice;
8. SearchHit will flow directly into read/context through a locator when that later handoff increment lands;
9. neighbor/container/structural context semantics remain explicit;
10. TextLocator and every cursor type remain non-interchangeable;
11. stale cursor/locator identity fails closed;
12. non-prose remains readable without fake Sentences under source-preserving enumeration;
13. `eligible_only` never claims all-source completion;
14. reliability/degradation/coverage is visible and reproducible;
15. Document/Section remain source truth and indexes remain rebuildable derived state;
16. Sentence persistence is never required to reconstruct canonical content or identity.

## Consequences

Positive:

- an Agent can perform complete Section-scoped Sentence-first reading with explicit end-of-stream state;
- Paragraph/Sentence traversal has one generic, source-ordered contract;
- read, enumeration, search, and context retain clear responsibilities;
- non-prose degradation and completion claims are explicit;
- TextUnitCursor can continue from persisted canonical Document without a Sentence source store;
- the surface grew only where multiple real use cases required it.

Costs:

- the runtime surface now contains seven rather than six Tools;
- a TextUnit enumeration state machine and cursor type must be maintained;
- capability/reliability/coverage metadata is part of contract design;
- locator-input read/context/search handoff is still a separate migration step;
- end-to-end acceptance must validate task completion and coverage, not only schema success.

## Review Outcome

The design was accepted after review corrected three initial risks:

1. a pure Sentence stream would have hidden non-prose;
2. overloading `read_document` would have conflated read and enumeration cursors;
3. a generic context parameter bag would have hidden neighbor/container/structural semantics.

Implementation review later added two important constraints:

1. `eligible_only` never claims all-source completion;
2. Sentence persistence is unnecessary for correctness because cursor continuation is version-bound and deterministically rebuildable from canonical persisted Document state.

The resulting decision remains consistent with ADR 0002 and ADR 0003.
