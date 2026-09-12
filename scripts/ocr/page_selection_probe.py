"""Public frozen-fixture inspection evidence, not a replacement quality gate."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from html.parser import HTMLParser

INSPECT = r'''
import contextlib, json, sys
with contextlib.redirect_stdout(sys.stderr):
    import pymupdf, pymupdf4llm
    pymupdf4llm.use_layout(True)
    raw = sys.stdin.buffer.read()
    with pymupdf.open(stream=raw, filetype="pdf") as doc:
        raw_regions = []
        for page in doc:
            page.get_layout(return_raw=True)
            raw_regions.append({"page": page.number + 1,
                                "rect": list(page.rect), "rotation": page.rotation,
                                "regions": page.layout_information})
        pages = [{"page": page.number + 1, "rect": list(page.rect),
                  "rotation": page.rotation, "cropbox": list(page.cropbox),
                  "texttrace": page.get_texttrace(),
                  "images": page.get_image_info(),
                  "drawings": [{"rect": list(path["rect"]), "type": path["type"]}
                               for path in page.get_drawings()]}
                 for page in doc]
        layout = json.loads(pymupdf4llm.to_json(doc, use_ocr=False))
def encode_array(value):
    if hasattr(value, "tolist"):
        return value.tolist()
    raise TypeError("unsupported raw layout evidence type: " + type(value).__name__)
json.dump({"layout": layout, "original_pages": pages,
           "raw_layout_regions": raw_regions}, sys.stdout, ensure_ascii=False, default=encode_array)
'''


def invoke(command, raw):
    try:
        completed = subprocess.run(command, input=raw, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, timeout=90)
        record = {"returncode": completed.returncode,
                  "stderr": completed.stderr.decode(errors="replace")[-4096:]}
        try:
            record["payload"] = json.loads(completed.stdout)
        except (ValueError, UnicodeError) as error:
            record["payload_error"] = str(error)
        return record
    except subprocess.TimeoutExpired as error:
        return {"error": "subprocess timeout (90 seconds)",
                "stderr": (error.stderr or b"").decode(errors="replace")[-4096:]}


def hocr_observations(raw, config, namespace):
    """Inspect engine-native region classes; never turn these into gold masks."""
    import pymupdf

    class Regions(HTMLParser):
        def __init__(self):
            super().__init__()
            self.regions = []

        def handle_starttag(self, tag, attributes):
            attributes = dict(attributes)
            if attributes.get("class", "").startswith("ocr"):
                self.regions.append({"tag": tag, "attributes": attributes})

    pages = []
    with pymupdf.open(stream=raw, filetype="pdf") as document:
        for page in document:
            namespace["raster_pixel_count"](page, config["dpi"])
            with tempfile.TemporaryDirectory(prefix="public-hocr-probe-") as directory:
                image = Path(directory) / "page.png"
                output = Path(directory) / "engine"
                page.get_pixmap(dpi=config["dpi"], colorspace=pymupdf.csRGB, alpha=False).save(image)
                result = subprocess.run([config["engine_path"], str(image), str(output),
                    "--tessdata-dir", config["tessdata_path"], "-l", "+".join(config["languages"]),
                    "--dpi", str(config["dpi"]), "--oem", str(config["oem"]),
                    "--psm", str(config["psm"]), "tsv", "hocr"],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15, check=True,
                    env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "OMP_THREAD_LIMIT": "1"})
                hocr = output.with_suffix(".hocr").read_text()
                regions = Regions()
                regions.feed(hocr)
                pages.append({"page": page.number + 1, "hocr": hocr,
                              "tsv": output.with_suffix(".tsv").read_text(),
                              "regions": regions.regions,
                              "stderr": result.stderr.decode(errors="replace")[-4096:]})
    return pages


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    fixtures = root / "tests/fixtures/scanned_pdf"
    worker = (root / "src/parsing/pdf_layout_worker.py").read_text()
    namespace = {}
    exec(worker, namespace)
    config = {"enabled": True, "engine_path": "/usr/bin/tesseract",
              "tessdata_path": "/usr/share/tesseract-ocr/5/tessdata",
              "languages": ["eng"], "operator_revision": "1", "dpi": 300,
              "oem": 1, "psm": 3, "detector_version": "pdf-layout/v1",
              "protocol_version": "pdf-layout/v1"}
    report = {"schema": "ocr-page-selection-diagnostic/v1", "cases": {},
              "acceptance": "diagnostic only; existing quality gates unchanged"}
    for case in ("F01", "F03", "F04-form", "F04-flat", "F05", "F09", "F10", "F11", "F12", "F13"):
        item = {}
        report["cases"][case] = item
        try:
            raw = (fixtures / "pdf" / f"{case}.pdf").read_bytes()
            item["original_sha256"] = hashlib.sha256(raw).hexdigest()
            identity = namespace["runtime_identity"](
                config, namespace["fingerprint_dependencies"](config))
            item["identity"] = identity
            item["inspection"] = invoke([sys.executable, "-I", "-c", INSPECT], raw)
            item["worker"] = invoke([sys.executable, "-I", "-c", worker,
                                     "2000", "134217728", "16000000",
                                     json.dumps(config), json.dumps(identity)], raw)
            if case in ("F11", "F12", "F13"):
                item["hocr_diagnostic"] = hocr_observations(raw, config, namespace)
            # Gold is an observation after both pipelines, never an input to
            # classification, region selection, ordering or recognition.
            item["frozen_gold"] = json.loads((fixtures / "gold" / f"{case}.json").read_text())
        except Exception as error:
            item["error"] = str(error)
        print(json.dumps({"page_selection_case": case, "evidence": item}, ensure_ascii=False, indent=2), flush=True)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    # Classification/worker failures are diagnostic evidence. Infrastructure
    # failure of the independent native inspection must not masquerade as data.
    if any("error" in item or item.get("inspection", {}).get("returncode") != 0
           or "payload_error" in item.get("inspection", {})
           for item in report["cases"].values()):
        raise SystemExit("page inspection diagnostic could not collect all cases")


if __name__ == "__main__":
    main()
