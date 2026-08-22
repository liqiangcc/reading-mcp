# Reading MCP — Codex Development, Release, and Production Rollout Plan

> Status: execution plan / release source of truth
>
> Target: finish the accepted v0.1 development scope, deploy the production Secure MCP Tunnel service, complete external ChatGPT acceptance, and cut `v0.1.0`
>
> Snapshot date: 2026-08-23
>
> Repository: `liqiangcc/reading-mcp`
>
> Important: the snapshot below is evidence from the repository at the time this document was written. Codex MUST re-read `main`, open PRs, CI, Issues, and deployment state before each phase instead of treating SHA/status values as permanently current.

---

## 1. Mission

The remaining work is no longer “add more reading features until the repository feels complete”. The goal is to converge the already accepted use-case-first design into one production-ready release and put that exact release online.

The required end state is:

```text
accepted reading Use Cases
        ↓
implemented capabilities
        ↓
7 stable MCP Tools
        ↓
all continuation / reliability claims actionable
        ↓
full local + HTTP + tunnel acceptance
        ↓
reproducible production deployment
        ↓
ChatGPT external acceptance
        ↓
v0.1.0 tag + GitHub Release
```

The launch target remains a **single-user / trusted deployment**. Do not silently turn v0.1 into a public multi-tenant document service.

---

## 2. Definition of Done

Development and launch are complete only when all of the following are true:

1. Active Open Reading Profile work is merged with green Format / Clippy / full Test gates.
2. The accepted use-case-first contracts no longer contain a release-blocking “truncated but impossible to continue” workflow for structure or discovery.
3. Streamable HTTP has a real full reading lifecycle E2E, not only transport/security probes.
4. Runtime/design/release docs describe the actual seven-Tool runtime and current identity versions.
5. Stale GitHub Issues are reconciled with current implementation evidence instead of being mechanically followed.
6. Production deployment can be recreated from repository-controlled scripts/unit templates without depending on undocumented shell history.
7. No secret, control-plane key, Bearer token, or private path content is committed.
8. Production runs the exact reviewed `main` commit through a supervised systemd service and Secure MCP Tunnel.
9. Production restart preserves persistent state and previously opened document identity where the current contract promises it.
10. External ChatGPT acceptance discovers exactly the current seven Tools and completes representative reading workflows.
11. A rollback path is proven before the release is considered complete.
12. The production commit is tagged `v0.1.0` and the GitHub Release records the acceptance evidence.

A green unit test suite alone is **not** production completion. A running tunnel alone is also **not** production completion.

---

## 3. Current Repository Baseline

### 3.1 `main`

At this snapshot, the mainline has already merged the major precise-reading foundation through PR #35.

Relevant merged sequence includes:

```text
PR #26  canonical Paragraph/Sentence lexical index
PR #27  EPUB navigation map
PR #28  EPUB structure reconciliation
PR #29  normalized native block model
PR #30  persisted EPUB structure validator
PR #31  block-aware identity design
PR #32  block-aware TextUnit identity migration
PR #33  block-aware context evidence correction
PR #34  TextLocator-anchored TextUnit enumeration
PR #35  Open Reading Profile design / ADR 0006
```

The `main` baseline used to create the current Open Reading Profile implementation branch was:

```text
2a5e3f189d790955f1bad821eff783a0ed6378dc
```

Codex must re-check the actual current `main` before doing any work.

### 3.2 Current runtime Tool surface

The accepted/current runtime surface is seven Tools:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

Do not add format-specific Tools merely to complete deployment or acceptance.

### 3.3 Current precise identity / derived-state versions

Current design/runtime contracts are based on:

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

These versions are part of correctness. Do not change them as a side effect of deployment work.

Any future change that redefines source addressing, segmentation, persisted parser output, cursor claims, or persistent lexical semantics must be explicitly versioned and justified before implementation.

### 3.4 Active PR #36 — immediate blocker

Active work at this snapshot:

```text
PR #36
feat/open-reading-profile
head: 129963fe460dcb784da46d33db31b8cd33ee45c9
```

The feature adds:

```text
open_document
→ reading-profile/v1
   ├── capabilities
   ├── canonical_text_coverage
   └── reliability
```

Latest observed CI:

```text
CI #969
Format  failure
Clippy  skipped
Test    skipped
```

The failure is rustfmt-only at the observed head. This means **the implementation has not yet earned a behavioral green gate**. Codex must not treat PR #36 as functionally accepted until a later head passes all three gates.

### 3.5 Existing release/hardening evidence

The repository already has substantial release hardening:

- local/public-HTTPS source policy separation;
- SSRF and redirect validation;
- input/resource budgets;
- bounded MCP responses;
- persistent Raw/Parsed cache;
- SQLite canonical Document persistence;
- SQLite derived TextUnit and lexical indexes;
- stable MCP errors with retryability;
- structured stderr telemetry without document-body/secret logging;
- stdio E2E;
- multiple format acceptance suites;
- Streamable HTTP implementation and security tests;
- Secure MCP Tunnel smoke workflow.

