"""Author deterministic synthetic PDF/gold assets; never call OCR or application code.

This is fixture construction, not a recognizer or a benchmark. Gold comes from
corpus.json before rendering; no extracted/OCR output is used as its own oracle.
Run with --output pointing at a new fixture root (fonts are copied as needed).
"""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import shutil

import pymupdf as fitz

SOURCE = Path(__file__).resolve().parent
DESIGN_SHA = "76f449061b20b6c8d79b57b871ce67ac1cfa7c6b"
FONT_HASHES = {
    "DejaVuSans.ttf": "ae7b7855e115a5966d8b1b3f80f254ccc117ec86f9965e202ee2940453837280",
    "NotoSansCJKsc-Regular.otf": "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b",
}
WIDTH, HEIGHT, SIZE, LEADING = 595, 842, 12, 18


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def paragraph_text(identifier, sentences):
    return ("" if identifier.startswith("C") else " ").join(sentences)


def wrap(text, font, width, chinese):
    start = 0
    while start < len(text):
        end = start
        while end < len(text) and font.text_length(text[start:end + 1], fontsize=SIZE) <= width:
            end += 1
        if end == start:
            raise ValueError("glyph exceeds fixture column")
        if end < len(text) and not chinese:
            space = text.rfind(" ", start, end + 1)
            if space <= start:
                raise ValueError("English fixture word exceeds column")
            end = space
        yield start, end
        start = end
        while start < len(text) and text[start] == " ":
            start += 1


def author(columns, corpus, fonts, extra=None, hidden=False):
    doc = fitz.open()
    page = doc.new_page(width=WIDTH, height=HEIGHT)
    paragraphs, regions = [], []
    for identifiers, x0, x1 in columns:
        baseline = 72
        for identifier in identifiers:
            sentences = corpus[identifier]
            text = paragraph_text(identifier, sentences)
            chinese = identifier.startswith("C")
            name = "goldcjk" if chinese else "goldeng"
            font = fonts[name]
            page.insert_font(fontname=name, fontfile=str(SOURCE / "fonts" / ("NotoSansCJKsc-Regular.otf" if chinese else "DejaVuSans.ttf")))
            lines = []
            for start, end in wrap(text, font, x1 - x0, chinese):
                if baseline + SIZE * abs(font.descender) >= HEIGHT - 48:
                    raise ValueError("fixture paragraph does not fit declared page")
                line = text[start:end]
                page.insert_text((x0, baseline), line, fontname=name, fontsize=SIZE, render_mode=3 if hidden else 0)
                bbox = [x0, baseline - font.ascender * SIZE,
                        x0 + font.text_length(line, fontsize=SIZE), baseline - font.descender * SIZE]
                lines.append({"text": line, "paragraph_range": [start, end],
                              "bbox": [round(v, 6) for v in bbox], "baseline": baseline})
                baseline += LEADING
            paragraphs.append({"id": identifier, "text": text, "sentences_text": sentences,
                               "font": name, "lines": lines})
            baseline += 18
    if extra == "chart":
        page.draw_rect(fitz.Rect(100, 540, 125, 610), color=(0, 0, 0), fill=(0, 0, 0))
        page.draw_rect(fitz.Rect(180, 520, 205, 610), color=(0, 0, 0), fill=(0, 0, 0))
        for x, y, text in [(100, 635, "A"), (180, 635, "B"), (180, 515, "12"), (100, 680, "x²+y²=z²")]:
            page.insert_text((x, y), text, fontname="goldeng", fontsize=SIZE)
        # Include below-chart labels in the chart evidence, not only bar extents.
        regions.extend([{"kind": "chart", "bbox": [48, 500, 547, 650], "expected": "coarse_or_visual"},
                        {"kind": "formula", "bbox": [48, 660, 547, 700], "expected": "coarse_or_visual"}])
    if extra == "footer":
        page.insert_text((48, 815), "Page 1", fontname="goldeng", fontsize=SIZE)
        regions.append({"kind": "footer", "bbox": [48, 798, 120, 822], "text": "Page 1", "expected": "not_body_prose"})
    return doc, paragraphs, regions


