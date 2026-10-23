# PDF-4a: an image-only PDF reports `ocr_required`

## The bug

Only the layout backend raises `ApplicationError::OcrRequired`
(`src/parsing/layout_pdf.rs:805`, after its Python worker reports a text layer
it cannot read and OCR is off). The default backend never raises it, so an
image-only PDF that happens to carry a watermark opens as `ok` with almost no
text. The local case is `/root/mcp-data/doc/naturebp.pdf`: 4 pages, `ok`, 156
characters of watermark. A reader that trusts `ok` reads an empty book and never
learns why.

A PDF with *zero* extractable text does not open `ok`; before this change it
fails with `ParseFailed` ("contains no extractable text"). That is the wrong
code — the document is not unreadable, it needs OCR — and v2 maps `ParseFailed`
to `unreadable`, whose hint says "the format or encoding is unsupported". A
reader is told the file is broken when the truth is that it is a pure scan.
Both shapes are fixed here:

* zero text, image-dominated → `ocr_required` with the page and character count;
* a few characters of watermark, image-dominated → `ocr_required` under the
  two-condition rule.

## Local repro

Both files are outside the repository; nothing is committed.

| file | pages | extracted chars | raster coverage | before | after |
|---|---:|---:|---:|---|---|
| `/root/mcp-data/doc/如何培养分析能力 (孙英陆编著) (z-library.sk, 1lib.sk, z-lib.sk).pdf` | 99 | 0 | 1.0 (197 `/Image`, 0 `/Font`, 0 `/ToUnicode`) | `unreadable` | `ocr_required` — "this PDF has no text layer (99 pages, 0 characters); it needs OCR" |
| `/root/mcp-data/doc/naturebp.pdf` | 4 | 156 (watermark) | 1.0 | `ok` | `ocr_required` — "this PDF has no text layer (4 pages, 156 characters); it needs OCR" |

The first file is the user report that motivated the message change: a pure
image scan (99 pages, 197 image objects, no fonts at all) answered
`error=unreadable` with the hint "来源无法解析或文本编码不受支持".

## The message

`ApplicationError::OcrRequired` now carries the evidence it measured:
`OcrRequiredDetail { pages, characters }`. The Display string is one English
sentence with the document's own numbers and no path, and it is identical on
both surfaces:

* measured: `this PDF has no text layer (99 pages, 0 characters); it needs OCR`
  (singular counts read `1 page`, `1 character`)
* unmeasured (the layout backend's worker path, which has no text-layer count):
  the message it already had,
  `source contains image-only pages requiring inspection; enable local OCR or provide a usable text layer`

v1 reads this through `error.to_string()` and puts it in the JSON-RPC message;
v2 carries the same string as `ReadingError.detail`, which the v2 contract
never sends as-is. v2's wire `hint` stays the contract's localized hint for the
`ocr_required` code — "该来源需要本地 OCR 才能读取，请启用 OCR 后重新 open。" —
identical to the layout backend's case, and its `retry` stays absent.

## What the default backend already has

The task assumed the default backend already computes image statistics for
`source_view` and that this change could reuse them. It does not, and there is
nothing to reuse:

* `src/parsing/source_view.rs` renders one page with `pdf_render` and returns a
  PNG. It computes no image-area statistic, and it is never called during parse.
* `src/parsing/pdf_source_view_worker.py` calls `page.get_images(full=True)`
  only to bound embedded image pixels for the isolated renderer. It does not
  report coverage, and it runs in a separate process on demand.
* `src/parsing/pdf.rs` only calls lopdf's text extractor.
* The layout backend's coverage (`native_raster_coverage` in
  `pdf_layout_worker.py`) is a 300-DPI render of every page inside the OCR
  worker; the default backend has no OCR worker and no render step.

Reusing any of these would mean rendering every page (a full raster scan and a
PyMuPDF subprocess per page) just to answer "is this a scan". Instead the change
adds one small content-stream walk (`src/parsing/pdf_raster.rs`) that follows
`Do` into image and form XObjects and tracks the graphics-state CTM. It decodes
no image data and renders nothing. It runs only after the text is already known
to be nearly empty, so a text PDF never pays for it.

## Step 1: today's table

Measured on `origin/main` (`c372f26`) by the ignored test
`parsing::pdf::tests::pdf4a_measurement_table`, run in the `PDF4a measure`
workflow (run 39909217913). `raster coverage` is the mean fraction of page area
painted by raster images, the statistic the new rule uses. `unreadable` is the
current `ParseFailed` for "contains no extractable text".

