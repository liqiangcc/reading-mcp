# ADR 0004: Use-Case-First MCP Tool Contracts

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/tool-contract-use-cases`
- Reviewed against main: `e55fc203aa4c6e184b71125a2022140c31a4b762`
- Implementation status: runtime surface is seven Tools; `get_text_units`, locator-driven context, exact TextLocator read, shared locator resolution, and SearchHit → Section TextLocator handoff are implemented; Paragraph/Sentence lexical candidates remain future.
- Related design: `docs/tool-contract-use-case-design.md`
- Related implementation contracts: `docs/text-unit-enumeration-contract.md`, `docs/context-granularity-contract.md`, `docs/precise-read-locator-contract.md`, `docs/search-locator-contract.md`
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

Those Tools supported the existing coarse workflow, but source-first review found independent contract gaps: bounded continuation, precise TextUnit enumeration, locator-driven context/read, direct SearchHit handoff, and richer reliability/coverage evidence.

The review started from Actor goals and use cases, derived capabilities/state machines, and only then evaluated Tool candidates. For ordered Paragraph/Sentence enumeration it compared:

```text
A. overload read_document with granularity/view
B. add one generic get_text_units Tool
C. overload get_context
```

## Decision

### 1. Use-case-first is the Tool admission rule

A Tool or contract mode is accepted only when an Actor goal and use case establish task-level success, failure/degradation semantics, required state, legal next operations, and acceptance evidence.

“Keep Tool count small” and “adding a Tool is convenient” are both insufficient reasons.

### 2. Six-Tool reviewed baseline; seven-Tool implemented runtime

ADR 0004 accepted exactly one additional independent responsibility:

```text
get_text_units
```

Current runtime remains seven Tools:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

The six pre-existing Tool calls remain valid.

### 3. Ordered TextUnit enumeration remains independent

`get_text_units` owns bounded source-ordered Paragraph/Sentence-first discovery, per-item TextLocator, actionable TextUnitCursor pagination, completion, and truthful non-prose coverage.

No separate `get_sentences`, `get_paragraphs`, or format-specific TextUnit Tool is accepted.

`read_document` still reads an already-known source target/stream. Exact TextLocator read does not violate this boundary because it does not discover unknown child units.

```text
ReadCursor     = progress through one read stream
TextUnitCursor = progress through one enumeration stream
```

### 4. Context semantics are explicitly tagged

Implemented variants under `get_context` are:

```text
neighbor(section | paragraph | sentence)
container(paragraph | section)
structural(owner_section | ancestors | siblings | children)
```

Legacy `section_id + before + after` maps only to Section-neighbor context. A generic optional-parameter bag that silently changes relation semantics remains rejected.

### 5. Read has explicit legacy and exact modes

```text
legacy section_id
→ section_tree / section-tree-markdown/v1

TextLocator target
→ exact_target / exact-normalized-source/v1
```

Exact read supports Section, Paragraph, Sentence, and CharacterRange locators. Cursor progress remains distinct from source ranges; exact responses expose truthful `returned_locator` CharacterRanges.

### 6. SearchHit direct handoff is implemented without overstating precision

The accepted candidate surface is:

```text
section | paragraph | sentence
```

Each hit carries the strongest truthful TextLocator. A title-only hit remains Section-level.

The direct workflow is now implemented:

```text
search_document
  ↓
SearchHit.text_locator
  ├→ read_document
  └→ get_context
