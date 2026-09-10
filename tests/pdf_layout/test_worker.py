"""Self-authored layout fixtures; no external PDFs or Python packages required."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("worker", Path(__file__).parents[2] / "src/parsing/pdf_layout_worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


def box(text, kind="text", x=10):
    return {"boxclass": kind, "x0": x, "y0": 10, "x1": x+100, "y1": 50,
            "textlines": [{"spans": [{"text": line, "bbox": [x, 10, x+100, 20], "size": 10, "flags": 0}]} for line in text.split("\n")]}


def layout(*pages):
    return {"page_count": len(pages), "pages": [{"page_number": i+1, "boxes": b} for i, b in enumerate(pages)]}


def ocr_line(text, y, confidence=None, x=40, width=420, size=8):
    span = {"text": text, "bbox": [x, y, x + width, y + size], "size": size, "flags": 12}
    if confidence is not None:
        span["confidence"] = confidence
    return {"bbox": [x, y, x + width, y + size], "spans": [span]}


def ocr_picture(lines, hidden_chars=2000, visible_chars=0, x0=20, y0=20, x1=560, y1=770):
    return {
        "boxclass": "picture", "x0": x0, "y0": y0, "x1": x1, "y1": y1,
        "textlines": lines,
        "ocr_text_layer": {"hidden_chars": hidden_chars, "visible_chars": visible_chars},
    }


def ocr_layout(*pages):
    return {
        "page_count": len(pages),
        "pages": [
            {"page_number": i + 1, "width": 581, "height": 794, "boxes": boxes}
            for i, boxes in enumerate(pages)
        ],
    }


class WorkerTests(unittest.TestCase):
    def test_genuine_hyphen_is_never_deleted_at_a_wrap(self):
        self.assertEqual(worker.join_lines(["English-", "to-German translation."], {"Englishto"})[0], "English-to-German translation.")
        self.assertEqual(worker.join_lines(["cross-", "page reference."], set())[0], "cross-page reference.")

    def test_discretionary_hyphen_requires_independent_evidence(self):
        self.assertEqual(worker.join_lines(["implemen-", "tation."], {"implementation"})[0], "implementation.")
        self.assertEqual(worker.join_lines(["implemen-", "tation."], set()), ("implemen-tation.", 1))
        self.assertEqual(worker.join_lines(["soft\u00ad", "ware."], set())[0], "software.")

    def test_same_line_hyphen_and_styling_boundary(self):
        self.assertEqual(worker.line_text({"spans": [
            {"text": "English-", "bbox": [0,0,30,10], "size": 10},
            {"text": "to-German", "bbox": [30,0,70,10], "size": 10},
            {"text": "text", "bbox": [74,0,90,10], "size": 10},
        ]}), "English-to-German text")

    def test_footer_never_interrupts_cross_page_sentence(self):
        result = worker.project(layout([box("Abstract", "section-header"), box("This continues"), box("Copyright 2026. All rights reserved.", "page-footer")], [box("across pages. Next sentence.")]))
        blocks = result["sections"][1]["blocks"]
        self.assertEqual(len(blocks), 1)
        self.assertEqual(blocks[0]["text"], "This continues across pages. Next sentence.")
        self.assertEqual(blocks[0]["parts"], [{"start":0,"end":14,"page":1},{"start":15,"end":43,"page":2}])
        self.assertEqual(result["sections"][-1]["blocks"][0]["kind"], "preformatted")

    def test_metadata_and_table_are_not_prose(self):
        result = worker.project(layout([box("A. Author. a@example.org"),box("Abstract", "section-header"),box("A complete sentence."),box("Count 2.3", "table"),box("", "picture")]))
        self.assertEqual(result["sections"][0]["blocks"][0]["kind"], "preformatted")
        self.assertEqual([b["kind"] for b in result["sections"][1]["blocks"]], ["paragraph", "table"])
        self.assertEqual(result["regions"][-1]["class"], "picture")

    def test_title_heading_does_not_promote_authors_to_prose(self):
        result = worker.project(layout([box("Paper title", "section-header"), box("A. Author. Department."), box("Abstract", "section-header"), box("Actual prose.")]))
        self.assertEqual(len(result["sections"]), 2)
        self.assertTrue(all(b["kind"] == "preformatted" for b in result["sections"][0]["blocks"]))

    def test_superscript_geometry_identifies_misclassified_footnotes(self):
        note=box("Note")
        note["textlines"][0]["spans"]=[{"text":"∗", "size":6,"flags":0,"bbox":[0,0,3,6],"origin":[0,5]}, {"text":"Author contribution.","size":9,"flags":0,"bbox":[4,0,90,10],"origin":[4,8]}]
        result=worker.project(layout([box("Abstract","section-header"),box("Prose."),note]))
        self.assertEqual(len(result["sections"][1]["blocks"]),1)
        self.assertEqual(result["sections"][-1]["title"],"Page notes and captions")

    def test_completed_paragraphs_remain_separate(self):
        result = worker.project(layout([box("Done.")], [box("another paragraph.")]))
        self.assertEqual(len(result["sections"][0]["blocks"]), 2)

    def test_external_ocr_promotes_regular_body_bands_but_keeps_unknown_regions_coarse(self):
        lines = [ocr_line(f"This is a regular OCR body line number {i} with enough words.", 30 + i * 10) for i in range(10)]
        lines.append(ocr_line("x y 2 4", 150, width=35))
        result = worker.project(ocr_layout([ocr_picture(lines)]))
        blocks = result["sections"][0]["blocks"]
        self.assertGreater(len([b for b in blocks if b["kind"] == "paragraph"]), 0)
        self.assertGreater(len([b for b in blocks if b["kind"] == "preformatted"]), 0)
        self.assertGreater(len(blocks), 1)
        self.assertEqual(result["ocr_pages"], [1])
        self.assertTrue(all(part["page"] == 1 for block in blocks for part in block["parts"]))
        self.assertTrue(all(len(block["text"]) < 1000 for block in blocks))

    def test_picture_chart_labels_are_not_promoted_without_body_evidence(self):
        lines = [ocr_line(f"label{i}", 30 + i * 12, width=40) for i in range(10)]
        result = worker.project(ocr_layout([ocr_picture(lines)]))
        self.assertFalse(any(b["kind"] == "paragraph" for s in result["sections"] for b in s["blocks"]))
        self.assertEqual(result["ocr_pages"], [])

    def test_low_confidence_and_unknown_picture_text_stays_unprojected(self):
        lines = [ocr_line(f"This is a low confidence OCR line number {i}.", 30 + i * 10, confidence=0.42) for i in range(10)]
        result = worker.project(ocr_layout([ocr_picture(lines)]))
        self.assertFalse(any(b["kind"] == "paragraph" for s in result["sections"] for b in s["blocks"]))

    def test_picture_without_hidden_text_layer_is_not_treated_as_external_ocr(self):
        lines = [ocr_line(f"This is visible-looking text line number {i} with enough words.", 30 + i * 10) for i in range(10)]
        result = worker.project(ocr_layout([ocr_picture(lines, hidden_chars=0)]))
        self.assertFalse(any(b["kind"] == "paragraph" for s in result["sections"] for b in s["blocks"]))

    def test_external_ocr_projection_keeps_page_mapping_for_multiple_pages(self):
        page_one = [ocr_line(f"Page one body line number {i} has enough words for projection.", 30 + i * 10) for i in range(8)]
        page_two = [ocr_line(f"Page two body line number {i} has enough words for projection.", 30 + i * 10) for i in range(8)]
        result = worker.project(ocr_layout([ocr_picture(page_one)], [ocr_picture(page_two)]))
        blocks = [b for s in result["sections"] for b in s["blocks"] if b["kind"] == "paragraph"]
        self.assertGreaterEqual(len(blocks), 2)
        self.assertEqual(set(part["page"] for block in blocks for part in block["parts"]), {1, 2})


if __name__ == "__main__":
    unittest.main()
