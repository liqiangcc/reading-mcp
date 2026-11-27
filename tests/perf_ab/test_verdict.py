"""Unit tests for the perf-ab verdict logic (scripts/perf-ab.py).

The gate is one-sided: positive deltas (head slower / more memory) beyond +5% fail,
negative deltas pass, and deltas below -5% pass as improvements. Improvements must
never trigger a recheck, and a recheck that no longer misses the bar passes.
"""

import importlib.util
import os
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                      "..", "..", "scripts", "perf-ab.py")
spec = importlib.util.spec_from_file_location("perf_ab", SCRIPT)
perf_ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(perf_ab)


class VerdictForDelta(unittest.TestCase):
    def test_six_percent_slower_fails(self):
        # +6% is beyond the +5% bar: the head is slower, this is a regression.
        self.assertEqual(perf_ab.verdict_for_delta(6.0), "FAIL")
        self.assertTrue(perf_ab.verdict_fails(perf_ab.verdict_for_delta(6.0)))

    def test_four_percent_slower_passes(self):
        # +4% is inside the bar: noise, not a regression.
        self.assertEqual(perf_ab.verdict_for_delta(4.0), "pass")
        self.assertFalse(perf_ab.verdict_fails(perf_ab.verdict_for_delta(4.0)))

    def test_twenty_nine_percent_faster_passes_as_improved(self):
        # -29% means the head is much faster: an improvement, never a failure.
        self.assertEqual(perf_ab.verdict_for_delta(-29.0), "improved")
        self.assertFalse(perf_ab.verdict_fails(perf_ab.verdict_for_delta(-29.0)))

    def test_exact_bar_edges(self):
        # Exactly +5% is inside the bar; exactly -5% is not yet an improvement.
        self.assertEqual(perf_ab.verdict_for_delta(5.0), "pass")
        self.assertEqual(perf_ab.verdict_for_delta(-5.0), "pass")
        self.assertEqual(perf_ab.verdict_for_delta(5.01), "FAIL")
        self.assertEqual(perf_ab.verdict_for_delta(-5.01), "improved")

    def test_no_data_is_a_failure(self):
        self.assertEqual(perf_ab.verdict_for_delta(None), "no data")
        self.assertTrue(perf_ab.verdict_fails(perf_ab.verdict_for_delta(None)))


class EvaluateRecheck(unittest.TestCase):
    """evaluate() decides which feeding jobs must be rechecked after run1."""

    def metrics_with_delta(self, key: str, delta_pct: float) -> dict:
        # Every gate key is present (a missing key is a "no data" miss); only the
        # probed key deviates from parity.
        metrics = {gate_key: 1.0 for gate_key, _job, _label in perf_ab.GATES}
        metrics[key] = 1.0 + delta_pct / 100.0
        return metrics

    def test_improvement_never_triggers_recheck(self):
        # A 29% faster source_view must not rerun the readpath job.
        metrics = self.metrics_with_delta("readpath_view_p50", -29.0)
        needs = perf_ab.evaluate(metrics)
        self.assertFalse(needs["readpath"])
        self.assertFalse(needs["corpus"])
        self.assertEqual(needs["misses"], [])

    def test_small_positive_delta_never_triggers_recheck(self):
        metrics = self.metrics_with_delta("readpath_view_p50", 4.0)
        needs = perf_ab.evaluate(metrics)
        self.assertFalse(needs["readpath"])
        self.assertEqual(needs["misses"], [])

    def test_regression_triggers_recheck(self):
        # +6% on a readpath metric: a real miss, the readpath job is rechecked.
        metrics = self.metrics_with_delta("readpath_view_p50", 6.0)
        needs = perf_ab.evaluate(metrics)
        self.assertTrue(needs["readpath"])
        self.assertFalse(needs["corpus"])
        self.assertEqual(len(needs["misses"]), 1)
        self.assertEqual(needs["misses"][0]["key"], "readpath_view_p50")

    def test_missing_data_triggers_recheck(self):
        needs = perf_ab.evaluate({})
        self.assertTrue(needs["corpus"])
        self.assertTrue(needs["readpath"])
        self.assertTrue(all(miss["delta_pct"] is None for miss in needs["misses"]))


class RecheckOutcome(unittest.TestCase):
    """A metric that missed the bar in run1 is judged on its recheck run."""

    def test_run1_plus6_run2_plus3_passes(self):
        # run1: +6% (miss, recheck scheduled); run2: +3% (inside the bar).
        # The final verdict judges run2, and +3% passes.
        run2_delta = 3.0
        verdict = perf_ab.verdict_for_delta(run2_delta)
        self.assertEqual(verdict, "pass")
        self.assertFalse(perf_ab.verdict_fails(verdict))

    def test_run1_plus6_run2_plus6_still_fails(self):
        run2_delta = 6.0
        verdict = perf_ab.verdict_for_delta(run2_delta)
        self.assertEqual(verdict, "FAIL")
        self.assertTrue(perf_ab.verdict_fails(verdict))


if __name__ == "__main__":
    unittest.main()
