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
    def test_real_native_pdf_layout_worker_preserves_page_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "native.pdf"
            make = """import pymupdf,sys
d=pymupdf.open()
for number in range(1,3):
 p=d.new_page(width=595,height=842)
 p.insert_text((48,50),'Abstract' if number==1 else 'Results',fontsize=18)
 p.insert_textbox(pymupdf.Rect(48,80,547,300),
  'The observatory records clear images every morning. Each sample retains its original page reference. '
  'The researchers compare the exposure times carefully. A reliable location does not imply perfect transcription.',fontsize=12)
d.save(sys.argv[1])
"""
            subprocess.run([PYTHON, "-I", "-c", make, str(source)], check=True, timeout=30)
            worker = WORKER.with_name("pdf_layout_worker.py")
            result = subprocess.run(
                [PYTHON, "-I", str(worker), "8", "1048576", "100000"],
                input=source.read_bytes(), capture_output=True, timeout=60,
            )
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            payload = json.loads(result.stdout)
            self.assertEqual(payload["schema_version"], "pdf-layout/v1")
            self.assertEqual(payload["page_count"], 2)
            prose = [b for s in payload["sections"] for b in s["blocks"] if b["kind"] == "paragraph"]
            self.assertTrue(prose)
            self.assertIn("The observatory records clear images", " ".join(b["text"] for b in prose))
            self.assertEqual({p["page"] for b in prose for p in b["parts"]}, {1, 2})

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
