"""Render original PDF bytes with the optional pinned PyMuPDF engine.

The parent stages private files and enforces wall time. No canonical text or OCR
is used to construct the image. Hard process limits bound malformed PDF decoding.

A rejection over a configured limit is reported as data, not message text: the
worker prints `READING-MCP-SOURCE-VIEW-LIMIT {"kind": ..., "measured": ...,
"limit": ...}` on stderr and exits 1, matching the record the Rust
`--reading-mcp-source-view-*-worker` modes emit. Any other failure keeps the
plain diagnostic and means a crash or an invalid PDF.
"""
import importlib.metadata
import json
import math
import os
from pathlib import Path
import sys

# Keep in sync with WORKER_LIMIT_STDERR_MARKER / WorkerLimitKind in
# src/parsing/source_view.rs.
LIMIT_MARKER = "READING-MCP-SOURCE-VIEW-LIMIT "


class LimitRejected(Exception):
    """A configured source-view limit was hit, with the numbers when known."""

    def __init__(self, kind, message, measured=None, limit=None):
        super().__init__(message)
        self.kind = kind
        self.measured = measured
        self.limit = limit


def main():
    if len(sys.argv) != 13:
        raise ValueError("invalid source-view worker arguments")
    _, source, output, metadata = sys.argv[1:5]
    page_no, dpi, max_pages, max_width, max_height, max_pixels, max_image, max_stream = map(int, sys.argv[5:])
    if min(page_no, dpi, max_pages, max_width, max_height, max_pixels, max_image, max_stream) <= 0:
        raise ValueError("source-view limits must be positive")
    if importlib.metadata.version("pymupdf") != "1.28.2":
        raise ValueError("PyMuPDF 1.28.2 is required")
    # These limits protect the host before MuPDF allocates decoded streams/images.
    # The explicit checks below additionally enforce the configured per-item limits.
    if sys.platform.startswith("linux"):
        import resource
        memory = min(1024 * 1024 * 1024, 256 * 1024 * 1024 + os.path.getsize(source) * 3 + max_pixels * 16 + max_stream * 2)
        resource.setrlimit(resource.RLIMIT_AS, (memory, memory))
        resource.setrlimit(resource.RLIMIT_FSIZE, (max(max_image, 4096) + 1,) * 2)
    import pymupdf
    with pymupdf.open(source) as doc:
        if doc.needs_pass:
            raise ValueError("encrypted PDF")
        if len(doc) > max_pages:
            raise LimitRejected("page_count", "PDF page count exceeds configured limit", len(doc), max_pages)
        if page_no > len(doc):
            raise ValueError("page number is outside the PDF page range")
        page = doc[page_no - 1]
        width = math.ceil(page.rect.width * dpi / 72)
        height = math.ceil(page.rect.height * dpi / 72)
        if width <= 0 or height <= 0:
            raise ValueError("source-view page has unusable dimensions")
        if width > max_width:
            raise LimitRejected("page_dimensions", "source-view width exceeds configured limit", width, max_width)
        if height > max_height:
            raise LimitRejected("page_dimensions", "source-view height exceeds configured limit", height, max_height)
        if width * height > max_pixels:
            raise LimitRejected("rendered_pixels", "source-view pixel count exceeds configured limit", width * height, max_pixels)
        for image in page.get_images(full=True):
            if image[2] * image[3] > max_pixels:
                raise LimitRejected("embedded_image_pixels", "embedded image exceeds pixel limit", image[2] * image[3], max_pixels)
        # Conservatively validate all streams, including shared resources. An
        # oversized stream on another page may therefore reject this preview.
        for xref in range(1, doc.xref_length()):
            if doc.xref_is_stream(xref):
                stream = doc.xref_stream(xref)
                if stream is not None and len(stream) > max_stream:
                    raise LimitRejected("decoded_stream_bytes", f"decoded PDF stream exceeds configured limit ({len(stream)} > {max_stream})", len(stream), max_stream)
                del stream
        pixmap = page.get_pixmap(dpi=dpi, colorspace=pymupdf.csRGB, alpha=False)
        if pixmap.width > max_width:
            raise LimitRejected("page_dimensions", "rendered width exceeds configured limit", pixmap.width, max_width)
        if pixmap.height > max_height:
            raise LimitRejected("page_dimensions", "rendered height exceeds configured limit", pixmap.height, max_height)
        if pixmap.width * pixmap.height > max_pixels:
            raise LimitRejected("rendered_pixels", "rendered pixel count exceeds configured limit", pixmap.width * pixmap.height, max_pixels)
        encoded = pixmap.tobytes("png")
        if len(encoded) > max_image:
            raise LimitRejected("image_bytes", "source-view image exceeds byte limit", len(encoded), max_image)
        Path(output).write_bytes(encoded)
        Path(metadata).write_text(json.dumps({"width": pixmap.width, "height": pixmap.height, "page_count": len(doc), "image_bytes": len(encoded)}))


if __name__ == "__main__":
    try:
        main()
    except LimitRejected as error:
        print(f"PyMuPDF source view failed: {error}", file=sys.stderr)
        record = {"kind": error.kind}
        if error.measured is not None:
            record["measured"] = error.measured
        if error.limit is not None:
            record["limit"] = error.limit
        print(f"{LIMIT_MARKER}{json.dumps(record)}", file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        print(f"PyMuPDF source view failed: {error}", file=sys.stderr)
        sys.exit(1)
