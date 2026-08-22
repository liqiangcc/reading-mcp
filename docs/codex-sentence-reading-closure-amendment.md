# Reading MCP — Sentence-Reading Closure Amendment

> Status: normative amendment to `docs/codex-development-production-rollout-plan.md`
>
> Scope: close the remaining end-user sentence-reading workflow gaps before `v0.1.0`
>
> Baseline reviewed: `main` at `80e5f152c7d8994fc4042b1865802e3608a461f9`
>
> Rule: Codex must read this amendment together with the master rollout plan. Where the master plan only proves Section-scoped Sentence reading, this amendment adds the stronger whole-document / resume / interactive acceptance requirements below. It does not authorize an eighth Tool by itself.

---

## 1. Why this amendment exists

The current precise-reading foundation is strong at the TextUnit level:

```text
Section
→ get_text_units(sentence, preserve_source)
→ Sentence / coarse Paragraph items in source order
→ TextLocator per item
→ cursor continuation
→ section_complete
→ read_document / get_context / search handoff
```

The current contracts also correctly keep:

```text
TextLocator = canonical source address
Cursor      = bounded stream progress
```

and already support exact anchor continuation from a known TextLocator.

However, a source-first review of the accepted use cases and production rollout plan found that the release proof still stops at one Section. A user-level goal such as “read this whole book sentence by sentence, ask questions on one sentence, stop today, and resume tomorrow” needs three additional closure properties:

1. deterministic whole-document progression across Section boundaries;
2. resumable reading from a saved source locator across client/MCP restart;
3. interactive one-reading-item-at-a-time behavior where questions do not silently advance progress.

These are P0 **workflow/acceptance gaps**. They are not evidence that Sentence segmentation itself is missing.

---

## 2. Architecture constraints

The amendment preserves all existing architecture boundaries.

### 2.1 Do not add a Tool merely because the workflow is larger

The first implementation hypothesis is composition of the existing seven Tools:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

A new Tool is allowed only if a separate use-case-first design proves an independent responsibility that cannot be truthfully composed from the current surface.

Specifically, do **not** introduce by default:

```text
read_book_sentence_by_sentence
next_sentence
save_reading_progress
reading_session
get_current_sentence
```

### 2.2 Reading progress is not canonical source truth

Reading MCP owns source identity and resumability primitives. It does not become a user-profile/session database.

Default model:

```text
Reading checkpoint
= the last fully consumed source-preserving reading item's TextLocator
```

The upper-layer Agent/client may persist that locator in its own memory/state.

Reading MCP must make the locator safe to reuse after restart when the referenced normalized document identity is still current.

### 2.3 Whole-document completion need not become one synthetic MCP flag

Do not add `document_complete=true` to a Tool merely for convenience.

The initial completion proof is compositional:

```text
requested structure enumeration complete
AND
all selected/readable owner Sections accounted for in canonical source order
AND
each source-preserving TextUnit stream reaches section_complete
AND
publication/source reliability reports no hidden unsupported gap for the claimed scope
```

If later implementation evidence shows this cannot be proved safely by the Agent, return to use-case design before adding a new contract.

### 2.4 Preserve coarse source regions

Sentence-first means:

```text
eligible prose → Sentence
coarse structural/non-prose region → coarse Paragraph reading item
```

The whole-document workflow must advance over both. A checkpoint may therefore be a Sentence locator **or** a coarse Paragraph locator emitted by `preserve_source`.

Never skip a coarse item simply because the user asked to read “sentence by sentence”.

---

# 3. P0 Use Case A — Whole-Document Sentence-First Traversal

## User / Agent goal

Read a requested document from an initial structural boundary to the end in canonical source order, using Sentence precision wherever justified and explicit coarse items elsewhere, without hidden gaps or duplicate reading items.

## Preconditions

- document is opened and version identity is known;
- reading profile/reliability is available;
- structure can be enumerated completely for the requested scope;
- per-Section source-preserving TextUnit enumeration is supported where advertised.

## Main success flow

```text
open_document
  ↓
inspect reading_profile
  ↓
get_document_structure
  ↓
continue StructureCursor until requested structure scope is complete
  ↓
derive canonical readable Section order from returned canonical structural/source facts
  ↓
for each owner Section in that order:
    get_text_units(
      section_id = current,
      requested_kind = sentence,
      coverage_policy = preserve_source
    )
      ↓
    continue TextUnitCursor until section_complete
      ↓
    move to next canonical readable Section
  ↓
all requested Sections exhausted
  ↓
verify aggregate reliability / explicit unsupported gaps
```

