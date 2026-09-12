# #95 Stage A/B evidence

Observed 2026-09-12 UTC. This document records discovery, not OCR acceptance.
No source text, source images, engine output, credentials, or private environment
contents are included. Commands below were read-only except isolated design
worktree creation and the subsequent documentation commit/push/PR.

## Authority and mandatory reads

- Actually retrieved full #95 body/comments with `gh issue view 95 --json ...`
  and the exact Coordinator comment with
  `gh api repos/liqiangcc/reading-mcp/issues/comments/5643642213`.
  [Issue is OPEN](https://github.com/liqiangcc/reading-mcp/issues/95);
  [comment](https://github.com/liqiangcc/reading-mcp/issues/95#issuecomment-5643642213)
  created/updated 2026-09-12T05:07:00Z. New assignment explicitly limits this turn
  to A/B and supersedes old withdrawal context.
- Also actually retrieved and read [static review 5643665196](https://github.com/liqiangcc/reading-mcp/issues/95#issuecomment-5643665196),
  timestamp 2026-09-12T05:10:27Z. Its 41-file main delta, Chinese/short-page
  heuristics, 12-line split, mocked-only evidence and child-tree cleanup gaps are
  reflected in design sections 2/4/6/7. These are static risks, not claimed new
  failures reproduced on real samples.
- Checked `/AGENTS.md`, `/root/AGENTS.md`, workspace root and tracked main/worktree
  files: no applicable AGENTS.md found. No additional agent instructions discovered.
- Read `docs/CODEX_EXECUTION.md` fully, plus its two full mandated specifications:
  rollout plan (1,525 lines) and sentence closure amendment (650 lines). Read
  `docs/release-process.md` (263), `docs/package-process.md` (218), and
  `docs/production-deployment.md` (207) completely. Re-read truncated output in
  bounded ranges. Verified these files unchanged between initial checkout and
  fetched live main. Also read candidate PDF layout deployment/dependency rules,
  main CI workflow and external ChatGPT acceptance runbook; inspected deploy
  checkpoint logic. Older seven-tool and local-build examples are superseded by
  current nine-tool/package protocol and explicit user restrictions.

## Live repository and candidate identities

| Item | Observation |
| --- | --- |
| main | `de4306a3b486a8aee69cf159dcf2c1f545d1e732` via GitHub API and fetched origin/main |
| Initial checkout | `66e0c9540e468f531131d926fc02256709eede00`, feat/issue-69-named-section-boundary, clean, behind tracking branch 2 |
| Design base | exact main above; isolated `/tmp/reading-mcp-95-design-20260912`, branch `design/issue-95-scanned-pdf` |
| PR96 | OPEN DRAFT, main base; head `bf6454b9d12460224274716c2dae4529c59be588` |
| PR96 ancestry vs main | 4 candidate-only commits, 1 main-only commit; candidate-only layout plus older-branch #92 plus OCR adapter |
| PR96 own delta | production `1569c68` to `bf6454b`: 11 files, +349/-28; not same as PR's entire main comparison |
| Main version/identity | Cargo 0.3.0; normalization v8, segmentation v2, normalized hash v2 |
| Production-source identity versions | normalization v9, segmentation v3, hash v2 |
| PR96 identity proposal | normalization v10, segmentation v3, layout worker/cache v2 |
| Latest formal Release | v0.3.0 -> `1cfbc4ca032a8bab7b59134a9b03c2526a34b7f4` |
| Existing prerelease | v0.4.0-rc.1 -> `2f31f95c8f16b8c64ee015254bed6864c2a97ddb` |

[PR96](https://github.com/liqiangcc/reading-mcp/pull/96) has successful historical
hosted CI [run 34447988509](https://github.com/liqiangcc/reading-mcp/actions/runs/34447988509)
and [run 34447970014](https://github.com/liqiangcc/reading-mcp/actions/runs/34447970014)
on its recorded head. Those checks do not prove real OCR generation, current
design, main integration, deployment, or private-paper accuracy.

Source inspected: application open_document saves repository before separately
updating indexes; current cache key binds raw bytes/normalization but not an OCR
engine; normalized hash v2 binds sections/blocks but not arbitrary metadata or
original page-map digest. PR96's hidden text/body heuristics include English
word-count assumptions. These are design inputs, not accepted implementation.

## Old executors: non-interference/conflict check

Read tmux session metadata, recent terminal state, descendant process trees and
git status. No keys, interrupts, messages, new prompts, or tasks were sent.

- `r-89`: pane PID 774847, existing Codex process 774865 remained alive but the
  terminal was at the user input prompt after its completed nine-tool comparison.
  Repeated CPU-time observation stayed `00:10:52`; no cargo/build/deploy child.
  `/root/reading-mcp-issue89` clean at `2f31f95...`; original cwd also clean.
  This is an idle existing session, not a killed/terminated session.
- `reading-mcp-issue-95-fix`: pane PID 1571568, `pstree` showed only bash, terminal
  back at shell after `run-actions.sh`. Independent source checkout under its
  private task directory clean, HEAD `bf6454b9d12460224274716c2dae4529c59be588`.
  No running old OCR implementation/build/deployment child observed.
- Old task outputs were only inventoried; private prepared-layout JSON and paper
  content were not copied into this worktree. No old branch/worktree was removed.

This proves no active conflicting task **at observation time**, not a lock against
future user activity. Recheck before implementation/deployment if time has passed.

## Live deployment and capacity

Read `systemctl show`, symlink target/hash, non-secret PDF drop-in, dependency
versions, `free`, `df`, `nproc`, and package policy. Did not restart service,
change config, install packages, run OCR/tests, or touch canonical state.

| Item | Live observation |
| --- | --- |
| Service | reading-mcp-tunnel.service, active/running, MainPID 987247, user root |
| Working directory | /root/reading-mcp |
| ExecStart | /root/.local/bin/tunnel-client run --profile-dir /root/.config/tunnel-client --profile reading-mcp |
| Secret file | /root/.env (contents not emitted) |
| Binary symlink | /root/reading-mcp/target/release/reading-mcp -> reading-mcp-1569c68d6220d129beb1f3218cd22d57f26f331c |
| Actual binary SHA256 | `1d56567c7c766a665de0f2287947dcf20bac652f51d660086e5f8c8810191ca6` |
| PDF interpreter | /opt/reading-mcp/pdf-layout-1.28.2/bin/python |
| Python packages | PyMuPDF, pymupdf4llm, pymupdf-layout all 1.28.2 |
| Source-view pixel configuration | 16000000 |
| Tunnel client | 0.0.14+0f870e50a973fa820d4c409000059e181e8d242b |
| State present | /root/.reading-mcp, 580 MiB; SQLite/WAL/SHM and cache present |
| PDF dependency directory | 293 MiB |
| Host | Ubuntu 24.04.4 LTS, x86_64 package target, 2 CPUs |
| Memory snapshot | 1966 MiB total, 868 MiB available |
| Swap snapshot | 4095 MiB total, 3670 MiB used |
| Filesystem | 40 GiB total, 36 GiB used, 1.8 GiB available, 96% used; /tmp shares it |
| OCR engine in PATH | absent; apt says tesseract-ocr/libtesseract5/liblept5 and eng/chi-sim data not installed |
| Apt candidates | Tesseract/libtesseract 5.3.4-1build5; Leptonica 1.82.0-3build4; language packages 1:4.1.0-2 |

The executable identity matches the historical #92 candidate, not main. This
phase verified the installed file and service metadata, not a fresh connector
health/accuracy test. No actual connector timeout measurement is available.
Resource admission in design currently fails. Disk/memory remediation and a
consistent migration rollback snapshot are deployment blockers, not permission
to delete any existing task/canonical data.

## Prior paper evidence: context only

Issue comments report the four-page original scan and existing-layer projection
failure; this phase did not rerun the private workload. Historical local original
hash: `d26997baf588222109d32545604a2a2ed400dc769a21fd49a5acdc4a955396ae`.
Recompute it from the authorized original before later local acceptance. A few
footer characters/page are not proof of native body coverage. Existing-layer
projection failure is distinct from engine transcription quality.

Historical exploratory pilot outputs are not approved gold or hosted acceptance.
In particular, an earlier signed-number annotation had an unresolved transcription
error; do not reuse its reported accuracy. New original synthetic corpus and
independently reviewed gold are required. No private text is included here.

## Stage ledger / unproven gates

| Stage | Status |
| --- | --- |
| A live discovery / old-task checks / full normative reads | Done as recorded above |
| B design/prompt/evidence | Proposed in this docs-only PR; Coordinator review pending |
| Fixture byte/font/gold SHA freeze | Not done; fixture-only reviewed PR required before OCR implementation/tuning |
| Dependency complete hash lock | Not done; approved implementation/package gate |
| Connector deadline / cold four-page performance | Unmeasured; synchronous hypothesis only |
| Layout/OCR implementation / integration | Not started by this executor |
| New-head compilation/tests/CI | No local compilation/tests; any auto-triggered PR CI is hosted and reported separately |
| Merge / Release / Package / Deploy | Not performed |
| Production scan / accuracy / final Coordinator acceptance | Not performed |

Design changes are restricted to these three Markdown files. Check `git diff
--check`, inspect staged paths, commit/push this isolated branch, create a design
PR against main, and report its exact head. Those publication identifiers belong
in the PR/terminal handoff, not a fabricated self-referential commit hash here.
