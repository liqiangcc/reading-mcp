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
                'textlines':[{'bbox':bounds, 'spans':[{'text':text, 'bbox':bounds, 'flags':0,
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
        projected = self.worker['project']({'page_count':1, 'pages':[{'page_number':1, 'width':100, 'height':100, 'boxes':result}]})
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

    def test_only_corroborated_adjacent_unfinished_fragments_merge(self):
        boxes = [self.box([10,10,40,20], 'A source continues to', 1),
                 self.box([10,25,40,35], 'its next line.', 2)]
        model = [{'label':'text', 'score':.9, 'coordinate':[50,50,450,400]}]
        original = copy.deepcopy(boxes)
        result, evidence = self.apply(boxes, model)
        self.assertEqual(boxes, original)
        self.assertEqual(len(result), 1)
        self.assertEqual([line['spans'][0]['ocr_block'] for line in result[0]['textlines']], [1,2])
        self.assertEqual(evidence['paragraph_merges'][0]['source_boxes'], [0,1])
        self.assertEqual(evidence['projected_source_groups'], [[0,1]])
        for text in ('A complete sentence.', 'A heading:'):
            boxes[0]['textlines'][0]['spans'][0]['text'] = text
            self.assertEqual(len(self.apply(boxes, model)[0]), 2)
        boxes = copy.deepcopy(original)
        self.assertEqual(len(self.apply(boxes, model * 2)[0]), 2, 'ambiguous region must not merge')
        self.assertEqual(len(self.apply(boxes, [])[0]), 2, 'geometry alone is insufficient')
        boxes[1] = self.box([10,50,40,60], 'its distant line.', 2)
        model[0]['coordinate'][3] = 700
        self.assertEqual(len(self.apply(boxes, model)[0]), 2, 'separate blocks must remain separate')

    def test_bottom_visuals_follow_both_columns_without_sorting_prose(self):
        boxes = [self.box([1,20,20,30], 'Left column.', 1),
                 self.box([22,62,25,65], 'Real figure label', 2),
                 self.box([52,82,65,85], 'Real formula', 3),
                 self.box([50,1,80,10], 'Right column.', 4)]
        model = [{'label':'image', 'score':.7, 'coordinate':[200,600,300,700]},
                 {'label':'formula', 'score':.6, 'coordinate':[500,800,700,900]}]
        original = copy.deepcopy(boxes)
        result, evidence = self.apply(boxes, model)
        self.assertTrue(evidence['complete'])
        self.assertEqual(boxes, original)
        self.assertEqual([box['ocr_block'] for box in result], [1,4,2,3])
        self.assertEqual(evidence['projected_source_groups'], [[0],[3],[1],[2]])
        order = evidence['terminal_visual_order']
        self.assertEqual(order['input_source_groups'], [[0],[1],[2],[3]])
        self.assertEqual(order['output_group_indices'], [0,3,1,2])
        self.assertEqual(len(order['moves']), 2)
        self.assertEqual([b['textlines'] for b in result], [boxes[i]['textlines'] for i in [0,3,1,2]])
        # A model box overlapping prose is NOT evidence for a trailing move.
        model[0]['coordinate'][1] = 100
        result, evidence = self.apply(boxes, model)
        self.assertEqual([box['ocr_block'] for box in result], [1,2,4,3])
        self.assertEqual(evidence['terminal_visual_order']['moves'][0]['source_boxes'], [2])

    def test_reversed_terminal_objects_fail_instead_of_coordinate_sort(self):
        boxes = [self.box([1,1,10,10], 'Prose.', 1),
                 self.box([52,82,65,85], 'Formula first in engine', 2),
                 self.box([22,62,25,65], 'Figure later in engine', 3)]
        model = [{'label':'image', 'score':.7, 'coordinate':[200,600,300,700]},
                 {'label':'formula', 'score':.6, 'coordinate':[500,800,700,900]}]
        result, evidence = self.apply(boxes, model)
        self.assertFalse(evidence['complete'])
        self.assertEqual([box['ocr_block'] for box in result], [1,2,3])
        self.assertEqual(evidence['failures'][0]['reason'], 'unproven_terminal_visual_order')
