# ADR 0004: Use-Case-First MCP Tool Contracts

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/tool-contract-use-cases`
- Reviewed against main: `e55fc203aa4c6e184b71125a2022140c31a4b762`
- Current implementation status: seven-Tool runtime; `get_text_units`, locator-driven context/read, direct SearchHit handoff, shared locator resolution, and canonical Section/Paragraph/Sentence lexical candidates are implemented.
- Related: `docs/tool-contract-use-case-design.md`, ADR 0002, ADR 0003, and implementation contracts under `docs/`.

## Context

The original six-Tool surface supported coarse reading but lacked independent ordered Paragraph/Sentence enumeration, bounded continuation, precise locator handoff, explicit context semantics, and reliable search→source handoff.

The review used:

```text
Actor Goal → Use Case → Capability / State Machine → Tool
```

instead of designing Tools from convenience or Tool-count targets.

## Decision

### 1. Use-case-first is the Tool admission rule

A new Tool or contract mode requires a real Actor goal, task-level success/failure/degradation semantics, required state, legal next operations, and acceptance evidence.

### 2. Runtime surface is seven Tools

Only one new independent Tool was admitted:

```text
get_text_units
```

Current runtime:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

The six historical Tool calls remain valid.

### 3. Enumeration is not read

```text
get_text_units
= discover/enumerate source-ordered Paragraph/Sentence-first items

read_document
= read one already-known source target/stream
```

```text
TextUnitCursor ≠ ReadCursor
```

No separate `get_sentences` / `get_paragraphs` Tool is accepted.

### 4. Context semantics are tagged

Implemented under one `get_context` Tool:

```text
neighbor(section | paragraph | sentence)
container(paragraph | section)
structural(owner_section | ancestors | siblings | children)
```

Legacy `section_id + before + after` means only Section neighbor context.

### 5. Read has explicit legacy/exact modes

```text
section_id
→ SectionTreeReadStream/v1

TextLocator
→ exact_target / exact-normalized-source/v1
```

Exact read supports Section / Paragraph / Sentence / CharacterRange and separates stream progress from source `returned_locator`.

### 6. SearchHit carries direct source handoff

Accepted and now implemented candidate kinds:

```text
section | paragraph | sentence
```

Each hit carries a canonical `TextLocator` and can flow directly to read/context:

```text
search_document
→ SearchHit.text_locator ─┬→ read_document
                          └→ get_context
```

Title-only hit remains Section-level. Recognized non-prose may remain Paragraph-level without fake Sentence identity.

### 7. Lexical precision is evidence-gated

The initial `feat/search-locator` handoff correctly emitted only Section locators because historical paragraph-like search rows were not canonical TextUnits.

The later `feat/lexical-text-unit-index` added independent evidence required for precise candidates:

```text
canonical Paragraph/Sentence TextUnits
lexical-tokenizer/v1
lexical-search-index/v2
stored canonical TextLocator
versioned migration/rebuild semantics
```

Only after that evidence existed did `search_document` begin emitting Paragraph/Sentence candidate kinds.

### 8. Shared locator resolution is normative

Read/context/search handoff relies on a shared resolver for canonical identity/staleness:

```text
document/raw/normalized identity
owner Section
locator shape
Paragraph/Sentence ordinal
segmentation version
normalized range equality
```

Capability support is evaluated after locator validity. No consumer gets a private fuzzy-repair rule.

### 9. Locator, cursor, and lexical token are distinct

```text
TextLocator = source address
Cursor      = stream progress
Token       = retrieval implementation detail
```

Tokenizer policy never becomes source identity.

### 10. Segmentation and tokenizer versions are independent

```text
text-segmentation/v1
→ Paragraph/Sentence identity

