# Reading MCP — Codex Execution Entrypoint

> Status: authoritative execution entrypoint for the remaining v0.1 work
>
> Repository: `liqiangcc/reading-mcp`
>
> Baseline when this entrypoint was created: `main` at `13f4cc0b1bedd2219b3907e12ed2b41a38ea1994`
>
> Important: this SHA is a snapshot only. Codex must re-read current GitHub state before acting.

## 1. Purpose

This file is the single startup entrypoint for Codex. It does not replace the detailed plans. It prevents execution from starting with only one of the required documents in context.

Before modifying code, Codex MUST read both authoritative documents in full:

```text
1. docs/codex-development-production-rollout-plan.md
2. docs/codex-sentence-reading-closure-amendment.md
```

The two documents together define the current release plan.

If they appear inconsistent, do not guess. Re-read current `main`, the relevant contracts/ADRs, open PRs, CI, Issues, and runtime source, then resolve the conflict with a bounded documentation/design PR before implementation.

## 2. Mission

Finish the accepted Reading MCP v0.1 scope, make the sentence-reading workflow complete at user level, deploy the exact reviewed release candidate through the existing Secure MCP Tunnel topology, perform real ChatGPT acceptance, and release `v0.1.0`.

The intended end state is:

```text
reliable source acquisition / parsing
        ↓
canonical Document + structure
        ↓
Paragraph / Sentence-first TextUnits
        ↓
precise TextLocator handoff
        ↓
whole-document sentence-first composition
        ↓
restart-safe locator resume
        ↓
one-item-at-a-time ChatGPT interaction
        ↓
production deployment
        ↓
external ChatGPT acceptance
        ↓
v0.1.0
```

## 3. Do Not Reopen Settled Architecture Without Evidence

Current invariants remain normative:

```text
Document / Section = canonical source facts
TextUnitIndex / SearchIndex = rebuildable derived state

TextLocator = canonical source address
Cursor = bounded stream progress

Reading MCP = document context infrastructure
AI/Agent = explanation, QA, tutoring, reasoning, notes, user reading state
```

Do not add format-specific Tools or a user-session/progress database merely to make orchestration convenient.

The current runtime Tool surface remains:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

A new Tool requires a separate use-case-first decision proving an independent responsibility.

## 4. Required Sentence-Reading P0 Closure

The original rollout plan's Section-scoped Sentence acceptance is necessary but not sufficient.

Before release, the combined plan requires all three of the following:

### 4.1 Whole-document / multi-Section sentence-first traversal

Prove composition of:

```text
complete requested structure scope
→ canonical readable Section order
→ per-Section get_text_units(sentence, preserve_source)
→ each Section reaches section_complete
→ transition to the next canonical owner Section
→ all requested Sections/source gaps accounted for
```

Acceptance must cover at least:

```text
parent Section with intro body + children
empty Section
title-only Section
sibling transition
duplicate titles
coarse structural/non-prose item
explicit unsupported source gap
```

Do not fabricate a global `document_complete` flag unless a bounded use-case-first design proves it is necessary. The first completion proof is compositional.

### 4.2 Saved TextLocator resume across session / MCP restart

Long-lived reading checkpoint:

```text
last fully consumed preserve-source item TextLocator
```

Do not use `TextUnitCursor` as the durable bookmark.

Required proof:

```text
consume item N
→ save locator N outside Reading MCP
→ terminate/restart MCP with persistent state
→ reopen/reuse same document version
→ get_text_units(anchor_locator=N, direction=forward, preserve_source)
→ first returned item is the exact next source item
```

Also prove stale source identity returns `STALE_LOCATOR` rather than fuzzy relocation.

### 4.3 Interactive one-item-at-a-time ChatGPT acceptance

The user may stay on one current item for arbitrarily many follow-up questions.

Observational operations do not advance the saved reading position:

```text
read_document(current locator)
get_context(current locator)
search_document(...)
backward inspection
```

Only explicit advance intent such as `下一句` / `继续` moves once to the next preserve-source item.

If the next item is coarse Paragraph-level evidence, it must not be skipped just because the requested style is sentence-first.

## 5. Execution Rule: Direct Development Does Not Mean Skip Design

Codex should execute the plan directly, but every phase must respect its required decision depth.

Use this rule:

