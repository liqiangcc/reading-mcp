"""Render original PDF bytes with the optional pinned PyMuPDF engine.

The parent stages private files and enforces wall time. No canonical text or OCR
is used to construct the image. Hard process limits bound malformed PDF decoding.
"""
import importlib.metadata
import json
import math
import os
from pathlib import Path
import sys


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
        if doc.needs_pass or not 0 < len(doc) <= max_pages or page_no > len(doc):
            raise ValueError("encrypted PDF or invalid page/page limit")
        page = doc[page_no - 1]
        width = math.ceil(page.rect.width * dpi / 72)
        height = math.ceil(page.rect.height * dpi / 72)
        if width <= 0 or height <= 0 or width > max_width or height > max_height or width * height > max_pixels:
            raise ValueError("source-view dimensions exceed configured limits")
        for image in page.get_images(full=True):
            if image[2] * image[3] > max_pixels:
                raise ValueError("embedded image exceeds pixel limit")
        # Conservatively validate all streams, including shared resources. An
        # oversized stream on another page may therefore reject this preview.
        for xref in range(1, doc.xref_length()):
            if doc.xref_is_stream(xref):
                stream = doc.xref_stream(xref)
                if stream is not None and len(stream) > max_stream:
                    raise ValueError(f"decoded PDF stream exceeds configured limit ({len(stream)} > {max_stream})")
                del stream
        pixmap = page.get_pixmap(dpi=dpi, colorspace=pymupdf.csRGB, alpha=False)
        if pixmap.width > max_width or pixmap.height > max_height or pixmap.width * pixmap.height > max_pixels:
            raise ValueError("rendered dimensions exceed configured limits")
        encoded = pixmap.tobytes("png")
        if len(encoded) > max_image:
            raise ValueError("source-view image exceeds byte limit")
        Path(output).write_bytes(encoded)
        Path(metadata).write_text(json.dumps({"width": pixmap.width, "height": pixmap.height, "page_count": len(doc), "image_bytes": len(encoded)}))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"PyMuPDF source view failed: {error}", file=sys.stderr)
        sys.exit(1)
