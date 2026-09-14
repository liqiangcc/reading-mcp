"""Negotiated worker input header and checkpoint binding tests.

Pure protocol validation: no PDF bytes, no engine, no external packages.
"""
import importlib.util
import json
from pathlib import Path
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('resume_worker', Path(__file__).parents[2] / 'src/parsing/pdf_layout_worker.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)

IDENTITY = 'sha256:' + 'a' * 64
SOURCE = 'b' * 64
RASTER = 'c' * 64


def entry(page, original=SOURCE, identity=IDENTITY, raster=RASTER, observation=None):
    record = {'schema': 'ocr-page-checkpoint/v1', 'page': page,
              'original_sha256': original, 'runtime_identity_sha256': identity,
              'page_raster_sha256': raster, 'boxes': [{'boxclass': 'text'}],
              'ocr_retry_diagnostic': {'schema': 'ocr-regional-observations/v3',
                                       'page': page, 'complete': True}}
    if observation is not None:
        record['observation'] = observation
    return record


def header(**fields):
    return json.dumps({'schema': 'ocr-worker-input/v1', **fields}).encode()


class WorkerInputHeaderTests(unittest.TestCase):
    def test_valid_header_parses_budget_and_pages(self):
        budget, limit, pages = worker.parse_worker_input(
            header(budget_seconds=42.5, max_pages=1, pages={}))
        self.assertEqual(budget, 42.5)
        self.assertEqual(limit, 1)
        self.assertEqual(pages, {})
        budget, limit, pages = worker.parse_worker_input(
            header(pages={'2': entry(2), '4': entry(4)}))
        self.assertIsNone(budget)
        self.assertIsNone(limit)
        self.assertEqual(sorted(pages), [2, 4])

    def test_non_json_and_wrong_schema_rejected(self):
        for line in (b'not json', json.dumps({'schema': 'other/v1'}).encode(),
                     json.dumps({'pages': {}}).encode()):
            with self.assertRaises(ValueError):
                worker.parse_worker_input(line)

    def test_invalid_budget_values_rejected(self):
        for budget in (True, False, 0, -1.0, 301, '50', [30], {'s': 30}):
            with self.assertRaises(ValueError, msg=repr(budget)):
                worker.parse_worker_input(header(budget_seconds=budget, pages={}))
        # Boundary and fractional budgets are valid.
        for budget in (0.001, 1, 300):
            parsed, _, _ = worker.parse_worker_input(header(budget_seconds=budget, pages={}))
            self.assertEqual(parsed, budget)

    def test_invalid_page_limit_values_rejected(self):
        for limit in (True, 0, -1, 65, 1.5, '1', [1]):
            with self.assertRaises(ValueError, msg=repr(limit)):
                worker.parse_worker_input(header(max_pages=limit, pages={}))
        # Absent or explicit-null means "no page quota".
        for raw in (header(pages={}), header(max_pages=None, pages={})):
            _, parsed, _ = worker.parse_worker_input(raw)
            self.assertIsNone(parsed)
        for limit in (1, 8, 64):
            _, parsed, _ = worker.parse_worker_input(header(max_pages=limit, pages={}))
            self.assertEqual(parsed, limit)

    def test_pages_must_be_a_map_of_checked_entries(self):
        for pages in ([], 'x', 5, None):
            with self.assertRaises(ValueError, msg=repr(pages)):
                worker.parse_worker_input(header(pages=pages))
        bad_entries = [
            {'x': entry(1)},                       # non-numeric key
            {'1': entry(2)},                       # page/key mismatch
            {'1': dict(entry(1), schema='other')},
            {'1': dict(entry(1), original_sha256='short')},
            {'1': dict(entry(1), runtime_identity_sha256=5)},
            {'1': dict(entry(1), page_raster_sha256='short')},
            {'1': dict(entry(1), boxes='x')},
            {'1': dict(entry(1), ocr_retry_diagnostic=[])},
            {'1': dict(entry(1), observation='x')},
        ]
        for pages in bad_entries:
            with self.assertRaises(ValueError, msg=repr(pages)):
                worker.parse_worker_input(header(pages=pages))
        # A structured observation is accepted.
        _, _, parsed = worker.parse_worker_input(
            header(pages={'1': entry(1, observation={'schema': 'ocr-visual-model-attempt/v1'})}))
        self.assertIn(1, parsed)


class CheckpointBindingTests(unittest.TestCase):
    def test_source_or_runtime_drift_never_replays(self):
        bound = worker.bound_checkpoint(entry(1), SOURCE, IDENTITY)
        self.assertIsNotNone(bound)
        for other in ('d' * 64, 'sha256:' + 'e' * 64):
            self.assertIsNone(worker.bound_checkpoint(entry(1), other, IDENTITY))
        self.assertIsNone(worker.bound_checkpoint(
            entry(1, identity='sha256:' + 'f' * 64), SOURCE, IDENTITY))

    def test_invocation_deadline_bounds_remaining_time(self):
        # The invocation bound shrinks but never extends a page deadline.
        with patch.object(worker, 'PAGE_DEADLINE', None), \
                patch.object(worker, 'INVOCATION_DEADLINE', time.monotonic() - 1):
            with self.assertRaises(worker.OcrStageFailure) as caught:
                worker.page_time_remaining()
            self.assertEqual(caught.exception.code, 'OCR_TIMEOUT')
        with patch.object(worker, 'PAGE_DEADLINE', time.monotonic() + 60), \
                patch.object(worker, 'INVOCATION_DEADLINE', time.monotonic() + 5):
            self.assertLessEqual(worker.page_time_remaining(), 5)
        with patch.object(worker, 'PAGE_DEADLINE', None), \
                patch.object(worker, 'INVOCATION_DEADLINE', None):
            self.assertEqual(worker.page_time_remaining(), 15.0)


if __name__ == '__main__':
    unittest.main()