```

However, current InMemory/SQLite SearchIndex rows are paragraph-like **retrieval units**, not canonical Paragraph TextUnits. Their historical split/location facts do not carry the normalized range + segmentation identity required for Paragraph/Sentence source addressing.

Therefore current runtime deliberately emits:

```text
candidate_kind = section
text_locator   = owning canonical Section locator
```

for every current SearchIndex hit, while retaining snippet/score/legacy Location as retrieval preview/provenance.

Paragraph/Sentence candidate kinds are contractually allowed but may only be emitted after a later lexical TextUnit index can prove those identities.

### 7. Shared locator resolution is a cross-consumer invariant

Before SearchHit handoff, exact read and structured context had overlapping locator validation. This increment consolidates the identity/stale logic into one application-level resolver covering:

```text
document_id
raw content_hash
normalized_document_hash
owner Section
Section / CharacterRange / Paragraph / Sentence shape
Paragraph/Sentence ordinal
segmentation version
normalized range equality
INVALID_LOCATOR / STALE_LOCATOR
```

Consumers still decide which valid locator kinds they support. Exact read accepts CharacterRange; current context does not. Unsupported capability semantics must not be mislabeled malformed locator identity.

### 8. Locator and cursor identities remain separate

ADR 0002 remains normative:

```text
TextLocator = canonical source address
Cursor      = opaque progress through one versioned stream
```

No cursor is a citation. Search snippet, score, legacy search-unit Location, or index row ID are also not canonical source identity.

### 9. Stale state fails closed

Forbidden behavior includes:

- nearest-looking Sentence relocation;
- same-ordinal rebasing into changed content;
- cursor rebasing onto another stream;
- snippet text as identity;
- treating a retrieval row boundary as canonical Paragraph identity without evidence.

Index rebuild alone does not invalidate a still-resolvable source locator when canonical normalized/segmentation identity is unchanged.

### 10. Non-prose remains readable without fabricated Sentences

Under source-preserving Sentence enumeration, recognized code/table content may appear as an explicitly coarser Paragraph item. It never receives a fake Sentence ordinal.

`eligible_only` cannot claim all-source completion.

A coarse Paragraph TextLocator can be passed directly to exact read/context without inventing finer identity.

### 11. Reliability and coverage stay at decision points

No dedicated reliability Tool is accepted here. Capability/provenance/degradation/coverage belongs in the workflows where the Agent makes a decision: open, structure, enumeration, read/context/search results as relevant.

### 12. Additive contract evolution

Current valid forms include:

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

SearchHit keeps all historical response fields and adds:

```text
candidate_kind
text_locator
```

No SearchIndex persistence migration is required merely to provide the current Section-level handoff.

## Tool Contract Status

### `list_documents`

Current discovery semantics are implemented. Explicit discovery cursor/completion remains future.

### `open_document`

Raw/normalized identity and normalization diagnostics are implemented. Richer capability/open-outcome grading remains future.

### `get_document_structure`

Structural Section hierarchy is implemented and excludes Paragraph/Sentence enumeration. Actionable subtree pagination remains future.

### `get_text_units`

Implemented: Section-boundary start, Paragraph/Sentence, forward/backward, preserve_source/eligible_only, TextLocator, TextUnitCursor, completion, coverage. Anchor-based before/after starts remain future.

### `search_document`

Implemented direct canonical handoff:

```text
legacy preview fields
+ candidate_kind
+ text_locator
```

Current actual candidate kind is Section because the existing lexical rows do not prove canonical Paragraph/Sentence identity.

### `get_context`

Implemented tagged relations with Section/Paragraph/Sentence locator anchors and shared identity resolver.

### `read_document`

Implemented Section-tree continuation and exact TextLocator read/continuation, including truthful returned source CharacterRanges.

## Relationship to Earlier ADRs

ADR 0002 remains normative for addressing levels, source/index separation, normalized identity/ranges, TextLocator vs cursor, search candidate kinds, tokenizer/segmentation separation, and additive compatibility.

ADR 0003 remains normative for EPUB structure provenance, resolution/degradation, coverage, and non-prose behavior.

This ADR changes Tool-surface/deferment status, not those identity/reliability foundations.

## Implementation Order / Status

```text
P0 read continuation                                  ✓
P0 normalized source identity/range                  ✓
P1 canonical/rebuildable Paragraph TextUnits         ✓
P1 deterministic Sentence locator/coverage           ✓
P1 get_text_units + TextUnitCursor enumeration       ✓
P1 tagged locator-driven context                     ✓
P1 exact TextLocator read + continuation             ✓
P1 shared TextLocator resolver                       ✓
P1 SearchHit → truthful Section TextLocator handoff  ✓
P1 Paragraph/Sentence lexical TextUnit index         next
P1 EPUB reliability/coverage increments              later/evidence-driven
```

## Acceptance Invariants

A conforming runtime requires:

1. use-case success is defined at reading-task level, not Tool-call level;
2. the six pre-existing Tool calls remain valid and `get_text_units` is the only new Tool admitted by this decision;
3. structure does not expand into a Sentence-sized TOC;
4. read does not become an implicit TextUnit enumerator;
5. incomplete declared read/TextUnit streams have actionable continuation or explicit unsupported state;
6. Section/exact/TextUnit continuation is gap-free and overlap-free when cursors are used as-is;
7. precise TextUnits are exact normalized source slices;
8. exact-read returned locators reproduce response source exactly;
9. SearchHit flows directly into read/context through a locator;
10. current paragraph-like search rows remain Section candidates until canonical TextUnit evidence exists;
11. title-only Section search remains available without fabricated Paragraph identity;
12. neighbor/container/structural context semantics are explicit;
13. TextLocator and every cursor type are non-interchangeable;
14. stale cursor/locator identity fails closed;
15. non-prose remains readable without fake Sentences;
16. `eligible_only` never claims all-source completion;
17. Document/Section remain source truth and indexes remain rebuildable derived state;
18. Sentence persistence is not required for canonical identity;
19. shared locator identity rules are reused by consumers instead of copied into a third private implementation;
20. legacy search Location/snippet/index rows never become canonical locator precision by implication.

## Consequences

Positive:

- enumeration and search candidates can hand locators directly into read/context;
- search handoff is useful immediately without a premature FTS schema migration;
- source identity remains independent of ranking/snippet implementation;
- the current Section-only candidate precision is truthful and upgradeable;
- read/enumeration/context/search responsibilities remain distinct.

Costs:

- SearchDocumentUseCase must consult canonical DocumentRepository after ranking;
- shared locator resolution is now a maintained application primitive;
- Paragraph/Sentence search precision requires a separate lexical-index migration with tokenizer/version evidence;
- task-level E2E tests must continue to prove direct handoff, not only schema presence.

## Review Outcome

The original design was accepted after correcting risks around non-prose loss, read/enumeration conflation, and ambiguous context parameters.

Subsequent implementation evidence added further constraints: `eligible_only` cannot claim all-source completion; Sentence persistence is not a correctness dependency; exact read must separate stream progress from source range; precise context must avoid duplicate body projection; locator consumers must share identity/stale rules; and search must not promote historical retrieval-unit boundaries into canonical Paragraph/Sentence identity without proof.
