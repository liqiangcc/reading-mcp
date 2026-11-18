# PDF-7a: the PyMuPDF layout backend on the 121-paper corpus

Measurement only. The question PDF-6a left open was whether line and paragraph reconstruction
should come from **A** — a Rust positional text layer inside the default backend — or from
**B** — the PyMuPDF layout backend (`READING_MCP_PDF_LAYOUT_PYTHON`, PyMuPDF / PyMuPDF4LLM /
Layout 1.28.2, `docs/pdf-layout-deployment.md`), which the production host does not run today.
PDF-6a measured the default backend; this note measures B on the same corpus, the same measures
and the same 600 MB cap, plus what it costs.

Runs, all on `scripts/pdf-quality.py` extended with `--backend`:

| Run | Branch / head | Workflow run | What it is |
| --- | --- | --- | --- |
| default | `measure/pdf7a-layout-backend` `ca39a50` | [44259190443](https://github.com/liqiangcc/reading-mcp/actions/runs/44259190443) | `backend=default`, verifies the harness changed nothing |
| layout | `measure/pdf7a-layout-backend` `ca39a50` | [44260714472](https://github.com/liqiangcc/reading-mcp/actions/runs/44260714472) | run A: the layout backend as shipped |
| ceiling | `measure/pdf7a-ceiling` `cfcdc7e` | [44268641717](https://github.com/liqiangcc/reading-mcp/actions/runs/44268641717) | run B: as shipped plus one measurement-only commit that accepts a mixed text-and-raster page without OCR |

The default side of every comparison below is PDF-6d's final run
[44251624748](https://github.com/liqiangcc/reading-mcp/actions/runs/44251624748), which is what
`main` has now (PDF-6d changed the numbers slightly; the PDF-6a baseline `44236511100` is in §6
for reference). "Paired" means both columns are measured on the same papers — the papers the
run and the default run both opened `ok`.

## What the harness measures that earlier runs did not

- Peak resident set is the **whole process tree** (server + Python worker), the larger of a
  50 ms `/proc` sampler and the kernel's `ru_maxrss` for the process the harness starts. PDF-1a
  reported the server alone and said so.
- A document the `MemoryMax=600M` scope kills is its own outcome (`cap_killed`), counted
  through the cgroup's `oom_kill` counter — including kills of the worker while the server
  survives and reports a parse failure.
- The layout worker's refusal reason is read from the worker source the binary embeds, wrapped
  at `require_disabled_page_coverage`, because the server maps worker `OCR_REQUIRED` to
  `OcrRequiredDetail::unmeasured` and does not carry the reason (PDF-4a).

The ceiling run is a one-commit patch on `measure/pdf7a-ceiling` (never merged): inside the
worker, a page that `has_native_body` is allowed to keep its raster regions as visual regions
instead of refusing. It reproduces what PDF-1a's wrapper did past
`require_disabled_page_coverage`. The run itself is the check that the patch took effect:
papers run A refused with `mixed page contains raster…` open `ok` in run B.

## 1. Outcomes

| Outcome | default | layout (run A) | ceiling (run B) |
| --- | --- | --- | --- |
| `ok` | 121 | 14 | 41 |
| `ocr_required` | 0 | 93 | 5 |
| `cap_killed` (the 600 MB cap) | 0 | 13 | 13 |
| `parse_failed` | 0 | 1 | 62 |
| `timeout` | 0 | 0 | 0 |

Refusal reasons (run A, measured per page): **90** papers `mixed page contains raster content
without source coverage` — the ordinary text page carrying a figure, the dominant case PDF-1a
saw on a smaller corpus — and **3** `image-only page requires local OCR inspection` (true
scans). No `native image coverage cannot be proven`.

The ceiling removes the mixed-page refusals as designed (the 5 remaining `ocr_required` are all
`image-only page`), and 31 papers run A refused now open `ok`. But it exposes a second wall:
**62 of 121 papers die with `PDF layout failed: 'NoneType' object is not iterable`** inside
`pymupdf_layout` on mixed pages the policy change lets through. This is not deterministic —
four papers that opened `ok` in run A crashed this way in run B — so the count is
run-dependent, not a stable 62.

The 13 `cap_killed` are the same papers in both runs: the OOM counter fires inside the 600 MB
scope. Ten of them are papers of 260+ pages; three are small papers (12–37 pages) whose worker
still did not fit. The `parse_failed` in run A (1 paper, 2403.04803, 9 pages) is the same
`'NoneType'` crash, rare when few mixed pages are reached.

## 2. Paired text quality (run A vs default, 14 papers)

| Measure | default | layout |
| --- | --- | --- |
| Units > 800 chars | 127 | 33 |
| Papers above 5% long units | 4 | **0** |
| Units < 5 chars | 708 | 690 |
| `heading_number` units | 58 | 53 |
| `page_number` units | 88 | 196 |
| One-character `.` units | 497 | 350 |
| Paragraphs per page (median) | 1.19 | **12.68** |
| Header/footer occurrences (80% / 50% tier) | 5 | 5 |
| …own unit / prefix / inside / glued | 0 / 0 / 0 / 0 | **4 / 0 / 0 / 0** |
| Whitespace ratio (median) | 1.312 | 0.984 |
| Papers below 0.7 of reference whitespace | 0 | 0 |
| 5-gram containment median / p05 | 0.968 / 0.839 | **0.918 / 0.786** |
| Papers below 0.9 containment | 3 | 6 |

## 3. Paired text quality (run B ceiling vs default, 41 papers)

| Measure | default | ceiling |
| --- | --- | --- |
| Units > 800 chars | 298 | 49 |
| Papers above 5% long units | 13 | **0** |
| `heading_number` units | 223 | **61** |
| `page_number` units | 211 | 516 |
| Paragraphs per page (median) | 1.17 | **11.33** |
| Header/footer occurrences | 101 | 84 |
| …own unit / prefix / inside / glued | 1 / 57 / 15 / 20 | **82 / 0 / 0 / 0** |
| Whitespace ratio (median) | 1.150 | 0.979 |
| Papers below 0.7 whitespace | 1 | **0** |
| 5-gram containment median / p05 | 0.975 / 0.867 | **0.921 / 0.789** |
| Papers below 0.9 containment | 6 | 15 |

## 4. Reading

Where the layout backend opens a paper, its text is real progress on the PDF-6a failures:
paragraphs are actual paragraphs (≈11–12 per page, against ≈1.2), running headers arrive as
units of their own (share 0.80/0.98 against 0.00/0.01 — the fix for "header glued to body"),
papers with >5% long units go to zero, and `heading_number` units drop 4× in run B's larger
population. Containment does **not** improve: it drops ~0.05 below the default in both runs,
which on these papers is figure-captions, tables and footnotes the layout keeps out of the
body stream. PDF-1a's 0.866-vs-0.923 containment gap still holds at corpus scale.

But the corpus is not the 14 (or 41) papers it opens:

- As shipped it opens **14 of 121 (11.6%)**. The blocker is the mixed-page refusal — 90 of 93
  refusals — which is deliberate policy (no OCR and no provable native coverage means no
  partial publish), not a defect.
- The ceiling shows the policy is not the only blocker. Changing it still yields **41 of 121
  (33.9%)**: the remaining losses are a real instability in `pymupdf_layout` itself (the
  nondeterministic `'NoneType' object is not iterable`, 62 papers) plus 13 cap kills at 600 MB
  plus 5 true image-only scans.
- Cost is real and persistent: ~420 MiB median tree, up to ~645 MiB before the cap kills,
  against the default's 114/307 MiB; ~5 s median, up to 115 s, against 0.04/0.44 s. PDF-1a's
  400–540 MB and 4–9 s per open still hold.

## 5. Decision criteria (user-approved 2026-11-18)

The user chooses B only if it meets **all** rows. `ok ≤ 30 p` means open-time p95 computed over
papers the run actually opened; `attempted` counts every open the corpus asked for. The
containment clause needs the median not below default − 0.01.

| Criterion | Target | layout (run A) | ceiling (run B) | Pass |
| --- | --- | --- | --- | --- |
| Opens ≥ 95% of 121 without refusing | ≥ 114.95 | 14 (11.6%) | 41 (33.9%) | **fail / fail** |
| — of which: needs the mixed-page refusal policy changed | — | yes: 90 of 93 refusals are mixed-page; the change itself needs the user's approval | yes — the ceiling is that change, and it still opens only 34% because `pymupdf_layout` then crashes on 62 | — |
| Clearly better on ≥ 3 of 4 paired measures | 3 of 4 | better: long>5% (4→0), headers own-unit (0.00→0.80), heading_number (58→53); whitespace below-0.7 ties (0→0) | better: long>5% (13→0), headers own-unit (0.010→0.976), heading_number (223→61); whitespace (1→0) | 3 of 4 / 4 of 4 |
| — and containment median not below default − 0.01 | ≥ 0.958 / ≥ 0.965 | 0.918 (−0.050) | 0.921 (−0.054) | **fail / fail** |
| Peak tree RSS p95 ≤ 600 MB, max ≤ 800 MB | p95 ≤ 600, max ≤ 800 | p95 600.0 (all opens) / 434 (ok only); max 645 | p95 600.0 / 556; max 649 | pass (at the cap) / pass |
| Open p95 ≤ 15 s for papers ≤ 30 pages | ≤ 15 s | attempted 31.5 s; ok-only 8.4 s | attempted 11.3 s; ok-only 15.8 s | borderline **fail / borderline** |
| Layout environment ≤ 1 GB on disk | ≤ 1 GB | 426 MiB (`du -sh`, 433,160,097 B) | 426 MiB | pass / pass |

Verdict under the criteria: **B fails** — rows 1, 2 and (run A) 4 — so by the user's rule the
choice is A. Notably the ceiling run shows B fails even after removing the refusal policy the
measurement was built to isolate: the failure is then the upstream library's own crashes, not
only the policy.

Licence, stated not scored: PyMuPDF / MuPDF and PyMuPDF4LLM / Layout are AGPL / commercial;
the repository's MIT licence does not re-licence them and no commercial licence was purchased
(`docs/pdf-layout-deployment.md`). That is the user's call.

## 6. Cost of each option on the production host

The host has 1.9 GB of RAM and no Python layout environment today.

- **Option A (Rust positional layer).** No new dependency, no new memory class — the default
  backend's measured peak is 114 MiB median / 307.5 MiB p95 across this corpus, well inside
  the cap and the host. The cost is engineering: paragraph/line reconstruction, header removal
  and heading-number joining in Rust on top of `page.get_text` spans, i.e. the fixes ranked
  3, 4 and 6 in PDF-6a.
- **Option B (layout backend).** Needs a 426 MiB pinned venv, a worker process that itself
  runs ~420 MiB median (several hundred MB above the server) and would kill 13 of 121 papers
  at the existing 600 MB cap — on a 1.9 GB host a single open plus the venv and the rest of
  the service is viable only with headroom to spare and no concurrency. Opens are ~5 s median,
  ~2–3 orders of magnitude slower than the default. It would also need the mixed-page refusal
  policy changed (the ceiling commit shows what that buys and at what yield), and the upstream
  `'NoneType'` crash would have to be chased or tolerated, since it is inside PyMuPDF Layout
  1.28.2 and flaky.

### Reference: PDF-6a baseline (44236511100, pre-PDF-6d)

| Outcome | default |
| --- | --- |
| `ok` | 121 |

Key paired numbers are nearly identical to PDF-6d's (papers above 5% long units 61 vs 58,
`heading_number` 1,236 vs 1,248, headers own-unit share 0.0284, containment median 0.982).
The harness's byte-identical check for `backend=default` held: every per-document record
matches `44236511100`'s except timings and the order of ties (hash-order dependent); the
final default run on this branch is [44259190443](https://github.com/liqiangcc/reading-mcp/actions/runs/44259190443).
