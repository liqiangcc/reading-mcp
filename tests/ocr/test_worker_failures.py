import contextlib
import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('failure_worker', Path(__file__).parents[2] / 'src/parsing/pdf_layout_worker.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class WorkerFailureTests(unittest.TestCase):
    def test_disabled_requirement_envelope_is_source_bound_without_engine_identity(self):
        worker.OCR_CONFIG = {'enabled': False}
        worker.EXPECTED_IDENTITY = None
        worker.INPUT_SHA256 = 'c' * 64
        output = io.StringIO()
        with (patch.object(worker, 'main', side_effect=worker.OcrRequired()),
              contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO())):
            self.assertEqual(worker.run(), 1)
        self.assertEqual(json.loads(output.getvalue()), {
            'schema':'pdf-layout-ocr-required/v1', 'original_sha256':'c' * 64, 'error':'OCR_REQUIRED'})

    def test_footer_or_empty_spans_do_not_prove_native_body(self):
        def box(kind, text):
            return {'boxclass':kind,'textlines':[{'spans':[{'text':text}]}]}
        self.assertFalse(worker.has_native_body({'boxes':[box('page-footer', 'Page 1'), box('text', ' ')]}))
        self.assertTrue(worker.has_native_body({'boxes':[box('text', 'Native prose.')]}))
        self.assertTrue(worker.page_requires_ocr({'boxes':[box('page-footer', 'Page 1')]}))
        self.assertFalse(worker.page_requires_ocr({'boxes':[box('text', 'Native prose.')]}))
        native_with_figure = {'boxes': [box('text', 'Native prose.'), {'boxclass': 'figure', 'textlines': []}]}
        self.assertFalse(worker.page_requires_ocr(native_with_figure))
        for body_class in ('section-header', 'title', 'list-item', 'table'):
            self.assertTrue(worker.has_native_body({'boxes':[box(body_class, 'Native prose.')]}))

        class Rect:
            width = 100
            height = 100

        class Page:
            rect = Rect()

            def __init__(self, bbox):
                self.bbox = bbox

            def get_image_info(self):
                return [] if self.bbox is None else [{'bbox': self.bbox}]

        self.assertFalse(worker.page_requires_ocr(native_with_figure, Page([5, 5, 25, 25])))
        self.assertTrue(worker.page_requires_ocr(native_with_figure, Page([0, 0, 100, 100])))

    def test_non_body_text_layer_does_not_prove_native_body(self):
        # A scanned page whose embedded text layer sits inside a picture box
        # (plus a page-footer) is raster content, not authoritative prose:
        # the page must still take the OCR path.
        def box(kind, text):
            return {'boxclass':kind,'textlines':[{'spans':[{'text':text}]}]}
        for non_body_class in ('picture', 'figure', 'page-footer', 'page-header',
                               'caption', 'footnote', 'formula'):
            layout = {'boxes':[box(non_body_class, 'Embedded text layer.'), box('text', ' ')]}
            self.assertFalse(worker.has_native_body(layout), non_body_class)
            self.assertTrue(worker.page_requires_ocr(layout), non_body_class)
        self.assertTrue(worker.page_requires_ocr(
            {'boxes':[box('picture', 'Embedded text layer.'), box('page-footer', 'Page 1')]}))

    def test_full_page_raster_with_substantial_hidden_layer_skips_ocr(self):
        # An embedded invisible OCR layer that is both substantial and laid out
        # as regular body lines spanning most of the page is authoritative:
        # the dominant raster must not trigger a second engine pass.  Hidden
        # OCR output commonly has no word separators, so the positive fixture
        # is a two-column page of spaceless lines.
        line_text = 'embeddedocrtextlayerwithoutwordseparatorsinthisline.'

        def column(x0, x1):
            return {'boxclass': 'text', 'x0': x0, 'y0': 60, 'x1': x1, 'y1': 740,
                    'textlines': [
                        {'bbox': [x0, 60 + i * 40, x1, 60 + i * 40 + 12],
                         'spans': [{'text': line_text, 'size': 9.0}]}
                        for i in range(17)]}

        class Page:
            rect = type('R', (), {'width': 600, 'height': 800})()

            def __init__(self, hidden_chars, visible_chars=0):
                self.hidden = hidden_chars
                self.visible = visible_chars

            def get_image_info(self):
                return [{'bbox': [0, 0, 600, 800]}]

            def get_texttrace(self):
                traces = []
                if self.hidden:
                    traces.append({'type': 3, 'chars': [0] * self.hidden})
                if self.visible:
                    traces.append({'type': 0, 'chars': [0] * self.visible})
                return traces

        strong = {'boxes': [column(40, 280), column(320, 560),
                            {'boxclass': 'page-footer', 'x0': 250, 'y0': 770,
                             'x1': 350, 'y1': 780,
                             'textlines': [{'bbox': [250, 770, 350, 780],
                                            'spans': [{'text': '1', 'size': 8.0}]}]}]}
        self.assertTrue(worker.has_authoritative_existing_ocr_layer(Page(3600), strong))
        self.assertFalse(worker.page_requires_ocr(strong, Page(3600)))

        # Sparse hidden labels: too few hidden characters and body lines.
        sparse = {'boxes': [{'boxclass': 'text', 'x0': 40, 'y0': 60, 'x1': 200, 'y1': 80,
                             'textlines': [{'bbox': [40, 60, 200, 80],
                                            'spans': [{'text': 'Fig. 1', 'size': 9.0}]}]}]}
        self.assertFalse(worker.has_authoritative_existing_ocr_layer(Page(60), sparse))
        self.assertTrue(worker.page_requires_ocr(sparse, Page(60)))

        # A dense block that is confined to a small vertical slice of the
        # page (a long caption or a label cluster) is not a body layer even
        # when it has many qualifying-width lines.
        slab = {'boxes': [{'boxclass': 'text', 'x0': 40, 'y0': 300, 'x1': 560, 'y1': 360,
                           'textlines': [{'bbox': [40, 300 + i * 3, 560, 300 + i * 3 + 2],
                                          'spans': [{'text': line_text, 'size': 9.0}]}
                                         for i in range(20)]}]}
        self.assertFalse(worker.has_authoritative_existing_ocr_layer(Page(4000), slab))
        self.assertTrue(worker.page_requires_ocr(slab, Page(4000)))

        # A strong layer that is mostly *visible* text is not a hidden OCR
        # layer and keeps the raster OCR-eligible.
        self.assertFalse(worker.has_authoritative_existing_ocr_layer(
            Page(200, visible_chars=2000), strong))
        self.assertTrue(worker.page_requires_ocr(strong, Page(200, visible_chars=2000)))

        # No text layer at all: the pure-scan shape still requires OCR even
        # though the layout may carry a stray short line.
        self.assertFalse(worker.has_authoritative_existing_ocr_layer(Page(0), strong))
        self.assertTrue(worker.page_requires_ocr(
            {'boxes': [{'boxclass': 'text', 'x0': 40, 'y0': 60, 'x1': 200, 'y1': 80,
                        'textlines': [{'bbox': [40, 60, 200, 80],
                                       'spans': [{'text': 'Stray.', 'size': 9.0}]}]}]},
            Page(0)))

    def test_production_unit_pins_source_view_decoded_stream_budget(self):
        template = Path(__file__).parents[2] / 'deploy/systemd/reading-mcp-tunnel.service'
        self.assertIn(
            'Environment=READING_MCP_SOURCE_VIEW_MAX_DECODED_STREAM_BYTES=33554432',
            template.read_text(encoding='utf-8'),
        )

    def setUp(self):
        worker.OCR_CONFIG = {'enabled': True}
        worker.EXPECTED_IDENTITY = {'config': worker.OCR_CONFIG, 'sha256': 'sha256:' + 'a' * 64}
        worker.INPUT_SHA256 = None

    def test_dependency_failure_before_input_has_no_invented_source_or_private_details(self):
        args = ['worker', '100', '100000', '100000', json.dumps(worker.OCR_CONFIG), json.dumps(worker.EXPECTED_IDENTITY)]
        output, errors = io.StringIO(), io.StringIO()
        with patch.object(worker.sys, 'argv', args), patch.object(worker, 'fingerprint_dependencies',
                side_effect=FileNotFoundError('private path /secret/model')), contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            self.assertEqual(worker.run(), 1)
        self.assertEqual(json.loads(output.getvalue()), {'schema':'ocr-worker-failure/v2',
            'stage':'dependency','original_sha256':None,'runtime_identity_sha256':worker.EXPECTED_IDENTITY['sha256'],
            'error':'OCR_UNAVAILABLE'})
        self.assertNotIn('/secret', errors.getvalue())

    def test_ingestion_timeout_envelope_binds_complete_input_hash(self):
        worker.INPUT_SHA256 = 'b' * 64
        failure = worker.OcrStageFailure('OCR_TIMEOUT', 'local OCR page exceeded shared %d second budget' % worker.PAGE_UNIT_SECONDS)
        output = io.StringIO()
        with patch.object(worker, 'main', side_effect=failure), contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(worker.run(), 1)
        result = json.loads(output.getvalue())
        self.assertEqual(result['stage'], 'ingestion')
        self.assertEqual(result['original_sha256'], 'b' * 64)
        self.assertEqual(result['error'], 'OCR_TIMEOUT')

    def test_real_budget_producers_assign_resource_and_timeout_types(self):
        budget = worker.OcrRasterBudget()
        with self.assertRaises(worker.OcrStageFailure) as caught:
            budget.reserve_raster(64_000_001)
        self.assertEqual(caught.exception.code, 'OCR_RESOURCE_LIMIT')
        for page in range(8):
            budget.require_page(page)
        with self.assertRaises(worker.OcrStageFailure) as caught:
            budget.require_page(8)
        self.assertEqual(caught.exception.code, 'OCR_RESOURCE_LIMIT')
        with patch.object(worker, 'PAGE_DEADLINE', 0):
            with self.assertRaises(worker.OcrStageFailure) as caught:
                worker.page_time_remaining()
        self.assertEqual(caught.exception.code, 'OCR_TIMEOUT')

    def test_unclassified_failure_is_not_promoted_to_a_retryable_error(self):
        output = io.StringIO()
        with (patch.object(worker, 'main', side_effect=RuntimeError('unclassified failure')),
                contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO())):
            self.assertEqual(worker.run(), 1)
        self.assertEqual(output.getvalue(), '')


if __name__ == '__main__':
    unittest.main()
