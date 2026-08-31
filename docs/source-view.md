# Original Source View / Source Fidelity

`get_source_view` is the bounded visual-fidelity path for an already opened
document. It accepts a `TextLocator`, resolves that locator against the
persisted canonical `Document`, retrieves the document source again, and only
renders after all of these identities still match:

```text
document_id + raw content_hash + normalized_document_hash + final source
        ↓
canonical Section page binding
        ↓
original PDF bytes → rendered PNG image
```

The response contains structured audit metadata (`page_number`, PDF page
count, image dimensions, image byte count, source and both hashes) plus an MCP
`image/png` content block. The image is rendered from the original PDF bytes;
normalized text and OCR are never used to create visual evidence.

The first implementation supports PDF only, but the application boundary is a
format-neutral `SourceViewRenderer` port. Future HTML, EPUB, or other original
artifact renderers can implement that port without adding format-specific MCP
tools.

## Safety and limits

The tool renders one locator-bound page per request. The runtime bounds PDF
object depth, decoded stream bytes, decoded image pixels, page count, DPI,
output dimensions, output pixels, encoded image bytes, and wall-clock render
time. Defaults are deliberately suitable for review images rather than bulk
PDF export and can be configured with the `READING_MCP_SOURCE_VIEW_*`
variables documented in [Runtime Configuration](runtime-configuration.md).

If the source bytes, final source, normalized document, locator shape, or page
binding no longer matches, the request fails closed with a stale/invalid
error. There is no page-number-only API and no fuzzy or nearest-text rebase.
`read_document` remains the canonical normalized-text reading path.
