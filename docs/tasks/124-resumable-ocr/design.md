# #124 resumable OCR ingestion — bounded per-page checkpoint design

Status: **implemented for review** (PR on `fix/ocr-resumable-checkpoint`).
Parent tracking: #95. Baseline: v0.4.4 / merge `4ae68f2`.

## Problem

`open_document` wraps PDF ingestion in a 60s deadline (90s whole-open).
The worker processes the whole document in one invocation: layout parse,
then per-required-page work at a shared per-page bound
(`PAGE_UNIT_SECONDS`, 45s — see "Page unit bound" below). Each required
page is one bounded unit — raster, visual-model classification, regional
OCR, projection, checkpoint — so a resumed invocation only ever pays model
cost for the pages it completes that round (an upfront whole-document
model pass would burn the retry budget before the first unfinished page
and stall progress forever). The page raster is prepared once per unit and
shared by the model child and the OCR stage, so pixels are charged to the
raster budget exactly once. A multi-page scanned PDF whose per-page OCR
cost × required pages exceeds 60s fails with typed retryable `OCR_TIMEOUT`
and all completed page work is discarded — the next call restarts from
zero. Real `naturebp.pdf` (4 scanned pages) exceeds the budget on current
host performance.

## Contract (separation of concerns)

The engine still only recognizes pixels. All orchestration — budget,
checkpoint identity, persistence, resume decisions, invalidation — stays in
reading-mcp (Rust side). The worker remains read-only inside its existing
sandbox; it merely *reports* completed page results and *accepts* verified
prior results. No writable paths are added to the unit; `ProtectSystem`,
`RootDirectory`, `/tmp`+`/run` tmpfs and `validate_host` are unchanged.

### Protocol additions (opt-in, backward compatible)

The worker input gains an optional single-line JSON header preceding the PDF
bytes on stdin. The header is only consumed when OCR is enabled and the first
stream byte is `{`; raw-PDF callers (smoke scripts, native path) are
unchanged. Header schema `ocr-worker-input/v1`:

- `pages`: map of page number → `ocr-page-checkpoint/v1` payload to resume.
- `budget_seconds`: optional soft invocation deadline. The worker folds it
  into `page_time_remaining()` so expiry raises the existing typed
  `OCR_TIMEOUT` *between/inside* page work — a clean typed failure rather
  than a mid-write kill. The hard Rust deadline/cgroup kill remains the
  backstop; nothing ever continues after it.
- `max_pages`: optional per-invocation quota of *newly computed* OCR pages.
  Reaching it raises the same typed `OCR_TIMEOUT` after the last completed
  page's checkpoint is emitted. Replayed pages are nearly free and do not
  consume the quota; it makes partial progress deterministic (no wall-clock
  calibration) and is what the hosted F05 resume test uses.

When (and only when) a valid header is present, the worker emits flushed
single-line JSON records on stdout before the final result line:

- `ocr-required-pages/v1` — required OCR page numbers, emitted once after
  layout parse, before page work.
- `ocr-page-checkpoint/v1` — after each required page finishes its full
  OCR+projection step:
  - fresh page: `{schema, page, original_sha256, runtime_identity_sha256,
    page_raster_sha256, boxes, ocr_retry_diagnostic, observation}`.
  - resumed page: `{schema, page, resumed: true}` — progress signal only;
    the stored payload is not retransmitted.
- The final line is the existing result or failure JSON — unchanged.

### Per-page checkpoint content

For each required page the checkpoint stores exactly the state needed to
replay the page without calling the engine or the model child:

- `boxes` — the page-layout boxes after the OCR step (what
  `_regional_ocr` produced, or the originals when it returned none).
- `ocr_retry_diagnostic` — the `ocr-regional-observations/v3` record.
- `observation` — the visual-model observation (present only when the model
  ran; its `raster_sha256` binds it to the page pixels).
- `page_raster_sha256` — sha256 of the raster `_regional_ocr` consumed.

On resume the worker re-rasterizes the page (cheap, same
`OCR_CONFIG['dpi']` pipeline), compares `page_raster_sha256`, counts the
page against `RASTER_BUDGET.require_page` (the 8-required-page limit still
applies), applies stored `boxes`/`diagnostic`, then re-runs the *pure*
visual-projection step against the stored observation — every existing
complete/incomplete/fatal decision is recomputed, not trusted. A raster or
schema mismatch discards that entry and recomputes the page; an identity
mismatch can never reach this point because the Rust-side key already binds
the pair.

### Rust-side checkpoint store