| input | outcome | pages | chars | chars/page | sections | raster coverage |
|---|---|---:|---:|---:|---:|---:|
| pdf-text-1500p.pdf | ok | 1500 | 2999183 | 1999.5 | 1500 | 0.000 |
| F01.pdf | ok | 1 | 448 | 448.0 | 1 | 0.000 |
| F02.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F03.pdf | ok | 1 | 448 | 448.0 | 1 | 1.000 |
| F04-flat.pdf | ok | 1 | 448 | 448.0 | 1 | 1.000 |
| F04-form.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F05.pdf | ok | 4 | 276 | 69.0 | 4 | 0.750 |
| F06.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F07.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F08.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F09.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F10.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F11.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F12.pdf | ok | 1 | 4 | 4.0 | 1 | 1.000 |
| F13.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |
| F14.pdf | unreadable | 1 | 0 | 0.0 | - | 1.000 |

The fixture set is not uniform, and that is the point of the false-positive
guard. Per `tests/fixtures/scanned_pdf/generate.py`:

* `F01` is a **native** text PDF.
* `F03`, `F04-flat` carry a flat hidden text layer and `F04-form` carries the
  same layer in a Form XObject (which lopdf cannot reach, so it extracts 0).
* `F05` is mixed: 4 pages of native, scan, hidden-flat and blank.
* `F09` is a **blank** page.
* The rest are image-only scans.

`F12` is the bug in miniature: a scan with a visible "Page 1" footer, 4
extracted characters, `ok`. `naturebp.pdf` is the same shape at 156 characters.

## Step 1: chars/page distribution of the 121-document corpus

Computed from the committed PDF-5a corpus (`scripts/pdf-sweep-ids.txt`, now on
`main` as of `4e750f8`; originally `4bc298f` on `bench/pdf5a`) and the reference
`PDF wide sweep` run 39898185205; every one of the 121 documents opened `ok`
there. The statistic is the sweep's `quality.default_chars_per_page`: the text
the **default** backend extracted, divided by the page count. Percentiles are
nearest-rank over the 121 sorted values.

| statistic | chars/page |
|---|---:|
| min | 842.6 (`2403.02163`) |
| p1 | 977.1 |
| p5 | 1461.4 |
| p25 | 2179.3 |
| median | 3140.5 |
| p75 | 5011.2 |
| max | 22034.2 (`2403.04803`) |

Documents below 400 chars/page: **0**. Below 200: **0**. The minimum, 842.6, is
the number the threshold must sit under. `2311.05131` is the hardest control in
the corpus: 495 pages, 130,172 embedded images on 446 pages, and still 977.1
chars/page because no page has a raster over 20% of its area.

## The rule

`ocr_required` when **both** hold:

1. **nearly no text for the page count:** `chars / pages < 200`.
   Margin: 4.2x below the 842.6 minimum of every legitimate text PDF in the
   corpus, and no corpus document is even below 400. Text length alone never
   flags a document: a 30-character one-page text PDF passes this condition and
   still stays `ok` because condition 2 fails.
2. **pages dominated by raster images:** mean raster coverage `>= 0.5`.
   A text PDF with a cover image, one blank page or a figures appendix has a
   mean far below 0.5 (one full-page image in ten pages is 0.1). `F05` at 0.75
   is flagged because half its pages are scans the reader would silently miss.

Both conditions are required, so:

* `F01` (448 chars, coverage 0) stays `ok`.
* `F03`/`F04-flat` (448 chars, coverage 1) stay `ok` — a usable text layer wins.
* A 1-page text PDF with 30 characters and no raster stays `ok`.
* A 10-page text PDF with one image page (54 chars/page, coverage 0.1) stays
  `ok`.

The walk counts only XObject images and forms. Inline images (`BI ... ID ...
EI`) are not counted, so such a page reports coverage 0 and is not flagged; the
rule never guesses.

## Step 2: the fixture table after the change

Measured by the same ignored test in the `PDF4a measure` workflow on `5aa9bdd`
(run 39912839747) and reproduced unchanged on the final head; the final head's
run ID is in the PR body.

| input | before | after | pages | chars | chars/page | coverage |
|---|---|---|---:|---:|---:|---:|
| pdf-text-1500p.pdf | ok | ok | 1500 | 2999183 | 1999.5 | 0.000 |
| F01.pdf | ok | ok | 1 | 448 | 448.0 | 0.000 |
| F02.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F03.pdf | ok | ok | 1 | 448 | 448.0 | 1.000 |
| F04-flat.pdf | ok | ok | 1 | 448 | 448.0 | 1.000 |
| F04-form.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F05.pdf | ok | **ocr_required** | 4 | 276 | 69.0 | 0.750 |
| F06.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F07.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F08.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F09.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F10.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F11.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F12.pdf | ok | **ocr_required** | 1 | 4 | 4.0 | 1.000 |
| F13.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |
| F14.pdf | unreadable | **ocr_required** | 1 | 0 | 0.0 | 1.000 |