```text
accepted semantics already frozen
→ implementation / fix PR

semantics or state machine still open
→ source-first bounded design PR
→ review/merge design
→ implementation PR
```

Examples:

- PR #36 Open Reading Profile: continue the existing implementation PR; do not redesign it from scratch.
- Structure continuation: design `StructureCursor` state machine before DTO/runtime changes.
- Discovery continuation: design `DiscoveryCursor` state machine before DTO/runtime changes.
- Whole-document sentence progression: first verify current canonical Section/source-order facts. If they are sufficient, compose existing capabilities and add tests/docs. If they are insufficient or ambiguous, stop and create one bounded design PR for the missing ordering contract.
- Production paths/service names/tunnel profile/state paths: discover the actual server state before writing hardcoded deployment behavior.

## 6. Mandatory Execution Order

Execute in this order unless current repository evidence proves an earlier blocker must be resolved first:

```text
Phase 0  converge active PR #36 Open Reading Profile
   ↓
Phase 1  StructureCursor / actionable structure continuation
          + verify facts needed for cross-Section reading order
   ↓
Phase 2  DiscoveryCursor / actionable discovery continuation
   ↓
Phase 3  Streamable HTTP full reading lifecycle E2E
          + multi-Section sentence-first composition
          + restart/resume evidence where practical
   ↓
Phase 4  release-hardening + Issue/docs reconciliation
          + sentence-reading closure docs
   ↓
Phase 5  reproducible production systemd/tunnel deployment + rollback
   ↓
Phase 6  final release-candidate gate
          + whole-document / resume acceptance matrix
   ↓
Phase 7  deploy one exact main SHA
   ↓
Phase 8  external ChatGPT acceptance
          + whole-document/multi-Section
          + saved-locator resume
          + one-item-at-a-time interaction
   ↓
Phase 9  tag exact accepted SHA as v0.1.0 + GitHub Release
```

Do not deploy a feature branch. Do not tag before external acceptance.

## 7. Definition of Done Addendum

In addition to the master rollout plan's Definition of Done, release is incomplete until all of these are proven:

- multi-Section source-preserving Sentence-first traversal has deterministic no-gap/no-overlap composition;
- parent intro body is not lost when child Sections exist;
- empty/title-only Sections do not fabricate reading items;
- coarse items remain in source position;
- saved TextLocator resume works after MCP process restart with persistent state;
- stale saved TextLocator fails closed when normalized identity changes;
- final-item-of-Section resume can transition to the next canonical Section without replaying consumed content;
- production ChatGPT follow-up questions stay on the current reading item;
- production ChatGPT advances exactly once only after explicit advance intent;
- requested-scope completion is never stronger than structure/publication reliability evidence permits.

## 8. Branch / PR / CI Discipline

Use short-lived branches:

```text
design/<bounded-decision>
feat/<bounded-capability>
fix/<bounded-defect>
docs/<bounded-doc-update>
```

Rules:

1. Never develop directly on `main`.
2. One primary change reason per PR.
3. Re-read current `main` after each merge.
4. Review the final diff independently before merge.
5. Merge only the latest PR head.
6. Latest head must pass:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

7. Do not weaken assertions merely to make CI green.
8. Do not suppress Clippy warnings when the code can be corrected.
9. Do not deploy or tag from an unreviewed branch.

## 9. Identity / Reliability Guardrails

Do not change these as accidental side effects:

```text
reading-mcp-normalization/v6
normalized-document-hash/v2
normalized-block-model/v1
text-segmentation/v2
text-unit-id/v1
text-unit-cursor/v1
read-cursor/v2
lexical-search-index/v3
lexical-tokenizer/v1
epub-structure-validator/v1
```

If a change truly alters source addressing, segmentation, persisted parser output, cursor claims, or persistent lexical semantics, stop and explicitly review/version that contract.

Keep these distinctions explicit:

```text
canonical normalized-text coverage
!= source-publication coverage

eligible Sentence coverage
!= all-source completion

Section completion
!= requested-document-scope completion
```

No fuzzy locator/cursor rebasing.

## 10. Scope Control

Do not pull these into the v0.1 launch path without concrete release-blocking evidence:

