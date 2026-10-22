"""No network: the retry wrapper is exercised through a fake command runner."""
import importlib.util
import io
from pathlib import Path
import subprocess
import unittest

spec = importlib.util.spec_from_file_location(
    "download_retry", Path(__file__).resolve().parents[2] / "scripts/ocr/download_retry.py")
download_retry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(download_retry)


class FakeRunner:
    """Yields the queued outcomes in order; records every call it receives."""

    def __init__(self, *outcomes):
        self.outcomes = list(outcomes)
        self.calls = []

    def __call__(self, args, **kwargs):
        self.calls.append((args, kwargs))
        outcome = self.outcomes.pop(0)
        if isinstance(outcome, BaseException):
            raise outcome
        return outcome


def apt_run(runner, pauses, log):
    return download_retry.run_with_retry(
        ["apt-get", "install", "tesseract-ocr"], download_retry.apt_transient,
        runner=runner, timeout=600, pause=15, sleep=pauses.append,
        log=download_retry.AttemptLog(stream=log), text=True, capture_output=True)


def apt_error(returncode, output):
    return subprocess.CalledProcessError(returncode, ["apt-get", "install"], output=output)


class RetryTests(unittest.TestCase):
    def test_two_timeouts_then_success(self):
        runner = FakeRunner(subprocess.TimeoutExpired(["apt-get", "install"], 600),
                            subprocess.TimeoutExpired(["apt-get", "install"], 600),
                            subprocess.CompletedProcess(["apt-get", "install"], 0))
        pauses, log = [], io.StringIO()
        completed = apt_run(runner, pauses, log)
        self.assertEqual(completed.returncode, 0)
        self.assertEqual(len(runner.calls), 3)
        self.assertEqual(pauses, [15, 15])
        for args, kwargs in runner.calls:
            self.assertEqual(kwargs["timeout"], 600)
            self.assertTrue(kwargs["check"])
            self.assertTrue(kwargs["capture_output"])
        lines = log.getvalue().splitlines()
        self.assertEqual(len(lines), 3)
        self.assertIn("attempt 1/3", lines[0])
        self.assertIn("timed out after 600 s", lines[0])
        self.assertIn("retrying in 15 s", lines[0])
        self.assertIn("attempt 3/3", lines[2])
        self.assertIn("ok", lines[2])

    def test_always_failing_gives_up_after_three_attempts(self):
        runner = FakeRunner(*[subprocess.TimeoutExpired(["apt-get", "install"], 600)] * 3)
        pauses, log = [], io.StringIO()
        with self.assertRaises(subprocess.TimeoutExpired):
            apt_run(runner, pauses, log)
        self.assertEqual(len(runner.calls), 3)
        self.assertEqual(pauses, [15, 15])
        self.assertIn("giving up", log.getvalue().splitlines()[-1])

    def test_transient_exit_is_retried(self):
        runner = FakeRunner(
            apt_error(100, "Err:1 http://archive.ubuntu.com/ubuntu noble/main amd64 liblept5\n"
                           "  Temporary failure resolving 'archive.ubuntu.com'\n"
                           "E: Failed to fetch http://archive.ubuntu.com/ubuntu/liblept5.deb\n"),
            subprocess.CompletedProcess(["apt-get", "install"], 0))
        pauses, log = [], io.StringIO()
        self.assertEqual(apt_run(runner, pauses, log).returncode, 0)
        self.assertEqual(len(runner.calls), 2)
        self.assertEqual(pauses, [15])

    def test_non_transient_failure_is_not_retried(self):
        runner = FakeRunner(
            apt_error(100, "E: Unable to locate package tesseract-ocr\n"),
            subprocess.CompletedProcess(["apt-get", "install"], 0))
        pauses, log = [], io.StringIO()
        with self.assertRaises(subprocess.CalledProcessError):
            apt_run(runner, pauses, log)
        self.assertEqual(len(runner.calls), 1)
        self.assertEqual(pauses, [])
        self.assertIn("not transient", log.getvalue())

    def test_other_exit_codes_and_messages_are_not_transient(self):
        for returncode, output in ((100, "E: Sub-process /usr/bin/dpkg returned an error code (1)"),
                                   (1, "Temporary failure resolving 'archive.ubuntu.com'"),
                                   (100, "")):
            with self.subTest(returncode=returncode, output=output):
                self.assertFalse(download_retry.apt_transient(apt_error(returncode, output)))
        self.assertTrue(download_retry.apt_transient(
            apt_error(100, "E: Failed to fetch http://x/hash  Hash Sum mismatch")))
        self.assertTrue(download_retry.apt_transient(
            apt_error(100, "Could not connect to archive.ubuntu.com:80")))

    def test_pip_predicate_retries_only_network_failures(self):
        self.assertTrue(download_retry.pip_transient(
            subprocess.CalledProcessError(1, ["pip"], stderr="ERROR: Read timed out.")))
        self.assertTrue(download_retry.pip_transient(
            subprocess.CalledProcessError(1, ["pip"], stderr="ERROR: Connection reset by peer")))
        self.assertFalse(download_retry.pip_transient(
            subprocess.CalledProcessError(1, ["pip"], stderr="ERROR: No matching distribution found")))
        self.assertFalse(download_retry.pip_transient(
            subprocess.CalledProcessError(2, ["pip"], stderr="ERROR: Connection reset by peer")))

    def test_attempt_log_is_bounded(self):
        log = io.StringIO()
        attempt_log = download_retry.AttemptLog(stream=log)
        for index in range(download_retry.MAX_LOG_LINES + 5):
            attempt_log.write("line %d" % index)
        self.assertEqual(len(log.getvalue().splitlines()), download_retry.MAX_LOG_LINES)


if __name__ == "__main__":
    unittest.main()