def raster_page(native, dpi=300):
    doc = fitz.open()
    page = doc.new_page(width=WIDTH, height=HEIGHT)
    png = native[0].get_pixmap(dpi=dpi, colorspace=fitz.csRGB, alpha=False).tobytes("png")
    if dpi != 300:
        # F10: low-resolution information upsampled to the declared 300-DPI raster.
        low = fitz.open()
        low.new_page(width=WIDTH, height=HEIGHT).insert_image(page.rect, stream=png)
        png = low[0].get_pixmap(dpi=300, colorspace=fitz.csRGB, alpha=False).tobytes("png")
        low.close()
    page.insert_image(page.rect, stream=png)
    return doc


def add_flat_hidden(page, paragraphs):
    # Direct page content streams; no Form XObject. Coordinates match the native gold.
    for para in paragraphs:
        name = para["font"]
        filename = "DejaVuSans.ttf" if name == "goldeng" else "NotoSansCJKsc-Regular.otf"
        page.insert_font(fontname=name, fontfile=str(SOURCE / "fonts" / filename))
        for line in para["lines"]:
            page.insert_text((line["bbox"][0], line["baseline"]), line["text"],
                             fontname=name, fontsize=SIZE, render_mode=3)


def build(output):
    if importlib.metadata.version("pymupdf") != "1.28.2":
        raise ValueError("fixture generation requires PyMuPDF 1.28.2")
    if (output / "manifest.json").exists() or (output / "pdf").exists():
        raise ValueError("refuse to overwrite existing generated/frozen fixtures")
    for name, expected in FONT_HASHES.items():
        if sha(SOURCE / "fonts" / name) != expected:
            raise ValueError("font identity mismatch: " + name)
    output.mkdir(parents=True, exist_ok=True)
    if output.resolve() != SOURCE.resolve():
        shutil.copytree(SOURCE / "fonts", output / "fonts")
        for name in ("corpus.json", "generate.py"):
            shutil.copyfile(SOURCE / name, output / name)
    for folder in ("pdf", "gold", "previews"):
        (output / folder).mkdir()
    source = json.loads((SOURCE / "corpus.json").read_text(encoding="utf-8"))
    corpus = source["paragraphs"]
    long = source["long_sentence"]
    corpus["L1"] = [long["join"].join(long["clause"].replace("N", str(i)) for i in range(1, 49)) + long["terminal"]]
    fonts = {"goldeng": fitz.Font(fontfile=str(SOURCE / "fonts/DejaVuSans.ttf")),
             "goldcjk": fitz.Font(fontfile=str(SOURCE / "fonts/NotoSansCJKsc-Regular.otf"))}
    english = list(source["paragraphs"])[0:6]
    single = lambda ids: [(ids, 48, 547)]
    columns = [(english[:3], 48, 285), (english[3:], 310, 547)]
    # Each tuple is (mode, columns, extra). No source classification is inferred from OCR.
    cases = {
        "F01": [("native", single(english), None)],
        "F02": [("scan", single(english), None)],
        "F03": [("existing_flat", single(english), None)],
        "F04-form": [("existing_form", single(english), None)],
        "F04-flat": [("existing_flat", single(english), None)],
        "F05": [("native", single(english[:2]), None), ("scan", single(english[2:4]), None),
                ("existing_flat", single(english[4:]), None), ("blank", [], None)],
        "F06": [("scan", columns, None)],
        "F07": [("scan", single(["C1", "C2", "C3", "C4"]), None)],
        "F08": [("scan", single(["E1", "C1", "E2", "C2"]), None)],
        "F09": [("blank", [], None)],
        "F10": [("low_quality", single(english), None)],
        "F11": [("scan", columns, "chart")],
        "F12": [("scan_visible_footer", single(english), None)],
        "F13": [("scan", single(["E1"]), None)],
        "F14": [("scan", single(["L1"]), None)],
    }
    records = []
    for identifier, specs in cases.items():
        result = fitz.open()
        gold = {"schema": "scanned-pdf-gold/v1", "fixture": identifier, "paragraphs": [], "pages": [], "text": ""}
        for number, (mode, cols, extra) in enumerate(specs, 1):
            native, paragraphs, regions = author(cols, corpus, fonts, extra)
            if mode == "native":
                derived = native
            else:
                derived = raster_page(native, 100 if mode == "low_quality" else 300)
                if mode == "existing_flat":
                    add_flat_hidden(derived[0], paragraphs)
                elif mode == "existing_form":
                    hidden, _, _ = author(cols, corpus, fonts, hidden=True)
                    derived[0].show_pdf_page(derived[0].rect, hidden, 0)
                    hidden.close()
                elif mode == "scan_visible_footer":
                    derived[0].insert_font(fontname="goldeng", fontfile=str(SOURCE / "fonts/DejaVuSans.ttf"))
                    derived[0].insert_text((48, 815), "Page 1", fontname="goldeng", fontsize=SIZE)
                    regions.append({"kind": "footer", "bbox": [48, 798, 120, 822], "text": "Page 1", "expected": "not_body_prose"})
            result.insert_pdf(derived)
            if derived is not native:
                derived.close()
            native.close()
            expected = "native" if mode == "native" else "existing_ocr" if mode.startswith("existing") else "blank" if mode == "blank" else "requires_ocr"
            gold["pages"].append({"page": number, "mode": mode, "expected_class": expected,
                                  "width_points": WIDTH, "height_points": HEIGHT, "rotation": 0,
                                  "raster_dpi": None if mode == "native" else 300,
                                  "regions": regions})
            for para in paragraphs:
                if gold["text"]:
                    gold["text"] += "\n\n"
                start = len(gold["text"])
                gold["text"] += para["text"]
                cursor, sentences = start, []
                for sentence in para.pop("sentences_text"):
                    at = gold["text"].index(sentence, cursor)
                    sentences.append({"text": sentence, "source_range": [at, at + len(sentence)]})
                    cursor = at + len(sentence)
                para.update(page=number, source_order=len(gold["paragraphs"]),
                            source_range=[start, len(gold["text"])], sentences=sentences)
                gold["paragraphs"].append(para)
        result.set_metadata({"title": identifier, "author": "reading-mcp synthetic fixtures", "producer": "fixture-author/v1"})
        (output / "pdf" / (identifier + ".pdf")).write_bytes(result.tobytes(garbage=4, deflate=True, no_new_id=True))
        previews = []
        for number, page in enumerate(result, 1):
            name = f"previews/{identifier}-p{number}.png"
            (output / name).write_bytes(page.get_pixmap(dpi=96, colorspace=fitz.csRGB, alpha=False).tobytes("png"))
            previews.append(name)
        result.close()
        write_json(output / "gold" / (identifier + ".json"), gold)
        records.append({"id": identifier, "pdf": f"pdf/{identifier}.pdf", "gold": f"gold/{identifier}.json",
                        "pages": len(specs), "paragraphs": len(gold["paragraphs"]),
                        "sentences": sum(len(p["sentences"]) for p in gold["paragraphs"]), "previews": previews})
    # Contact sheets are mechanical previews of the authored PDFs, not OCR results.
    for offset in range(0, len(records), 6):
        sheet = fitz.open()
        page = sheet.new_page(width=900, height=900)
        for i, record in enumerate(records[offset:offset + 6]):
            x, y = (i % 3) * 300, (i // 3) * 450
            page.insert_text((x + 12, y + 20), record["id"], fontsize=12)
            page.insert_image(fitz.Rect(x + 8, y + 30, x + 292, y + 440), filename=str(output / record["previews"][0]))
        (output / f"previews/contact-{offset // 6 + 1}.png").write_bytes(page.get_pixmap().tobytes("png"))
        sheet.close()
    files = {}
    for path in sorted(output.rglob("*")):
        if path.is_file() and (path.parent.name in {"fonts", "pdf", "gold", "previews"} or path.name in {"corpus.json", "generate.py"}):
            files[str(path.relative_to(output))] = {"sha256": sha(path), "bytes": path.stat().st_size}
    manifest = {"schema": "scanned-pdf-fixture-manifest/v1", "design_sha": DESIGN_SHA,
                "generator": "fixture-author/v1", "pymupdf": "1.28.2", "mupdf": fitz.VersionFitz,
                "coordinate_space": "original-page-points-top-left/v1; gold ranges are Unicode scalars in gold.text",
                "ocr_executed": False, "acceptance": "pending Coordinator SHA freeze; not OCR accuracy evidence",
                "cases": records, "files": files}
    write_json(output / "manifest.json", manifest)
    (output / "SHA256SUMS").write_text("".join(f"{value['sha256']}  {path}\n" for path, value in files.items())
                                       + f"{sha(output / 'manifest.json')}  manifest.json\n", encoding="utf-8")
    print(json.dumps({"cases": len(records), "pages": sum(r["pages"] for r in records),
                      "manifest_sha256": sha(output / "manifest.json"), "ocr_executed": False}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    build(parser.parse_args().output)
