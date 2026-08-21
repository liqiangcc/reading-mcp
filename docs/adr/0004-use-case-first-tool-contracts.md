# ADR 0004: Use-Case-First MCP Tool Contracts

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/tool-contract-use-cases`
- Reviewed against main: `e55fc203aa4c6e184b71125a2022140c31a4b762`
- Related design: `docs/tool-contract-use-case-design.md`
- Related identity architecture: `docs/adr/0002-text-index-locator-identity.md`
- Related reliability architecture: `docs/adr/0003-epub-first-structure-reliability.md`

## Context

Reading MCP must support AI Agents that complete reading goals, not merely clients that successfully invoke Tools. The current runtime exposes six Tools:

```text
list_documents
open_document
get_document_structure
search_document
get_context
read_document
```

Those Tools support the existing coarse workflow, but source-first review of the implementation and tests found contract gaps:

1. a truncated Section read has no actionable continuation;
2. a bounded structure response has no expansion continuation;
3. SearchHit can identify an owning Section but cannot hand an exact Paragraph/Sentence locator directly to read/context;
4. context only means neighboring Sections;
5. no current contract can discover the first Paragraph/Sentence, enumerate subsequent TextUnits in source order, or prove that a Section-level TextUnit stream is complete;
6. open/structure responses do not expose the capability, reliability, provenance, and coverage facts needed to qualify precise-reading claims;
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

### 2. Current runtime fact is six Tools

The reviewed runtime exposes six Tools. Existing valid calls and response fields remain supported.

Documentation must distinguish:

```text
current implemented surface = six Tools
accepted future surface      = seven Tools
```

The seventh Tool must not be advertised as implemented until its supporting capabilities and invariants exist.

### 3. Add one generic ordered TextUnit enumeration responsibility

The accepted future surface adds:

```text
get_text_units
```

Its responsibility is:

> Enumerate bounded Paragraph or Sentence-first reading items within one structural target in canonical source order, with per-item TextLocator, actionable pagination, explicit completion, and truthful non-prose/coverage semantics.

Use-case evidence includes:

- list Paragraphs in a Section;
- previous/next Paragraph;
- find the first Sentence in a Section;
- advance Sentence by Sentence in source order;
- previous/next Sentence;
- continue until Section completion;
- account for code/table/non-prose without fake Sentence identity;
- verify no gap/overlap under response budgets.

No separate `get_sentences`, `get_paragraphs`, or format-specific Tool is accepted.

### 4. Do not overload `read_document` with enumeration

`read_document` remains responsible for canonical reading of an already-known source target or continuation of one deterministic read stream.

`get_text_units` discovers/enumerates unknown child reading items. Combining both would conflate:

```text
ReadCursor     = progress through one read/render stream
TextUnitCursor = progress through one ordered enumeration stream
```

and create a parameter cross-product across target, read mode, granularity, anchor, direction, pagination, rendering, and non-prose policy.

### 5. Future recommended Tool surface

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

The following are independent capabilities:

```text
neighbor context
container context
structural context
```

They may share `get_context`, but only through an explicit tagged relation contract. A bag of interacting optional parameters that changes meaning implicitly is rejected.

Legacy `section_id + before + after` maps only to Section-neighbor context.

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

Copying a snippet and searching again is not a conforming precise-reading workflow.

### 8. Locator and cursor identities remain separate

ADR 0002 remains normative:

```text
TextLocator = canonical source address
Cursor      = opaque progress through one versioned stream
```

Fine-grained identity remains bound to the normalized document and segmentation policy. Raw `content_hash` remains source provenance and is not silently redefined.

A stream cursor is never a citation and never becomes a normalized source range.

### 9. Stale state fails closed

For a locator/cursor whose required identity no longer matches:

- resolve the exact retained historical version only when explicitly available and selected; otherwise
- return an explicit stale failure.

Forbidden:

- map to the nearest-looking Sentence;
- reuse the same Paragraph/Sentence ordinal in changed content;
- rebase a cursor onto a new stream;
- use snippet text as identity.

Index rebuild alone does not invalidate a SearchHit locator when canonical normalized/segmentation identity is unchanged.

### 10. Non-prose remains readable without fabricated Sentences

A Sentence-first request may encounter code, tables, formulas, or other non-prose.

Under source-preserving enumeration, the response may emit an explicitly coarser exact reading item with:

```text
requested kind
actual/effective kind
content class
locator/range
degradation reason
```

It must not create fake Sentence ordinals. Coverage reports distinguish:

- eligible prose represented by Sentences;
- coarser non-prose items;
- intentionally skipped regions;
- unsupported gaps.

Sentence coverage and all-content coverage must not share a misleading denominator.

### 11. Reliability and coverage are returned at decision points

No dedicated reliability/inspection Tool is accepted in this ADR.

Capability, provenance, degradation, and coverage information belongs additively in the workflows where the Agent must choose what to do:

```text
open_document
get_document_structure
get_text_units
read/context results when relevant
```

A dedicated inspection Tool requires a later independent use-case decision.

### 12. Contract evolution is additive first

Current valid calls remain valid:

```text
list_documents(path?, recursive, max_results)
open_document(source, auth_profile?, force_refresh)
get_document_structure(document_id, max_depth?)
search_document(document_id, query, limit)
get_context(document_id, section_id, before, after, max_chars?)
read_document(document_id, section_id, max_chars?)
```

Rules:

1. existing fields retain meaning;
2. legacy `Location.char_start/end` are not silently reinterpreted;
3. legacy Section reads retain current recursive subtree rendering;
4. legacy Section context retains current neighbor semantics;
5. precise locator/cursor/capability fields are additive initially;
6. persisted migrations are explicit and tested;
7. the new Tool is introduced only after the underlying domain/application capability exists.

## Tool Contract Direction

The accepted logical direction is:

### `list_documents`

Add completion/continuation metadata for bounded discovery.

### `open_document`

Add normalized-document identity, open/version outcome, capability grading, reliability, and coverage summary while preserving raw `content_hash` semantics.

### `get_document_structure`

Add actionable subtree/pagination semantics, child completeness, source order, provenance, and structural coverage. It continues to exclude Paragraph/Sentence enumeration.

### `get_text_units`

Add a generic Section-scoped ordered enumeration contract for Paragraph/Sentence requests, exact text and per-item locator, anchor/direction, `TextUnitCursor`, completion, non-prose degradation, and coverage.

### `search_document`

Add `auto | section | paragraph | sentence` granularity and locator-bearing candidate kinds. Search remains bounded retrieval, not body reading.

### `get_context`

Add tagged `neighbor | container | structural` relations and structured locator-bearing items; preserve the legacy concatenated Section-neighbor response.

### `read_document`

Add tagged source targets and `ReadCursor`, exact-target mode, completion metadata, and strict cursor validation. Rendered Section-tree stream positions remain distinct from canonical normalized ranges.

## Relationship to Earlier ADRs

ADR 0002 remains accepted and normative for:

- addressing levels;
- canonical Document/Section versus derived TextUnit/Search state;
- normalized-document identity;
- exact normalized ranges;
- TextLocator versus ReadCursor;
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

- described the current primary surface as five Tools; or
- left the need for a generic `get_text_units` Tool undecided.

It does not supersede locator, persistence, segmentation, retrieval, or EPUB reliability decisions.

## Implementation Order

This design ADR does not authorize implementation on the design branch. After merge, use short-lived branches and preserve separation from the existing `feat/read-continuation` branch.

Recommended dependency order:

```text
P0 read continuation
   - deterministic SectionTreeReadStream
   - actionable ReadCursor
   - strict version/mode binding