Do not rewrite already proven foundations while converging release blockers.

### 3.6 Deployment assets that exist today

The repository currently contains:

```text
deploy-tunnel.sh
.github/workflows/deploy-tunnel.yml
.env.example
```

The GitHub Actions tunnel workflow explicitly describes itself as a **temporary cloud smoke test**. It is not production deployment.

README/runtime docs say production is handled by a local systemd service such as:

```text
reading-mcp-tunnel.service
```

but the repository currently does not contain the corresponding systemd unit or a complete idempotent production install/upgrade/rollback path. This is a real reproducibility gap and must be closed before “online” is considered complete.

### 3.7 Open Issues that must be reconciled, not blindly followed

#### Issue #9

`phase: converge Reading MCP hardening and quality improvements`

It still contains older checklist state.

Important examples:

- CJK search is listed as incomplete, but canonical CJK lexical behavior was later implemented by the lexical TextUnit index work.
- full HTTP reading lifecycle E2E still needs explicit verification/closure.
- relaxed fallback search/snippet improvements are P1 quality work, not automatically release blockers.
- parser isolation requires a focused evidence review rather than an unbounded refactor.

Codex must update the Issue according to actual current evidence.

#### Issue #3

`External acceptance: validate Reading MCP in ChatGPT via Secure MCP Tunnel`

Its old acceptance list refers to the historical five-Tool runtime and older transport configuration. Current runtime has seven Tools and precise TextLocator/TextUnit workflows.

Before final external acceptance, update the Issue/runbook to the current contract. Do not declare failure just because the old checklist is stale.

---

## 4. Architecture and Scope Invariants

These are release constraints, not suggestions.

### 4.1 Use-case-first remains normative

For every remaining feature gap:

```text
User / Agent Goal
        ↓
Use Case
        ↓
Success / Failure / Degradation
        ↓
Required Capability
        ↓
Interaction / State Machine
        ↓
MCP Tool Contract
```

Forbidden reasoning:

```text
existing Tool
→ add convenient parameters
→ invent a scenario afterward
```

### 4.2 Source truth remains separate from derived state

```text
Document / Section / persisted normalized facts
= source truth

TextUnitIndex / SearchIndex
= rebuildable derived state
```

Index rows, snippets, scores, cursor offsets, or lexical tokens never become canonical citation identity.

### 4.3 Source locator and progress cursor remain separate

```text
TextLocator = canonical source address
Cursor      = progress inside one bounded stream
```

Never expose a cursor as a citation locator.

### 4.4 Format evidence must not leak into Application as concrete parser types

Keep:

```text
application → neutral Port / neutral evidence model
parsing      → format-specific implementation
```

Do not make `OpenDocumentUseCase` parse EPUB metadata JSON or import validator implementation types.

### 4.5 MCP is an adapter

MCP handlers may map/validate DTOs and invoke use cases. They must not become the place where:

- EPUB is validated;
- SQLite is queried directly;
- source freshness is guessed;
- search ranking is implemented;
- deployment policy is embedded.

### 4.6 Seven Tools are not a numerical target, but no new Tool is currently justified

Release work must not create:

```text
get_epub_coverage
read_pdf_range
get_sentences
get_paragraphs
deploy_server
```

A new Tool requires a new independent use-case decision.

---

## 5. Release Priority Model

Use these priorities throughout the Codex run.

### P0 — must complete before production release

1. PR #36 Open Reading Profile convergence and merge.
2. Actionable continuation for oversized structure.
3. Actionable continuation for bounded document discovery.
4. Full Streamable HTTP MCP reading lifecycle E2E.
5. Mainline docs/runtime contract alignment.
6. Reproducible production systemd/tunnel deployment artifacts.
7. Final local release gate.
8. Production deployment + rollback preparation.
9. External ChatGPT acceptance.
10. Tag + GitHub Release from the production-proven commit.

### P1 — review before release; fix only when evidence says blocker

- relaxed lexical fallback strategy;
- snippet quality improvement;
- synchronous parser/CPU isolation review;
- PDF/archive isolation refinements beyond existing budgets;
- deployment hardening beyond the minimum safe single-user topology.

P1 work must not cause uncontrolled scope expansion.

### Post-v0.1 — explicitly not launch blockers

- OCR / scanned PDF;
- browser rendering for JS-heavy sites;
- Confluence/Notion/Feishu/Yuque product APIs;
- OAuth/Cookie interactive login;
- public multi-tenant hosting;
- general crawler;
- AI summary/QA/notes inside Reading MCP;
- vector database/general RAG;
- Sentence SQLite persistence without performance evidence;
- nested/leaf block identity beyond current persisted evidence;
- speculative ranking/tokenizer changes;
- caller-selectable historical segmentation;
- fuzzy locator rebasing.

---

## 6. Master Execution Graph

