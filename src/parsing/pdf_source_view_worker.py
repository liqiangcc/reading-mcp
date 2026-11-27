"""Render original PDF bytes with the optional pinned PyMuPDF engine.

The parent stages private files and enforces wall time — except for a local
source, whose original path the worker opens itself. No canonical text or OCR
is used to construct the image. Hard process limits bound malformed PDF decoding.

The worker hashes the input file in bounded chunks before parsing and records
it as `source_sha256` in the metadata. On a mismatch with the expected hash the
parent passed, it writes just that hash to the metadata file and exits early —
the parent turns it into the fail-closed stale-source answer.

A rejection over a configured limit is reported as data, not message text: the
worker prints `READING-MCP-SOURCE-VIEW-LIMIT {"kind": ..., "measured": ...,
"limit": ...}` on stderr and exits 1, matching the record the Rust
`--reading-mcp-source-view-*-worker` modes emit. Any other failure keeps the
plain diagnostic and means a crash or an invalid PDF.
"""
import hashlib
import importlib.metadata
import json
import math
import os
from pathlib import Path
import sys

# Keep in sync with WORKER_LIMIT_STDERR_MARKER / WorkerLimitKind in
# src/parsing/source_view.rs.
LIMIT_MARKER = "READING-MCP-SOURCE-VIEW-LIMIT "
JPEG_QUALITY = 85
RASTER_COVERAGE_THRESHOLD = 0.5


class LimitRejected(Exception):
    """A configured source-view limit was hit, with the numbers when known."""

    def __init__(self, kind, message, measured=None, limit=None):
        super().__init__(message)
        self.kind = kind
        self.measured = measured
        self.limit = limit


def source_sha256(source):
    """sha256 of the file in bounded chunks — never the whole file in one buffer."""
    digest = hashlib.sha256()
    with open(source, "rb") as handle:
        for chunk in iter(lambda: handle.read(8 << 20), b""):
            digest.update(chunk)
    return "sha256:" + digest.hexdigest()


def raster_dominated(page):
    """Deterministic scan/photo rule: drawn raster bounding boxes cover at least half the crop box."""
    clip = page.cropbox
    page_area = max(0.0, clip.width) * max(0.0, clip.height)
    if page_area <= 0:
        return False
    covered = 0.0
    for image in page.get_image_info():
        bbox = pymupdf.Rect(image["bbox"]).intersect(clip)
        covered += max(0.0, bbox.width) * max(0.0, bbox.height)
    return covered / page_area >= RASTER_COVERAGE_THRESHOLD


def main():
    if len(sys.argv) not in (13, 14, 15):
        raise ValueError("invalid source-view worker arguments")
    _, source, output, metadata = sys.argv[1:5]
    page_no, dpi, max_pages, max_width, max_height, max_pixels, max_image, max_stream = map(int, sys.argv[5:13])
    expected_sha256 = sys.argv[13] if len(sys.argv) >= 14 else None
    output_format = sys.argv[14] if len(sys.argv) == 15 else "auto"
    if output_format not in ("auto", "png"):
        raise ValueError("invalid source-view output format")
    if min(page_no, dpi, max_pages, max_width, max_height, max_pixels, max_image, max_stream) <= 0:
        raise ValueError("source-view limits must be positive")
    actual_sha256 = source_sha256(source)
    # Residual window: a file modified in place between this hash and MuPDF's
    # own reads can produce one render of mixed bytes; the next call hashes the
    # file again and fails closed.
    if expected_sha256 is not None and actual_sha256 != expected_sha256:
        Path(metadata).write_text(json.dumps({"source_sha256": actual_sha256}))
        raise ValueError(f"source-view input {actual_sha256} does not match expected {expected_sha256}")
    if importlib.metadata.version("pymupdf") != "1.28.2":
        raise ValueError("PyMuPDF 1.28.2 is required")
    # These limits protect the host before MuPDF allocates decoded streams/images.
    # The explicit checks below additionally enforce the configured per-item limits.
    if sys.platform.startswith("linux"):
        import resource
        memory = min(1024 * 1024 * 1024, 256 * 1024 * 1024 + os.path.getsize(source) * 3 + max_pixels * 16 + max_stream * 2)
        resource.setrlimit(resource.RLIMIT_AS, (memory, memory))
        resource.setrlimit(resource.RLIMIT_FSIZE, (max(max_image, 4096) + 1,) * 2)
    global pymupdf
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
        jpeg = output_format == "auto" and raster_dominated(page)
        encoded = pixmap.tobytes("jpeg", jpg_quality=JPEG_QUALITY) if jpeg else pixmap.tobytes("png")
        media_type = "image/jpeg" if jpeg else "image/png"
        if len(encoded) > max_image:
            raise LimitRejected("image_bytes", "source-view image exceeds byte limit", len(encoded), max_image)
        Path(output).write_bytes(encoded)
        Path(metadata).write_text(json.dumps({"width": pixmap.width, "height": pixmap.height, "page_count": len(doc), "image_bytes": len(encoded), "media_type": media_type, "source_sha256": actual_sha256}))


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
