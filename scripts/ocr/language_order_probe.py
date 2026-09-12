"""Bounded language-order probe for the frozen F07/F08 PDFs."""
import json, re, subprocess, sys, unicodedata
from pathlib import Path

CASES = {"F07": [["chi_sim"], ["eng", "chi_sim"], ["chi_sim", "eng"]],
         "F08": [["chi_sim"], ["eng", "chi_sim"], ["chi_sim", "eng"]]}

def norm(s): return " ".join(unicodedata.normalize("NFC", s).split())
def distance(a, b):
    row = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        nxt = [i]
        for j, y in enumerate(b, 1): nxt.append(min(nxt[-1] + 1, row[j] + 1, row[j - 1] + (x != y)))
        row = nxt
    return row[-1]
def metric(ref, got, words=False):
    a, b = norm(ref), norm(got)
    if words: a, b = a.split(), b.split()
    errors = distance(a, b)
    return {"errors": errors, "denominator": len(a), "rate": errors / len(a) if a else None}

def main():
    p = Path("tests/fixtures/scanned_pdf"); worker = Path("src/parsing/pdf_layout_worker.py")
    ns = {}; exec(worker.read_text(), ns)
    base = {"enabled": True, "engine_path": "/usr/bin/tesseract", "tessdata_path": "/usr/share/tesseract-ocr/5/tessdata",
            "languages": [], "operator_revision": "1", "dpi": 300, "oem": 1, "psm": 3,
            "detector_version": "pdf-layout/v1", "protocol_version": "pdf-layout/v1"}
    report = {"schema": "ocr-language-order-probe/v1", "cases": {}}
    for case, variants in CASES.items():
        gold = json.loads((p / "gold" / f"{case}.json").read_text())
        report["cases"][case] = []
        for languages in variants:
            config = {**base, "languages": languages}
            item = {"languages": languages}
            try:
                deps = ns["fingerprint_dependencies"](config)
                identity = {"config": config, "dependencies": deps, "sha256": "language-order-probe"}
                cmd = [sys.executable, "-I", "-c", worker.read_text(), "2000", "134217728", "16000000", json.dumps(config), json.dumps(identity)]
                proc = subprocess.run(cmd, input=(p / "pdf" / f"{case}.pdf").read_bytes(), stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, timeout=90, check=True)
                result = json.loads(proc.stdout)
                blocks = [b for section in result["sections"] for b in section["blocks"] if b["kind"] == "paragraph"]
                text = "\n\n".join(b["text"] for b in blocks)
                if case == "F08":
                    expected = "\n\n".join(x["text"] for x in gold["paragraphs"] if x["font"] == "goldeng")
                    actual = "\n\n".join(x["text"] for x in blocks if re.search(r"[A-Za-z]", x["text"]) and not re.search(r"[\u3400-\u9fff]", x["text"]))
                    wer = metric(expected, actual, True)
                else: wer = None
                item.update({"cer": metric(gold["text"], text), "english_wer": wer, "paragraph_count": len(blocks),
                             "words": result.get("ocr_evidence", []), "dependencies": deps,
                             "canonical_paragraphs": [{"text": b["text"], "region": b.get("region")} for b in blocks]})
            except Exception as error:
                stderr = getattr(error, "stderr", b"") or b""
                item.update({"error": str(error), "stderr": stderr.decode(errors="replace")[-4096:]})
            report["cases"][case].append(item)
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/language-order-probe.json")
    out.parent.mkdir(parents=True, exist_ok=True); rendered = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    out.write_text(rendered); print(rendered, end="")
if __name__ == "__main__": main()
