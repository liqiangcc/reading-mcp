"""Optional real renderer tests. Set READING_MCP_TEST_PYMUPDF_PYTHON to a pinned venv."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

PYTHON = os.environ.get("READING_MCP_TEST_PYMUPDF_PYTHON")
WORKER = Path(__file__).parents[2] / "src/parsing/pdf_source_view_worker.py"


@unittest.skipUnless(PYTHON, "optional pinned PyMuPDF environment not configured")
class RenderTests(unittest.TestCase):
    def test_source_text_is_visible_and_budgets_fail_explicitly(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, output, metadata = [root / n for n in ("source.pdf", "page.png", "meta.json")]
            subprocess.run([PYTHON, "-I", "-c", "import pymupdf,sys; d=pymupdf.open(); p=d.new_page(width=200,height=100); p.insert_text((20,40),'Original visible source.',fontsize=14); d.save(sys.argv[1])", str(source)], check=True)
            args = [PYTHON, "-I", str(WORKER), "--reading-mcp-source-view-file-worker", str(source), str(output), str(metadata), "1", "72", "5", "500", "500", "250000", "1048576", "1048576"]
            result = subprocess.run(args, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(metadata.read_text())["page_count"], 1)
            self.assertTrue(output.read_bytes().startswith(b"\x89PNG"))
            pixels = subprocess.run([PYTHON, "-I", "-c", "import pymupdf,sys; p=pymupdf.Pixmap(sys.argv[1]); print(sum(v<200 for v in p.samples))", str(output)], check=True, capture_output=True, text=True)
            self.assertGreater(int(pixels.stdout), 100, "source preview must not be an empty white image")
            for index, limit, message in [(12, "100", "dimensions"), (13, "8", "byte limit"), (14, "1", "stream")]:
                limited = args.copy(); limited[index] = limit
                failure = subprocess.run(limited, capture_output=True, text=True)
                self.assertNotEqual(failure.returncode, 0)
                self.assertIn(message, failure.stderr)


if __name__ == "__main__":
    unittest.main()