The workflow may start from a selected Chapter/Section rather than the whole publication, but the target scope must be explicit.

## Canonical ordering requirements

Codex must design/test the transition using current canonical structure/source-order facts rather than guessing from titles.

At minimum define behavior for:

```text
Section with its own intro body + child Sections
empty Section
heading/title-only Section
parent/child/sibling transitions
duplicate Section titles
fallback structural nodes
unresolved/coarse source resources
```

The transition rule must not silently switch between incompatible ordering notions such as:

```text
navigation tree order
vs
spine/source order
vs
lexical search order
```

Use the current canonical source-order contract; if existing Section facts are insufficient to derive one unambiguous body-consumption order, stop and create a bounded design PR before implementation.

## Section transition invariant

Completing one Section is **not** document completion.

```text
section_complete(current) = true
→ choose next canonical owner Section in requested scope
```

Only when no remaining owner Section or explicit source gap exists may the Agent claim the requested scope was consumed.

## No-gap / no-overlap proof

Acceptance must prove, for a representative multi-Section fixture:

```text
all source-preserving reading items returned exactly once
all per-Section pages concatenate without overlap
all Section transitions preserve canonical source order
no title-only/empty Section causes false content
no parent intro body is lost when children exist
```

Known unsupported publication regions remain explicit degradations and must prevent a stronger “publication fully read” claim when the reliability contract says coverage is incomplete.

## Failure / degradation

Fail or qualify completion when:

- structure continuation cannot enumerate the requested scope;
- document/normalized identity changes during traversal;
- a Section/TextUnit cursor is stale or mismatched;
- canonical Section order is ambiguous under current facts;
- publication coverage has an unsupported gap inconsistent with the requested completion claim.

Do not recover by fuzzy title matching or snippet search.

## Expected implementation impact

Prefer no new MCP Tool.

Likely work is:

- a use-case/design note defining cross-Section orchestration and completion proof;
- fixture/property/E2E coverage that composes structure continuation + per-Section TextUnit continuation;
- documentation for Agent callers;
- only minimal runtime changes if current returned structural facts are insufficient for unambiguous progression.

---

# 4. P0 Use Case B — Resume From Saved TextLocator Across Session / Restart

## User / Agent goal

Stop after one fully consumed reading item and later resume from the next source-ordered item without restarting the Section or relying on remembered display text.

## Core model

Persist outside Reading MCP:

```text
checkpoint_locator = last_consumed_item.locator
```

Do not persist a `TextUnitCursor` as the long-lived bookmark. Cursor is progress state for one bounded stream contract; `TextLocator` is the canonical version-bound source address.

## Main success flow

Session 1:

```text
get_text_units(sentence, preserve_source, max_items=1)
→ item N + locator N
→ user/Agent finishes item N
→ upper layer saves locator N
→ session ends
```

Later / after MCP process restart:

```text
open/reuse same document version
→ get_text_units(
     section_id = locatorN.owner_section_id,
     anchor_locator = locatorN,
     requested_kind = sentence,
     direction = forward,
     coverage_policy = preserve_source
   )
→ first returned reading item is N+1
```

If N was the final item in the owner Section, anchored traversal may immediately reach that Section boundary; whole-document orchestration then advances to the next canonical Section from Use Case A.

## Restart acceptance

At minimum prove:

```text
open document
→ consume an interior Sentence/coarse item
→ save its TextLocator
→ terminate MCP process
→ restart with persistent state
→ reuse/reopen document
→ anchor from saved locator
→ exact next item is returned
```

Also test a checkpoint on:

- an ordinary Sentence;
- a coarse `preserve_source` Paragraph item;
- the final item of a Section;
- a document restored from SQLite state without Sentence-row persistence.

## Stale source behavior

If source/normalized identity changed:

```text
saved locator
→ STALE_LOCATOR
```

The system must not map the bookmark to “the closest sentence”, the same ordinal, or a snippet match in the new version.

The upper layer may explicitly reopen/re-search/re-navigate and save a new checkpoint.

## Responsibility boundary

For v0.1, do not add a user progress database to Reading MCP.

Reading MCP proves:

```text
saved canonical locator can be resolved or rejected truthfully
```

The Agent/client owns:

```text
which book the user is reading
which checkpoint to remember
whether the user considers an item finished
```

---

# 5. P0 Use Case C — Interactive One-Item-at-a-Time Reading

## User goal

Read one current source item, ask any number of questions about it, and advance only after an explicit request such as “下一句 / 继续”.

