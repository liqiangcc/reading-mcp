import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/ocr"))
from boundary_quality import alignment, boundaries, score, reading_order


class BoundaryTests(unittest.TestCase):
    def test_order_detects_swapped_paragraphs_without_changing_actual(self):
        actual = ["Bravo sentence.", "Alpha sentence.", "Charlie sentence."]
        before = actual.copy()
        result = reading_order(["Alpha sentence.", "Bravo sentence.", "Charlie sentence."], actual)
        self.assertEqual(actual, before)
        self.assertEqual(result["actual_to_reference"], [1, 0, 2])
        self.assertEqual(result["concordant_pairs"], 2)
        self.assertEqual(result["expected_pairs"], 3)
        self.assertLess(result["score"], 1)

    def test_order_omissions_and_ambiguous_duplicates_cannot_pass(self):
        missing = reading_order(["Alpha.", "Bravo.", "Charlie."], ["Alpha.", "Charlie."])
        self.assertFalse(missing["proven"])
        self.assertEqual(missing["expected_pairs"], 3)
        self.assertIsNone(missing["score"])
        ambiguous = reading_order(["Same.", "Same."], ["Same.", "Same."])
        self.assertFalse(ambiguous["proven"])
        self.assertEqual(ambiguous["optimal_assignments"], 2)

    def test_order_accepts_unique_matching_with_recognition_error(self):
        result = reading_order(["Alpha.", "Bravo."], ["Alxha.", "Bravo."])
        self.assertTrue(result["proven"])
        self.assertEqual(result["score"], 1)
        self.assertEqual(result["minimum_edit_cost"], 1)

    def test_interior_character_substitution_does_not_change_boundary(self):
        mapping, errors = alignment("abc. def.", "axc. def.")
        self.assertEqual(errors, 1)
        self.assertEqual(score([4, 9], [4, 9], mapping)["f1"], 1)

    def test_artificial_split_and_duplicate_prediction_are_penalized(self):
        mapping, _ = alignment("abcd", "abcd")
        self.assertLess(score([4], [2, 4], mapping)["f1"], .95)
        self.assertLess(score([4], [4, 4], mapping)["f1"], .95)

    def test_missing_boundary_is_penalized(self):
        mapping, _ = alignment("abcd", "abcd")
        self.assertLess(score([2, 4], [4], mapping)["f1"], .95)

    def test_actual_ranges_cannot_omit_or_rewrite_text(self):
        for sentence in ({"text": "bc.", "start": 1, "end": 4},
                         {"text": "xyz.", "start": 0, "end": 4}):
            with self.assertRaises(ValueError):
                boundaries([{"text": "abc.", "sentences": [sentence]}])

    def test_cjk_adjacent_sentences_keep_true_character_offsets(self):
        text, paragraphs, sentences = boundaries([{"text": "甲。乙。", "sentences": [
            {"text": "甲。", "start": 0, "end": 2}, {"text": "乙。", "start": 2, "end": 4}]}])
        self.assertEqual((text, paragraphs, sentences), ("甲。乙。", [4], [2, 4]))


if __name__ == "__main__":
    unittest.main()