P0 normalized source identity/range
   - normalized_document_hash
   - exact Section-relative normalized ranges

P1 canonical/rebuildable Paragraph TextUnits
   - segmentation policy/version
   - locator/range validation

P1 deterministic Sentence TextUnits
   - prose eligibility
   - non-prose classification/coverage

P1 get_text_units contract + application capability
   - ordered enumeration
   - TextUnitCursor
   - completion/gap/overlap tests

P1 precise read/context/search locator handoff
   - exact locator reads
   - tagged context relations
   - locator-bearing search candidates

P1 EPUB reliability/coverage increments
   - navigation/spine reconciliation
   - canonical block provenance
   - validator evidence
```

Implementation branches may refine order where dependencies prove different, but must not collapse locator/cursor identity or pull Sentence/TextUnit work into the read-continuation branch.

## Acceptance Invariants

A future implementation conforms only if:

1. use-case success is defined at reading-task level, not Tool-call level;
2. current six valid Tool calls remain valid;
3. `get_text_units` is the only accepted new Tool in this decision;
4. structure does not expand into a Sentence-sized TOC;
5. read does not become an implicit TextUnit enumerator;
6. every truncated/partial stream has actionable continuation or explicit unsupported state;
7. repeated Section/TextUnit continuation is gap-free and overlap-free;
8. every TextUnit is an exact normalized source slice;
9. SearchHit flows directly into read/context through a locator;
10. neighbor/container/structural context semantics are explicit;
11. TextLocator and every cursor type remain non-interchangeable;
12. stale locator/cursor identity fails closed;
13. non-prose remains readable without fake Sentences;
14. reliability/degradation/coverage is visible and reproducible;
15. Document/Section remain source truth and indexes remain rebuildable derived state;
16. no Rust runtime, parser, FTS, or storage implementation is implied by this design commit.

## Consequences

Positive:

- an Agent can eventually perform complete Sentence-first reading with explicit end-of-Section state;
- Paragraph/Sentence traversal has one generic, source-ordered contract;
- read, enumeration, search, and context each retain a clear responsibility;
- source handoff no longer depends on copied snippets;
- stale state and non-prose behavior become testable rather than implicit;
- the surface grows only where multiple real use cases require it.

Costs:

- the future surface contains seven rather than six Tools;
- a new TextUnit enumeration state machine and cursor type must be maintained;
- capability/reliability/coverage metadata becomes part of contract design;
- additive migration requires old and new request/response forms to coexist for a period;
- end-to-end acceptance must validate task completion and coverage, not only schema success.

## Review Outcome

Accepted after an independent review corrected three initial design risks:

1. a pure Sentence stream would have hidden non-prose;
2. overloading `read_document` would have conflated read and enumeration cursors;
3. a generic context parameter bag would have hidden neighbor/container/structural semantics.

The resulting decision is consistent with ADR 0002 and ADR 0003 and is sufficiently stable to guide later implementation planning.
