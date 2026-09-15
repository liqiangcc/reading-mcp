import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from projection_quality import (BOUNDARY_MIN, COVERAGE_MIN, DETERMINISM_RUNS,
                                gold_sentence_ranges, score_case)


def paragraph(text, sentences=(), eligible=True, section="Document", page=1):
    return {"section": section, "page": page, "eligible": eligible,
            "content_class": "native_paragraph" if eligible else "preformatted",
            "text": text, "sentences": list(sentences)}


def observed(paragraph_gold):
    """Materialize the report shape for a gold paragraph, exactly tiled."""
    text = paragraph_gold["text"]
    sentences = []
    cursor = 0
    for sentence in paragraph_gold["sentences"]:
        start = text.index(sentence, cursor)
        sentences.append({"text": sentence, "start": start,
                          "end": start + len(sentence)})
        cursor = start + len(sentence)
    page = paragraph_gold["page"]
    return {"section": paragraph_gold["section"], "text": text,
            "source_order": 0, "eligible": paragraph_gold["eligible"],
            "content_class": paragraph_gold["content_class"],
            "exact_read": True,
            "page": None if page is None else
            {"kind": "page", "page_number": page},
            "sentences": sentences}


def entry(paragraphs, hashes=("sha256:a",) * 3, reads=None, codes=()):
    total = sum(1 + len(p["sentences"]) for p in paragraphs)
    return {"normalized_hashes": list(hashes),
            "unit_fingerprints": ["sha256:u"] * len(hashes),
            "exact_reads": reads or {"total": total, "equal": total},
            "degradation_codes": list(codes),
            "paragraphs": paragraphs}


GOLD_P = paragraph(
    "Alpha holds. Beta holds.",
    ["Alpha holds.", "Beta holds."])


class GoldTilingTests(unittest.TestCase):
    def test_gold_ranges_tile_exactly(self):
        self.assertEqual(gold_sentence_ranges(GOLD_P), [(0, 12), (13, 24)])

    def test_gold_omission_or_bad_sentence_is_rejected(self):
        bad = paragraph("Alpha holds. Beta holds.", ["Alpha holds.", "Gamma."])
        with self.assertRaises(ValueError):
            gold_sentence_ranges(bad)
        bad = paragraph("Alpha holds. Beta holds.", ["Alpha holds."])
        with self.assertRaises(ValueError):
            gold_sentence_ranges(bad)


