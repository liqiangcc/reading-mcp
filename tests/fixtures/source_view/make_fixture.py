"""Build the source-view limit fixtures.

Each file is one page with a native text layer (the document opens without
OCR) and a compressible, dark image drawn on under half the page:

- `big_stream.pdf` — a solid-colour 3000x3000 RGB image: decoded it is
  27,000,000 bytes, over the default 16-MiB decoded-stream limit, while its
  compressed stream stays a few KB.
- `big_pixels.pdf` — a DeviceGray 2500x2000 image (5,000,000 px, over the
  default 4,000,000-pixel embedded-image limit) drawn directly by the page.
  Decoded it is 5,000,000 bytes, under the stream limit, so only the pixel
  limit trips.
- `big_pixels_form.pdf` — the same image drawn only inside a form XObject
  (two levels of `Do`, via `show_pdf_page`).
- `big_pixels_unused.pdf` — the page's `/Resources` list the same image but
  the content never draws it.
- `big_smask.pdf` — a 500x500 image whose `/SMask` is a 2500x2000 image.
- `corrupt_image.pdf` — a small image whose stream is invalid for its filter
  (`/DCTDecode` over garbage bytes).

The images use gray level 40 so a rendered page shows whether the image was
drawn. Run: `python3 make_fixture.py` from this directory (needs PyMuPDF
1.28.2).
"""
import zlib

import pymupdf

IMAGE_SIDE = 3000
PIXEL_IMAGE_W = 2500
PIXEL_IMAGE_H = 2000
# Over 200 normalized characters, so the page counts as a usable text layer.
PAGE_TEXT = (
    "This page carries a real text layer, so the document opens without OCR. "
    "Its embedded image is cheap to store but expensive to decode: three "
    "thousand by three thousand RGB samples inflate to twenty-seven million "
    "bytes once the PDF stream is decompressed, which is what the source-view "
    "decoded-stream limit is measured against."
)
TEXT_RECT = pymupdf.Rect(72, 72, 540, 200)
# Drawn on under half the page, the image stays a picture region rather
# than a dominant full-page raster, so the layout parser does not route
# the page to OCR.
IMG_RECT = pymupdf.Rect(72, 360, 300, 588)


def new_doc_with_text():
    document = pymupdf.open()
    page = document.new_page()
    page.insert_textbox(TEXT_RECT, PAGE_TEXT, fontsize=11)
    return document, page


def gray_pixmap(width, height, level=40):
    pixmap = pymupdf.Pixmap(pymupdf.csGRAY, pymupdf.IRect(0, 0, width, height), False)
    pixmap.set_rect(pixmap.irect, (level,))
    return pixmap


def device_gray(document, page):
    """Name `/DeviceGray` directly instead of the ICCBased object PyMuPDF adds."""
    for image in page.get_images(full=True):
        document.xref_set_key(image[0], "ColorSpace", "/DeviceGray")


def big_stream():
    document, page = new_doc_with_text()
    pixmap = pymupdf.Pixmap(
        pymupdf.csRGB, pymupdf.IRect(0, 0, IMAGE_SIDE, IMAGE_SIDE), False
    )
    pixmap.set_rect(pixmap.irect, (0, 60, 120))
    page.insert_image(IMG_RECT, pixmap=pixmap)
    # `deflate` keeps the image a few KB in the file while its decoded stream
    # stays the full 27,000,000 bytes.
    document.save("big_stream.pdf", deflate=True)


def big_pixels():
    document, page = new_doc_with_text()
    page.insert_image(
        IMG_RECT, pixmap=gray_pixmap(PIXEL_IMAGE_W, PIXEL_IMAGE_H)
    )
    device_gray(document, page)
    document.save("big_pixels.pdf", deflate=True)


def big_pixels_form():
    source = pymupdf.open()
    source_page = source.new_page()
    source_page.insert_image(
        IMG_RECT, pixmap=gray_pixmap(PIXEL_IMAGE_W, PIXEL_IMAGE_H)
    )
    document, page = new_doc_with_text()
    # `show_pdf_page` nests the source page's content — including its image
    # `Do` — under a form XObject on the target page.
    page.show_pdf_page(IMG_RECT, source, 0)
    device_gray(document, page)
    document.save("big_pixels_form.pdf", deflate=True)


def big_pixels_unused():
    document, page = new_doc_with_text()
    page_xref = page.xref
    # Snapshot the text-only contents, draw the image, then restore: the image
    # stays in `/Resources` but the content stream never draws it.
    contents_before = document.xref_get_key(page_xref, "Contents")
    saved_streams = {x: document.xref_stream(x) for x in page.get_contents()}
    page.insert_image(
        IMG_RECT, pixmap=gray_pixmap(PIXEL_IMAGE_W, PIXEL_IMAGE_H)
    )
    device_gray(document, page)
    document.xref_set_key(page_xref, "Contents", contents_before[1])
    for xref, data in saved_streams.items():
        document.update_stream(xref, data)
    document.save("big_pixels_unused.pdf", deflate=True)


def big_smask():
    document, page = new_doc_with_text()
    page.insert_image(IMG_RECT, pixmap=gray_pixmap(500, 500))
    image_xref = page.get_images(full=True)[0][0]
    # No high-level call adds an `/SMask`, so the mask image object is built
    # through an xref object/stream edit.
    mask_xref = document.get_new_xref()
    document.update_object(
        mask_xref,
        "<< /Type /XObject /Subtype /Image "
        f"/Width {PIXEL_IMAGE_W} /Height {PIXEL_IMAGE_H} "
        "/ColorSpace /DeviceGray /BitsPerComponent 8 >>",
    )
    document.update_stream(
        mask_xref, bytes([40]) * (PIXEL_IMAGE_W * PIXEL_IMAGE_H)
    )
    document.xref_set_key(image_xref, "SMask", f"{mask_xref} 0 R")
    document.save("big_smask.pdf", deflate=True)


def corrupt_image():
    document, page = new_doc_with_text()
    page.insert_image(IMG_RECT, pixmap=gray_pixmap(120, 80))
    image_xref = page.get_images(full=True)[0][0]
    document.update_stream(
        image_xref, b"\xff\xd8\xff\xe0 not a real jpeg stream", compress=0
    )
    document.xref_set_key(image_xref, "Filter", "/DCTDecode")
    document.save("corrupt_image.pdf", deflate=True)


def main():
    big_stream()
    big_pixels()
    big_pixels_form()
    big_pixels_unused()
    big_smask()
    corrupt_image()


if __name__ == "__main__":
    main()
