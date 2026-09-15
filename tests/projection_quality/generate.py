"""Generate the public synthetic projection-quality corpus (Issue #91).

Run with the pinned pdf-layout Python (pymupdf 1.28.2). All text is original
synthetic prose authored for this corpus; no external paper content is used.
EPUB control is built with the standard library only.
"""
import io
import json
from pathlib import Path
import zipfile

import pymupdf

HERE = Path(__file__).resolve().parent
PAGE = (595, 842)  # A4 points


def lines(page, x, y, items, size=10.0, leading=14.0):
    for i, text in enumerate(items):
        page.insert_text((x, y + i * leading), text, fontsize=size)


def p01(doc):
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    page.insert_textbox(
        pymupdf.Rect(40, 60, 555, 240),
        "The archive preserves every measurement. Each record keeps its "
        "original page.\n\nResearchers review the ledger each morning. A "
        "clear label binds each entry to its source.\n\nThe catalog stays "
        "open for review. Every correction requires a second signature.",
        fontsize=10.0)


def p02(doc):
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    page.insert_textbox(
        pymupdf.Rect(40, 60, 285, 400),
        "The first column carries the early results. Each figure keeps a "
        "stable reference.\n\nObservers compare the northern samples. The "
        "ledger records clear totals.",
        fontsize=10.0)
    page.insert_textbox(
        pymupdf.Rect(310, 140, 555, 460),
        "The second column continues the analysis. Reviewers confirm each "
        "measurement.\n\nFinal notes close the section. The appendix lists "
        "every source.",
        fontsize=10.0)


def p03(doc):
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    page.insert_textbox(
        pymupdf.Rect(40, 60, 555, 300),
        "The survey opens before dawn. Field notes begin with a short "
        "summary.\n\nThe first page ends mid discussion.",
        fontsize=10.0)
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    page.insert_textbox(
        pymupdf.Rect(40, 60, 555, 300),
        "The second page continues the same analysis. Readers follow the "
        "thread across the page break.\n\nA closing note confirms the "
        "record. The final entry remains bound to page two.",
        fontsize=10.0)


def p04(doc):
    # Explicit line placement: "pipe-" ends a rendered line while "pipeline"
    # appears unbroken elsewhere, so the wrap hyphen is dehyphenated exactly.
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    lines(page, 40, 80, [
        "The research pipeline keeps every stage bound to its source page.",
        "Reviewers replay the pipe-",
        "line whenever a measurement changes.",
        "Every replay keeps the original ordering of the record.",
    ])


def p05(doc):
    # Native visible text with no inter-word spaces (whitespace-loss shape).
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    lines(page, 40, 80, [
        "Thequickbrownfoxjumpsover.",
        "Everyrecordstaysboundtoitspage.",
        "Reviewersconfirmthewholesale.",
    ])


def p06(doc):
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    page.insert_textbox(
        pymupdf.Rect(40, 60, 555, 300),
        "The ratio stayed at 3.14 through the Fig. 2 review. The build kept "
        "tag v1.2.3 stable. Prior work [12] reached the same result. The "
        "team used e.g. careful sampling.",
        fontsize=10.0)


def p07(doc):
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    page.insert_text((40, 90), "Studies in Observational Cartography",
                     fontsize=18.0)
    page.insert_text((40, 120), "M. Reyes and L. Okafor", fontsize=11.0)
    page.insert_text((40, 180), "Abstract", fontsize=13.0)
    page.insert_textbox(
        pymupdf.Rect(40, 200, 555, 320),
        "This report examines how observatories preserve source order. "
        "Each claim remains bound to its original page.",
        fontsize=10.0)
    page.insert_text((40, 360), "1 Introduction", fontsize=13.0)
    page.insert_textbox(
        pymupdf.Rect(40, 380, 555, 500),
        "The introduction frames the preservation problem. Prior studies "
        "left ordering implicit.",
        fontsize=10.0)


def p08(doc):
    # "transfor-" + "mation" never appears unbroken anywhere: the wrap hyphen
    # is preserved and the region is flagged pdf_ambiguous_hyphens_preserved.
    page = doc.new_page(width=PAGE[0], height=PAGE[1])
    lines(page, 40, 80, [
        "The survey records a gradual transfor-",
        "mation of the field ledger.",
        "Each revision remains attached to its page.",
    ])


CASES = {
    "P01-single-column": p01,
    "P02-double-column": p02,
    "P03-cross-page": p03,
    "P04-dehyphenation": p04,
    "P05-whitespace-loss": p05,
    "P06-token-stress": p06,
    "P07-front-matter": p07,
    "P08-preserved-hyphen": p08,
}

EPUB_XHTML = [
    ("chapter1.xhtml", "Chapter One",
     ["The opening chapter states the claim plainly. Every sentence stays "
      "bound to its spine item."]),
    ("chapter2.xhtml", "Chapter Two",
     ["The closing chapter reviews the evidence. A final note completes "
      "the record."]),
]


def build_epub(path):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        z.writestr(zipfile.ZipInfo("mimetype"), "application/epub+zip",
                   compress_type=zipfile.ZIP_STORED)
        z.writestr("META-INF/container.xml", """<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>""")
        manifest = "".join(
            f'<item id="c{i}" href="{name}" media-type="application/xhtml+xml"/>'
            for i, (name, _, _) in enumerate(EPUB_XHTML, 1))
        spine = "".join(f'<itemref idref="c{i}"/>'
                        for i in range(1, len(EPUB_XHTML) + 1))
        z.writestr("OEBPS/content.opf", f"""<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:uuid:projection-quality-epub</dc:identifier>
    <dc:title>Projection Quality Control</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>{manifest}</manifest>
  <spine>{spine}</spine>
</package>""")
        for name, heading, paragraphs in EPUB_XHTML:
            body = "".join(f"<p>{p}</p>" for p in paragraphs)
            z.writestr(f"OEBPS/{name}", f"""<?xml version="1.0"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>{heading}</title></head>
<body><h1>{heading}</h1>{body}</body></html>""")
    path.write_bytes(buf.getvalue())


def main():
    out = HERE / "pdf"
    out.mkdir(exist_ok=True)
    for name, build in CASES.items():
        doc = pymupdf.open()
        build(doc)
        target = out / f"{name}.pdf"
        doc.save(target, deflate=True, garbage=3)
        doc.close()
        print(target.name)
    epub = HERE / "epub"
    epub.mkdir(exist_ok=True)
    build_epub(epub / "P09-epub-control.epub")
    print("P09-epub-control.epub")


if __name__ == "__main__":
    main()
