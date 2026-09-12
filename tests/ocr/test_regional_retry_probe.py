import importlib.util
from pathlib import Path
import unittest
import copy
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location("regional_retry_probe", Path(__file__).parents[2] / "scripts/ocr/regional_retry_probe.py")
probe = importlib.util.module_from_spec(spec); spec.loader.exec_module(probe)

class RegionalGeometryTests(unittest.TestCase):
    def deadline_worker(self):
        worker = {}
        exec((Path(__file__).parents[2] / "src/parsing/pdf_layout_worker.py").read_text(), worker)
        worker["OCR_CONFIG"] = {"enabled": True, "psm": 3, "dpi": 300}
        clock = [100.0]
        worker["time"] = SimpleNamespace(monotonic=lambda: clock[0])
        return worker, clock

    def test_primary_and_retry_share_page_deadline(self):
        worker, clock = self.deadline_worker()
        primary = [self.box([10,10,20,20], "primary", 1),
                   self.box([12,12,14,14], "overlap", 2)]
        remaining = []
        def observe(page, excluded_regions=()):
            remaining.append(worker["page_time_remaining"]())
            clock[0] += 14 if len(remaining) == 1 else .5
            return primary if len(remaining) == 1 else [self.box([10,10,20,20], "retry", 1)]
        worker["ocr_page"] = observe
        worker["_regional_ocr"](SimpleNamespace(number=0, rect=SimpleNamespace(width=100, height=100)))
        self.assertEqual(remaining, [15, 1])
        self.assertIsNone(worker["PAGE_DEADLINE"])

    def test_exhausted_primary_never_starts_retry_and_restores_state(self):
        worker, clock = self.deadline_worker()
        calls = []
        def observe(page, excluded_regions=()):
            calls.append(worker["OCR_CONFIG"]["psm"])
            clock[0] += 15
            return [self.box([10,10,20,20], "primary", 1),
                    self.box([12,12,14,14], "overlap", 2)]
        worker["ocr_page"] = observe
        with self.assertRaisesRegex(RuntimeError, "shared 15 second budget"):
            worker["_regional_ocr"](SimpleNamespace(number=0, rect=SimpleNamespace(width=100, height=100)))
        self.assertEqual(calls, [3])
        self.assertEqual(worker["OCR_CONFIG"]["psm"], 3)
        self.assertIsNone(worker["PAGE_DEADLINE"])

    def test_raster_preflight_rejects_oversized_and_invalid_pages(self):
        worker, _ = self.deadline_worker()
        def pixels(x1, y1):
            return worker["raster_pixel_count"](
                SimpleNamespace(rect=SimpleNamespace(x0=0, y0=0, x1=x1, y1=y1)), 300)
        self.assertEqual(pixels(72, 72), 90_000)
        for x1, y1 in [(2000, 2000), (0, 72), (float("nan"), 72)]:
            with self.assertRaises(RuntimeError):
                pixels(x1, y1)

    def test_total_raster_budget_counts_retry_and_rejects_before_reservation(self):
        worker, _ = self.deadline_worker()
        budget = worker["OcrRasterBudget"]()
        for page in range(2):
            budget.reserve(page, 16_000_000)
            budget.reserve(page, 16_000_000)  # retry is a real second allocation
        self.assertEqual(budget.pixels, 64_000_000)
        self.assertEqual(budget.pages, {0, 1})
        with self.assertRaisesRegex(RuntimeError, "64 million"):
            budget.reserve(2, 1)
        self.assertEqual(budget.pixels, 64_000_000)
        self.assertEqual(budget.pages, {0, 1})

    def test_required_page_limit_does_not_double_count_retry(self):
        worker, _ = self.deadline_worker()
        budget = worker["OcrRasterBudget"]()
        for page in range(8):
            budget.reserve(page, 100)
            budget.reserve(page, 100)
        with self.assertRaisesRegex(RuntimeError, "8 required page"):
            budget.reserve(8, 100)
        self.assertEqual(len(budget.pages), 8)
        self.assertEqual(budget.pixels, 1600)

    def run_worker_retry(self, primary, retry):
        worker = {}
        exec((Path(__file__).parents[2] / "src/parsing/pdf_layout_worker.py").read_text(), worker)
        worker["OCR_CONFIG"] = {"enabled": True, "psm": 3, "dpi": 300}
        calls = []
        def observe(page, excluded_regions=()):
            calls.append(worker["OCR_CONFIG"]["psm"])
            return primary if calls[-1] == 3 else retry
        worker["ocr_page"] = observe
        selected, evidence = worker["_regional_ocr"](
            SimpleNamespace(number=0, rect=SimpleNamespace(width=100, height=100)))
        self.assertEqual(worker["OCR_CONFIG"]["psm"], 3)
        return selected, evidence, calls

    @staticmethod
    def box(bbox, text, block):
        return {"bbox": bbox, "ocr_block": block,
                "textlines": [{"bbox": bbox, "spans": [{"bbox": bbox, "text": text}]}]}

    def test_production_retry_retains_raw_observations_and_references(self):
        primary = [self.box([10,10,20,20], "primary", 1),
                   self.box([10,50,20,60], "unrelated", 2),
                   self.box([12,12,14,14], "overlap", 3)]
        before = copy.deepcopy(primary)
        retry = [self.box([10,10,20,20], "retry", 1)]
        selected, evidence, calls = self.run_worker_retry(primary, retry)
        self.assertEqual(calls, [3,6])
        self.assertEqual(primary, before)
        self.assertEqual(selected, [retry[0], primary[1]])
        self.assertTrue(evidence["complete"])
        self.assertEqual(evidence["attempts"][0]["boxes"], before)
        self.assertEqual(evidence["selection"][0]["source"],
                         {"page":1,"attempt":"retry","box":0})
        self.assertEqual(evidence["selection"][1]["source"]["box"], 1)
        self.assertEqual([ref["box"] for ref in evidence["components"][0]["replaced_refs"]], [0,2])

    def test_production_uncovered_component_is_incomplete(self):
        primary = [self.box([10,10,20,20], "primary", 1),
                   self.box([12,12,14,14], "overlap", 2)]
        selected, evidence, calls = self.run_worker_retry(primary, [])
        self.assertEqual(calls, [3,6])
        self.assertEqual(selected, primary)
        self.assertFalse(evidence["complete"])
        self.assertEqual(evidence["components"][0]["replaced_refs"], [])

    def test_production_nonconflicting_page_never_retries(self):
        primary = [self.box([10,10,20,20], "primary", 1)]
        selected, evidence, calls = self.run_worker_retry(primary, [])
        self.assertEqual(calls, [3])
        self.assertEqual(selected, primary)
        self.assertEqual(len(evidence["attempts"]), 1)
        self.assertTrue(evidence["complete"])

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