class ScoreCaseTests(unittest.TestCase):
    def gold(self, paragraphs, codes=()):
        return {"paragraphs": paragraphs,
                "required_degradation_codes": list(codes)}

    def test_exact_match_passes(self):
        result = score_case("T", entry([observed(GOLD_P)]), self.gold([GOLD_P]))
        self.assertEqual(result["violations"], [])
        self.assertEqual(result["boundary"]["precision"], 1.0)
        self.assertEqual(result["sentence_readable_coverage"], 23 / 24)

    def test_wrong_merge_fails(self):
        merged = observed(GOLD_P)
        merged["sentences"] = [{"text": GOLD_P["text"], "start": 0,
                                "end": len(GOLD_P["text"])}]
        result = score_case("T", entry([merged]), self.gold([GOLD_P]))
        self.assertEqual(result["counts"]["wrong_merge"], 1)
        self.assertTrue(result["violations"])

    def test_wrong_split_fails(self):
        split = observed(GOLD_P)
        split["sentences"] = [
            {"text": "Alpha holds.", "start": 0, "end": 12},
            {"text": "Beta", "start": 13, "end": 17},
            {"text": " holds.", "start": 17, "end": 24}]
        result = score_case("T", entry([split]), self.gold([GOLD_P]))
        self.assertEqual(result["counts"]["wrong_split"], 1)
        self.assertTrue(result["violations"])

    def test_omitted_paragraph_and_sentence_fail(self):
        result = score_case("T", entry([]), self.gold([GOLD_P]))
        self.assertGreater(result["counts"]["omission"], 0)
        empty = observed(GOLD_P)
        empty["sentences"] = []
        result = score_case("T", entry([empty]), self.gold([GOLD_P]))
        self.assertGreater(result["counts"]["omission"], 0)
        self.assertLess(result["boundary"]["recall"], BOUNDARY_MIN)

    def test_duplicated_paragraph_and_overlapping_sentence_fail(self):
        result = score_case("T", entry([observed(GOLD_P), observed(GOLD_P)]),
                            self.gold([GOLD_P]))
        self.assertGreater(result["counts"]["duplication"], 0)
        dup = observed(GOLD_P)
        dup["sentences"].append({"text": "Beta holds.", "start": 13, "end": 24})
        result = score_case("T", entry([dup]), self.gold([GOLD_P]))
        self.assertGreater(result["counts"]["duplication"], 0)

    def test_metadata_contamination_fails(self):
        meta = paragraph("Conference Paper Title", (), eligible=False,
                         section="Front matter")
        meta["metadata"] = True
        contaminated = observed(GOLD_P)
        contaminated["text"] = "Conference Paper Title Alpha holds. Beta holds."
        contaminated["sentences"] = [
            {"text": "Conference Paper Title Alpha holds.", "start": 0,
             "end": 36},
            {"text": "Beta holds.", "start": 37, "end": 48}]
        result = score_case(
            "T", entry([observed(meta), contaminated]),
            self.gold([meta, GOLD_P]))
        self.assertGreater(result["counts"]["metadata_contamination"], 0)

    def test_metadata_promoted_to_eligible_fails(self):
        meta = paragraph("Conference Paper Title", (), eligible=False,
                         section="Front matter")
        meta["metadata"] = True
        promoted = observed(meta)
        promoted["eligible"] = True
        result = score_case("T", entry([promoted]), self.gold([meta]))
        self.assertGreater(result["counts"]["metadata_contamination"], 0)

    def test_nondeterminism_and_inexact_reads_fail(self):
        result = score_case(
            "T", entry([observed(GOLD_P)],
                       hashes=("sha256:a", "sha256:b", "sha256:a")),
            self.gold([GOLD_P]))
        self.assertTrue(result["violations"])
        result = score_case(
            "T", entry([observed(GOLD_P)],
                       reads={"total": 3, "equal": 2}),
            self.gold([GOLD_P]))
        self.assertTrue(result["violations"])

    def test_low_coverage_fails(self):
        sparse = paragraph("Alpha holds. " + "word " * 30 + "tail.",
                           ["Alpha holds.", "word " * 30 + "tail."])
        partial = observed(sparse)
        partial["sentences"] = partial["sentences"][:1]
        result = score_case("T", entry([partial]), self.gold([sparse]))
        self.assertLess(result["sentence_readable_coverage"], COVERAGE_MIN)
        self.assertTrue(result["violations"])

    def test_missing_required_degradation_fails(self):
        result = score_case(
            "T", entry([observed(GOLD_P)]),
            self.gold([GOLD_P], codes=["pdf_ambiguous_hyphens_preserved"]))
        self.assertTrue(result["violations"])
        result = score_case(
            "T", entry([observed(GOLD_P)],
                       codes=["pdf_ambiguous_hyphens_preserved"]),
            self.gold([GOLD_P], codes=["pdf_ambiguous_hyphens_preserved"]))
        self.assertEqual(result["violations"], [])

    def test_non_exact_region_without_codes_fails(self):
        degraded = observed(GOLD_P)
        degraded["exact_read"] = False
        result = score_case("T", entry([degraded]), self.gold([GOLD_P]))
        self.assertTrue(result["violations"])
        result = score_case(
            "T", entry([degraded], codes=["pdf_layout_inferred"],
                       reads={"total": 3, "equal": 3}),
            self.gold([GOLD_P]))
        self.assertEqual(result["violations"], [])

    def test_wrong_page_binding_fails(self):
        wrong = observed(GOLD_P)
        wrong["page"] = {"kind": "page", "page_number": 2}
        result = score_case("T", entry([wrong]), self.gold([GOLD_P]))
        self.assertTrue(result["violations"])

    def test_determinism_run_count_is_enforced(self):
        result = score_case(
            "T", entry([observed(GOLD_P)], hashes=("sha256:a",) * 2),
            self.gold([GOLD_P]))
        self.assertTrue(result["violations"])
        self.assertEqual(DETERMINISM_RUNS, 3)


if __name__ == "__main__":
    unittest.main()
