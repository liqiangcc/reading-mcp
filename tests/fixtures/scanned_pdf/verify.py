"""Fixture integrity only: authored gold, PDF text layers, hashes and geometry.

Run in hosted CI. No OCR, reading-mcp runtime or accuracy scoring is invoked.
"""
import hashlib
import json
from pathlib import Path

import pymupdf

ROOT = Path(__file__).resolve().parent


def compact(text):
    return "".join(text.split())


def main():
    manifest = json.loads((ROOT / "manifest.json").read_text(encoding="utf-8"))
    corpus = json.loads((ROOT / "corpus.json").read_text(encoding="utf-8"))
    assert manifest["ocr_executed"] is False
    assert len(manifest["cases"]) == 15
    assert sum(case["pages"] for case in manifest["cases"]) == 18
    for name, expected in manifest["files"].items():
        raw = (ROOT / name).read_bytes()
        assert len(raw) == expected["bytes"], name
        assert hashlib.sha256(raw).hexdigest() == expected["sha256"], name
    total_sentences = total_paragraphs = 0
    long_lines = None
    for case in manifest["cases"]:
        gold = json.loads((ROOT / case["gold"]).read_text(encoding="utf-8"))
        assert len(gold["paragraphs"]) == case["paragraphs"]
        assert "\n\n".join(p["text"] for p in gold["paragraphs"]) == gold["text"]
        with pymupdf.open(ROOT / case["pdf"]) as doc:
            assert len(doc) == case["pages"]
            for number, (page, page_gold) in enumerate(zip(doc, gold["pages"]), 1):
                assert page.rect == pymupdf.Rect(0, 0, 595, 842)
                assert page.rotation == 0 and page_gold["page"] == number
                paragraphs = [p for p in gold["paragraphs"] if p["page"] == number]
                expected = compact(" ".join(p["text"] for p in paragraphs))
                extracted = compact(page.get_text())
                mode = page_gold["mode"]
                if mode == "native" or mode.startswith("existing"):
                    assert extracted == expected, (case["id"], number, "text layer differs from gold")
                    hidden = sum(len(t["chars"]) for t in page.get_texttrace() if t["type"] == 3)
                    assert (hidden > 0) == mode.startswith("existing")
                else:
                    assert extracted == ("Page1" if mode == "scan_visible_footer" else ""), case["id"]
                    assert page.get_images(full=True), (case["id"], "scan must contain raster")
                if mode == "existing_form":
                    assert page.get_xobjects(), "F04-form must exercise real Form extraction"
                elif mode == "existing_flat":
                    assert not page.get_xobjects(), "flat control must not contain Forms"
            for order, paragraph in enumerate(gold["paragraphs"]):
                assert paragraph["source_order"] == order
                start, end = paragraph["source_range"]
                assert gold["text"][start:end] == paragraph["text"]
                sentences = [s["text"] for s in paragraph["sentences"]]
                if paragraph["id"] != "L1":
                    assert sentences == corpus["paragraphs"][paragraph["id"]]
                for sentence in paragraph["sentences"]:
                    left, right = sentence["source_range"]
                    assert start <= left < right <= end
                    assert gold["text"][left:right] == sentence["text"]
                last = 0
                for line in paragraph["lines"]:
                    left, right = line["paragraph_range"]
                    assert left >= last and paragraph["text"][last:left].strip() == ""
                    assert paragraph["text"][left:right] == line["text"]
                    x0, y0, x1, y1 = line["bbox"]
                    assert 0 <= x0 < x1 <= 595 and 0 <= y0 < y1 <= 842
                    last = right
                assert last == len(paragraph["text"])
                total_paragraphs += 1
                total_sentences += len(sentences)
                if case["id"] == "F14":
                    long_lines = len(paragraph["lines"])
                    assert long_lines > 12 and len(sentences) == 1
            if case["id"] == "F13":
                assert sum(len(p["lines"]) for p in gold["paragraphs"]) < 8
    reference = (ROOT / "previews/F02-p1.png").read_bytes()
    for name in ("F03", "F04-form", "F04-flat"):
        assert (ROOT / f"previews/{name}-p1.png").read_bytes() == reference
    print(json.dumps({"fixture_integrity": "pass", "cases": 15, "pages": 18,
                      "paragraphs": total_paragraphs, "sentences": total_sentences,
                      "F14_lines_one_sentence": long_lines, "ocr_executed": False,
                      "accuracy_claim": "none; Coordinator gold freeze pending"}))


if __name__ == "__main__":
    main()
