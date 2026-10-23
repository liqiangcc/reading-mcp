#!/usr/bin/env python3
"""Generate deterministic EPUBs with many one-paragraph sections.

The section-scale gate needs books whose section count, not whose text size, is the variable, so
every section here holds exactly one short paragraph. Two layouts are generated for each count:

- `nested`: a three-level nest of `h1`, `h2` and `h3` headings, so every hundredth section is a
  level-1 root, every tenth is a level-2 child and the rest are level-3 leaves;
- `flat`: one `h2` heading per section, so every section is a root of its own.

The bytes are a pure function of `--sections` and `--layout`: entries are written in a fixed order
with a fixed timestamp, no file carries the current time, and the identifier is derived from the
count. Standard library only; this script never starts a server.

Usage:
    python3 scripts/gen-sections-epub.py --out-dir inputs --sections 500 2000 8000 20000
"""

from __future__ import annotations

import argparse
import os
import zipfile

MIMETYPE = "application/epub+zip"
ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)

# How many sections one XHTML spine document holds. One entry per section would mean 20,000 ZIP
# entries for the largest book, which measures the archive and not the section tree.
SECTIONS_PER_DOCUMENT = 200

CONTAINER_XML = """<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"""


def escape(text: str) -> str:
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def section_heading(index: int, layout: str) -> tuple[int, str]:
    """The heading level and title of section `index`."""
    title = f"Section {index:06d}"
    if layout == "flat":
        return (2, title)
    if index % 100 == 0:
        return (1, f"Chapter {index // 100:04d}")
    if index % 10 == 0:
        return (2, f"Part {index // 10:05d}")
    return (3, title)


def section_paragraph(index: int) -> str:
    return (
        f"Paragraph {index:06d} of the synthetic section book: the reader walks this text "
        f"one section at a time. 第{index}节的一段文字，用于测量按节读取的成本。"
    )


def xhtml_document(title: str, body: str) -> str:
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<!DOCTYPE html>\n"
        '<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en" lang="en">\n'
        f"<head><title>{escape(title)}</title></head>\n"
        f"<body>\n{body}</body>\n"
        "</html>\n"
    )


def part_document(sections: range, layout: str) -> str:
    body = []
    for index in sections:
        level, title = section_heading(index, layout)
        body.append(f"<h{level}>{escape(title)}</h{level}>")
        body.append(f"<p>{escape(section_paragraph(index))}</p>")
    return xhtml_document(f"Part starting at section {sections.start}", "\n".join(body))


def part_name(part: int) -> str:
    return f"part-{part:04d}.xhtml"


def package_document(sections: int, layout: str, parts: int) -> str:
    manifest = [
        '<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>',
        '<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>',
    ]
    spine = []
    for part in range(parts):
        name = part_name(part)
        manifest.append(
            f'<item id="part-{part:04d}" href="{name}" media-type="application/xhtml+xml"/>'
        )
        spine.append(f'<itemref idref="part-{part:04d}"/>')
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">\n'
        '  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">\n'
        f'    <dc:identifier id="bookid">urn:reading-mcp:sections-{layout}-{sections}</dc:identifier>\n'
        f"    <dc:title>Synthetic sections {layout} {sections}</dc:title>\n"
        "    <dc:language>en</dc:language>\n"
        '    <meta property="dcterms:modified">2024-01-01T00:00:00Z</meta>\n'
        "  </metadata>\n"
        "  <manifest>\n"
        + "".join(f"    {item}\n" for item in manifest)
        + "  </manifest>\n"
        '  <spine toc="ncx">\n'
        + "".join(f"    {item}\n" for item in spine)
        + "  </spine>\n"
        "</package>\n"
    )


def nav_document(parts: int) -> str:
    entries = "".join(
        f'      <li><a href="{part_name(part)}">{escape(f"Part {part:04d}")}</a></li>\n'
        for part in range(parts)
    )
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<!DOCTYPE html>\n"
        '<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"'
        ' xml:lang="en" lang="en">\n'
        "<head><title>Contents</title></head>\n"
        "<body>\n"
        '  <nav epub:type="toc" id="toc">\n'
        "    <h1>Contents</h1>\n"
        "    <ol>\n"
        f"{entries}"
        "    </ol>\n"
        "  </nav>\n"
        "</body>\n"
        "</html>\n"
    )


def ncx_document(sections: int, layout: str, parts: int) -> str:
    points = "".join(
        "    <navPoint id=\"navpoint-{part}\" playOrder=\"{order}\">\n"
        "      <navLabel><text>{label}</text></navLabel>\n"
        "      <content src=\"{href}\"/>\n"
        "    </navPoint>\n".format(
            part=part,
            order=part + 1,
            label=escape(f"Part {part:04d}"),
            href=part_name(part),
        )
        for part in range(parts)
    )
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">\n'
        '  <head>\n'
        f'    <meta name="dtb:uid" content="urn:reading-mcp:sections-{layout}-{sections}"/>\n'
        "  </head>\n"
        f"  <docTitle><text>Synthetic sections {layout} {sections}</text></docTitle>\n"
        "  <navMap>\n"
        f"{points}"
        "  </navMap>\n"
        "</ncx>\n"
    )


def write_epub(path: str, sections: int, layout: str) -> None:
    parts = (sections + SECTIONS_PER_DOCUMENT - 1) // SECTIONS_PER_DOCUMENT
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        # The `mimetype` entry must be first and uncompressed.
        info = zipfile.ZipInfo("mimetype", ZIP_TIMESTAMP)
        info.compress_type = zipfile.ZIP_STORED
        archive.writestr(info, MIMETYPE)

        def add(name: str, text: str) -> None:
            info = zipfile.ZipInfo(name, ZIP_TIMESTAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, text)

        add("META-INF/container.xml", CONTAINER_XML)
        add("OEBPS/content.opf", package_document(sections, layout, parts))
        add("OEBPS/nav.xhtml", nav_document(parts))
        add("OEBPS/toc.ncx", ncx_document(sections, layout, parts))
        for part in range(parts):
            start = part * SECTIONS_PER_DOCUMENT
            end = min(start + SECTIONS_PER_DOCUMENT, sections)
            add(
                f"OEBPS/{part_name(part)}",
                part_document(range(start, end), layout),
            )


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate many-section EPUBs.")
    parser.add_argument("--out-dir", required=True, help="where the EPUBs are written")
    parser.add_argument(
        "--sections",
        type=int,
        nargs="+",
        default=[500, 2000, 8000, 20000],
        help="section counts, one EPUB per count and layout",
    )
    parser.add_argument(
        "--layouts",
        nargs="+",
        default=["nested", "flat"],
        choices=["nested", "flat"],
        help="section layouts",
    )
    args = parser.parse_args()

    os.makedirs(args.out_dir, exist_ok=True)
    for sections in args.sections:
        for layout in args.layouts:
            path = os.path.join(args.out_dir, f"sections-{layout}-{sections}.epub")
            write_epub(path, sections, layout)
            print(f"{path} {os.path.getsize(path)} bytes sections={sections} layout={layout}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