`FileOcrCheckpointStore` under `state_dir/ocr-checkpoints/<key>/`:

- `key = sha256("ocr-checkpoint/v1" | original_sha256 |
  runtime_identity.sha256 | PDF_LAYOUT_CACHE_NAMESPACE)` — binds raw source,
  runtime/config/engine/model fingerprints (identity already covers
  retry/inspection policy and dependency hashes), and the projection/
  normalization namespace.
- `meta.json` records the binding tuple; `page-NN.json` files are written
  via tmp+rename (atomic). Load validates every entry; invalid entries are
  ignored, so corruption degrades to recompute, never wrong data.
- Concurrent writers (single-flight is per-parser) merge by union of page
  files — no whole-file clobber.

### Streaming persistence (the cancellation gap)

Today `parse()` reads stdout to EOF; on timeout the future is dropped and
emitted-but-unread lines would be lost. Instead, when the resumable path is
active the parser spawns a reader task that owns stdout: it validates and
persists each `ocr-page-checkpoint/v1` line as it arrives, tracks
`required_pages`, and returns the final payload over a oneshot at EOF. If
`parse()` is cancelled, the task still drains the already-buffered pipe
(the write end closing does not discard buffered data) and exits. The
parser keeps the task handle; the next `parse()` call awaits any pending
drain before loading the store — resume is deterministic, and at most the
in-flight page is ever redone.

### Outcome handling

- success → delete the checkpoint directory for that key.
- definitive failure (`OCR_FAILED`, `OCR_RESOURCE_LIMIT`,
  `OCR_NO_SUPPORTED_PROJECTION`, `OCR_UNAVAILABLE`, parse errors) → delete:
  deterministic failures must not masquerade as resumable progress.
- `OCR_TIMEOUT` / cancellation → retain: the next identical call resumes.

`document.metadata["ocr_resume"]` records `{"computed":[...],
"resumed":[...]}` on the final canonical document — observable proof that
resumed pages did not re-run the engine.

## Page unit bound

`PAGE_UNIT_SECONDS` bounds one page's whole unit of work — the visual-model
child call and the engine passes share the same `PAGE_DEADLINE`. It was
raised from 15s to 45s: with durable checkpoints the invocation deadline is
the real total budget, while the page bound only terminates genuinely
pathological single-page work. 45s stays below the 50s invocation soft
deadline used by `open_document`, so a page unit still cannot outlive its
invocation. The bound remains hard — the engine subprocess gets
`timeout=page_time_remaining()` and an overrun maps to typed `OCR_TIMEOUT`.

## Regional retry for unresolved components

A whole-page psm-6 retry can merge a conflicted region into a
page-spanning block, leaving zero candidates contained in the component's
`effective_roi` (observed on `naturebp` page 3: the retry produced a single
page-wide box). Each still-unresolved component gets one bounded regional
re-observation: the page raster is re-rendered clipped to the component's
`effective_roi` (pixel-quantized, charged to the raster budget, engine
subprocess on the shared page deadline), and the resulting psm-6 boxes are
appended to the same `retry` attempt. The component's `effective_roi` is
updated to the actually-searched rectangle (still ⊇ `roi`, ⊆ page bounds),
and the unchanged coverage rule — all primary word centers inside
candidate line boxes — decides resolution. Everything is bounded: at most
8 regional passes per page, each guarded by `page_time_remaining()`, and a
component that still cannot be resolved keeps its
`candidate_does_not_cover_component` failure — identical fail-closed
semantics, strictly more candidate evidence. The Rust evidence schema and
validation needed no changes: regional boxes are ordinary `retry`-attempt
observations contained in `effective_roi`.

## What is preserved

- 8-required-page and raster-pixel limits, cgroup/unit kill semantics,
  single-flight admission — all unchanged.
- Native/mixed routing unchanged: pages that never required OCR never enter
  the checkpoint; `requires_ocr` classification is unchanged.
- No publication before completeness: checkpoints are internal intermediate
  state; the canonical document is only built from the worker's complete
  result. No second reading model, no new MCP tool.
- No remote/paid OCR; no fixture special-casing; the 60s budget is not
  raised (the optional soft deadline only stops work *earlier* and cleanly).

## Progress metadata

The retryable `OCR_TIMEOUT` error surface is unchanged (`code`,
`retryable`); the outer deadline generates it without worker context, so
error `data` does not carry page counts. Progress is instead durable in the
checkpoint store (`meta.json` + page files) and provable post-hoc via
`ocr_resume` metadata on the completed document.
