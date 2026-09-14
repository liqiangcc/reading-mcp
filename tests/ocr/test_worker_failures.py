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
        failure = worker.OcrStageFailure('OCR_TIMEOUT', 'local OCR page exceeded shared 15 second budget')
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
