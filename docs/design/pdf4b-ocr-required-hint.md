# PDF-4b: the v2 `ocr_required` answer says what the PDF lacks and what the reader can do

PDF-4a (#224) made the default PDF backend answer `ApplicationError::OcrRequired(
OcrRequiredDetail::measured(pages, characters))` for an image-only PDF, and the
`Display` string carries both numbers. v2 threw that away: `src/mcp/v2_contracts.rs`
mapped every `ocr_required` to one fixed hint, so a reader could not tell a scan
from a broken file, and its only advice — enable OCR — is not something a reader
of the deployed server can do.

## What v1 sends

v1 is unchanged by this task, byte for byte:

* `to_mcp_error` puts `error.to_string()` in the JSON-RPC `message` and
  `{"code":"OCR_REQUIRED","retryable":false}` in `data`
  (`src/mcp/server.rs:1303`, `:1363`).
* measured: `this PDF has no text layer (4 pages, 156 characters); it needs OCR`
* unmeasured: `source contains image-only pages requiring inspection; enable local OCR or provide a usable text layer`

So PDF-4a's "the same message on both surfaces" holds for `ReadingError.detail`
(which v2 never sends as-is) but not for the v2 wire `hint`, which was a fixed
sentence with no numbers.

## What v2 sends now

Both hints are one Chinese sentence with no path, and both name the two routes
that actually work: a copy with a text layer, or a server that runs OCR.

* measured (`pages` and `characters` both `Some`):
  `该 PDF 没有可用的文字层（4 页只取到 156 个字符），需要 OCR：请改用带文字层的版本（例如离线 OCR 后的 PDF 或文本），或在支持 OCR 的服务上重新 open。`
* unmeasured (the layout backend's worker path, `OcrFailed`, `OcrUnavailable`,
  `OcrNoSupportedProjection`, all of which map to `ocr_required`):
  `该来源没有可用的文字层，需要 OCR：请改用带文字层的版本（例如离线 OCR 后的 PDF 或文本），或在支持 OCR 的服务上重新 open。`

The evidence reaches the hint through two new fields on the internal
`ReadingError` (`pages`, `characters`), set from `OcrRequiredDetail` in
`From<ApplicationError>`. The wire error object is unchanged — `error`, `hint`
and the optional `retry` — because it has no optional numeric detail field to
extend; `too_long`'s `chars` lives on `ReadingError` and in the hint, not in the
error object. `retry` stays absent for `ocr_required`.

## Reopen

Nothing is stored for a failed open: `open_results_v1` holds only complete
results, and `OpenDocumentUseCase` returns the parser's error before it reaches
the store. A reopen re-retrieves the same bytes and re-measures them, so the same
counts come back with no stored detail and no schema change.
`tests/pdf4b_ocr_required_hint.rs` opens each image-only fixture twice and
asserts the two v2 bodies are identical bytes.

## Fixtures

`tests/fixtures/scanned_pdf/pdf/F02.pdf` (1 page, no text) and `F05.pdf` (4 pages,
a page footer as the only text — the shape of the reported `naturebp.pdf`) are the
image-only fixtures the test opens. `F01.pdf` is a native text-layer fixture (448
characters, no raster) and opens normally, so it cannot show `ocr_required`.
