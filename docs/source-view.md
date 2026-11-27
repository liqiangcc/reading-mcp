# Original Source View / Source Fidelity

`get_source_view` is the bounded visual-fidelity path for an already opened
document. It accepts a `TextLocator`, resolves that locator against the
persisted canonical `Document`, retrieves the document source again, and only
renders after all identity checks still match:

```text
document_id + raw content_hash + normalized_document_hash + final source
        ↓
TextLocator normalized range
        ↓
original-source-binding/v1
        ↓
original PDF page
        ↓
original PDF bytes → rendered PNG/JPEG image
```

For PDF, the parser persists page bindings as exact Section-relative normalized
ranges. A locator inside page 2 of a logical Section spanning pages 1–2 therefore
resolves to page 2 rather than the Section start page. A locator spanning more
than one original page is rejected and the caller must use a narrower
Paragraph/Sentence locator. Legacy multi-page persisted Sections without precise
binding evidence fail closed and must be reopened with the current parser.

The response contains structured audit metadata including `page_number`, PDF page
count, image dimensions, image byte count, source, raw/normalized hashes,
`normalized_document_hash_version`, and `source_binding_version`, plus an MCP
image content block. In the default `auto` mode a PDF page is emitted as JPEG
when the renderer's deterministic page-content analysis shows drawn images
covering at least half of the page crop box; other pages remain PNG. JPEG uses
quality 85, a constant covered by the render-cache key. Setting
`READING_MCP_SOURCE_VIEW_FORMAT=png` keeps every page PNG exactly as before.
The image is rendered from the identity-checked original PDF bytes; normalized
text and OCR are never used to reconstruct visual evidence.

The raster rule is structural, not a pixel scan. The Rust renderer walks the
page content stream once before rendering, follows `Do` into form XObjects
bounded by the renderer's form-depth limit, tracks `q`/`Q` and `cm`
transformations plus each form's `/Matrix`, clips form content to its `/BBox`,
and treats every drawn image or inline image as the unit square in its current
coordinate system. It reduces each transformed image rectangle to its
axis-aligned bounding box, intersects that box with the effective crop box, and
sums the covered area. Overlaps do not subtract, vector/text painting does not
subtract, and clipping paths are not rasterized; the rule is deterministic
because it depends only on the PDF operators and matrices. At 50% coverage or
more the page is raster-dominated. The pinned PyMuPDF worker applies the same
crop-box coverage threshold through MuPDF's drawn-image bounding-box API.

The first implementation supports PDF only, but the application boundary is a
format-neutral `SourceViewRenderer` port. Future HTML, EPUB, or other original
artifact renderers can implement that port without adding format-specific MCP
tools.

## Safety and limits

The tool renders one locator-bound page per request. The runtime bounds PDF
object depth, decoded stream bytes, decoded image pixels, page count, DPI,
output dimensions, output pixels, encoded image bytes, and render-process
wall-clock time. Production rendering runs in a separate worker process. For a
document whose source is a local file, the worker maps the original PDF
read-only and verifies its sha256 itself; a URL source whose raw-cache entry
still holds its bytes renders the same way — the worker maps the stored body
and checks its sha256, without contacting the origin — while a URL source
without a usable cache entry keeps the revalidating retrieval path; other
sources' PDF bytes are staged in a private temporary directory instead of
being streamed through a potentially back-pressured child stdin pipe; worker
input, image, metadata, and diagnostics are file-backed, and an over-deadline
worker is terminated with `kill + wait`. The raw cache is bounded by
`READING_MCP_RAW_CACHE_MIB`, so an entry the LRU evicted is a re-download on the
revalidating path — never a wrong render.

Defaults are deliberately suitable for review images rather than bulk PDF export
and can be configured with the `READING_MCP_SOURCE_VIEW_*` variables documented
in [Runtime Configuration](runtime-configuration.md).

Successful encoded pages are cached on disk under `<state>/cache/source-view`,
bounded by `READING_MCP_SOURCE_VIEW_CACHE_MIB` (256 MiB by default; `0` disables
cache reads and writes). The cache key includes the raw source hash, page, DPI,
all render-affecting limits, output format mode and actual image media type, and
the renderer identity. Entries rendered from directly mapped files additionally
store cheap length/time/device/inode freshness evidence, which must match before
a hit is served. Entries are written through a unique temp file, synced,
atomically renamed, and evicted by access recency. A hit returns exactly the same
encoded bytes as the cold render, but only after the same local root, URL policy,
document-size, identity, and stale-source checks; refusals are never cached.

If the source bytes, final source, normalized document, locator shape, or source
binding no longer matches, the request fails closed with a stale/invalid error.
There is no page-number-only API and no fuzzy or nearest-text rebase.
`read_document` remains the canonical normalized-text reading path;
`get_source_view` is the optional original-visual fidelity path.
