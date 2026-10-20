#!/usr/bin/env python3
"""Generate the BENCH-S stress inputs: big synthetic text files and big PDFs.

Standard library plus PyMuPDF (`pip install pymupdf fonttools`). Outputs go into `--out`:

- cjk-16mib.txt, cjk-64mib.txt: one-section synthetic Chinese prose.
- en-64mib.txt: one-section synthetic ASCII prose.
- pdf-text-1500p.pdf: War and Peace plain text laid out on about 1,500 A4 pages (11 pt).
- pdf-text-cjk-1000p.pdf: Chinese text (the Song history EPUB if its text is extractable, else
  synthetic CJK) laid out on about 1,000 pages with the bundled Noto Sans CJK font.
- pdf-scan-300p.pdf: the first 300 pages of pdf-text-1500p.pdf rendered to grayscale JPEG
  images and wrapped in an image-only PDF (no text layer). Its size must stay under 128 MiB.

A manifest with every output's size is printed as JSON on stdout. Standard inputs are checked,
not trusted: a missing optional source falls back to synthetic text.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import random
import re
import sys
import zipfile

MIB = 1024 * 1024
DOCUMENT_LIMIT = 128 * MIB

CJK_CHARACTERS = (
    "的一是了我不人在他有这个上们来到时大地为子中你说生国年着就那和要她出也得里后自以会家可下而过"
    "天去能对小多然于心学么之都好看起发当没成只如事把还用第样道想作种开美总从无情己面最女但现前些"
    "所同日手又行意动方期它头经长儿回位分爱老因很给名法间斯知世什两次使身者被高已亲其进此话常与活"
    "正感金玉光明显达理思义信德智勇忠孝节仁礼山林川河湖海森木火土风雷雨雪云雾花鸟鱼虫龙凤虎豹马牛"
    "羊鸡犬禾苗稻麦米谷刀剑弓箭车船房屋门窗户墙壁桌椅床笔墨纸砚书画琴棋诗酒茶"
)
CJK_SENTENCE_ENDINGS = "。？！"


def write_cjk(path: str, size_bytes: int) -> None:
    """Deterministic one-section Chinese prose, exactly `size_bytes` bytes."""
    rng = random.Random(0xC1C1E)
    chunks: list[bytes] = []
    total = 0
    while total < size_bytes:
        paragraph = "".join(
            "".join(rng.choice(CJK_CHARACTERS) for _ in range(rng.randint(8, 30)))
            + rng.choice(CJK_SENTENCE_ENDINGS)
            for _ in range(rng.randint(4, 8))
        )
        block = (paragraph + "\n\n").encode("utf-8")
        chunks.append(block)
        total += len(block)
    # Trim to size, then drop a partial trailing UTF-8 sequence.
    data = b"".join(chunks)[:size_bytes].decode("utf-8", errors="ignore").encode("utf-8")
    with open(path, "wb") as handle:
        handle.write(data)
        gap = size_bytes - len(data)
        if gap > 0:
            handle.write(b"\n" * gap)


def write_ascii(path: str, size_bytes: int) -> None:
    """Deterministic one-section English prose, exactly `size_bytes` bytes."""
    rng = random.Random(0xA5C11)
    words = [
        "the", "river", "house", "letter", "winter", "garden", "silence", "market", "window",
        "history", "memory", "reader", "page", "chapter", "quiet", "distant", "lantern", "ledger",
        "harbor", "theory", "morning", "candle", "archive", "bridge", "stone", "orchard",
    ]
    total = 0
    with open(path, "wb") as handle:
        while total < size_bytes:
            sentence_words = [rng.choice(words) for _ in range(rng.randint(8, 22))]
            sentence = " ".join(sentence_words).capitalize() + ". "
            if rng.random() < 0.15:
                sentence += "\n\n"
            block = sentence.encode("ascii")
            remaining = size_bytes - total
            if len(block) > remaining:
                block = block[:remaining]
            handle.write(block)
            total += len(block)


def extract_epub_text(path: str) -> str | None:
    """Return the visible text of an EPUB's XHTML parts, or None if it has no CJK text."""
    if not path or not os.path.isfile(path):
        return None
    pieces: list[str] = []
    with zipfile.ZipFile(path) as archive:
        for name in sorted(archive.namelist()):
            if not name.lower().endswith((".xhtml", ".html", ".htm")):
                continue
            raw = archive.read(name).decode("utf-8", errors="ignore")
            raw = re.sub(r"(?is)<(script|style)[^>]*>.*?</\1>", " ", raw)
            raw = re.sub(r"(?i)<br\s*/?>|</p>|</div>|</h[1-6]>", "\n", raw)
            text = html.unescape(re.sub(r"<[^>]+>", "", raw))
            pieces.append(re.sub(r"[ \t\r\f\v]+", " ", text))
    text = "\n".join(piece.strip() for piece in pieces)
    text = re.sub(r"\n{3,}", "\n\n", text).strip()
    cjk = sum(1 for ch in text[:200000] if "一" <= ch <= "鿿")
    return text if cjk > 1000 else None


def read_plain_text(path: str | None) -> str | None:
    if not path or not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8", errors="ignore") as handle:
        text = handle.read()
    # Gutenberg plain text: drop the license header and footer if present.
    start = re.search(r"\*\*\* ?START OF (THE|THIS) PROJECT GUTENBERG.*?\*\*\*", text)
    end = re.search(r"\*\*\* ?END OF (THE|THIS) PROJECT GUTENBERG", text)
    if start and end and start.end() < end.start():
        text = text[start.end():end.start()]
    return re.sub(r"\s+", " ", text).strip()


