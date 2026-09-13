import hashlib, json, os, subprocess, sys, tempfile
from pathlib import Path

ROOT = Path(__file__).parents[2]
worker = ROOT / "src/parsing/pdf_layout_worker.py"
config = {"enabled": True, "engine_path": "/usr/bin/tesseract", "tessdata_path": "/usr/share/tesseract-ocr/5/tessdata", "languages": ["chi_sim"], "operator_revision": "1", "dpi": 300, "oem": 1, "psm": 3, "detector_version": "pdf-layout/v1", "protocol_version": "pdf-layout/v1"}
ns = {}; exec(worker.read_text(), ns)
with tempfile.TemporaryDirectory(prefix="public-identity-io-") as directory:
    path = Path(directory) / "dependency"
    data = b"x" * (3 * 1024 * 1024 + 7)
    path.write_bytes(data)
    assert ns["dependency_sha256"](path) == hashlib.sha256(data).hexdigest()
    link = Path(directory) / "library.so"
    link.symlink_to(path)
    assert ns["dependency_sha256"](link) == ns["dependency_sha256"](path)
    fifo = Path(directory) / "fifo"
    os.mkfifo(fifo, 0o600)
    # A subprocess timeout bounds the regression even if nonblocking open is lost.
    reject = subprocess.run([sys.executable, "-I", "-c",
        'import sys; ns={"__name__":"identity_test"}; exec(sys.argv[1], ns); ns["dependency_sha256"](sys.argv[2])',
        worker.read_text(), str(fifo)],
        capture_output=True, timeout=5)
    assert reject.returncode != 0 and b"OCR dependency must be a regular file" in reject.stderr
identity = ns["runtime_identity"](config, ns["fingerprint_dependencies"](config))
pdf = ROOT / "tests/fixtures/scanned_pdf/pdf/F07.pdf"
cmd = [sys.executable, "-I", "-c", worker.read_text(), "2000", "134217728", "16000000", json.dumps(config), json.dumps(identity)]
ok = subprocess.run(cmd, input=pdf.read_bytes(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=90)
assert ok.returncode == 0, ok.stderr.decode()
payload = json.loads(ok.stdout)
blocks = [b for section in payload["sections"] for b in section["blocks"]]
evidence = payload["ocr_evidence"]
observations = payload["ocr_attempts"]
assert observations and all(page["complete"] for page in observations)
for page in observations:
    attempts = {attempt["id"]: attempt for attempt in page["attempts"]}
    assert set(attempts) == {"primary", "retry"}, "F07 must preserve both actual attempts"
    assert page["components"] and all(c["replaced_refs"] for c in page["components"])
    for selection in page["selection"]:
        reference = selection["source"]
        source_box = attempts[reference["attempt"]]["boxes"][reference["box"]]
        assert reference["page"] == 1 and source_box["textlines"]
        for word in selection["words"]:
            assert source_box["textlines"][word["line"]]["spans"][word["word"]]["text"]
assert len(blocks) > 1 and len(evidence) > 1
assert all(e["paragraph"] and e["confidence"] is not None and len(e["bbox"]) == 4 for e in evidence)
Path(os.environ.get("RUNNER_TEMP", "."), "ocr-first-probe", "f07-canonical.json").parent.mkdir(parents=True, exist_ok=True)
Path(os.environ.get("RUNNER_TEMP", "."), "ocr-first-probe", "f07-canonical.json").write_text(json.dumps(payload, ensure_ascii=False, indent=2))
bad = json.loads(json.dumps(identity)); model = next(d for d in bad["dependencies"] if d["name"].startswith("model:")); model["sha256"] = "0" * 64
failed = subprocess.run(cmd[:-1] + [json.dumps(bad)], input=pdf.read_bytes(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
assert failed.returncode != 0 and b"identity mismatch" in failed.stderr
failure = json.loads(failed.stdout)
assert failure == {'schema':'ocr-worker-failure/v2', 'stage':'dependency',
    'original_sha256':None, 'runtime_identity_sha256':bad['sha256'], 'error':'OCR_UNAVAILABLE'}
print("worker identity and tamper rejection passed")
