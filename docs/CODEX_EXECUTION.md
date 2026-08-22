# Codex Execution Entrypoint

> Status: authoritative execution entrypoint for finishing Reading MCP v0.1
>
> Rule: Codex starts here. Do not execute the release plan from only one subordinate document.

## 1. Authoritative documents

Before changing code, Codex MUST read both documents completely:

1. `docs/codex-development-production-rollout-plan.md`
2. `docs/codex-sentence-reading-closure-amendment.md`

Together they form the authoritative v0.1 development, acceptance, deployment, and release plan.

If the master rollout plan only proves Section-scoped sentence reading, the sentence-reading closure amendment is the stronger requirement for v0.1.

Repository snapshots, PR numbers, SHAs, CI results, Issue state, and deployment paths recorded in either document are historical evidence, not permanent truth. Re-read current GitHub/repository/deployment state before each phase.

## 2. Definition of done amendment

In addition to the master plan's Definition of Done, v0.1 is not complete until all of these are proven:

```text
multi-Section / requested-scope sentence-first traversal
+ no hidden Section transition gap/duplication
+ source-preserving coarse items retained
+ saved TextLocator restart/resume
+ stale saved locator fails closed
+ final-item-of-Section → next canonical Section transition
+ interactive one-item-at-a-time ChatGPT acceptance
+ follow-up questions do not implicitly advance reading progress
```

These requirements are P0 release gates.

They do not by themselves authorize an eighth MCP Tool.

## 3. Execution discipline

"Execute the plan" does not mean "immediately edit Rust for every remaining item".

For every phase:

```text
re-read current state
→ identify the bounded use case/change reason
→ determine whether semantics are already accepted
→ if not, create a bounded design PR first
→ review/merge design
→ create implementation PR
→ Format + Clippy + full Test on latest head
→ independent diff review
→ merge only current CI-green head
→ continue from updated main
```

### Design-first phases

At minimum, preserve the master plan's design-first gates for:

```text
StructureCursor / structure continuation
DiscoveryCursor / discovery continuation
```

For whole-document sentence-first traversal, first verify whether current canonical Section/source-order facts are sufficient to derive an unambiguous body-consumption order.

If sufficient:

```text
compose existing structure + get_text_units capabilities
→ add acceptance/evidence
```

If insufficient:

```text
STOP implementation
→ bounded source-first design PR
→ define the missing canonical transition contract
→ only then implement
```

Do not guess a cross-Section order from titles, search ranking, or transient parser state.

## 4. Sentence-reading responsibility boundary

Reading MCP owns:

```text
canonical source facts
TextLocator identity
ordered TextUnit enumeration
precise read/context/search handoff
version/stale validation
coverage/reliability evidence
restart-safe reconstruction from persisted canonical facts
```

The upper-layer Agent/client owns:

```text
AI explanation / QA / tutoring
which item the user considers consumed
current interactive reading position
long-lived user reading checkpoint storage
```

The long-lived checkpoint is the last fully consumed source-preserving reading item's `TextLocator`, not a `TextUnitCursor`.

Do not add by default:

```text
save_reading_progress
reading_session
get_current_sentence
next_sentence
read_book_sentence_by_sentence
```

A new Tool requires a separately reviewed use-case-first decision.

## 5. Required release acceptance additions

The master checklist is amended with the following mandatory checks.

### Development / automated acceptance

- [ ] Representative multi-Section source can be traversed in canonical source order.
- [ ] Parent Section intro body is not lost when child Sections exist.
- [ ] Empty/title-only Sections do not fabricate TextUnits or terminate traversal incorrectly.
- [ ] Coarse structural/non-prose reading items remain in source order.
- [ ] Per-Section source-preserving streams complete with no gap/overlap.
- [ ] Final item of one Section transitions to the next canonical readable Section without duplication.
- [ ] Saved interior Sentence `TextLocator` resumes at the exact next reading item after MCP restart.
- [ ] Saved coarse Paragraph `TextLocator` resumes correctly.
- [ ] Saved final-item-of-Section locator reaches Section boundary and then the next canonical Section through whole-document orchestration.
- [ ] Changed normalized identity returns `STALE_LOCATOR`; no fuzzy/ordinal/snippet rebasing occurs.

### Streamable HTTP acceptance

- [ ] HTTP lifecycle includes at least two body-owning Sections.
- [ ] HTTP lifecycle proves Section A completion → Section B completion.
- [ ] Exact TextLocator → read/context handoff remains valid during the multi-Section flow.

### Production ChatGPT acceptance

