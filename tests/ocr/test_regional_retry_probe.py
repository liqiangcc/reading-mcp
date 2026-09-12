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
        rebuilt = probe.rebuild_boxes(original, [(2, {0, 2}, [{"id": "retry"}])])
        self.assertEqual(rebuilt, [{"id": "retry"}, {"id": 1}, {"id": 3}, {"id": 4}])
        self.assertEqual(original, [{"id": i} for i in range(5)])

if __name__ == "__main__": unittest.main()