This is primarily an Agent-product acceptance requirement. Do not move tutoring/question-answering into Reading MCP.

## Required interaction state

Conceptual Agent state:

```text
current_item_locator
last_consumed_checkpoint
next-item transition only on explicit advance intent
```

Recommended retrieval pattern:

```text
get_text_units(
  requested_kind = sentence,
  coverage_policy = preserve_source,
  max_items = 1
)
```

Then:

```text
current item
  ↓
user asks meaning / why / terminology / evidence
  ↓
read_document(current locator)      # when exact re-read is useful
get_context(current locator)        # when local/structural context is useful
search_document(...)                # when locating other evidence is useful
  ↓
current_item_locator remains unchanged
```

Only explicit advancement causes:

```text
get_text_units(anchor_locator=current_item_locator, direction=forward, ...)
→ next source-preserving item
```

## Required acceptance conversation

Production ChatGPT acceptance must exercise a flow equivalent to:

```text
Sentence/item N
→ ask “这句话是什么意思？”
→ answer while staying on N
→ ask “为什么作者这么说？”
→ context/search as needed, still stay on N
→ ask “前一句是什么？”
→ inspect backward context/enumeration, still keep N as reading position
→ ask another follow-up, still N
→ user says “下一句”
→ advance exactly once to N+1
```

If the next source item is coarse rather than Sentence, return/read it as that coarse item and do not silently jump over it.

## Progress mutation rule

Questions and evidence lookups are observational:

```text
read
context
search
backward inspection
```

They do **not** mutate the saved reading checkpoint by themselves.

Only a successful explicit advance to a new reading item may update the checkpoint after the Agent considers that transition committed.

This rule belongs to ChatGPT/Agent orchestration and acceptance, not an MCP-side mutable session.

---

# 6. Integration Into the Master Rollout Plan

This amendment changes the release interpretation of existing phases without renumbering them.

## Phase 0 — PR #36

No responsibility change. Finish Open Reading Profile as already planned.

The profile is needed so whole-document traversal can qualify precise-reading and publication coverage claims before starting.

## Phase 1 — Structure continuation

In addition to existing oversized-structure tests, ensure the final contract exposes enough deterministic canonical ordering/scope evidence to support Use Case A.

Do not add body TextUnits to `get_document_structure`.

## Phase 2 — Discovery continuation

No new sentence-specific responsibility.

## Phase 3 — Streamable HTTP full lifecycle E2E

The HTTP lifecycle E2E must now include a representative multi-Section source and prove:

```text
open/profile
→ structure continuation if needed
→ Section A sentence-first completion
→ Section B sentence-first completion
→ exact locator read/context handoff
```

Add restart-resume coverage at the strongest practical layer; if process restart is unsuitable inside the HTTP test harness, keep a repository/runtime persistence test plus production restart acceptance.

## Phase 4 — Hardening/docs/Issue reconciliation

Add the three use cases to current runtime/acceptance documentation.

Reconcile stale statements that imply Sentence reading is only an isolated Section demo.

Do not describe Reading MCP as a tutoring/session-state service.

## Phase 6 — Final release candidate gate

The required acceptance matrix gains:

```text
whole-document / multi-Section sentence-first composition
saved TextLocator restart/resume
final-item-of-Section → next-Section transition
interactive one-item invariant at product acceptance layer
```

## Phase 8 — External ChatGPT acceptance

The existing “Sentence-first TextUnit continuation” check is necessary but no longer sufficient.

Add all three production acceptance scenarios in Section 8 below.

---

# 7. Required Automated Acceptance Evidence

Before production release, automated/source-level evidence should cover at least:

### A. Multi-Section source-order traversal

Fixture shape:

```text
Chapter/Section A
  intro body
  child A.1
Section B
  body
Section C
  title-only or empty
Section D
  body + coarse non-prose/structural item
```

Assertions:

- structural enumeration returns deterministic scope/order;
- every owner Section that has body is selected exactly once;
- each source-preserving TextUnit stream completes exactly once;
- concatenated per-Section item identities contain no duplicate/gap attributable to orchestration;
- empty/title-only nodes do not fabricate TextUnits;
- coarse items remain present in their source position.

### B. Cross-restart checkpoint

Assertions:

- saved interior Sentence locator resumes at exact next item;
- saved coarse Paragraph locator resumes at exact next item;
- saved terminal Section item produces truthful boundary completion then orchestration advances to next Section;
- restart/repository reopen does not require persisted Sentence rows;
- changed normalized identity rejects checkpoint as stale.

### C. Contract separation

Tests/review must continue proving:

