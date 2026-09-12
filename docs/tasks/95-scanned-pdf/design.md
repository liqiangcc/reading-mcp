# #95 scanned PDF — bounded local OCR design

Status: **proposed, design review only**. No runtime implementation, merge,
release, installation, or acceptance is claimed by this PR.

Business authority: [reopened #95](https://github.com/liqiangcc/reading-mcp/issues/95)
and [Coordinator design, 2026-09-12 05:07 UTC](https://github.com/liqiangcc/reading-mcp/issues/95#issuecomment-5643642213).
The subsequent direct assignment authorizes r1 to develop/deploy after design
approval; the old withdrawal and earlier no-deployment instructions are obsolete.
Coordinator owns business decisions and acceptance. This existing r1 session is
the sole new executor; no replacement session or delegated agent is created.

Review entrypoint: section 9 lists the decisions; sections 2, 6 and 7 cover the
integration risks, resource limits and fixed acceptance gates. This is a bounded
#95 supplement to the Coordinator baseline, not a new global design.
Also read and incorporate [Coordinator static review 5643665196](https://github.com/liqiangcc/reading-mcp/issues/95#issuecomment-5643665196).

## 中文决策与验收摘要

依据 [Coordinator 对 e037ca10 的裁决](https://github.com/liqiangcc/reading-mcp/pull/97#issuecomment-5643741913)
作本次有界修订；业务方向已认可，新 head 仍待最终设计批准，不提前实现。

- 已接受本地 Tesseract fast（`eng`/`chi_sim`/`eng+chi_sim`），保留既有
  layout/render 依赖与许可说明；不购买商业授权、不新增云 OCR、不上传私有正文。
- 已接受 60s ingestion / 90s whole-open 的待实测方案。四页冷启动必须符合
  实际 connector 时限并留余量；不符合时由 Coordinator 裁决架构，不重复问用户。
- 已接受 v11/hash-v3、明确的 PDF/EPUB reopen、operator revision 与
  `force_refresh` 分离。保留旧数据，不静默重绑。layout 先合主线，保留 #92，
  最终版本才部署。
- 存储复用原子 Document upsert，以 normalized hash v3 作 generation；先原子
  持久化 immutable OCR evidence blobs，再一次提交完整 Document 的引用/绑定/
  derivation。索引按 identity 校验、重建或显式拒绝，不建全局 generation 系统。
- 资源先在 hosted CI 实测峰值和解包大小，再最小化 staging，部署前计算空间、
  内存需求及明确安全余量。现状只是不满足候选保守门槛，尚不能证明无法完成。
- fixture 可放在实现分支首个独立、只含 fixture 的 commit，按精确 SHA 审查冻结；
  后续 commit 才加 OCR 实现。不强制另开 PR，不重复跑同一 head 的全库测试。
- clean synthetic 门槛保留：英文 CER ≤1%/WER ≤3%，中文及双语 CER ≤2%，
  支持正文覆盖 ≥99%、边界 F1 ≥0.95、顺序 1.0；原页绑定与精确读取 100%。
  详见第 7 节逐 fixture 规则，不把这些指标外推成真实论文准确率。
- 真实论文先在本机隔离状态证明一个自然句子的边界及原始页，再完成受支持范围
  遍历统计并回到实际 connector。缺口不能被 `source_complete` 掩盖；不替代
  paper-reading-lab 人工消费/READY 验收。

## 1. Goal, scope, and truth boundary

An authorized caller opens the **original** scanned PDF with `open_document`.
When the deployer enables the local engine, ingestion obtains readable canonical
prose, Paragraph/Sentence units, exact reads, and original-page evidence through
the existing nine tools. Default OCR is off. No new OCR-specific tool, cloud OCR,
private uploads, paid inference, AI reconstruction, or reading-progress storage.
OCR quality is measured on a frozen corpus, not guaranteed for arbitrary PDFs.
Exact read means exact canonical text, not glyph-perfect transcription.

Supported v1: horizontal English and simplified Chinese prose, upright pages,
single column and separable two-column layouts. Configure `eng`, `chi_sim`, or
`eng+chi_sim` explicitly; do not guess language or silently switch models.
Existing trustworthy native/OCR text is reused per page. Formula recognition,
handwriting, complex tables, vertical Chinese, and unprovable order remain
explicit unsupported/coarse regions, not invented sentences. Blank pages are
accounted for, not fabricated as text. A successful document may contain declared
visual/coarse regions, but no required OCR page may be silently dropped.

## 2. Mainline integration is a prerequisite, not a blind merge

The live identities in [evidence.md](evidence.md) show main has normalization v8 /
segmentation v2, production has v9 / v3, and PR #96 proposes v10 / v3. Production
already contains layout/source-view behavior not present on main.

After design approval, use short-lived branches from refreshed main:

1. A separately reviewed layout-integration PR ports the relevant delta from
   `389d861` and `2f31f95` (full identities recoverable from ancestry of recorded
   production SHA). Preserve opt-in routing, original-page renderer, pinned
   dependencies, v3 segmentation, cache separation, and its regression tests.
   Audit every file relative to main; do not import the historical RC version,
   stale release assertions, or unrelated branch history wholesale.
2. Keep main's #92 fix `de4306a3b486a8aee69cf159dcf2c1f545d1e732` and its tests.
   Production `1569c68` contains the equivalent older-branch fix: do not apply it
   twice. Assert backward/forward anchored completion and unanchored full-section
   completion independently, including terminal/empty/filtered cases.
3. PR #96 relative to main changes **41 files**. Review its OCR delta separately
   against production `1569c68`, currently 11 files,
   349 additions / 28 deletions. Reuse justified hidden-text detection and binding
   tests, not its whole branch. Its five-word English heuristic cannot define
   Chinese prose; hidden text is not confidence; its global v10 bump alone does
   not bind engine/config identity. No existing-layer-only fix closes #95.
4. Implement local OCR orchestration in a bounded follow-on PR from integrated
   main. Coordinate versions in section 5. Do not deploy the intermediate layout
   integration merely because it merged. Compare the final runtime to BOTH main
   and the deployed candidate. Keep #96 untouched until Coordinator accepts its
   replacement/disposition; this design does not close it.

Every merge-candidate implementation head needs hosted Format + Clippy + full
Test, real-engine OCR tests, and Coordinator/independent diff review. Reuse the
same exact-head evidence; fixture review alone does not require repeated full
suite runs. Old #96 CI does not approve a
new head. No feature branch may be the formal deployed source.

## 3. Local engine and dependency delivery

Propose Tesseract **5.3.4**, Ubuntu 24.04 package `5.3.4-1build5`, with
`libtesseract5` at the same package version and Leptonica `1.82.0-3build4`.
Use the fast LSTM `eng` and `chi_sim` data supplied by Ubuntu's
`tesseract-ocr-eng` / `tesseract-ocr-chi-sim` `1:4.1.0-2`; verify actual model
digests and provenance during the dependency-lock PR. Runtime uses OEM 1, PSM 3,
300 DPI; engine input/output is local raster plus TSV/hOCR, not a replacement
source PDF. Fast models favor the synchronous CPU budget; best models are not an
automatic retry/fallback. Changing models needs a new reviewed configuration and
identity, never threshold adjustment after inspecting results.

The upstream [model documentation](https://tesseract-ocr.github.io/tessdoc/Data-Files.html)
describes fast/best tradeoffs and LSTM-only operation; the
[CLI documentation](https://tesseract-ocr.github.io/tessdoc/Command-Line-Usage.html)
defines TSV/hOCR and word confidence. Tesseract and upstream fast data use
[Apache-2.0](https://github.com/tesseract-ocr/tesseract/blob/main/LICENSE),
[model license](https://github.com/tesseract-ocr/tessdata_fast/blob/main/LICENSE);
[Leptonica has its own BSD-style notice](https://github.com/DanBloomberg/leptonica/blob/master/leptonica-license.txt).
Retain distribution copyright files for every shipped dependency. Existing
PyMuPDF/PyMuPDF4LLM/Layout 1.28.2 remain required for layout/rendering, with their
AGPL/commercial terms; repository MIT does not relicense them. See
[existing upstream notices](https://pymupdf.readthedocs.io/en/latest/about.html).
This is dependency disclosure, not a legal compliance determination. Coordinator
has accepted retaining these existing dependencies and notices; this does not
purchase a commercial license or require the user to reconfirm existing paths.

Build dependency artifacts on GitHub-hosted `ubuntu-24.04`, never production or
self-hosted runners. Resolve the complete .deb dependency closure, Python wheels,
language files, and license notices into an immutable companion runtime archive.
Pin every package/wheel version AND SHA256; record engine executable, shared
libraries, model hashes, Python interpreter ABI, architecture, renderer/layout
versions, and package sources in an OCR dependency manifest. Refuse unresolved
dependencies or mutable downloads at runtime. The Python dependencies currently
listed in `scripts/pdf-layout/requirements.txt` on #96 are an input, not a complete
hash-locked/offline manifest. Validate the companion archive on a clean hosted
runner without access to production state. No production `pip install latest`,
apt upgrade, compilation, or model network fetch.

Install into a new versioned `/opt/reading-mcp/` directory, not the live 1.28.2
environment. Absolute engine/model/interpreter paths are deployer-owned config;
no caller-selected command line, arbitrary language path, or shell execution.
Package Issue records companion archive hash and every executable/model digest
alongside the server archive/binary identities. Missing/changed configured files
fail closed. Exact hashes are a later package gate, not invented in this design.

## 4. Ingestion and evidence contract

```text
SourcePolicy -> Retriever -> immutable original bytes/raw hash
  -> application ingestion coordinator
     -> neutral inspection Port: per-page evidence
     -> OCR Port only for required pages (bounded infrastructure worker)
     -> versioned evidence store
     -> projection Port -> canonical blocks/bindings/coverage
  -> validate all canonical facts -> atomic publication
  -> rebuildable TextUnit/FTS generations
```

The application sees typed format-neutral plan/results, not PyMuPDF JSON or
Tesseract internals. Parser/worker never retrieves a URL. Domain facts contain no
process launch, OCR orchestration, or MCP dependencies. Source policy remains
effective on reopen/cache hits; an OCR cache is not authorization.

Inspection assigns each page `native`, `existing_ocr`, `requires_ocr`, or
`unsupported`, plus region evidence and explicit blank/visual-only flags.
Use glyph validity, visibility, text/image geometry, body coverage and order;
paragraph absence or a repeated footer is insufficient evidence of readable
body text. Native text over a scanned body does not suppress required OCR.
Within a mixed page retain supported native regions and mask/exclude overlapping
regions from OCR projection; do not duplicate text. An existing OCR layer must
pass geometry/Unicode/order validation, not a minimum English word count alone.
Ambiguous classification remains unsupported with evidence; never manufacture
confidence from font, invisibility, character count, or successful extraction.

Persist `ocr-evidence/v1`: original hash, original 1-based page number, dimensions,
crop/rotation/raster-to-original transform, region bounds, ordered block/line/word
IDs, engine-native text and confidence (Tesseract scale 0..100; non-word -1 becomes
absent), origin `native|existing_ocr|local_ocr`, engine/config/model fingerprints,
and raw output hash. Existing layers commonly lack confidence: record null.
Do not reinterpret confidence as probability or accuracy. Provenance identifies
inferred order vs source/native order. Validate finite in-page coordinates,
monotone scalar ranges and complete transform accounting before publication.

Projection reconstructs prose paragraphs using engine block/line evidence and
layout regions. Preserve column order and explicit paragraph boundaries; no
whole-page prose fallback or cross-column join just to obtain sentences. Source
line-count caps (including #96's 12-line flush) must not create artificial
paragraph/sentence boundaries. Preserve continuation evidence across bounded
worker chunks; reject unprovable projection rather than cutting a natural
sentence to satisfy a buffer limit. Short/sparse pages must not be excluded by
the existing 8-line/5-word/160-point-width heuristics.
Source
range parts retain every contributing original page, including cross-page prose.
No fuzzy punctuation correction, LLM repair, or ungrounded dehyphenation. Preserve
the reviewed layout v3 rule for independently evidenced hyphen joins.

`get_source_view` renders immutable original bytes, never OCR-generated PDF or
OCR raster. Single-page targets must bind exactly; multi-page locators follow
the existing explicit unsupported-target contract rather than choosing a page.
Reliability separates canonical coverage from publication coverage. Every page
and region is accounted for as supported text, blank, coarse/visual, or unsupported.
Coarse figure/formula data cannot count toward supported prose/Sentence coverage.
`section_complete` retains #92 scope semantics, not an OCR-accuracy assertion.
If any required page times out/fails, return failure, not a partially published
successful document. Declared visual gaps remain visible to callers even when
all supported prose has been enumerated.

## 5. Identity, forced recognition, cache and migration

Keep original `ContentHash` and source-based `DocumentId` semantics unchanged.
Never substitute OCR bytes into their calculation. Define separately:

| Identity | Bound facts |
| --- | --- |
| raw hash | Original acquired PDF bytes |
| OCR lookup key | raw hash, ordered selected pages/regions, detector version, engine binary/library digests, model digests and ordered languages, DPI/OEM/PSM and preprocessing, worker protocol, deployment OCR revision |
| OCR output hash | Exact persisted engine output bytes plus validated provenance serialization |
| projection identity | OCR key/output hash, original page transforms/bindings, native evidence, projection version |
| normalized hash v3 | Canonical sections/blocks as v2, plus canonical typed derivation identity and original binding-map digest; explicit absent marker for non-derived inputs |
| segmentation identity | Reviewed layout segmentation v3, independently versioned from OCR and normalization |

Use canonical length-delimited encoding, sorted named fields and explicit schema
versions, not arbitrary metadata-map iteration. Retain source ordering for lists.
Cache key changes must miss even when OCR text happens to be identical. Merely
adding metadata to current normalized hash v2 is insufficient: v2 hashes sections
and blocks, not that metadata or bindings. New normalized hash v3 explicitly binds
these facts, so changed derivation/order/page mapping cannot reuse old locators.

Propose global normalization **v11**, hash **v3**, segmentation **v3** for the final
OCR integration; v10 is already used by the historical #96 candidate and must not
mean a different contract. Layout prerequisite may retain its reviewed v9/v3.
Use `pdf-ingestion/v1`, `ocr-evidence/v1`, `pdf-ocr-projection/v1` namespaces; do not
reuse #96's layout-v2 key for a different pipeline. New versions propagate through
repository guards, parsed cache, locator/cursor checks, index fingerprints, docs
and tests. Enumerate all affected guards in implementation review.

No new public force-OCR parameter in v1. Existing `force_refresh` keeps its source
retrieval semantics: refresh HTTP/local source, then reuse OCR if raw bytes and
OCR key are unchanged. Explicit re-recognition is a deployer change to bounded
`READING_MCP_OCR_REVISION` (default `1`), followed by reopen. It intentionally
invalidates the deployment's OCR namespace; new revision produces a different
normalized identity even for byte-identical text. It is not a secret cache purge.
Coordinator has accepted this operator-scoped choice; per-document caller
forcing would need an additive `open_document` contract design, not overloading
`force_refresh` or adding a tenth tool.

Persist immutable successful engine outputs across process restart. Single-flight
the full OCR key; concurrent opens await one result within their own deadlines.
No SQLite write transaction during detection/OCR. Reuse in-process supervision
and bounded per-source publication serialization for this single-runtime v1;
different OCR keys for the same Document must not race index replacement. Do not
introduce a distributed lease service or global task system. If concurrent
writers outside this runtime require stronger coordination, first demonstrate
the invariant the existing mechanism cannot preserve and return the concrete
case to Coordinator for a minimal change, rather than claiming distributed safety.
Cancellation of one waiter does not cancel work still needed by another; no
remaining waiters or global deadline cancels and reaps the worker. Failed jobs
never enter the successful OCR cache. Completed page outputs may be reused by a
retry, but never exposed as a partial canonical success. No background jobs API.

Canonical publication reuses the existing single-statement SQLite `document_json`
upsert in `DocumentRepository::save`; the complete Document is already atomic.
After validating projection, profile and units, atomically persist immutable,
content-addressed OCR evidence blobs (temporary file -> durable atomic publish).
Only then save references, bindings and derivation together in that complete
Document. A committed reference must never point to an unfinished/missing blob;
crash-before-upsert may leave an unreferenced blob but not a partial Document.
Use **normalized hash v3 itself as generation**, not a new global generation
schema or commit subsystem. TextUnit/FTS state remains derived and must match the
current canonical identity; reuse existing index fingerprints/validation and
rebuild or explicitly reject missing/mismatched state. Do not return stale hits
during a concurrent publication. Serialize publication/index replacement within
the bounded use case and revalidate canonical identity at read/handoff boundaries.
Crash after upsert reconstructs derived state from the committed Document.
Successful `open_document` requires derived readiness for that same identity.
Preserve crash-before/after and concurrent-read/write tests. Only proven inability
of existing Ports to maintain these invariants justifies a minimal new Port after
Coordinator review; separate repository/index calls alone do not justify a
repository-wide storage refactor.

Upgrade is explicit reopen, not in-place reinterpretation. Persisted v8/v9/v10
documents and all old raw bytes are retained. Old locators/cursors fail closed
under the new normalization guard and require caller reacquisition; no ordinal,
snippet or fuzzy rebinding. **Global v11/hash-v3 means EPUB also needs explicit
reopen**; Coordinator has accepted this declared cost; provide upgrade guidance.
Unchanged source/config
in the new version keeps identity and restart/resume stable. Never delete
canonical data or parsed/raw caches to hide migration bugs.

## 6. Bounded synchronous execution and resource admission

These are proposed hard defaults, not measured performance claims:

| Setting / constraint | v1 proposal |
| --- | --- |
| OCR enabled | false; true requires validated absolute engine/model paths |
| Languages | `eng`; explicit `chi_sim` / `eng+chi_sim` allowed |
| OCR page count | at most 8 required pages; native pages keep existing PDF cap |
| Raster | 300 DPI; maximum 16 million pixels/page, 64 million total OCR pixels |
| Parallelism | one OCR job, one raster page at a time, `OMP_THREAD_LIMIT=1` |
| Admission queue | at most 2 distinct keys; wait at most 2 seconds within deadline |
| Ingestion deadline | 60 seconds total for inspection/OCR/projection/publication; not 60 seconds per stage |
| Whole open deadline | 90 seconds including bounded retrieval; effective deadline is earliest caller/server deadline |
| Per-page recognition | at most 15 seconds and no more than remaining ingestion time |
| Child-tree budget | 768 MiB aggregate resident memory, 512 MiB private temp quota |
| Outputs | 32 MiB aggregate structured output; 64 KiB bounded stderr, no raw stderr returned/logged |
| Cancellation cleanup | TERM group, <=1 second grace, KILL group, reap within 2 seconds |
| Cold four-page acceptance | each of 5 successful runs <=45 seconds, no retries inside one call |
| Warm reopen acceptance | each of 5 runs <=5 seconds, engine execution count stays zero |

Keep the existing native parse timeout at 30 seconds; the OCR-enabled pipeline
uses its explicit 60-second shared budget rather than stacking existing timeouts.
Pixel/page limits are preflight checks, not downscale-and-pretend-success behavior.
Host resources and original byte/section/character budgets still apply; whichever
limit is stricter wins. Hard limits must bound worker allocations, not just output
after an oversized allocation has happened.

Reuse existing subprocess supervision, extending only the required descendant
termination/reaping behavior. Inspect and record host-available process-group,
cgroup and network-isolation mechanisms before choosing their concrete wiring;
do not assume delegated cgroups exist or create a new supervision architecture.
Constrain aggregate memory/PIDs, CPU threads, file descriptors and file size with
available host mechanisms; no shell invocation or inherited credentials. Strip
networking/proxy environment and enforce worker network denial with a verified
available mechanism (environment stripping alone is not network isolation).
Prove cleanup of every descendant, not only the Python parent. If existing
capabilities cannot enforce the required bounds/isolation, report the concrete
gap to Coordinator before any larger architecture change; never run unbounded.
Private 0700 temporary directories, bounded creation and cleanup on
success/error/cancellation/startup-orphan recovery. Remove only owned temp paths;
never delete canonical/OCR evidence when cleaning a failed job.

Distinct error codes and retryability (existing error envelope, no new tool):

| Code | Retryable | Caller action |
| --- | --- | --- |
| `OCR_REQUIRED` | false | Enable/configure local OCR or provide usable source |
| `OCR_UNAVAILABLE` | false | Operator repairs missing engine/model/isolation |
| `OCR_FAILED` | false by default | Report bounded cause; repair input/engine, no blind retry |
| `OCR_TIMEOUT` | true | Explicit later retry within limits; no automatic repeated loop |
| `OCR_RESOURCE_LIMIT` | false for input/config cap; true for busy/admission | Smaller input or operator capacity; busy may retry later |
| `OCR_NO_SUPPORTED_PROJECTION` | false | Inspect original/degradation, do not claim readable prose |

Preserve transport cancellation semantics (no successful result after cancellation).
Raw diagnostics/private paths never appear in errors or public telemetry.

**Connector deadline is unmeasured.** Design does not assume the production
connector allows 90 seconds. Before implementation tuning, Coordinator/executor
must establish its actual supported request deadline and benchmark the frozen
four-page workload with the exact Actions artifact in an isolated local state.
Acceptance requires measured call duration plus 10 seconds of transport headroom
to fit the connector limit. If not, stop synchronous v1 and return a design change
for recoverable asynchronous ingestion; do not silently increase client timeout,
retry until cached, or count a warm call as cold acceptance.

Resource admission must follow measured demand, not arbitrary absolute thresholds.
The earlier 1 GiB available-memory / 4 GiB disk values were candidate conservative
reservations: the host currently does not meet those proposed thresholds, which
does **not** prove the workload cannot complete. First measure actual process-tree
peak memory, temporary storage and unpacked artifact sizes in hosted CI. Process
pages serially and release unused layout/OCR workers instead of retaining both.
Before deployment, calculate peak simultaneously resident artifacts + consistent
snapshot/migration copy + workspace + explicitly recorded safety margin, accounting
for existing rollback artifacts without double counting. Compute memory from the
measured worker/server overlap, live host baseline and explicit safety margin;
confirm with an isolated local probe, including swap behavior and real deadlines.
Document inputs, byte/MiB totals and margin rationale in Deployment evidence.
Minimize staging and clean only this task's identified regenerable outputs;
never remove canonical data, private material or old tasks' files. Do not block
remaining authorized development on an expansion request. If final measured
requirements still exceed capacity, give Coordinator the measurements and minimum
additional disk/memory for a decision. Do not ask the user again by default.

## 7. Frozen acceptance specification (not accuracy already achieved)

Freeze these thresholds and the logical source/geometry specifications at the
Coordinator-approved design SHA **before any runtime tuning**. Fixture generator,
fonts/render dependency hashes, byte-identical PDFs/rasters, full gold character
spans, paragraph/sentence boundaries, region/page/order annotations, and manifest
SHA256 may be reviewed as the **first independent fixture-only commit on the
implementation branch**, with Coordinator freezing its exact SHA before later
commits introduce OCR implementation or benchmark-driven tuning. No separate
fixture PR is required and the same fixture/head does not need repeated full-suite
runs solely for this review. That byte freeze is mandatory: this documents-only
PR does not pretend fixture bytes already exist. No historical private pilot or
AI-visual transcription is approved gold. Corpus changes require a reviewed design
amendment; do not drop a failing case or lower a threshold to pass.

Original synthetic corpus, authored here for unrestricted project fixture use:

```text
E1: The observatory opens before sunrise. Each camera records one image every minute.
E2: Mira checks the northern lens. Leon writes the exposure time in the field notebook.
E3: A clear label connects each sample to its original page. The label does not measure recognition accuracy.
E4: The first column ends beside the diagram. Reading continues at the top of the second column.
E5: The measured change was -8.8 units, not +8.8 units. Trial 12 used 0.25 grams at 07:30.
E6: Questions do not advance the saved reading position. An explicit request moves to the next item.
C1: 清晨，研究员打开观测站。相机每分钟记录一张图像。
C2: 每个样本都有原始页码。页码不能证明文字识别完全准确。
C3: 左栏结束后，阅读从右栏顶部继续。不能把两栏的文字交错连接。
C4: 提问不会改变阅读位置。只有明确要求继续时，才读取下一项。
```

Each labelled line is one paragraph; labels are manifest IDs, not rendered text.
The two sentences per paragraph define gold sentence boundaries. No repeated
paragraph is substituted for independent truth. A4 595x842 points, upright white
background, black text; margins 48 points, line spacing 18 points, paragraph gap
18 points. Single-column area x=48..547; two-column areas x=48..285 and 310..547.
Font size 12 pt, DejaVu Sans for English and Noto Sans CJK SC for Chinese; font
files and versions locked at fixture-byte freeze. Wrap at spaces (English) or
Unicode scalar boundaries (Chinese) to fit the declared column, no hyphen insertion.
Top baseline 72 points. Reset at new page. Glyphs must fit without clipping.

| ID | Fixed logical fixture | Expected behavior |
| --- | --- | --- |
| F01 | Native PDF, E1..E6 in one column, one page | No OCR, exact text and paragraphs |
| F02 | F01 rasterized at 300 DPI, no text objects | Local OCR, English prose |
| F03 | F02 plus correct invisible native layer using F01 coordinates | Reuse existing layer, zero engine calls |
| F04 | F03 layer inside Form XObject; flattened equivalent as separate variant | Same supported prose/order, no Form-based assumption |
| F05 | Four pages: native E1/E2; scanned E3/E4; existing-layer E5/E6; blank | OCR only page 2; all 4 pages accounted for |
| F06 | One scanned page, E1..E3 left and E4..E6 right | Full left column before right; no interleaving |
| F07 | Scanned C1..C4, one column | Simplified Chinese paragraph/sentence support |
| F08 | Scanned E1,C1,E2,C2 in that order | Explicit eng+chi_sim; no English word-count gate |
| F09 | Blank white raster only | Zero hallucinated text; no readable-prose success |
| F10 | F02 raster reduced to 100 DPI then upsampled to 300 DPI | Low-quality metrics reported; downgrade/failure allowed, no unconditional accuracy claim |
| F11 | F06 plus isolated chart at y=500..620, labels A/B/12 and formula x²+y²=z² at y=680 | Chart/formula retained coarse/visual, never promoted to prose Sentence |
| F12 | F02 plus visible footer 'Page 1' at y=815 | Footer cannot suppress body OCR or become full source coverage |
| F13 | One scanned page containing only E1 at the standard top baseline | Short/sparse prose succeeds despite fewer than 8 lines |
| F14 | One scanned single-column paragraph: join 48 clauses `sample N remains bound to original page 1` (N=1..48) with comma-space, append one period | One natural sentence spanning more than 12 wrapped lines; no forced-flush boundary |

F11's chart uses two vertical bars at x=100..125 and 180..205, from y=610 to
y=540 and y=520 respectively; labels A/B at y=635, 12 at y=515. Its unsupported
region order is after both prose columns. All raster conversion retains original
page dimensions. Fixture review verifies actual layout, not just generator code.
Additional fault cases (corruption/encryption/budget overflow/cancel) are derived
from these fixtures and cannot replace the required successful scan cases.

Metrics: Unicode NFC; collapse whitespace runs to one space, trim outer whitespace;
retain case, punctuation, signs, decimal separators and digits. CER is Levenshtein
edits / gold Unicode scalar count; English WER uses whitespace-delimited tokens
with punctuation retained. Report numerator/denominator per fixture and aggregate,
both engine output and final canonical output. Chinese uses CER, not invented
space-separated WER. Alignment is to gold source spans, never OCR outputs used as
their own reference. Reading-order score is concordant gold paragraph pairs / all
ordered pairs; omissions separately count against coverage and cannot inflate it.
Boundary precision/recall/F1 use gold scalar-boundary alignment, not sentence count.

Mandatory thresholds, **each fixture**, no averaging away failures:

- F01/F03/F04 supported prose: CER/WER 0, order 1.0, paragraph/sentence F1 1.0,
  OCR execution count 0. F04's two variants must agree.
- Clean local OCR F02/F05/F06/F12/F13/F14 English: CER <=1%, WER <=3%; F07 Chinese
  CER <=2%; F08 bilingual CER <=2% and English-span WER <=3%.
- All clean supported prose: character coverage >=99%, paragraph and sentence
  boundary F1 >=0.95, reading order 1.0, no duplicated source span, no dropped page.
  E5 sign/number tokens must match exactly wherever included; no +/− substitution.
- F11 prose meets F06 thresholds; chart/formula region retention 100%, fabricated
  prose sentences from those regions 0. No requirement to transcribe the formula.
- F09 returns no supported prose, no fabricated words; F10 reports CER/WER and
  evidence-based degradation or explicit failure. F10 is **not** a successful
  reading case if it misses the clean thresholds. Low confidence may flag review;
  high confidence never suppresses the general OCR-unverified qualification.
- All successful cases: 100% returned unit locators exact-read canonical text,
  100% source parts map to the correct original page, zero out-of-range/fuzzy
  bindings. Traverse every Section/page to completion; no whole-page fallback
  counted as Paragraph/Sentence success. Verify search/context handoff as well.
- Reopen/restart same identity: zero extra OCR calls and identical normalized
  hash/locators. Change each identity input independently: cache miss/new identity,
  old locator and every cursor family rejected; include identical-text new OCR
  revision and changed page binding. Exercise crash-before/after publication and
  index rebuild with no stale generation exposed.
- Fault injection: disabled/missing engine/model, invalid output, missing pages,
  timeout, cancellation, output overflow, disk/memory/page/pixel cap, worker crash,
  concurrency/single-flight. Error class/retryability exact, no partial publication,
  no surviving descendant after 2 seconds, no owned temp leaks. Mock only faults;
  the successful OCR paths invoke the locked real engine through MCP.
  Hand-constructed layout dictionaries only prove projection branches. F03/F04
  must execute real PDF hidden-layer extraction; F02 and other scan cases must
  execute real rasterization and Tesseract. Existing `kill_on_drop` plus an outer
  timeout does not prove child-tree reclamation: test a surviving-grandchild
  scenario explicitly with process/cgroup observations.
- Existing native PDF/layout/#92/EPUB, source-view, HTTP/stdio, stale and
  restart/resume suites stay green. Tool count remains nine; no progress state.

Private four-page paper: authorized **local-only** original naturebp.pdf, hash in
evidence. Run cold original scan -> ingestion -> first prove one natural sentence's
reliable boundary, exact read and original-page binding -> then traverse the full
supported scope and report page/unit/coverage statistics, with the same packaged
runtime in isolated local state, then through the actual production connector.
Report unsupported gaps independently; `source_complete` must never mask them.
Clean synthetic CER/WER thresholds are not measured real-paper accuracy. Publish
hashes, timing/resource counts, page/unit/coverage
statistics and pass/fail only, never text/raster/hOCR. No private fixture in CI or
artifact logs. Coordinator privately annotates/reviews target prose scope before
quantitative accuracy claims; page 1 may contain another article, so document
coverage is not article selection. Private pilot cannot replace frozen public
tests; opening successfully or agreeing on sentence count cannot prove accuracy.
Do not update paper-reading-lab reading progress or declare #47 READY.

## 8. Release, deployment, rollback and closure

After design review, fixture-byte freeze, implementation review and exact-head CI:
merge approved code to main; create separate Release, Package and Deployment
Issues per repository protocols. Discover live tags/version again. Candidate
version proposal is **0.4.0** (new minor feature; v0.4.0-rc.1 already exists), only
if still unused; never overwrite RC/formal tags or assets. Source freeze -> tag /
Release -> verified hosted Package -> Deployment, not production compilation.
Record server and OCR companion archive/binary/model checksums and dependency
manifest in the identity chain. Coordinator acceptance remains separate from CI.

Before switch: resolve current binary/service/profile/state again, satisfy section
6 capacity gates, preserve old binary, existing Python environment and exact
config/drop-ins, and verify rollback. **Do not tar a live SQLite/WAL directory as
the only consistent backup.** Quiesce writers for a consistent state snapshot
(or verified SQLite online backup plus coordinated external evidence snapshot).
Use a new versioned state directory copied from that snapshot for migration;
the previous complete state remains untouched. Do not expose new production
writes until smoke verification completes. Record config/state-directory switch
and make rollback restore binary + dependencies + config + old state together.
Retain the failed/new state for later recovery; no canonical data deletion and no
old binary reading v11 state. Existing deploy script needs this audited handoff,
not an unreviewed live workaround.

Deploy exact formal package, atomic symlink/config switch and supervised restart.
Verify installed checksum, service stability, tunnel doctor, old-document explicit
reopen, same-version restart/resume, scan ingestion/coverage/source-view via real
connector and nine-tool discovery. On failure restore the verified old tuple,
verify service/tunnel and old documents, retain new state/evidence. Never fix live
uncommitted code. Cannot close #95 until Coordinator approves complete engine,
quality, CI, main, release/package/deploy and real-connector acceptance evidence.

## 9. 已裁决事项与后续证据边界

本地引擎、既有依赖、同步待测方案、v11/hash-v3 与 EPUB reopen、operator
revision、layout prerequisite 已获 Coordinator 方向认可，不再重复询问用户。
本次按裁决收窄存储/监督范围，改为实测资源准入，并简化 fixture SHA 冻结流程。
返回新 head 后等待 Coordinator 最终设计批准；本轮仍只修订三份文档。

尚待执行阶段取得的证据：fixture/gold 精确 SHA、实际依赖 hash、hosted 峰值/
解包测量、宿主隔离机制可行性、connector 时限与真实论文验证。资源不足或需要
扩大架构时附具体测量/不变量缺口交 Coordinator 裁决，不预先要求扩容。
设计批准不等于未来实现 head、性能、产物或生产验收通过。
