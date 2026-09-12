import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("regional_retry_probe", Path(__file__).parents[2] / "scripts/ocr/regional_retry_probe.py")
probe = importlib.util.module_from_spec(spec); spec.loader.exec_module(probe)

class RegionalGeometryTests(unittest.TestCase):
    def test_bridge_pairs_form_one_component(self):
        self.assertEqual(probe.merge_components([(0, 2), (2, 4)]), [{0, 2, 4}])

    def test_rebuild_skips_noncontiguous_members(self):
        original = [{"id": i} for i in range(5)]
        rebuilt = probe.rebuild_boxes(original, [(0, {0, 2}, [{"id": "retry"}])])
        self.assertEqual(rebuilt, [{"id": "retry"}, {"id": 1}, {"id": 3}, {"id": 4}])
        self.assertEqual(original, [{"id": i} for i in range(5)])

    def test_expansion_bridge_closes_components(self):
        self.assertEqual(probe.close_components([{0, 1}, {2, 3}, {1, 2}]), [{0, 1, 2, 3}])

    def test_multiple_candidates_keep_nonmembers(self):
        original = [{"id": i} for i in range(6)]
        rebuilt = probe.rebuild_boxes(original, [(1, {1, 3}, [{"id": "a"}, {"id": "b"}])])
        self.assertEqual([x["id"] for x in rebuilt], [0, "a", "b", 2, 4, 5])
        with self.assertRaises(ValueError):
            probe.rebuild_boxes(original, [(0, {0, 1}, []), (1, {1, 2}, [])])

if __name__ == "__main__": unittest.main()