12 of the 14 `F*` fixtures (15 files, because `F04` has a flat and a form
variant) report `ocr_required`. Three stay `ok` because they really do carry a
reachable text layer: `F01` (native, 448 chars), `F03` and `F04-flat` (flat
hidden layer, 448 chars). Flagging them would be a false positive on a text
layer, which the task forbids, and would change how a PDF that **has** a text
layer is extracted.

The task expected all 14 `F*.pdf` to report `ocr_required`. That expectation
does not hold for `F01`/`F03`/`F04-flat`, and should not: the fixture README
documents them as native/hidden-layer controls, not scans. The gate is met for
every genuinely image-only fixture (`F02`, `F04-form`, `F06`–`F14`: 11 of 11),
plus the mixed `F05`. `F04-form` carries the same hidden layer as `F04-flat`
inside a Form XObject, but lopdf cannot reach it, so the default backend extracts
0 characters — from the reader's point of view the text is not there, and
`ocr_required` is the honest answer.

## Why `F09` (blank) is flagged

`F09` is a full-page raster of an empty page: 0 characters, coverage 1.0. Without
decoding the raster's pixels it is structurally identical to a text scan, and the
task forbids a second raster scan. It therefore reports `ocr_required`. This is
the one fixture where the signal over-triggers, and it is harmless: OCR of a
blank page yields nothing, and the alternative is a pixel scan of every image.

## Behaviour on reopen

A failed parse publishes nothing: `OpenDocumentUseCase::ingest` only stores an
open result after the parser returns a document. Reopening the same bytes parses
again and, because the rule is a pure function of the bytes, reports
`ocr_required` again with the same message. The reopen test uses a real
`SqliteOpenResultStore` and asserts both opens fail identically.

That alone would leave one flap: a document that opened `ok` under an older
build has a stored open result, and the store would return it instead of the new
failure. The open-result key already carries `PDF_TEXT_EXTRACTION_VERSION`, so
this change bumps it to `pdf-text-extraction/v2`. The stored `ok` is then a
fingerprint miss, the PDF is re-parsed once, and every later open reports
`ocr_required` again. The same bump invalidates the parsed-document cache for
PDFs, so no cached `ok` document survives either.

## Gates

* **Fixtures.** 12 of 14 `F*` report `ocr_required`; the three text-layer
  controls stay `ok` (table above).
* **121-document corpus.** `PDF wide sweep` on the head reports 121/121 `ok`,
  the same outcome classes as the reference run 39898185205 (121/121 `ok`): no
  document changes class. `2403.02163` (842.6 chars/page) and `2311.05131`
  (977.1 chars/page, 130,172 images) stay `ok`; both have a text layer.
* **`ingest-parity`.** The six labelled inputs and the text-layer fixtures
  (`F01`, `F03`, `F04-flat`) are `same` in every column, including the PDF
  input's `result` and `reads`. The image-only fixtures differ in exactly the
  intended direction: `open payload` and `reads` go from `parse failed`/
  `unreadable` to `this PDF has no text layer ...; it needs OCR` and from `ok`
  to the same error (`F05`, `F12`), and their stored `documents`/`sections`/
  `layouts`/`text` columns differ only because the head no longer publishes the
  document at all. That is the deliverable, not a regression: no PDF with a
  reachable text layer changes a single byte.
* **The 300-page CJK text PDF.** Added to `ingest-parity`'s inputs
  (`text-long-cjk-300`, generated by `scripts/pdf-gen-inputs.py`) so base and
  head are compared on it directly: `result` and `text` are `same`. The
  1500-page `pdf-text-1500p` input is `same` in every column.
* **`book-profile`.** `mismatches=0` on the head (run ID in the PR body).

## Host repro

On the deployed host, the tunnel service runs `reading-mcp` over stdio
(`deploy/systemd/reading-mcp-tunnel.service`). Opening the local file must now
answer `ocr_required` instead of `ok` with 156 watermark characters. Run the
same server the tunnel runs and feed it the three MCP messages:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pdf4a","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"open_document","arguments":{"source":"/root/mcp-data/doc/naturebp.pdf"}}}' \
  | /root/reading-mcp/target/release/reading-mcp
```

The same call through the tunnel-connected client (or the HTTP binary at
`/mcp` with its Bearer token) returns the same answer. It carries
`"code":"OCR_REQUIRED"`, `"retryable":false` and the message
`this PDF has no text layer (4 pages, 156 characters); it needs OCR`. The v2
surface answers `"error":"ocr_required"` with no `retry`.