```text
OCR / scanned PDF
browser rendering
enterprise product APIs
OAuth/Cookie interactive login
public multi-tenant hosting
general crawler
AI summary/QA/notes inside Reading MCP
vector/general semantic RAG
Sentence SQLite persistence
nested/leaf BlockQuote/ListItem Sentence identity
Figure/Caption/Footnote first-class identity
MathML/formula first-class identity
inline semantic identity
speculative ranking/tokenizer redesign
fuzzy locator rebasing
```

Figure/Caption/Footnote/Math/inline semantics are legitimate future deep-reading gaps, not reasons to delay the current v0.1 unless real acceptance evidence makes one a blocker.

## 11. Production Rules

Before deployment:

- inspect the actual host/service/tunnel/state configuration;
- add repository-owned systemd/install/deploy/verify/rollback artifacts;
- parameterize unsafe hardcoded deployment assumptions;
- never commit secrets/private document content;
- preserve SQLite/Raw/Parsed state;
- preserve the previous working binary/unit/config and exact SHA;
- freeze one `RELEASE_CANDIDATE_SHA` from reviewed `main`.

On production failure:

```text
stop rollout
→ collect bounded evidence
→ rollback previous known-good release
→ prove old service healthy
→ fix in Git branch + CI
→ deploy a new reviewed SHA
```

Never leave a live production-only edit uncommitted.

## 12. Final External Acceptance

Through the actual production Secure MCP Tunnel, prove at minimum:

```text
Tools/list = exactly seven Tools

discovery → open → reading_profile

structure → continuation → select Section

multi-Section sentence-first traversal
→ preserve_source
→ TextUnit continuation
→ exact TextLocator read/context

saved TextLocator
→ service/process restart or equivalent production persistence boundary
→ exact resume

current item N
→ multiple follow-up questions/context/search
→ still N
→ explicit 下一句/继续
→ exactly N+1

SearchHit.text_locator
→ read_document
→ get_context
→ optional get_text_units(anchor_locator)

EPUB reliability/provenance/coarse handling
PDF traceability
instruction-like document content remains data
```

Record only non-sensitive evidence in the acceptance Issue.

## 13. Codex Startup Instruction

When starting or resuming Codex work, use this exact operating intent:

```text
Repository: liqiangcc/reading-mcp

Goal:
Finish the accepted v0.1 development scope, close the complete sentence-reading workflow,
deploy the exact reviewed release candidate through the existing Secure MCP Tunnel topology,
perform real ChatGPT acceptance, and release v0.1.0.

Before changing anything, read in full:
1. docs/codex-execution-entrypoint.md
2. docs/codex-development-production-rollout-plan.md
3. docs/codex-sentence-reading-closure-amendment.md

Then re-read the actual current main, active PRs, latest-head CI, open Issues, relevant contracts/ADRs,
and deployment state. Snapshot SHAs in docs are evidence only, never permanent truth.

Continue the current earliest unfinished Phase; at the snapshot that created this entrypoint,
that means converge active PR #36 first, but verify before assuming this is still true.

For a phase whose semantics are already accepted, implement/fix directly on a short-lived branch/PR.
For a phase whose state machine/contract remains open, do source-first bounded design first,
merge that design, then implement it.

Preserve the seven-Tool boundary unless a separately reviewed use case proves a new Tool necessary.
Preserve source identity/cursor/index version boundaries and fail-closed stale behavior.
Do not commit secrets. Do not deploy feature branches. Do not tag before external acceptance.
Every implementation PR latest head must pass Format + Clippy + full Test before merge.

Do not stop after Section-scoped Sentence reading. v0.1 acceptance must also prove:
- whole-document / multi-Section sentence-first composition;
- saved TextLocator resume across client/MCP restart;
- one-item-at-a-time ChatGPT interaction where follow-up questions do not advance progress.

Finish all development, production deployment, rollback preparation, external ChatGPT acceptance,
and v0.1.0 tag/GitHub Release before reporting final completion.
```

## 14. Final Rule

After this entrypoint is merged, do not keep expanding the general planning layer merely for completeness.

The default next action is execution:

```text
read current evidence
→ execute earliest unfinished Phase
→ create bounded design only where semantics are genuinely open
→ implement
→ prove with CI/E2E
→ merge
→ continue
```

The release goal is not maximum document count or feature count. It is truthful, resumable, source-grounded reading that can be deployed and externally proven end to end.