Codex should execute the release line in this order:

```text
Phase 0  converge PR #36
   ↓
Phase 1  StructureCursor / actionable structure continuation
   ↓
Phase 2  DiscoveryCursor / actionable list continuation
   ↓
Phase 3  Streamable HTTP full reading lifecycle E2E
   ↓
Phase 4  release-hardening review + stale Issue/doc reconciliation
   ↓
Phase 5  production deployment assets + rollback path
   ↓
Phase 6  final release-candidate gate on main
   ↓
Phase 7  deploy exact main SHA to production systemd + Secure MCP Tunnel
   ↓
Phase 8  external ChatGPT acceptance
   ↓
Phase 9  tag v0.1.0 + GitHub Release + close/reconcile Issues
```

Do not deploy a feature branch. Do not tag before external acceptance.

---

# Phase 0 — Converge and Merge PR #36

## Goal

Finish the accepted Open Reading Profile implementation without changing its approved responsibility.

## Starting evidence

Observed latest failure is rustfmt-only. First action is therefore to fetch the exact latest CI result for the actual current PR head.

## Required execution

1. Fetch PR #36 metadata and current head SHA.
2. Fetch latest workflow run for that head.
3. If Format still fails, apply **only** current rustfmt output first.
4. Let the new head run CI.
5. When Clippy becomes reachable, fix warnings by improving code rather than suppressing warnings.
6. When tests become reachable, fix failures source-first; do not weaken assertions merely to make CI green.
7. Review the final diff for responsibility leaks.
8. Verify branch is `behind=0` against current `main`.
9. Merge only when latest head has:

```text
Format  success
Clippy  success
Test    success
```

## Specific review points

Confirm the implementation preserves:

```text
reading-profile/v1
canonical normalized-text coverage != publication coverage
non-EPUB reliability = not_applicable, not fabricated valid
EPUB reliability from persisted canonical evidence
no ZIP/DOM re-open solely for profile projection
no source-identity version change
no new Tool
```

Confirm production runtime injects the real reliability inspector while Application stays format-neutral.

## Stop conditions

Do not merge if any of these occur:

- profile counters do not partition canonical text exactly;
- EPUB source gap is hidden by canonical-text completeness;
- malformed required EPUB validator evidence degrades to `not_applicable`;
- a parser/cache normalization bump is introduced without newly persisted parser facts;
- Tool count changes;
- CI success belongs to an older head.

## Output

Merged PR #36 and updated `main` SHA.

---

# Phase 1 — Actionable Structure Continuation

## Why this is P0

The accepted UC-STRUCTURE-03 says an oversized structure must be eventually inspectable. Current `get_document_structure` has a server cap of 1,000 nodes and `truncated=true`, but no cursor/continuation path.

This is a semantic gap:

```text
bounded response
without continuation
≠
actionable bounded response
```

A 20,000-Section parser limit combined with a 1,000-node response cap makes this observable in valid documents.

## Design first

Create a bounded design branch, for example:

```text
design/structure-continuation
```

Do not start by adding `cursor` to the DTO. Derive the state machine from the use case.

## Required use-case semantics

### Goal

The Agent can inspect the requested full structure/subtree under response limits with no hidden node loss or duplication.

### Required success state

```text
complete = true
next_cursor = null
```

means the requested structural enumeration is exhausted.

### Required continuation properties

Cursor must bind at least the identity/shape required to prevent cross-stream continuation, including the effective document identity and requested structural scope.

The exact claim set must be designed from current normalized identity and structure semantics, but should cover the equivalent of:

```text
document / normalized identity
root_section_id or whole-document scope
max_depth / traversal mode
canonical ordering contract
next structural position
cursor schema version
```

Changing scope on continuation must fail closed.

### Request evolution

Prefer additive fields compatible with the accepted Tool design:

```text
document_id
root_section_id?
max_depth?
max_nodes?
cursor?
```

A cursor request must not redefine incompatible scope parameters.

### Response evolution

```text
document_id
sections[]
complete
next_cursor?
truncated        # retained for compatibility; define mapping carefully
```

Do not return Paragraph/Sentence TextUnits from structure continuation.

### Ordering

Use one deterministic canonical structural order and test exact gap/overlap properties.

## Implementation branch

After design merge:

```text
feat/structure-continuation
```

## Acceptance tests

At minimum:

- >1,000 root Sections: first page + continuation(s) return every Section exactly once;
- deep tree at `max_depth` boundary;
- subtree-root continuation;
- terminal complete/cursor consistency;
- stale normalized/document identity fails closed;
- wrong root/depth/mode cursor mismatch fails closed;
- tampered/impossible cursor fails closed;
- stdio real MCP continuation;
- old `max_depth` request remains valid;
- Tool count remains seven.

## Output

Actionable structure pagination merged to `main` with green CI.

---

# Phase 2 — Actionable Document Discovery Continuation

## Why this is P0

`list_documents` is optional for a client that already knows a source, but once the Tool is used its bounded semantics must be truthful.