def synthetic_cjk_text(chars: int) -> str:
    rng = random.Random(0xC1C1F)
    parts: list[str] = []
    total = 0
    while total < chars:
        sentence = "".join(rng.choice(CJK_CHARACTERS) for _ in range(rng.randint(8, 30)))
        sentence += rng.choice(CJK_SENTENCE_ENDINGS)
        parts.append(sentence)
        total += len(sentence)
    return "".join(parts)[:chars]


def paginate(text: str, chars_per_page: int, pages: int) -> list[str]:
    """Split `text` into `pages` chunks, cycling through it when it runs out."""
    if not text:
        raise SystemExit("no source text for PDF generation")
    chunks: list[str] = []
    position = 0
    for _ in range(pages):
        if position >= len(text):
            position = 0
        chunks.append(text[position:position + chars_per_page])
        position += chars_per_page
    return chunks


def write_text_pdf(path: str, chunks: list[str], fontsize: float, font_path: str | None) -> None:
    import fitz  # PyMuPDF

    doc = fitz.open()
    margin = 50
    font_name = "helv"
    for chunk in chunks:
        page = doc.new_page(width=595, height=842)  # A4
        rect = fitz.Rect(margin, margin, 595 - margin, 842 - margin)
        if font_path:
            font_name = "noto"
            page.insert_font(fontname=font_name, fontfile=font_path)
        text = chunk
        # insert_textbox inserts nothing when the text overflows, so shorten and retry on the page.
        while text:
            left = page.insert_textbox(rect, text, fontsize=fontsize, fontname=font_name, align=0)
            if left >= 0:
                break
            text = text[: int(len(text) * 0.9)]
    if font_path:
        try:
            doc.subset_fonts()
        except Exception as exc:  # noqa: BLE001 - subsetting only shrinks the file
            print(f"::warning::font subsetting skipped: {exc}", file=sys.stderr)
    doc.save(path, garbage=3, deflate=True)
    doc.close()


def write_scan_pdf(source: str, path: str, pages: int) -> dict:
    """Render `pages` pages of `source` as grayscale JPEG images in an image-only PDF."""
    import fitz

    last_size = 0
    for dpi in (150, 120, 100, 80):
        out = fitz.open()
        src = fitz.open(source)
        for index in range(min(pages, src.page_count)):
            pix = src[index].get_pixmap(dpi=dpi, colorspace=fitz.csGRAY, alpha=False)
            image = pix.tobytes("jpeg", jpg_quality=85)
            page = out.new_page(width=src[index].rect.width, height=src[index].rect.height)
            page.insert_image(page.rect, stream=image)
        src.close()
        out.save(path, garbage=3, deflate=False)
        out.close()
        last_size = os.path.getsize(path)
        if last_size < DOCUMENT_LIMIT:
            return {"dpi": dpi, "bytes": last_size}
    raise SystemExit(f"scan PDF still {last_size} bytes at the lowest dpi")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    parser.add_argument("--out", required=True, help="output directory")
    parser.add_argument("--war-and-peace", default="", help="plain-text War and Peace (pg2600)")
    parser.add_argument("--song-shi", default="", help="Song history EPUB (pg24183)")
    parser.add_argument(
        "--cjk-font",
        default="tests/fixtures/scanned_pdf/fonts/NotoSansCJKsc-Regular.otf",
    )
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)
    manifest: dict[str, dict] = {}

    def record(name: str) -> None:
        path = os.path.join(args.out, name)
        manifest[name] = {"bytes": os.path.getsize(path)}

    write_cjk(os.path.join(args.out, "cjk-16mib.txt"), 16 * MIB)
    record("cjk-16mib.txt")
    write_cjk(os.path.join(args.out, "cjk-64mib.txt"), 64 * MIB)
    record("cjk-64mib.txt")
    write_ascii(os.path.join(args.out, "en-64mib.txt"), 64 * MIB)
    record("en-64mib.txt")

    war = read_plain_text(args.war_and_peace)
    if not war:
        war = " ".join(
            "Pierre and Natasha walked along the river while the winter light fell on the old house."
            for _ in range(60000)
        )
        manifest["war-and-peace-source"] = {"synthetic": True}
    chars_en = 2000
    text_pdf = os.path.join(args.out, "pdf-text-1500p.pdf")
    write_text_pdf(text_pdf, paginate(war, chars_en, 1500), fontsize=11, font_path=None)
    record("pdf-text-1500p.pdf")

    font = args.cjk_font if os.path.isfile(args.cjk_font) else None
    if font is None:
        raise SystemExit(f"CJK font not found: {args.cjk_font}")
    song = extract_epub_text(args.song_shi)
    if song is None:
        song = synthetic_cjk_text(1800 * 1000)
        manifest["song-shi-source"] = {"synthetic": True}
    write_text_pdf(
        os.path.join(args.out, "pdf-text-cjk-1000p.pdf"),
        paginate(song, 1000, 1000),
        fontsize=11,
        font_path=font,
    )
    record("pdf-text-cjk-1000p.pdf")

    scan = write_scan_pdf(text_pdf, os.path.join(args.out, "pdf-scan-300p.pdf"), 300)
    manifest["pdf-scan-300p.pdf"] = scan
    record("pdf-scan-300p.pdf")

    print(json.dumps(manifest, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
