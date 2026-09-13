"""Pure source geometry tests, no models or gold supplied to the adapter."""
import copy
from pathlib import Path
import unittest


class VisualProjectionTests(unittest.TestCase):
    def setUp(self):
        self.worker = {}
        exec((Path(__file__).parents[2] / 'src/parsing/pdf_layout_worker.py').read_text(), self.worker)

    def box(self, bounds, text, block):
        return {'boxclass':'text', 'bbox':bounds, 'x0':bounds[0], 'y0':bounds[1],
                'x1':bounds[2], 'y1':bounds[3], 'ocr_block':block, 'ocr_paragraph':1,
                'textlines':[{'spans':[{'text':text, 'bbox':bounds, 'flags':0,
                    'ocr_block':block, 'ocr_paragraph':1, 'ocr_line':1, 'confidence':71.25}]}]}

    def apply(self, boxes, predictions):
        return self.worker['project_visual_observations'](1, [0,0,100,100], [1000,1000], boxes, predictions)

    def test_real_words_become_coarse_without_reordering_or_changing_raw_observations(self):
        boxes = [self.box([1,1,8,8], 'Source prose.', 1), self.box([22,22,25,25], '12', 2),
                 self.box([52,52,65,65], 'x?+y?', 3)]
        model = [{'label':'image', 'score':.7, 'coordinate':[200,200,300,300]},
                 {'label':'formula', 'score':.6, 'coordinate':[500,500,600,600]}]
        before = copy.deepcopy((boxes, model))
        result, evidence = self.apply(boxes, model)
        self.assertEqual((boxes, model), before)
        self.assertTrue(evidence['complete'])
        self.assertEqual([b['boxclass'] for b in result], ['text','image','formula'])
        self.assertEqual([b['textlines'] for b in result], [b['textlines'] for b in boxes])
        self.assertEqual(result[2]['bbox'], [50,50,65,65])
        projected = self.worker['project']({'pages':[{'page_number':1, 'width':100, 'height':100, 'boxes':result}]})
        blocks = [b for s in projected['sections'] for b in s['blocks']]
        self.assertEqual([b['text'] for b in blocks], ['Source prose.', '12', 'x?+y?'])
        self.assertEqual([b['kind'] for b in blocks], ['paragraph','preformatted','preformatted'])
        self.assertEqual(len(projected['ocr_evidence']), 3)

    def test_partial_or_multiple_regions_cannot_claim_complete_projection(self):
        box = self.box([10,10,20,20], 'A', 1)
        box['textlines'][0]['spans'].append(copy.deepcopy(self.box([70,70,80,80], 'B', 1)['textlines'][0]['spans'][0]))
        model = [{'label':'image', 'score':.7, 'coordinate':[0,0,300,300]}]
        result, evidence = self.apply([box], model)
        self.assertFalse(evidence['complete'])
        self.assertEqual(result, [box])
        _, evidence = self.apply([self.box([10,10,20,20], 'A', 1)], model * 2)
        self.assertFalse(evidence['complete'])

    def test_empty_visual_objects_are_retained_as_unanchored_not_fabricated_text(self):
        result, evidence = self.apply([], [{'label':'image', 'score':.7, 'coordinate':[0,0,300,300]}])
        self.assertEqual(result, [])
        self.assertEqual(evidence['unanchored_visual_regions'], [0])
        self.assertFalse(evidence['complete'])
        self.assertEqual(evidence['regions'][0]['bbox'], [0,0,30,30])

    def test_invalid_bounds_and_model_scores_are_rejected(self):
        for bounds, score in [([-1,0,20,20], .7), ([20,0,10,20], .7), ([0,0,2000,20], .7),
                              ([0,0,20,20], float('nan')), ([0,0,20,20], 1.1)]:
            with self.assertRaises(ValueError):
                self.apply([], [{'label':'image', 'score':score, 'coordinate':bounds}])