Current behavior uses `max_results` but does not let the caller prove whether the configured discovery scope was exhausted.

Accepted UC-DISCOVER-01 requires bounded deterministic pagination.

## Design branch

```text
design/discovery-continuation
```

## Required semantics

Discovery must remain **discovery only**:

```text
list files/sources
≠
open/parse/index documents
```

Cursor should bind the effective discovery scope and deterministic ordering, e.g. equivalent facts for:

```text
configured/root scope
requested path
recursive flag
ordering contract
next position
cursor schema version
```

Do not put document content/hash identity into a cursor before a source is opened.

## Request/response direction

Additively evolve toward:

```text
request:
  path?
  recursive
  max_results
  cursor?

response:
  documents[]
  complete
  next_cursor?
```

## Important filesystem safety rule

Cursor continuation must not weaken canonical root authorization. Every continuation still operates under current configured allowed roots.

Decide fail-closed semantics if filesystem contents change between pages. Do not silently claim snapshot semantics unless a real snapshot exists.

A conservative restart/stale policy is preferable to fabricated continuity.

## Acceptance tests

- more files than page limit;
- every eligible file exactly once in deterministic order;
- recursive and non-recursive scopes isolated;
- requested path outside root blocked on first/continued request;
- cursor scope mismatch rejected;
- malformed cursor rejected;
- empty configured roots returns empty complete result;
- stdio continuation;
- no parsing/repository side effects;
- Tool count remains seven.

## Output

Actionable discovery pagination merged to `main` with green CI.

---

# Phase 3 — Full Streamable HTTP Reading Lifecycle E2E

## Why this is P0

Issue #9 still calls out the missing final HTTP E2E. Existing HTTP security probes prove authentication/Host/Origin/loopback boundaries, but production-quality transport acceptance should prove the actual reading contract through Streamable HTTP.

## Scope

This phase is **test/convergence work**, not a transport redesign.

Do not weaken:

```text
loopback-only bind
Bearer token minimum strength
Host validation
Origin validation
SSRF/source policy
```

## Required E2E

Start the real `reading-mcp-http` process with an isolated state/local-root fixture and execute the equivalent of:

```text
initialize
→ tools/list
→ list_documents
→ open_document
→ inspect reading_profile
→ get_document_structure
→ get_text_units
→ search_document
→ get_context
→ read_document
```

Use current seven Tool names.

The test should prove at least one direct TextLocator handoff through HTTP, not only legacy Section IDs.

## Security assertions

Keep/extend evidence that:

- missing/invalid Bearer token is rejected;
- invalid Host is rejected;
- invalid Origin is rejected when present;
- server refuses non-loopback bind;
- successful MCP body is not emitted before auth/Host/Origin checks.

## Output

Issue #9 P0 HTTP/MCP transport acceptance can be closed with direct CI evidence.

---

# Phase 4 — Release Hardening Review and State Reconciliation

## Goal

Before writing deployment scripts, make repository documentation and open Issues represent **current runtime facts**.

This is not a feature phase.

## 4.1 Reconcile Issue #9

Read current code/tests before editing the Issue.

Expected reconciliation work:

- mark CJK behavior complete if current lexical-tokenizer/index tests still prove it;
- mark final HTTP E2E complete only after Phase 3 merges;
- review relaxed fallback search/snippet items against actual user/release evidence;
- do not implement P1 search tuning merely because an old checkbox exists;
- perform a focused parser-isolation review and record the decision/evidence.

### Parser isolation review

Inspect where synchronous CPU-heavy parser work executes and what current budgets already constrain.

Questions:

1. Can one allowed PDF/archive operation block the Tokio runtime long enough to violate service availability despite current limits?
2. Are expensive operations already bounded by page/entry/bytes limits?
3. Is `spawn_blocking`/worker isolation needed now, or would it add complexity without a demonstrated release risk?
4. Do existing E2E/load-style tests expose an actual problem?

If no release blocker is found, document the evidence and defer. Do not create a large async refactor by default.

## 4.2 Reconcile Issue #3 / external acceptance runbook

Update old five-Tool expectations to the actual seven Tool surface.

Current acceptance must include precise-reading behavior, not only the historical coarse loop.

## 4.3 Align docs

At minimum review:

```text
README.md
docs/README.md
docs/requirements.md
docs/tool-contract-use-case-design.md
docs/phase6-mcp-stdio.md
docs/runtime-configuration.md
docs/release-hardening-plan.md
docs/mvp-review.md
docs/chatgpt-acceptance.md  # if present/current
docs/adr/0006-open-reading-profile.md
```

Known drift to correct includes README sections that still enumerate the historical six/five-Tool candidate surface.

Do not rewrite old ADR history to pretend the final state existed earlier; distinguish historical baseline from current implemented state.

## Output

Repository and Issues give Codex/operator one non-contradictory picture of what v0.1 actually contains.

---

