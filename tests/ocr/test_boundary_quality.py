import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/ocr"))
from boundary_quality import alignment, boundaries, score


class BoundaryTests(unittest.TestCase):
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