- [ ] ChatGPT reads one source-preserving item at a time.
- [ ] Repeated explanation/context/search questions leave the current reading position unchanged.
- [ ] Asking for the previous item is observational and does not silently change the saved checkpoint.
- [ ] Explicit "下一句 / 继续" advances exactly once.
- [ ] A coarse next item is surfaced rather than skipped.
- [ ] Reading crosses a real Section boundary correctly.
- [ ] A saved checkpoint is reused after a new client/session or production MCP restart when identity is unchanged.
- [ ] Stale checkpoint behavior is explicit when identity changed.

Do not tag `v0.1.0` until these checks and the original master release gates are satisfied by the exact production SHA.

## 6. Current execution order

Codex MUST verify current repository state first, then follow this logical order unless the current repository proves an earlier phase is already merged and accepted:

```text
Open Reading Profile convergence
→ Structure continuation design + implementation
→ Discovery continuation design + implementation
→ whole-document sentence-reading composition/evidence
→ saved-TextLocator restart/resume evidence
→ Streamable HTTP full lifecycle E2E
→ hardening/docs/Issue reconciliation
→ reproducible production deployment assets + rollback
→ final main release gate
→ deploy exact main SHA
→ production ChatGPT one-item / multi-Section / resume acceptance
→ v0.1.0 tag + GitHub Release
```

Interactive one-item behavior is primarily an Agent/product acceptance requirement; do not move AI state into the MCP kernel merely to test it.

## 7. Stop conditions

Codex must stop the current implementation phase and return to bounded design/review if any of the following is discovered:

- canonical facts cannot prove the required cross-Section order;
- a proposed change would silently reinterpret TextLocator/cursor identity;
- a new source-addressing or segmentation rule would require a version migration;
- a parser-output change would invalidate Parsed Cache semantics without a normalization-version decision;
- whole-document completion cannot be proved compositionally from structure completion + per-Section source-preserving completion + reliability evidence;
- the only proposed solution is a new Tool but no independent responsibility/use case has been demonstrated;
- production deployment requires guessing secret values, service paths, state paths, or tunnel configuration.

## 8. Post-v0.1 boundary

Do not pull these into the launch path without concrete blocker evidence:

```text
Figure / Caption first-class identity
Footnote / Endnote first-class identity
MathML / equation identity
inline code/link/emphasis semantic identity
nested/leaf BlockQuote/ListItem Sentence identity
OCR / scanned PDF
browser rendering
vector/general semantic RAG
AI QA/notes inside Reading MCP
user progress database inside Reading MCP
```

They remain valid future reading improvements, but they are not required to prove the current v0.1 sentence-reading goal.

## 9. Direct Codex instruction

Use this exact operating contract:

```text
Repository: liqiangcc/reading-mcp

Goal:
Finish Reading MCP v0.1 development, deploy the exact reviewed main commit to the production
systemd + Secure MCP Tunnel topology, complete real ChatGPT acceptance, and release v0.1.0.

Start here:
docs/CODEX_EXECUTION.md

Before modifying anything, read completely:
- docs/codex-development-production-rollout-plan.md
- docs/codex-sentence-reading-closure-amendment.md

Then re-read current main, open PRs, CI, Issues, and deployment state. Do not assume historical
snapshot SHAs/statuses remain current.

Follow the phase order. Where the documents require design-first, create/review/merge the bounded
design PR before implementation. For cross-Section sentence reading, first prove current canonical
Section/source-order facts are sufficient; if not, stop and design the missing contract rather than
guessing.

Use short-lived branches/PRs. Never develop directly on main. Merge only the latest head after
Format + Clippy + full Test are green and the diff has been independently reviewed.

Keep the seven-Tool surface unless a separately reviewed use-case-first decision proves a new Tool
is necessary. Never fuzzy-rebase locators/cursors. Never commit secrets/private document content.
Never deploy a feature branch.

The release is incomplete until production acceptance proves:
- multi-Section source-preserving sentence-first traversal;
- saved TextLocator resume across restart/session;
- one-item-at-a-time interaction where follow-up questions do not advance progress;
- explicit next/continue advances exactly once;
- coarse source items are not skipped;
- existing search/read/context/reliability/traceability gates;
- rollback readiness.

Tag v0.1.0 only after the exact deployed main SHA passes all automated and external acceptance.

At completion report:
- released SHA/tag;
- merged PRs and design decisions;
- final CI evidence;
- production service/tunnel verification;
- ChatGPT multi-Section/resume/one-item acceptance evidence;
- deferred non-blocking items;
- rollback target.
```

## 10. Final rule

After this entrypoint is merged, do not keep expanding the global design merely to make it feel more complete.

From here forward:

```text
execute the plan
→ make only bounded source-first design decisions when implementation evidence requires them
→ implement
→ prove
→ deploy
→ externally accept
→ release
```
