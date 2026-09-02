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
original PDF bytes → rendered PNG image
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
`image/png` content block. The image is rendered from the identity-checked
original PDF bytes; normalized text and OCR are never used to reconstruct visual
evidence.

The first implementation supports PDF only, but the application boundary is a
format-neutral `SourceViewRenderer` port. Future HTML, EPUB, or other original
artifact renderers can implement that port without adding format-specific MCP
tools.

## Safety and limits

The tool renders one locator-bound page per request. The runtime bounds PDF
object depth, decoded stream bytes, decoded image pixels, page count, DPI,
output dimensions, output pixels, encoded image bytes, and render-process
wall-clock time. Production rendering runs in a separate worker process. Input
PDF bytes are staged in a private temporary directory instead of being streamed
through a potentially back-pressured child stdin pipe; worker input, image,
metadata, and diagnostics are file-backed, and an over-deadline worker is
terminated with `kill + wait`.

Defaults are deliberately suitable for review images rather than bulk PDF export
and can be configured with the `READING_MCP_SOURCE_VIEW_*` variables documented
in [Runtime Configuration](runtime-configuration.md).

If the source bytes, final source, normalized document, locator shape, or source
binding no longer matches, the request fails closed with a stale/invalid error.
There is no page-number-only API and no fuzzy or nearest-text rebase.
`read_document` remains the canonical normalized-text reading path;
`get_source_view` is the optional original-visual fidelity path.
