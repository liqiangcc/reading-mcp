"""No OCR/model execution: faults exercise the actual worker dependency helper."""
import importlib.util
from pathlib import Path
import tempfile
import time
import unittest

spec = importlib.util.spec_from_file_location(
    "worker_dependency_test", Path(__file__).parents[2] / "src/parsing/pdf_layout_worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class DependencyProcessTests(unittest.TestCase):
    def test_both_streams_and_nonzero_exit_preserved(self):
        output = worker.dependency_command_output(
            ["/bin/sh", "-c", "printf library; printf missing >&2; exit 7"])
        self.assertEqual(output.stdout, "library")
        self.assertEqual(output.stderr, "missing")
        self.assertEqual(output.returncode, 7)

    def test_each_stream_is_bounded(self):
        for redirect in ("", " >&2"):
            with self.subTest(redirect=redirect), self.assertRaisesRegex(RuntimeError, "output limit"):
                worker.dependency_command_output(
                    ["/bin/sh", "-c", "while :; do printf '12345678901234567890123456789012'" + redirect + "; done"])

    def test_timeout_terminates_descendant_retaining_pipe(self):
        with tempfile.TemporaryDirectory() as directory:
            pidfile = Path(directory) / "pid"
            start = time.monotonic()
            with self.assertRaisesRegex(RuntimeError, "timed out"):
                worker.dependency_command_output(
                    ["/bin/sh", "-c", '/bin/sleep 30 & echo $! > "$1"; exit 0', "test", str(pidfile)], .2)
            self.assertLess(time.monotonic() - start, 2)
            pid = int(pidfile.read_text())
            deadline = time.monotonic() + 1
            while True:
                try:
                    state = Path(f"/proc/{pid}/stat").read_text().split(") ", 1)[1]
                except FileNotFoundError:
                    break
                if state.startswith("Z"):
                    break  # init owns reaping; no executing process or open pipe remains
                self.assertLess(time.monotonic(), deadline, "dependency descendant still running")
                time.sleep(.005)