# Phase 5 — Make Production Deployment Reproducible

## Goal

Convert the current locally known production approach into repository-controlled, auditable deployment artifacts.

Current README says production uses systemd + Secure MCP Tunnel, but the unit/install path is not fully reproducible from the repository.

## 5.1 Production topology for v0.1

Preferred launch topology:

```text
ChatGPT
  ↓
OpenAI Secure MCP Tunnel
  ↓
tunnel-client
  ↓
reading-mcp stdio binary
  ↓
local persistent state + explicitly allowed document roots
```

This matches the existing tunnel investment and does not require exposing Reading MCP directly on a public network interface.

Keep Streamable HTTP as a supported/tested transport, but do not introduce an additional public reverse-proxy deployment requirement just to launch v0.1.

## 5.2 Discover actual server state first

Codex running on the deployment host must inspect, not guess:

```text
current checkout/path
current binary
current systemd service/unit
current tunnel-client location/version
current profile directory
current state directory
current allowed local roots
current service user
current active tunnel/profile
```

Never print secret values while inspecting environment files.

If an existing manually-created systemd unit is present, compare it with the repository plan before replacing it.

## 5.3 Repository artifacts to add

Prefer a small explicit deployment area, for example:

```text
deploy/systemd/reading-mcp-tunnel.service
scripts/install-production.sh
scripts/deploy-production.sh
scripts/verify-production.sh
docs/production-deployment.md
```

Exact names may vary, but responsibilities must remain separate.

### systemd unit responsibilities

The unit should define only service supervision/composition, such as:

```text
WorkingDirectory
EnvironmentFile
ExecStart
Restart policy
startup timeout
safe signal/stop behavior
```

Do not embed secrets or a real API key/tunnel credential in the unit.

Security hardening options may be added only after verifying they do not block required read-only document roots or writable state paths.

### install script responsibilities

Install prerequisites/templates and validate required tools/paths. It should be idempotent.

It must not manufacture a `CONTROL_PLANE_API_KEY` or commit one.

### deploy/update script responsibilities

A production update should be atomic enough to roll back.

Recommended shape:

```text
fetch exact reviewed commit/tag
→ cargo build --release --locked --bin reading-mcp
→ run local release smoke/gates as appropriate
→ stage versioned binary/release directory
→ switch current binary/symlink atomically
→ systemctl daemon-reload if unit changed
→ restart service
→ verify
```

Avoid “git pull && cargo build in the live ExecStart path with no rollback copy”.

### verify script responsibilities

Verify observable state without leaking secrets:

```text
systemctl is-active
systemctl status --no-pager
recent journal errors
binary exists/is executable
tunnel-client doctor/profile connectivity
persistent state path writable by service
configured local roots readable by service
```

If the tunnel protocol exposes a safe discovery/diagnostic probe, use it. Do not add a new Reading MCP admin Tool solely for health checks.

## 5.4 Fix hardcoded environment assumptions

Review current `deploy-tunnel.sh` and `.env.example` for deployment-specific defaults.

Production scripts should prefer explicit environment/configuration over repository-hardcoded real tunnel IDs or `/root`-only assumptions.

Backward compatibility with the current server can be retained through defaults only when they are clearly safe and documented; otherwise parameterize them.

## 5.5 State safety

Production deploy must not delete:

```text
reading-mcp.sqlite
raw cache
parsed cache
```

unless an explicit migration requires it.

If a release requires derived-index invalidation, rebuild derived state according to versioned application contracts rather than deleting canonical Documents.

## 5.6 Rollback design

Before first production update, preserve:

```text
previous working binary/release path
previous unit/config template
previous exact git SHA/tag
persistent state backup strategy
```

Rollback procedure must be documented and executable without rebuilding the failed new commit.

## Output

A fresh operator/Codex session can reproduce the production service from repository instructions plus deployment secrets/configuration.

---

# Phase 6 — Final Release Candidate Gate on `main`

## Preconditions

All P0 development PRs are merged. No feature branch is the candidate.

## 6.1 Re-read repository state

Verify:

- no unexpected open release-blocker PR;
- planned P0 PRs merged;
- `main` is current;
- Issues/docs reconciled;
- deployment artifacts merged.

## 6.2 Mandatory source gates

Run exactly:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

Then build production binaries:

```bash
cargo build --release --locked --bins
```

Do not tag if CI/local results refer to different commits.

## 6.3 Required acceptance matrix

Confirm current tests prove:

- seven Tool discovery;
- Open Reading Profile;
- normalized hash v2 / segmentation v2;
- block-aware Paragraph/Sentence evidence;
- no fabricated Sentence for coarse quote/list/pre/table regions;
- TextUnit forward/backward/boundary/anchor continuation;
- structure continuation;
- discovery continuation;
- locator-driven context;
- exact read + ReadCursor;
- SearchHit locator direct handoff;
- CJK/technical lexical retrieval;
- SQLite reopen/rebuild behavior;
- EPUB nav/reconciliation/block/validator evidence;
- stale locator/cursor fail-closed;
- stdio E2E;
- Streamable HTTP full lifecycle E2E;
- architecture-boundary tests;
- no body/secret telemetry logging.

