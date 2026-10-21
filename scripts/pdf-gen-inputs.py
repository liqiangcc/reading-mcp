#!/usr/bin/env python3
"""Generate the BENCH-D2 PDF inputs, their ground truth and the arXiv download list.

Standard library plus PyMuPDF (`pip install pymupdf==1.28.2`). Nothing large is committed: the
workflow runs this at CI time and uploads the results as an artifact.

Outputs in `--out`:

- `paper-2col-en.pdf` / `paper-2col-cjk.pdf`: synthetic two-column papers, 12 pages. Title,
  abstract, numbered section headings, body paragraphs flowing left column then right column, a
  running header, a footer with "Page N of 12", one footnote per page, one figure caption and one
  table (as text).
- `text-long-en.pdf` / `text-long-cjk.pdf`: single-column long text, 300 A4 pages.
- `scan-small.pdf`: the first 10 pages of `paper-2col-en.pdf` rendered at 150 dpi grayscale into
  an image-only PDF (no text layer).
- `<name>.gt.txt`: ground-truth text. For the two-column papers it is the body paragraphs only,
  in reading order. For the long texts it is every line that was placed on a page, in page order.
- `<name>.meta.json`: page count, header/footer/footnote strings (the strings a correct extractor
  must not put in the body), paragraph count and the ground-truth file name.
- `manifest.json`: sizes and metadata for every generated file, printed on stdout.

Real arXiv papers are not generated here: `arxiv_inputs()` pins the ids and licence requirements
so the workflow can download and verify them.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import random
import re
import sys
import time
import zipfile

MIB = 1024 * 1024
PAGE_W, PAGE_H = 595, 842  # A4
MARGIN = 50
HEADER_Y = 42
FOOTER_Y = 806
BODY_TOP = 62
BODY_BOTTOM = 762
FOOTNOTE_Y = 780
COL_GAP = 22
COL_W = (PAGE_W - 2 * MARGIN - COL_GAP) / 2
PAPER_PAGES = 12
LONG_PAGES = 300
SCAN_PAGES = 10
SCAN_DPI = 150
BODY_FONTSIZE = 9.0
HEADING_FONTSIZE = 11.0
LEADING = 11.5
CJK_FONT = "tests/fixtures/scanned_pdf/fonts/NotoSansCJKsc-Regular.otf"

# Public-domain sources, downloaded by the workflow; the generators fall back to synthetic text.
WAR_AND_PEACE_URL = "https://www.gutenberg.org/cache/epub/2600/pg2600.txt"
SONG_SHI_URL = "https://www.gutenberg.org/cache/epub/24183/pg24183-images.epub"

# Pinned arXiv inputs: (label, arxiv id, why this paper). Every id was checked to carry a CC-BY
# 4.0 licence on its abs page; the workflow re-checks the licence line at download time.
ARXIV_INPUTS = [
    ("arxiv-ml-2col", "2309.06180", "two-column ML systems paper (vLLM)"),
    ("arxiv-math-heavy", "2610.28527", "math-heavy single-column paper (math.NT)"),
    ("arxiv-many-figures", "2401.04088", "many-figure ML report (Mixtral)"),
]
ARXIV_LICENSE_MARK = "creativecommons.org/licenses/by/4.0"

CJK_CHARACTERS = (
    "的一是了我不人在他有这个上们来到时大地为子中你说生国年着就那和要她出也得里后自以会家可下而过"
    "天去能对小多然于心学么之都好看起发当没成只如事把还用第样道想作种开美总从无情己面最女但现前些"
    "所同日手又行意动方期它头经长儿回位分爱老因很给名法间斯知世什两次使身者被高已亲其进此话常与活"
    "正感金玉光明显达理思义信德智勇忠孝节仁礼山林川河湖海森木火土风雷雨雪云雾花鸟鱼虫龙凤虎豹马牛"
    "羊鸡犬禾苗稻麦米谷刀剑弓箭车船房屋门窗户墙壁桌椅床笔墨纸砚书画琴棋诗酒茶"
)
CJK_SENTENCE_ENDINGS = "。？！"
CJK_HEADINGS = [
    "引言", "相关工作", "方法", "实验设置", "结果与分析", "消融实验", "讨论", "结论",
]
EN_WORDS = [
    "the", "reading", "order", "column", "paragraph", "sentence", "layout", "baseline", "extractor",
    "page", "document", "evidence", "result", "measure", "latency", "memory", "corpus", "reader",
    "structure", "section", "abstract", "figure", "table", "caption", "footnote", "header",
    "quality", "alignment", "model", "text", "stream", "window", "token", "region", "boundary",
    "accuracy", "variance", "baseline", "profile", "host", "engine", "runtime", "cache",
]
EN_HEADINGS = [
    "Introduction", "Related Work", "Method", "Experimental Setup", "Results", "Ablation Study",
    "Discussion", "Conclusion",
]
PAPER_TITLE_EN = "A Synthetic Study of Two-Column Reading Order"
PAPER_TITLE_CJK = "双栏排版阅读顺序的合成研究"
HEADER_EN = "Preprint: A Synthetic Study of Two-Column Reading Order"
HEADER_CJK = "预印本：双栏排版阅读顺序的合成研究"


def write_json(path: str, value: dict) -> None:
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, ensure_ascii=False, indent=1, sort_keys=True)


def normalize(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def load_font(path: str, name: str):
    """`helv` uses the built-in Base14 font; anything else is embedded from a file."""
    import pymupdf

    if path == "helv":
        return pymupdf.Font("helv"), "helv", None
    return pymupdf.Font(fontfile=path), name, path


LATIN1_FOLD = {
    0x2018: "'", 0x2019: "'", 0x201A: "'", 0x201B: "'", 0x201C: '"', 0x201D: '"',
    0x201E: '"', 0x201F: '"', 0x2010: "-", 0x2011: "-", 0x2012: "-", 0x2013: "-",
    0x2014: "-", 0x2015: "-", 0x2212: "-", 0x2026: "...", 0x00A0: " ", 0x00AD: "",
    0x2022: "*", 0x2032: "'", 0x2033: '"', 0x00B4: "'", 0x02BC: "'", 0x02B9: "'",
}


def latin1_sanitize(text: str) -> str:
    """Fold typographic punctuation into Latin-1 so the Base14 font can encode every character.

    The default backend decodes Base14 text exactly; embedded Type0 fonts are the case under
    measurement, so the English inputs keep to the built-in font and a Latin-1 alphabet.
    """
    text = text.translate(LATIN1_FOLD)
    return "".join(char if ord(char) < 256 else "?" for char in text)


class Measurer:
    """Text width from cached per-character advances; `Font.text_length` is far too slow per word."""

    def __init__(self, font, fontsize: float) -> None:
        self.font = font
        self.fontsize = fontsize
        self.cache: dict[str, float] = {}

    def advance(self, char: str) -> float:
        width = self.cache.get(char)
        if width is None:
            width = self.font.glyph_advance(ord(char)) * self.fontsize
            self.cache[char] = width
        return width

    def width(self, text: str) -> float:
        return sum(self.advance(char) for char in text)


def wrap_lines(measurer: Measurer, text: str, width: float, cjk: bool) -> list[str]:
    """Greedy line wrapping by measured width; CJK breaks anywhere, Latin at spaces."""
    lines: list[str] = []
    if cjk:
        current = ""
        used = 0.0
        for char in text:
            step = measurer.advance(char)
            if current and used + step > width:
                lines.append(current)
                current, used = char, step
            else:
                current += char
                used += step
        if current:
            lines.append(current)
        return lines
    current = ""
    used = 0.0
    for word in text.split():
        step = measurer.width(word)
        extra = step if not current else step + measurer.advance(" ")
        if current and used + extra > width:
            lines.append(current)
            current, used = word, step
        else:
            current = word if not current else f"{current} {word}"
            used += extra
    if current:
        lines.append(current)
    return lines


def en_paragraph(rng: random.Random, sentences: int) -> str:
    parts = []
    for _ in range(sentences):
        words = [rng.choice(EN_WORDS) for _ in range(rng.randint(8, 18))]
        parts.append(" ".join(words).capitalize() + ".")
    return " ".join(parts)


def cjk_paragraph(rng: random.Random, sentences: int) -> str:
    parts = []
    for _ in range(sentences):
        chars = "".join(rng.choice(CJK_CHARACTERS) for _ in range(rng.randint(10, 26)))
        parts.append(chars + rng.choice(CJK_SENTENCE_ENDINGS))
    return "".join(parts)


class ColumnFlow:
    """Place blocks of text into a fixed two-column page grid, left column then right column."""

    def __init__(self, doc, font, fontname: str, fontfile: str | None, cjk: bool,
                 pages: int, start_y_page0: float) -> None:
        self.doc = doc
        self.font = font
        self.measurer = Measurer(font, BODY_FONTSIZE)
        self.fontname = fontname
        self.fontfile = fontfile
        self.cjk = cjk
        self.pages = pages
        self.columns: list[tuple[int, float, float]] = []
        for index in range(pages):
            top = start_y_page0 if index == 0 else BODY_TOP
            for column in range(2):
                self.columns.append((index, MARGIN + column * (COL_W + COL_GAP), top))
        self.reserved: dict[tuple[int, int], float] = {}
        self.inserted: set[int] = set()
        self.column_index = 0
        self.y = self.columns[0][2]

    def reserve(self, page: int, column: int, height: float) -> float:
        """Reserve the top `height` points of a column; returns the first usable y."""
        self.reserved[(page, column)] = height
        return self.columns[page * 2 + column][2] + height

    def page(self, page_index: int):
        page = self.doc[page_index]
        if self.fontfile and page_index not in self.inserted:
            page.insert_font(fontname=self.fontname, fontfile=self.fontfile)
            self.inserted.add(page_index)
        return page

    def _enter(self, column_index: int) -> None:
        self.column_index = column_index
        page_index, _, top = self.columns[column_index]
        self.y = top + self.reserved.get((page_index, column_index % 2), 0.0)

    def add_block(self, text: str, fontsize: float = BODY_FONTSIZE, gap: float = 4.0) -> bool:
        """Place one paragraph or heading; returns False when no column has room left."""
        lines = wrap_lines(self.measurer, text, COL_W, self.cjk)
        leading = LEADING if fontsize <= BODY_FONTSIZE else fontsize + 3.0
        need = len(lines) * leading + gap
        index = self.column_index
        while self.y + need > BODY_BOTTOM:
            index += 1
            if index >= len(self.columns):
                return False
            self._enter(index)
        for line in lines:
            page_index, x, _ = self.columns[self.column_index]
            self.page(page_index).insert_text((x, self.y), line, fontname=self.fontname,
                                              fontsize=fontsize)
            self.y += leading
        self.y += gap
        return True


def add_page_furniture(doc, font, fontname: str, fontfile: str | None, pages: int,
                       header: str) -> dict:
    """Header, footer with page number and one footnote per page; returns the noise strings."""
    footers: list[str] = []
    footnotes: list[str] = []
    for index in range(pages):
        page = doc[index]
        if fontfile:
            page.insert_font(fontname=fontname, fontfile=fontfile)
        page.insert_text((MARGIN, HEADER_Y), header, fontname=fontname, fontsize=7.5)
        footer = f"Page {index + 1} of {pages}"
        footers.append(footer)
        width = font.text_length(footer, 7.5)
        page.insert_text(((PAGE_W - width) / 2, FOOTER_Y), footer, fontname=fontname, fontsize=7.5)
        if "一" <= header[0] <= "鿿":
            note = (f"脚注{index + 1}：本页数据由合成生成器"
                    f"产生，仅用于基准测量。")
        else:
            note = (f"Note {index + 1}: this page's numbers come from the synthetic generator "
                    f"and are not measurements of a real system.")
        footnotes.append(note)
        page.insert_text((MARGIN, FOOTNOTE_Y), note, fontname=fontname, fontsize=6.5)
        page.draw_line((MARGIN, FOOTNOTE_Y - 8), (MARGIN + 120, FOOTNOTE_Y - 8), width=0.4)
    return {"header": header, "footers": footers, "footnotes": footnotes}


def build_paper(out_dir: str, name: str, cjk: bool, font_path: str | None,
                pages: int = PAPER_PAGES) -> dict:
    import pymupdf

    font, fontname, fontfile = load_font(font_path, "noto" if cjk else "dv")
    doc = pymupdf.open()
    for _ in range(pages):
        doc.new_page(width=PAGE_W, height=PAGE_H)
    rng = random.Random(0x2C01 if cjk else 0x2C01E)
    title = PAPER_TITLE_CJK if cjk else PAPER_TITLE_EN
    header = HEADER_CJK if cjk else HEADER_EN
    noise = add_page_furniture(doc, font, fontname, fontfile, pages, header)

    # Page 1: title, author line, full-width abstract.
    page1 = doc[0]
    if fontfile:
        page1.insert_font(fontname=fontname, fontfile=fontfile)
    page1.insert_text((MARGIN, 80), title, fontname=fontname, fontsize=15)
    authors = "作者甲，作者乙，作者丙" if cjk else "Author One, Author Two, Author Three"
    page1.insert_text((MARGIN, 98), authors, fontname=fontname, fontsize=9.5)
    abstract = ("摘要：" if cjk else "Abstract. ") + (
        cjk_paragraph(rng, 5) if cjk else en_paragraph(rng, 5)
    )
    page1.insert_textbox(
        pymupdf.Rect(MARGIN, 112, PAGE_W - MARGIN, 206), abstract,
        fontname=fontname, fontsize=8.5, align=0,
    )
    flow = ColumnFlow(doc, font, fontname, fontfile, cjk, pages, start_y_page0=218)

    # One figure caption (page 4, right column) and one table as text (page 7, right column).
    figure_top = flow.reserve(3, 1, 96.0)
    figure_page = flow.page(3)
    figure_page.draw_rect(
        pymupdf.Rect(MARGIN + COL_W + COL_GAP + 20, figure_top - 88,
                     MARGIN + COL_W + COL_GAP + COL_W - 20, figure_top - 14),
        color=(0.3, 0.3, 0.3), width=0.6,
    )
    caption = ("图1：不同页数下阅读顺序的准确率。" if cjk
               else "Figure 1: Reading-order accuracy as a function of page count.")
    figure_page.insert_text((MARGIN + COL_W + COL_GAP + 4, figure_top - 6), caption,
                            fontname=fontname, fontsize=8.0)

    table_top = flow.reserve(6, 1, 112.0)
    table_page = flow.page(6)
    table_lines = [
        "表1：合成阅读器的消融实验。" if cjk else "Table 1: Ablation of the synthetic reader.",
        "setting             acc     latency",
        "baseline            71.2    120 ms",
        "no-layout           68.9    118 ms",
        "no-header-strip     64.4    121 ms",
        "full                73.1    131 ms",
    ]
    table_y = table_top - 102
    for line in table_lines:
        table_page.insert_text((MARGIN + COL_W + COL_GAP + 4, table_y), line,
                               fontname=fontname, fontsize=7.5)
        table_y += 10

    # Body: numbered section headings and paragraphs, left column then right column, until the
    # last column is full. The last paragraph is only recorded when it was placed in full.
    headings = CJK_HEADINGS if cjk else EN_HEADINGS
    body_paragraphs: list[str] = []
    number = 0
    while True:
        number += 1
        heading = f"{number} {headings[(number - 1) % len(headings)]}"
        if not flow.add_block(heading, fontsize=HEADING_FONTSIZE, gap=3.0):
            break
        placed_any = False
        for _ in range(3):
            text = cjk_paragraph(rng, 4) if cjk else en_paragraph(rng, 4)
            if not flow.add_block(text):
                break
            body_paragraphs.append(normalize(text))
            placed_any = True
        if not placed_any:
            break

    pdf_path = os.path.join(out_dir, f"{name}.pdf")
    doc.save(pdf_path, garbage=3, deflate=True)
    doc.close()

    gt_path = os.path.join(out_dir, f"{name}.gt.txt")
    with open(gt_path, "w", encoding="utf-8") as handle:
        handle.write("\n\n".join(body_paragraphs) + "\n")
    meta = {
        "name": name,
        "kind": "paper-2col",
        "lang": "cjk" if cjk else "en",
        "page_count": pages,
        "paragraphs": len(body_paragraphs),
        "gt": os.path.basename(gt_path),
        "noise_strings": [noise["header"], *noise["footers"], *noise["footnotes"]],
        "noise_kinds": {"header": [noise["header"]], "footer": noise["footers"],
                        "footnote": noise["footnotes"]},
        "title": title,
    }
    write_json(os.path.join(out_dir, f"{name}.meta.json"), meta)
    return meta


def read_plain_text(path: str | None) -> str | None:
    if not path or not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8", errors="ignore") as handle:
        text = handle.read()
    start = re.search(r"\*\*\* ?START OF (THE|THIS) PROJECT GUTENBERG.*?\*\*\*", text)
    end = re.search(r"\*\*\* ?END OF (THE|THIS) PROJECT GUTENBERG", text)
    if start and end and start.end() < end.start():
        text = text[start.end():end.start()]
    text = normalize(latin1_sanitize(text))
    return text or None


def extract_epub_text(path: str | None) -> str | None:
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
    text = normalize("\n".join(piece.strip() for piece in pieces))
    cjk = sum(1 for ch in text[:200000] if "一" <= ch <= "鿿")
    return text if cjk > 1000 else None


def synthetic_cjk_text(chars: int) -> str:
    rng = random.Random(0xC1C1F)
    parts: list[str] = []
    total = 0
    while total < chars:
        sentence = "".join(rng.choice(CJK_CHARACTERS) for _ in range(rng.randint(8, 30)))
        parts.append(sentence + rng.choice(CJK_SENTENCE_ENDINGS))
        total += len(parts[-1])
    return "".join(parts)[:chars]


def build_long_text(out_dir: str, name: str, source: str, cjk: bool,
                    font_path: str | None, pages: int = LONG_PAGES) -> dict:
    """Single-column long text: fill each line to the text width, recording every placed line."""
    import pymupdf

    font, fontname, fontfile = load_font(font_path, "noto" if cjk else "dv")
    doc = pymupdf.open()
    for _ in range(pages):
        doc.new_page(width=PAGE_W, height=PAGE_H)
    text_width = PAGE_W - 2 * MARGIN
    lines_per_page = int((PAGE_H - 2 * MARGIN) // LEADING)
    measurer = Measurer(font, BODY_FONTSIZE)
    space = measurer.advance(" ")
    tokens = list(source) if cjk else source.split(" ")
    position = 0
    placed_pages: list[str] = []
    for index in range(pages):
        page = doc[index]
        if fontfile:
            page.insert_font(fontname=fontname, fontfile=fontfile)
        page_lines: list[str] = []
        y = MARGIN + LEADING
        for _ in range(lines_per_page):
            words: list[str] = []
            used = 0.0
            while True:
                if position >= len(tokens):
                    position = 0  # cycle the source when it is shorter than the page budget
                token = tokens[position]
                step = measurer.width(token) + (0.0 if (cjk or not words) else space)
                if words and used + step > text_width:
                    break
                words.append(token)
                used += step
                position += 1
            line = "".join(words) if cjk else " ".join(words)
            page.insert_text((MARGIN, y), line, fontname=fontname, fontsize=BODY_FONTSIZE)
            page_lines.append(line)
            y += LEADING
        placed_pages.append("".join(page_lines) if cjk else " ".join(page_lines))

    pdf_path = os.path.join(out_dir, f"{name}.pdf")
    doc.save(pdf_path, garbage=3, deflate=True)
    doc.close()

    # Ground truth: the placed lines in page order, one paragraph per page.
    gt_path = os.path.join(out_dir, f"{name}.gt.txt")
    with open(gt_path, "w", encoding="utf-8") as handle:
        handle.write("\n\n".join(placed_pages) + "\n")
    meta = {
        "name": name,
        "kind": "long",
        "lang": "cjk" if cjk else "en",
        "page_count": pages,
        "paragraphs": len(placed_pages),
        "gt": os.path.basename(gt_path),
        "noise_strings": [],
        "noise_kinds": {"header": [], "footer": [], "footnote": []},
        "title": name,
    }
    write_json(os.path.join(out_dir, f"{name}.meta.json"), meta)
    return meta


def build_scan(out_dir: str, source_pdf: str, name: str, pages: int = SCAN_PAGES) -> dict:
    import pymupdf

    src = pymupdf.open(source_pdf)
    out = pymupdf.open()
    for index in range(min(pages, src.page_count)):
        pix = src[index].get_pixmap(dpi=SCAN_DPI, colorspace=pymupdf.csGRAY, alpha=False)
        image = pix.tobytes("jpeg", jpg_quality=85)
        page = out.new_page(width=src[index].rect.width, height=src[index].rect.height)
        page.insert_image(page.rect, stream=image)
    src.close()
    pdf_path = os.path.join(out_dir, f"{name}.pdf")
    out.save(pdf_path, garbage=3, deflate=False)
    out.close()
    meta = {
        "name": name,
        "kind": "scan",
        "lang": "en",
        "page_count": pages,
        "dpi": SCAN_DPI,
        "paragraphs": 0,
        "gt": None,
        "noise_strings": [],
        "noise_kinds": {"header": [], "footer": [], "footnote": []},
        "title": name,
    }
    write_json(os.path.join(out_dir, f"{name}.meta.json"), meta)
    return meta


def download_arxiv(out_dir: str) -> dict:
    """Fetch the pinned arXiv PDFs at CI time and verify the CC-BY licence line on the abs page."""
    import hashlib
    import urllib.request

    status: dict = {}
    for entry in arxiv_inputs():
        path = os.path.join(out_dir, f"{entry['label']}.pdf")
        record: dict = {"id": entry["id"], "note": entry["note"]}
        try:
            page = fetch(urllib.request, entry["abs_url"], 120).decode("utf-8", "ignore")
            if entry["license_marker"] not in page:
                record["status"] = "licence_not_cc_by"
                status[entry["label"]] = record
                continue
            data = fetch(urllib.request, entry["pdf_url"], 600)
            if not data.startswith(b"%PDF"):
                record["status"] = "download_failed"
                record["error"] = "response is not a PDF"
                status[entry["label"]] = record
                continue
            with open(path, "wb") as handle:
                handle.write(data)
            record.update({"status": "ok", "bytes": len(data),
                           "sha256": hashlib.sha256(data).hexdigest()})
        except Exception as exc:  # noqa: BLE001 - a failed download is a recorded result
            record["status"] = "download_failed"
            record["error"] = f"{type(exc).__name__}: {exc}"
        status[entry["label"]] = record
    return status


def fetch(urllib_request, url: str, timeout: float, attempts: int = 3) -> bytes:
    last: Exception | None = None
    for attempt in range(attempts):
        try:
            request = urllib_request.Request(url, headers={"User-Agent": "reading-mcp-bench-d2"})
            with urllib_request.urlopen(request, timeout=timeout) as response:
                return response.read()
        except Exception as exc:  # noqa: BLE001 - retried below
            last = exc
            time.sleep(5 * (attempt + 1))
    raise RuntimeError(f"{url}: {last}")


def arxiv_inputs() -> list[dict]:
    return [
        {"label": label, "id": arxiv_id, "note": note, "license_marker": ARXIV_LICENSE_MARK,
         "pdf_url": f"https://arxiv.org/pdf/{arxiv_id}",
         "abs_url": f"https://arxiv.org/abs/{arxiv_id}"}
        for label, arxiv_id, note in ARXIV_INPUTS
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    parser.add_argument("--out", default="", help="output directory")
    parser.add_argument("--war-and-peace", default="", help="plain-text War and Peace (pg2600)")
    parser.add_argument("--song-shi", default="", help="Song history EPUB (pg24183)")
    parser.add_argument("--cjk-font", default=CJK_FONT)
    parser.add_argument("--pages-paper", type=int, default=PAPER_PAGES)
    parser.add_argument("--pages-long", type=int, default=LONG_PAGES)
    parser.add_argument("--download-arxiv", default="",
                        help="download the pinned arXiv PDFs into this directory and exit")
    args = parser.parse_args()
    if args.download_arxiv:
        os.makedirs(args.download_arxiv, exist_ok=True)
        status = download_arxiv(args.download_arxiv)
        write_json(os.path.join(args.download_arxiv, "arxiv-status.json"), status)
        print(json.dumps(status, ensure_ascii=False, indent=1, sort_keys=True))
        return 0
    if not args.out:
        parser.error("--out is required unless --download-arxiv is used")
    os.makedirs(args.out, exist_ok=True)
    if not os.path.isfile(args.cjk_font):
        raise SystemExit(f"CJK font not found: {args.cjk_font}")

    manifest: dict = {"inputs": {}, "arxiv": arxiv_inputs(),
                      "sources": {"war_and_peace": WAR_AND_PEACE_URL,
                                  "song_shi": SONG_SHI_URL}}

    def record(meta: dict) -> None:
        path = os.path.join(args.out, f"{meta['name']}.pdf")
        meta = dict(meta)
        meta["bytes"] = os.path.getsize(path)
        manifest["inputs"][meta["name"]] = meta

    record(build_paper(args.out, "paper-2col-en", cjk=False, font_path="helv",
                       pages=args.pages_paper))
    record(build_paper(args.out, "paper-2col-cjk", cjk=True, font_path=args.cjk_font,
                       pages=args.pages_paper))

    war = read_plain_text(args.war_and_peace)
    if war is None:
        manifest["sources"]["war_and_peace_synthetic"] = True
        war = normalize(" ".join(
            "Pierre walked along the river while the winter light fell on the old house."
            for _ in range(20000)
        ))
    record(build_long_text(args.out, "text-long-en", war, cjk=False, font_path="helv",
                           pages=args.pages_long))

    song = extract_epub_text(args.song_shi)
    if song is None:
        manifest["sources"]["song_shi_synthetic"] = True
        song = synthetic_cjk_text(300 * 1000)
    record(build_long_text(args.out, "text-long-cjk", song, cjk=True, font_path=args.cjk_font,
                           pages=args.pages_long))

    record(build_scan(args.out, os.path.join(args.out, "paper-2col-en.pdf"), "scan-small"))

    write_json(os.path.join(args.out, "manifest.json"), manifest)
    print(json.dumps(manifest, ensure_ascii=False, indent=1, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
