"""Build the source-view limit fixture `big_stream.pdf`.

One page with a native text layer (the document opens without OCR) and a
solid-colour 3000x3000 RGB image drawn on part of the page: decoded it is
27,000,000 bytes, over the default 16-MiB decoded-stream limit, while its
compressed stream stays a few KB. The image only covers part of the page so
both parsers keep the page's native text as the authoritative body.
Run: `python3 make_fixture.py` from this directory (needs PyMuPDF 1.28.2).
"""
import pymupdf

IMAGE_SIDE = 3000
# Over 200 normalized characters, so the page counts as a usable text layer.
PAGE_TEXT = (
    "This page carries a real text layer, so the document opens without OCR. "
    "Its embedded image is cheap to store but expensive to decode: three "
    "thousand by three thousand RGB samples inflate to twenty-seven million "
    "bytes once the PDF stream is decompressed, which is what the source-view "
    "decoded-stream limit is measured against."
)


def main():
    document = pymupdf.open()
    page = document.new_page()
    page.insert_textbox(
        pymupdf.Rect(72, 72, 540, 200), PAGE_TEXT, fontsize=11
    )
    pixmap = pymupdf.Pixmap(pymupdf.csRGB, pymupdf.IRect(0, 0, IMAGE_SIDE, IMAGE_SIDE), False)
    pixmap.set_rect(pixmap.irect, (0, 60, 120))
    # Drawn on under half the page, the image stays a picture region rather
    # than a dominant full-page raster, so the layout parser does not route
    # the page to OCR.
    page.insert_image(pymupdf.Rect(72, 360, 300, 588), pixmap=pixmap)
    # `deflate` keeps the image a few KB in the file while its decoded stream
    # stays the full 27,000,000 bytes.
    document.save("big_stream.pdf", deflate=True)


if __name__ == "__main__":
    main()