## 6.4 Release diff review

Review changed files from the last known release candidate baseline.

Reject accidental:

```text
target/
secrets
.env
private document files
server-specific absolute paths
temporary debugging workflows
CI logs/artifacts committed as files
```

## 6.5 Freeze production candidate SHA

Record one exact SHA:

```text
RELEASE_CANDIDATE_SHA=<main sha>
```

From this point until external acceptance finishes, do not deploy “whatever main happens to be later”. Deploy this exact candidate.

---

# Phase 7 — Production Deployment

## Goal

Run the exact release-candidate SHA on the real deployment server through the supervised Secure MCP Tunnel service.

## 7.1 Pre-deploy snapshot

Record without secrets:

```text
old deployed SHA/version
old binary checksum
service active state
state directory path and backup/checkpoint status
tunnel-client version/profile
current local root list (paths only if non-sensitive)
```

## 7.2 Backup / rollback point

Before replacing the binary:

- keep the previous executable/release directory;
- preserve or checkpoint the SQLite/state directory according to the deployment procedure;
- preserve previous unit file if it is being changed;
- verify rollback command path.

## 7.3 Install exact candidate

Build/install from the recorded SHA, not an unpinned branch head.

Use locked dependencies.

## 7.4 Restart supervised service

Expected sequence is equivalent to:

```bash
systemctl daemon-reload   # only when unit changed
systemctl restart reading-mcp-tunnel.service
systemctl is-active reading-mcp-tunnel.service
```

Use the actual service name discovered/standardized by Phase 5.

## 7.5 Local deployment verification

Verify:

- service stays active rather than crash-looping;
- tunnel-client doctor succeeds;
- no authentication/profile initialization error;
- Reading MCP binary starts under the service user;
- state directory remains available;
- allowed local roots are readable;
- logs do not contain secrets/document bodies;
- previous persisted document state survives restart where promised.

## 7.6 Failure rule

If any production verification fails:

```text
stop rollout
→ collect bounded diagnostic evidence
→ rollback previous binary/unit
→ prove old service restored
→ fix via repository branch + CI
→ redeploy only a new reviewed SHA
```

Do not live-edit production source/binary and leave the fix uncommitted.

---

# Phase 8 — External ChatGPT Acceptance Through Secure MCP Tunnel

## Goal

Prove the real product client can use the exact production deployment, not merely that `tunnel-client doctor` is green.

## 8.1 Refresh the acceptance checklist first

Issue #3/runbook must reflect current runtime.

Expected Tool list:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

No extra admin/deployment Tool should appear.

## 8.2 Required production workflows

### A. Discovery/open/profile

```text
list_documents (when local roots are configured)
→ open_document
→ inspect reading_profile
```

Confirm the profile distinguishes canonical text coverage from format/publication reliability.

### B. Structure workflow

```text
open
→ get_document_structure
→ continue when bounded
→ select Section
```

Use a fixture/source that can exercise continuation where practical.

### C. Sentence-first reading

```text
Section
→ get_text_units(sentence, preserve_source)
→ continuation
→ TextLocator
→ read_document / get_context
```

Verify coarse non-prose/structural regions are not fabricated as Sentences.

### D. Search handoff

```text
search_document
→ SearchHit.text_locator
→ read_document
→ get_context
→ optional get_text_units(anchor_locator)
```

No snippet copy/re-search should be required.

### E. EPUB reliability

Use one representative EPUB and verify:

- structure provenance is truthful;
- native Paragraph precision where supported;
- coarse structural/non-prose handling;
- validator/reliability summary appears without inventing confidence.

### F. PDF traceability

Verify returned evidence retains page/native location information.

### G. Content-is-data safety

Use a document containing instruction-like text and confirm ChatGPT treats it as document content, not Tool/system instructions.

## 8.3 Acceptance evidence to record

Record in Issue #3 (without secrets/private content):

```text
production SHA
production service state
Tool discovery result
representative workflows passed
known degradations/non-goals
acceptance date
```

Do not paste private document body or credentials into the Issue.

## Output

External acceptance is complete for the same SHA deployed in Phase 7.

---

# Phase 9 — Tag and GitHub Release

## Preconditions

Only proceed if:

- release-candidate SHA is still the deployed production SHA;
- final main CI is green;
- production service is healthy;
- external ChatGPT acceptance passed;
- no P0 blocker remains open.

## 9.1 Version

`Cargo.toml` is already `0.1.0` at this snapshot. Revalidate before tagging.

Do not bump versions speculatively if the target remains `v0.1.0`.

## 9.2 Tag

Create annotated/reviewable release tag from the exact proven SHA:

```text
v0.1.0
```

Never tag a different later `main` commit merely because it is newer.