```text
checkpoint locator ≠ TextUnitCursor
TextLocator ≠ search snippet
context lookup ≠ reading progress mutation
structure cursor ≠ source citation
```

---

# 8. Required Production ChatGPT Acceptance Additions

The production tunnel acceptance for the exact release-candidate SHA must now include:

## 8.1 Whole-document / multi-Section progression

```text
open representative document
→ inspect profile
→ enumerate requested structure completely
→ start sentence-first reading in first selected Section
→ finish it
→ move to next canonical Section without manual title/snippet search
→ consume at least one additional Section
```

Record that cross-Section transition was deterministic and preserved coarse items.

A full giant book does not need to be consumed manually for acceptance; automated fixtures/property tests prove exhaustive behavior, while production proves the real client can perform the transition.

## 8.2 New-session resume

```text
read item N
→ retain its TextLocator as test checkpoint without exposing private body text in Issue evidence
→ restart/reconnect as required by acceptance setup
→ resume from locator N
→ receive N+1 or truthful Section boundary
```

If the production source changed, the acceptable result is explicit stale failure, not fuzzy continuation.

## 8.3 Interactive one-item reading

Use a real ChatGPT conversation:

```text
current item N
→ at least two follow-up questions
→ context/read/search evidence lookup as useful
→ verify current reading position remains N
→ explicit “下一句/继续”
→ exactly one forward transition
```

The external acceptance record may state pass/fail and locator kinds/Section IDs when non-sensitive. Do not paste private source body.

---

# 9. Updated P0 Release Checklist

The master P0 list is amended to include these items before final production acceptance:

- [ ] Define/test deterministic cross-Section sentence-first orchestration for a requested document scope.
- [ ] Prove parent/body/child/empty/title-only/coarse Section transition semantics without hidden gaps or duplicates.
- [ ] Prove a saved Sentence TextLocator resumes after MCP/repository restart.
- [ ] Prove a saved coarse source-preserving Paragraph TextLocator resumes after restart.
- [ ] Prove final item of one Section can transition to the next canonical Section without snippet/title re-search.
- [ ] Keep long-lived checkpoint identity as TextLocator, not TextUnitCursor.
- [ ] Do not add MCP-owned user/session progress persistence for v0.1 unless a new independently reviewed use case requires it.
- [ ] Add multi-Section sentence-first flow to HTTP/full-lifecycle acceptance.
- [ ] Add whole-document transition, new-session resume, and interactive one-item behavior to production ChatGPT acceptance.
- [ ] Preserve seven-Tool runtime unless use-case-first design independently proves another Tool is necessary.

The following remain post-v0.1 unless release evidence proves they block truthful reading:

```text
nested/leaf Sentence identity inside flat BlockQuote/ListItem
Figure/Caption/Footnote first-class reading units
MathML/formula-specific identity
inline semantic markup as source-addressing identity
AI tutoring/notes/progress database inside Reading MCP
```

These are real future deep-reading opportunities, especially for technical books, but they must not be mixed into the v0.1 closure without a separate use-case decision.

---

# 10. Codex Execution Amendment

When executing `docs/codex-development-production-rollout-plan.md`, Codex must also apply these rules:

```text
1. Treat this file as a normative v0.1 amendment to the master rollout plan.
2. Do not stop the sentence-reading proof at section_complete.
3. Design/prove deterministic transition from one completed Section to the next canonical readable Section.
4. Use a saved TextLocator as the default long-lived reading checkpoint; do not use TextUnitCursor as a bookmark.
5. Prove checkpoint resume across MCP/repository restart and fail closed on stale normalized identity.
6. Keep questions/read/context/search from implicitly advancing reading progress; this is an Agent acceptance invariant.
7. Prefer composing the current seven Tools. Do not introduce an eighth Tool without a new use-case-first decision.
8. Extend HTTP and production ChatGPT acceptance with multi-Section traversal, restart resume, and one-item interactive reading.
9. Preserve explicit coarse items and publication coverage degradations; never skip them to make sentence progression look cleaner.
10. Do not move AI QA, tutoring, notes, or user-profile/session persistence into Reading MCP.
```

Final v0.1 sentence-reading claim is therefore stronger than “Sentence TextUnits exist”. It is:

```text
A user/Agent can consume a requested document scope in deterministic source order,
Sentence by Sentence where justified and coarse item where required,
pause on one item for arbitrary reasoning,
advance only explicitly,
resume later from canonical source identity,
cross structural boundaries without re-search,
and distinguish true completion from explicit source degradation.
```
