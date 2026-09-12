import json, os, subprocess, sys, tempfile
from pathlib import Path

ROOT = Path(__file__).parents[2]
worker = ROOT / "src/parsing/pdf_layout_worker.py"
config = {"enabled": True, "engine_path": "/usr/bin/tesseract", "tessdata_path": "/usr/share/tesseract-ocr/5/tessdata", "languages": ["chi_sim"], "operator_revision": "1", "dpi": 300, "oem": 1, "psm": 3, "detector_version": "pdf-layout/v1", "protocol_version": "pdf-layout/v1"}
ns = {}; exec(worker.read_text(), ns)
identity = {"config": config, "dependencies": ns["fingerprint_dependencies"](config), "sha256": "probe"}
pdf = ROOT / "tests/fixtures/scanned_pdf/pdf/F07.pdf"
cmd = [sys.executable, "-I", "-c", worker.read_text(), "2000", "134217728", "16000000", json.dumps(config), json.dumps(identity)]
ok = subprocess.run(cmd, input=pdf.read_bytes(), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
assert ok.returncode == 0, ok.stderr.decode()
bad = json.loads(json.dumps(identity)); bad["dependencies"][0]["sha256"] = "0" * 64
failed = subprocess.run(cmd[:-1] + [json.dumps(bad)], input=pdf.read_bytes(), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
assert failed.returncode != 0 and b"identity mismatch" in failed.stderr
print("worker identity and tamper rejection passed")
