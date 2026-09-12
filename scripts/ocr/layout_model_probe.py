"""Approved public-only candidate diagnostic; never imported by production."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import subprocess
import sys
import time
import urllib.request
import urllib.error

REVISION = "8ac289e66575bb9bba6e15c53719d8b15cc9b3b2"
FILES = {
    "README.md": (7298, "e08620abd7706f53567c6123596b02809e5a9b91"),
    "config.json": (4715, "436e96f29b801071ebaa2b8126e9c6e76c5e0476"),
    "inference.json": (339876, "6178df1e64b9c8e99b8955db7a20ecc17ae8559b"),
    "inference.yml": (1579, "f0995b690b0ed7a9d37786cdee8f204bc16230e0"),
    "inference.pdiparams": (4804904, "491c3382d84ca04d2033afbee0c105942ed82fea392bb4a19170646adebe088a"),
}


def verify(path):
    records = []
    for name, (size, expected) in FILES.items():
        raw = (path / name).read_bytes()
        sha256 = hashlib.sha256(raw).hexdigest()
        digest = sha256 if len(expected) == 64 else hashlib.sha1(
            f"blob {len(raw)}\0".encode() + raw).hexdigest()
        if len(raw) != size or digest != expected:
            raise ValueError("pinned model file mismatch: " + name)
        records.append({"name": name, "bytes": size, "sha256": sha256})
    return records


def child(model, case, output):
    # Public diagnostic only. The enclosing systemd cgroup also bounds RSS/PIDs
    # and denies network. Do not mistake these exploratory limits for acceptance.
    resource.setrlimit(resource.RLIMIT_CPU, (45, 45))
    resource.setrlimit(resource.RLIMIT_NOFILE, (256, 256))
    resource.setrlimit(resource.RLIMIT_FSIZE, (64 * 1024 * 1024,) * 2)
    verify(model)
    started = time.monotonic()
    import pymupdf
    from paddlex import create_predictor
    from paddlex.inference.utils.pp_option import PaddlePredictorOption
    options = PaddlePredictorOption("PP-DocLayout-S", run_mode="paddle", cpu_threads=1)
    detector = create_predictor(model_name="PP-DocLayout-S", model_dir=str(model),
                                device="cpu", pp_option=options)
    loaded = time.monotonic()
    raw = Path(f"tests/fixtures/scanned_pdf/pdf/{case}.pdf").read_bytes()
    pages = []
    with pymupdf.open(stream=raw, filetype="pdf") as document:
        for page in document:
            raster = page.get_pixmap(dpi=300, colorspace=pymupdf.csRGB, alpha=False)
            image = output / f"{case}-{page.number + 1}.png"
            raster.save(image)
            begin = time.monotonic()
            result = list(detector.predict(str(image), batch_size=1, layout_nms=True))
            pages.append({"page": page.number + 1, "original_rect": list(page.rect),
                          "raster_size": [raster.width, raster.height], "dpi": 300,
                          "raster_sha256": hashlib.sha256(raster.samples).hexdigest(),
                          "inference_seconds": time.monotonic() - begin,
                          "raw_predictions": [item.json for item in result]})
            image.unlink()
    report = {"case": case, "original_sha256": hashlib.sha256(raw).hexdigest(),
              "load_seconds": loaded - started, "total_seconds": time.monotonic() - started,
              "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss, "pages": pages}
    (output / f"{case}.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("--case", choices=["F02", "F06", "F07", "F08", "F11", "F12", "F13", "F14"])
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    if args.prepare:
        args.model.mkdir(parents=True, exist_ok=False)
        for name, (size, _) in FILES.items():
            endpoint = "resolve" if name == "inference.pdiparams" else "raw"
            url = f"https://huggingface.co/PaddlePaddle/PP-DocLayout-S/{endpoint}/{REVISION}/{name}"
            for attempt in range(3):
                print(f"Downloading pinned {name}, attempt {attempt + 1}/3", flush=True)
                try:
                    with urllib.request.urlopen(url, timeout=60) as response:
                        raw = response.read(size + 1)
                    break
                except urllib.error.HTTPError as error:
                    if error.code not in (429, 502, 503, 504) or attempt == 2:
                        raise
                    retry_after = error.headers.get("Retry-After", "15")
                    delay = int(retry_after) if retry_after.isdigit() else 15
                    if delay > 60:
                        raise RuntimeError("model host requested a longer retry delay; stop bounded attempt") from error
                    time.sleep(max(1, delay))
            (args.model / name).write_bytes(raw)
        manifest = {"repository": "PaddlePaddle/PP-DocLayout-S", "revision": REVISION,
                    "license_declaration": "Apache-2.0 in pinned model card; candidate only",
                    "files": verify(args.model)}
        (args.output / "model-manifest.json").write_text(json.dumps(manifest, indent=2))
        print(json.dumps(manifest), flush=True)
        return
    if args.case:
        child(args.model, args.case, args.output)
        return
    report = {"schema": "layout-model-diagnostic/v1", "model_files": verify(args.model),
              "scope": "raw candidate regions only; no gold input, no OCR/projection modification",
              "cases": {}}
    for case in ("F02", "F06", "F07", "F08", "F11", "F12", "F13", "F14"):
        try:
            process = subprocess.run([sys.executable, str(Path(__file__).resolve()),
                "--model", str(args.model), "--output", str(args.output), "--case", case],
                capture_output=True, timeout=60, check=True)
            item = json.loads((args.output / f"{case}.json").read_text())
        except Exception as error:
            item = {"error": str(error), "stderr": (getattr(error, "stderr", b"") or b"").decode(errors="replace")[-4096:]}
        report["cases"][case] = item
        print(json.dumps({"layout_model_case": case, "result": item}, ensure_ascii=False), flush=True)
    (args.output / "layout-model-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    if any("error" in item for item in report["cases"].values()):
        raise SystemExit("candidate diagnostic failed; see all-case report")


if __name__ == "__main__":
    main()
