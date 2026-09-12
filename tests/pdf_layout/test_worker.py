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


if __name__ == "__main__":
    unittest.main()