## 9.3 GitHub Release notes

Release notes should summarize the actual capability set, including:

- seven MCP Tools;
- deterministic Paragraph/Sentence TextUnit identity;
- TextLocator direct handoff;
- continuation semantics;
- canonical lexical search;
- EPUB navigation/reconciliation/block/validator reliability;
- Open Reading Profile;
- persistent state/search;
- stdio + Streamable HTTP transports;
- Secure MCP Tunnel deployment path;
- security/resource-budget boundaries;
- explicit v0.1 non-goals.

Include the release gate and external acceptance reference.

## 9.4 Issue closure/reconciliation

Close or update Issues #9/#3 only according to evidence.

Leave deliberately deferred P1/post-v0.1 work as explicit future work rather than fake completion.

---

## 10. Codex Branch / PR Discipline

Codex must use short-lived branches.

Preferred pattern:

```text
design/<bounded-decision>   # when semantics/state machine need a decision
feat/<bounded-capability>   # implementation
fix/<bounded-defect>        # discovered correctness issue
docs/<bounded-doc-update>   # docs-only alignment
```

Rules:

1. Never develop directly on `main`.
2. One primary use case / change reason per implementation PR.
3. Rebase/update against `main` only when necessary; never force-rewrite shared history casually.
4. Every PR gets independent diff review before merge.
5. Merge only the latest CI-green head.
6. If CI exposes a formatting patch, apply formatting before reasoning about later skipped gates.
7. If Clippy exposes unused/incorrect structure, prefer real correction over `allow` suppression.
8. If tests fail because assumptions were wrong, reread parser/domain/application source first.
9. Do not weaken source-first invariants to preserve a proposed implementation.
10. After merge, start the next bounded phase from updated `main`.

---

## 11. Codex Evidence Rules

For each phase, Codex should leave a compact evidence trail in PR body/comments/docs:

```text
Goal
Source-first findings
Accepted semantics
Changed responsibilities
Explicit non-goals
Tests / acceptance evidence
Final CI run + head SHA
Compare against main (ahead/behind)
```

Avoid vague statements such as “works now” or “all good”.

For identity/cursor/deployment claims, state exactly what was proven.

---

## 12. Deployment Secret Rules

Never commit:

```text
CONTROL_PLANE_API_KEY
real Bearer tokens
private auth-profile tokens
/root/.env contents
private document content
SSH private keys
session cookies
```

`.env.example` may contain placeholders only.

Scripts may read an `EnvironmentFile`, but must not echo it.

CI should use GitHub Secrets only for temporary tunnel smoke tests.

Production secrets remain on the deployment host / approved secret provider.

---

## 13. Rollback Contract

A production rollout without rollback is incomplete.

Minimum rollback evidence:

1. identify previous known-good release SHA/binary;
2. preserve it before switch;
3. preserve persistent state or a safe checkpoint;
4. know exact `systemctl`/symlink command needed to restore it;
5. verify old service becomes healthy after rollback.

A failed new version must not force rebuilding the old version from the network during an incident.

---

## 14. What Codex Must Not Do During This Plan

Do not:

- redesign the entire architecture because release work touched multiple layers;
- add AI summarization/QA to Reading MCP;
- introduce vector search “for production quality” without a new use case;
- add fuzzy locator repair;
- silently reinterpret historical locators/cursors;
- make BlockQuote/ListItem nested Sentence claims without stronger persisted evidence;
- make GitHub-hosted Actions the production runtime;
- expose `0.0.0.0` to simplify remote access;
- move production secrets into GitHub files;
- delete persistent canonical state to fix a derived-index version mismatch;
- mark stale Issues complete without checking current source/tests;
- tag/release before the exact production SHA passes external acceptance.

---

## 15. Master Checklist

### Development convergence

- [ ] Re-read current `main`, PR #36, open Issues, CI.
- [ ] Fix PR #36 latest-head formatting/compile/test findings.
- [ ] Independent review PR #36.
- [ ] PR #36 CI green.
- [ ] Merge PR #36.
- [ ] Design Structure continuation from UC-STRUCTURE-03.
- [ ] Implement StructureCursor/actionable structure pagination.
- [ ] Structure continuation CI + stdio acceptance green.
- [ ] Merge structure continuation.
- [ ] Design discovery continuation from UC-DISCOVER-01.
- [ ] Implement DiscoveryCursor/actionable listing pagination.
- [ ] Discovery continuation CI + stdio acceptance green.
- [ ] Merge discovery continuation.
- [ ] Add Streamable HTTP full reading lifecycle E2E.
- [ ] HTTP E2E CI green and merge.

### Hardening/reconciliation

- [ ] Reconcile Issue #9 with current evidence.
- [ ] Perform bounded parser-isolation release review.
- [ ] Decide/document P1 search fallback/snippet scope.
- [ ] Update Issue #3 acceptance contract to seven Tools/current flows.
- [ ] Align README and docs with current runtime/profile/continuation behavior.
- [ ] Confirm release-hardening and MVP-review documents no longer contradict implemented HTTP/precise-reading state.

