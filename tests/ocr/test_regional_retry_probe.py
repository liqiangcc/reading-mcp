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
        worker["prepare_page_raster"] = lambda page: SimpleNamespace(n=3, width=1, height=1, samples=b"\0\0\0")
        clock = [100.0]
        worker["time"] = SimpleNamespace(monotonic=lambda: clock[0])
        return worker, clock

    def test_primary_and_retry_share_page_deadline(self):
        worker, clock = self.deadline_worker()
        primary = [self.box([10,10,20,20], "primary", 1),
                   self.box([12,12,14,14], "overlap", 2)]
        remaining = []
        rasters = []
        def observe(page, excluded_regions=()):
            remaining.append(worker["page_time_remaining"]())
            rasters.append(worker["PAGE_RASTER"])
            clock[0] += 14 if len(remaining) == 1 else .5
            return primary if len(remaining) == 1 else [self.box([10,10,20,20], "retry", 1)]
        worker["ocr_page"] = observe
        worker["_regional_ocr"](SimpleNamespace(number=0, rect=SimpleNamespace(width=100, height=100)))
        self.assertEqual(remaining, [15, 1])
        self.assertIsNone(worker["PAGE_DEADLINE"])
        self.assertIs(rasters[0], rasters[1])
        self.assertIsNone(worker["PAGE_RASTER"])

    def test_white_raster_skips_engine_but_a_single_nonwhite_sample_does_not(self):
        worker, _ = self.deadline_worker()
        white = SimpleNamespace(n=3, width=1, height=1, samples=b"\xff\xff\xff")
        page = SimpleNamespace(number=0, rect=SimpleNamespace(width=1, height=1), get_texttrace=lambda: [])
        worker["prepare_page_raster"] = lambda page: white
        def unexpected(*args, **kwargs):
            self.fail("blank raster must not invoke OCR")
        worker["ocr_page"] = unexpected
        selected, observed = worker["_regional_ocr"](page)
        self.assertEqual(selected, [])
        self.assertEqual(observed["attempts"], [])
        self.assertEqual(observed["blank_raster"]["white_samples"], 3)
        white.samples = b"\xff\xfe\xff"
        self.assertIsNone(worker["blank_raster_evidence"](page, white))
        white.samples = b"\xff\xff\xff"
        page.get_texttrace = lambda: [{"type": 3}]
        self.assertIsNone(worker["blank_raster_evidence"](page, white))

    def test_native_coverage_masks_only_bound_glyph_geometry_and_keeps_unknown_ink(self):
        worker, _ = self.deadline_worker()
        bbox = [0, .01, .1, .2]
        page = SimpleNamespace(rotation=0, rect=SimpleNamespace(width=2.4, height=.24),
            get_texttrace=lambda: [{'bbox':bbox, 'chars':[(ord('X'), 0, (0,0), bbox)]}])
        samples = bytearray(b'\xff' * 30)
        samples[0:3] = b'\0' * 3
        samples[24:27] = b'\0' * 3
        pix = SimpleNamespace(n=3, width=10, height=1, samples=bytes(samples))
        regions = [{'source_box':7, 'source_class':'text', 'bbox':[0,0,.2,.24], 'text':'X'}]
        result = worker['native_raster_coverage'](page, pix, regions)
        self.assertEqual(result['uncovered_samples'], 3)
        self.assertEqual(result['masks'][0]['source_box'], 7)
        self.assertEqual(result['masks'][0]['pixel_bbox'], [0,0,3,1])
        self.assertEqual(pix.samples, bytes(samples), 'inspection must not change the OCR image')
        regions[0]['text'] = 'Y'
        self.assertEqual(worker['native_raster_coverage'](page, pix, regions)['uncovered_samples'], 6)
        regions[0]['text'] = 'X'
        samples[24:27] = b'\xff' * 3
        pix.samples = bytes(samples)
        self.assertEqual(worker['native_raster_coverage'](page, pix, regions)['uncovered_samples'], 0)

    def test_disabled_mixed_coverage_needs_no_engine_and_preserves_visual_regions(self):
        worker, _ = self.deadline_worker()
        worker['OCR_CONFIG'] = {'enabled': False}
        bbox = [0, .01, .1, .2]
        page = SimpleNamespace(rotation=0, rect=SimpleNamespace(width=2.4, height=.24),
            get_image_info=lambda: [{}],
            get_texttrace=lambda: [{'bbox':bbox, 'chars':[(ord('X'), 0, (0,0), bbox)]}])
        pixels = bytearray(b'\xff' * 30)
        pixels[0:3] = b'\0' * 3
        pixels[24:27] = b'\0' * 3
        pixmap = SimpleNamespace(n=3, width=10, height=1, samples=bytes(pixels))
        worker['prepare_page_raster'] = lambda page, dpi: pixmap
        worker['ocr_page'] = lambda *args, **kwargs: self.fail('disabled must not invoke OCR')
        native = {'boxclass':'text', 'x0':0, 'y0':0, 'x1':.2, 'y1':.24,
                  'textlines':[{'spans':[{'text':'X', 'bbox':bbox, 'size':12, 'flags':0}]}]}
        layout = {'boxes':[native]}
        with self.assertRaises(worker['OcrRequired']):
            worker['require_disabled_page_coverage'](page, layout)
        self.assertIsNone(worker['PAGE_DEADLINE'])
        visual = {'boxclass':'picture', 'x0':1.68, 'y0':0, 'x1':2.4, 'y1':.24, 'textlines':[]}
        layout['boxes'].append(visual)
        original = copy.deepcopy(layout)
        worker['require_disabled_page_coverage'](page, layout)
        self.assertEqual(layout, original)
        self.assertEqual(pixmap.samples, bytes(pixels))
        # A region which misses the unknown pixel must not authorize publication.
        visual['x0'] = 2.16
        with self.assertRaises(worker['OcrRequired']):
            worker['require_disabled_page_coverage'](page, layout)
        layout['boxes'] = [native]
        pixels[24:27] = b'\xff' * 3
        pixmap.samples = bytes(pixels)
        worker['require_disabled_page_coverage'](page, layout)
        self.assertIsNone(worker['PAGE_DEADLINE'])

    def test_disabled_covered_native_return_cannot_escape_page_deadline(self):
        for previous, elapsed in [(None, 15), (104., 4)]:
            worker, clock = self.deadline_worker()
            worker['OCR_CONFIG'] = {'enabled':False}
            worker['PAGE_DEADLINE'] = previous
            worker['has_native_body'] = lambda layout: True
            worker['native_text_regions'] = lambda layout: []
            worker['prepare_page_raster'] = lambda page, dpi: None
            def covered(*args, **kwargs):
                clock[0] += elapsed
                return {'uncovered_samples':0, 'masks':[{}]}
            worker['native_raster_coverage'] = covered
            with self.assertRaisesRegex(RuntimeError, 'shared 15 second budget'):
                worker['require_disabled_page_coverage'](SimpleNamespace(get_image_info=lambda:[{}]), {})
            self.assertEqual(worker['PAGE_DEADLINE'], previous)

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

    def test_total_raster_budget_counts_allocations_and_rejects_before_reservation(self):
        worker, _ = self.deadline_worker()
        budget = worker["OcrRasterBudget"]()
        for page in range(2):
            budget.require_page(page)
            budget.reserve_raster(16_000_000)
            budget.reserve_raster(16_000_000)
        self.assertEqual(budget.pixels, 64_000_000)
        self.assertEqual(budget.pages, {0, 1})
        with self.assertRaisesRegex(RuntimeError, "64 million"):
            budget.reserve_raster(1)
        self.assertEqual(budget.pixels, 64_000_000)
        self.assertEqual(budget.pages, {0, 1})

    def test_required_page_limit_does_not_double_count_retry(self):
        worker, _ = self.deadline_worker()
        budget = worker["OcrRasterBudget"]()
        for page in range(8):
            budget.require_page(page)
            budget.reserve_raster(100)
            budget.require_page(page)
            budget.reserve_raster(100)
        with self.assertRaisesRegex(RuntimeError, "8 required page"):
            budget.require_page(8)
        self.assertEqual(len(budget.pages), 8)
        self.assertEqual(budget.pixels, 1600)

    def test_inspection_rasters_do_not_consume_required_recognition_slots(self):
        worker, _ = self.deadline_worker()
        budget = worker["OcrRasterBudget"]()
        for _ in range(10):
            budget.reserve_raster(100)
        self.assertEqual(budget.pages, set())
        self.assertEqual(budget.pixels, 1000)
        for page in range(8):
            budget.require_page(page)
        with self.assertRaisesRegex(RuntimeError, "8 required page"):
            budget.require_page(8)

    def run_worker_retry(self, primary, retry):
        worker = {}
        exec((Path(__file__).parents[2] / "src/parsing/pdf_layout_worker.py").read_text(), worker)
        worker["OCR_CONFIG"] = {"enabled": True, "psm": 3, "dpi": 300}
        worker["prepare_page_raster"] = lambda page: SimpleNamespace(n=3, width=1, height=1, samples=b"\0\0\0")
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

    def mixed_example(self, reverse_native=False):
        worker, clock = self.deadline_worker()
        primary = [self.box([10,y,20,y+10], text, i+1)
                   for i, (y,text) in enumerate([(10,'A'),(30,'engine B'),(50,'C'),(70,'engine D')])]
        native = [self.box([10,30,20,40], 'native B', 2), self.box([10,70,20,80], 'native D', 4)]
        if reverse_native:
            native.reverse()
        original = [{'boxclass':'picture'}] + native
        regions = [{'source_box':i+1,'source_class':'text','bbox':box['bbox'],
                    'text':box['textlines'][0]['spans'][0]['text']} for i,box in enumerate(native)]
        evidence = worker['_regional_evidence'](1, [0,0,100,100], primary, None, primary, [])
        selected, evidence = worker['exclude_native_boxes'](primary, evidence, regions)
        return worker, clock, original, selected, evidence

    def test_mixed_order_preserves_native_text_nontext_and_immutable_source_ids(self):
        worker, _, original, selected, evidence = self.mixed_example()
        before, raw_before = copy.deepcopy(original), copy.deepcopy(evidence['attempts'])
        ordered = worker['merge_native_order'](original, selected, evidence)
        self.assertEqual([box['_original_box'] for box in ordered], [0,3,1,4,2])
        self.assertEqual([box['textlines'][0]['spans'][0]['text'] for box in ordered if box.get('textlines')],
                         ['A','native B','C','native D'])
        self.assertEqual([entry['origin'] for entry in evidence['mixed_order']['entries']],
                         ['local_ocr','native','local_ocr','native'])
        self.assertEqual(original, before)
        self.assertEqual(evidence['attempts'], raw_before)

    def test_mixed_order_rejects_conflicting_native_order_and_missing_anchors(self):
        worker, _, original, selected, evidence = self.mixed_example(True)
        worker['merge_native_order'](original, selected, evidence)
        self.assertFalse(evidence['complete'])
        self.assertEqual(evidence['projection_failure'], 'unproven_mixed_order')
        worker, _, original, selected, evidence = self.mixed_example()
        original.append(self.box([10,90,20,100], 'unanchored native', 5))
        evidence['native_regions'].append({'source_box':3,'source_class':'text','bbox':[10,90,20,100], 'text':'unanchored native'})
        worker['merge_native_order'](original, selected, evidence)
        self.assertFalse(evidence['complete'])

    def test_mixed_order_uses_remaining_page_deadline(self):
        worker, clock, original, selected, evidence = self.mixed_example()
        worker['PAGE_DEADLINE'] = clock[0]
        with self.assertRaisesRegex(RuntimeError, 'shared 15 second budget'):
            worker['merge_native_order'](original, selected, evidence)

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

    def test_native_exclusion_uses_layout_coordinates_and_preserves_raw_words(self):
        worker, _ = self.deadline_worker()
        native = {"boxclass": "page-footer", "x0": 0, "y0": 0, "x1": 20, "y1": 10,
                  "textlines": [{"spans": [{"text": "Original footer", "bbox": [0,0,20,10]}]}]}
        regions = worker["native_text_regions"]({"boxes": [{"boxclass":"picture","textlines":None}, native]})
        self.assertEqual(regions[0]["bbox"], [0,0,20,10])
        self.assertEqual(regions[0]["source_box"], 1)
        primary = [self.box([2,2,8,8], "Different OCR spelling", 1),
                   self.box([30,30,40,40], "body", 2)]
        original = copy.deepcopy(primary)
        evidence = worker["_regional_evidence"](1, [0,0,100,100], primary, None, primary, [])
        selected, evidence = worker["exclude_native_boxes"](primary, evidence, regions)
        self.assertEqual(primary, original)
        self.assertEqual(selected, [primary[1]])
        self.assertEqual(evidence["attempts"][0]["boxes"], original)
        self.assertEqual(evidence["excluded_sources"], [{"page":1,"attempt":"primary","box":0}])
        self.assertEqual(evidence["selection"][0]["selected_box"], 0)
        self.assertEqual(evidence["selection"][0]["source"]["box"], 1)

    def test_partial_native_overlap_is_explicitly_unresolved(self):
        worker, _ = self.deadline_worker()
        box = self.box([1,1,30,9], "first", 1)
        box["textlines"][0]["spans"] = [{"text":"first","bbox":[2,2,8,8]}, {"text":"second","bbox":[22,2,28,8]}]
        original = copy.deepcopy(box)
        evidence = worker["_regional_evidence"](1, [0,0,100,100], [box], None, [box], [])
        selected, evidence = worker["exclude_native_boxes"]([box], evidence,
            [{"source_box":0,"source_class":"text","text":"native","bbox":[0,0,10,10]}])
        self.assertFalse(evidence["complete"])
        self.assertEqual(evidence["projection_failure"], "partial_native_overlap")
        self.assertEqual(selected, [original])
        self.assertEqual(evidence["attempts"][0]["boxes"], [original])

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