lexical-tokenizer/v1
→ lexical projection / matching / rebuild
```

Changing tokenizer version cannot change TextLocator identity.

### 11. Legacy SearchIndex adapters remain compatible

The Rust `SearchIndex::search` legacy path remains valid.

Precise-capable adapters advertise an independently versioned tokenizer and provide canonical lexical hits. Historical/custom adapters without that capability remain on the truthful fallback:

```text
legacy SearchHit
→ canonical owning Section TextLocator
→ candidate_kind = section
```

This avoids a source-compatible but runtime-breaking trait evolution.

### 12. Non-prose / coverage semantics remain explicit

Sentence-first source-preserving flows may expose coarse Paragraph items for recognized non-prose. They never fabricate Sentence identity.

`eligible_only` never claims all-source completion.

### 13. Additive external contract evolution

Requests remain compatible:

```text
search_document(document_id, query, limit)
read_document(document_id, section_id | target_locator, ...)
get_context(document_id, section_id | target_locator, ...)
```

SearchHit keeps historical preview fields and additive precise fields:

```text
candidate_kind
text_locator
```

Current Tool count stays seven; lexical precision did not justify another Tool.

## Tool Status

```text
list_documents
  implemented discovery; bounded cursor remains future

open_document
  raw/normalized identity implemented; richer capability grading can evolve additively

get_document_structure
  Section hierarchy only; Paragraph/Sentence remain outside TOC

get_text_units
  Paragraph/Sentence source-order enumeration + TextUnitCursor + coverage implemented

search_document
  canonical Section/Paragraph/Sentence candidates + direct TextLocator handoff implemented

get_context
  tagged locator-driven neighbor/container/structural relations implemented

read_document
  Section-tree and exact TextLocator modes + ReadCursor implemented
```

## Implementation Status

```text
P0 read continuation                                  ✓
P0 normalized source identity/range                  ✓
P1 Paragraph TextUnit index                          ✓
P1 Sentence locator/coverage                         ✓
P1 get_text_units + TextUnitCursor                   ✓
P1 locator-driven context                            ✓
P1 exact TextLocator read                            ✓
P1 shared locator resolver                           ✓
P1 SearchHit → TextLocator handoff                   ✓
P1 lexical TextUnit index v2                         ✓
   - Section title candidates
   - Paragraph/Sentence candidates
   - CJK/mixed technical tokenizer
   - persistent rebuild/migration
   - legacy adapter fallback
```

## Acceptance Invariants

1. Use-case success is task-level, not Tool-call-level.
2. `get_text_units` is the only Tool added by this design.
3. Structure never becomes a Sentence-sized TOC.
4. Read never becomes hidden enumeration.
5. Read/TextUnit cursors are actionable and non-interchangeable.
6. TextLocator is canonical source identity; cursor/token/search row is not.
7. SearchHit flows directly to read/context without snippet re-search.
8. Search candidate kind must match the resolved canonical locator kind.
9. Title-only Section search remains Section-level.
10. Non-prose never receives fake Sentence identity.
11. Tokenizer changes do not change TextUnit identity.
12. CJK/mixed technical retrieval does not depend on whitespace-only tokenization.
13. SearchIndex remains rebuildable derived state.
14. Missing/incompatible lexical state can rebuild from canonical persisted Document without source fetch/reparse.
15. Stale/invalid locator/cursor state fails closed.
16. Existing MCP requests and historical SearchIndex adapters retain truthful compatible behavior.

## Consequences

Positive:

- precise reading/search workflows share one locator model;
- lexical precision can improve without adding Tools or redefining source identity;
- tokenizer/index changes are isolated rebuildable concerns;
- old adapters degrade truthfully to Section precision instead of failing or fabricating ranges.

Costs:

- segmentation, tokenizer, index, cursor and rendering versions all require explicit maintenance;
- SearchDocumentUseCase validates derived hits against canonical DocumentRepository;
- persistent lexical state needs migration/rebuild semantics;
- ranking remains a separate evidence-driven optimization problem.

## Review Outcome

Accepted. Later implementation evidence preserved the original responsibility boundaries while realizing the full `section | paragraph | sentence` search candidate contract. Precise lexical search required canonical TextUnit evidence and independent tokenizer/index versioning; it did not justify a new MCP Tool.