### Deployment reproducibility

- [ ] Inspect actual production host state without printing secrets.
- [ ] Add repository-owned systemd unit template.
- [ ] Add idempotent install/deploy/verify scripts.
- [ ] Remove/parameterize unsafe hardcoded deployment assumptions.
- [ ] Document state directories, local roots, tunnel profile, service user.
- [ ] Document exact rollback procedure.
- [ ] Deployment artifact PR CI/review green and merged.

### Final release candidate

- [ ] `cargo fmt --all -- --check`.
- [ ] `cargo clippy --locked --all-targets --all-features -- -D warnings`.
- [ ] `cargo test --locked --all-features`.
- [ ] `cargo build --release --locked --bins`.
- [ ] stdio acceptance green.
- [ ] Streamable HTTP lifecycle acceptance green.
- [ ] architecture boundary tests green.
- [ ] no secrets/build artifacts/temp workflows in diff.
- [ ] freeze exact `RELEASE_CANDIDATE_SHA`.

### Production rollout

- [ ] Record old deployment SHA/binary/service state.
- [ ] Preserve rollback binary/config/state checkpoint.
- [ ] Deploy exact candidate SHA.
- [ ] Restart systemd service.
- [ ] Service remains active.
- [ ] tunnel-client doctor succeeds.
- [ ] persistent state survives.
- [ ] no secret/body leakage in logs.

### External acceptance

- [ ] ChatGPT connects to production tunnel.
- [ ] Tools/list returns exactly seven current Tools.
- [ ] discovery/open/profile flow passes.
- [ ] structure + continuation flow passes.
- [ ] Sentence-first TextUnit continuation passes.
- [ ] SearchHit → read/context direct handoff passes.
- [ ] EPUB reliability flow passes.
- [ ] PDF traceability passes.
- [ ] instruction-like document content remains data.
- [ ] acceptance evidence recorded in Issue #3.

### Release

- [ ] production SHA == accepted SHA == tag target.
- [ ] create `v0.1.0` tag.
- [ ] create GitHub Release with scope/gates/non-goals.
- [ ] reconcile/close release Issues according to evidence.
- [ ] record rollback target for the released version.

---

## 16. Direct Codex Execution Instructions

The following is the intended operating contract when this document is handed to Codex:

```text
Repository: liqiangcc/reading-mcp

Goal:
Finish the remaining accepted v0.1 development work, converge all release blockers,
make production deployment reproducible, deploy the exact reviewed main commit through
the existing Secure MCP Tunnel topology, perform real ChatGPT acceptance, and release v0.1.0.

Authoritative plan:
docs/codex-development-production-rollout-plan.md

Rules:
1. Read the plan and current repository state before modifying anything.
2. Do not assume the snapshot SHAs/statuses are still current; verify GitHub first.
3. Continue the active PR #36 first. Do not abandon/reimplement it on a new branch unless
   current Git history proves that is necessary.
4. Work use-case-first. For new continuation semantics, design the state machine before DTO code.
5. Use short-lived branches and PRs. Do not develop directly on main.
6. Merge only latest-head CI green: Format + Clippy + full Test.
7. Preserve the seven-Tool boundary unless a separately reviewed use case proves otherwise.
8. Preserve source identity/cursor/index version boundaries; never fuzzy-rebase old locators.
9. Do not commit secrets or private document content.
10. GitHub Actions tunnel workflow is smoke-only; production runs on the deployment server.
11. Before deployment, discover existing server/service/tunnel/state paths rather than guessing.
12. Add repository-owned systemd/install/deploy/verify/rollback artifacts before final rollout.
13. Never deploy a feature branch. Freeze and deploy one exact main SHA.
14. On production failure, roll back first, then fix via Git branch + CI.
15. External acceptance must use the actual production tunnel and current seven-Tool contract.
16. Tag v0.1.0 only after the deployed SHA passes ChatGPT acceptance.
17. Keep P1/post-v0.1 items out of the launch path unless concrete acceptance evidence makes
    one a blocker.

Execution order:
PR #36
→ Structure continuation
→ Discovery continuation
→ HTTP full lifecycle E2E
→ hardening/docs/Issue reconciliation
→ production deployment assets
→ final main gate
→ production deploy
→ ChatGPT acceptance
→ v0.1.0 tag/release

At the end, report:
- final released SHA/tag;
- merged PRs;
- final CI run evidence;
- production service/tunnel verification;
- external acceptance evidence;
- deferred non-blocking items;
- rollback target.
```

---

## 17. Final Principle

The final convergence rule is:

```text
Do not maximize feature count.
Maximize truthful, resumable, reproducible reading and deployment guarantees.
```

For v0.1, a smaller scope that can be completely read, safely continued, reproducibly deployed, externally verified, and rolled back is stronger than a larger feature set with hidden gaps.
