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
L_REVISION = "ca760c3336d922ee63cdbff85003181222e3911c"
L_FILES = {
    "README.md": (7434, "0972bad1446a331cb2e19bfff6154ab273dd05d1"),
    "config.json": (6134, "0a3507f7365b0554e4e86d436421c53c3a738fb1"),
    "inference.json": (1081389, "540c4f71da8b65c044f14a242314cc73de10d3d3"),
    "inference.yml": (1871, "632a2724ad049bbb1ea75777597ad7f12d106692"),
    "inference.pdiparams": (129021913, "4df69115349e1e6215629d058e3bc7f087cad7a0d2776ef40c463daf73c38982"),
}
ONNX_REVISION = "feb74619326f634e0e883218598096a3733ad9f7"
ONNX_FILES = {
    "README.md": (1669, "15d99d1c81c251b2b57b742b064e03b7ca4b4fd5"),
    "inference.yml": (1838, "9a236587eae068a1e7906fd158f712dc41400563"),
    "inference.onnx": (129736329, "77afb2caa74dd13240d087d2eced91d7fcd2caebd16006a0a66162fc8707ff0e"),
}


def onnx_detector(model):
    import cv2
    import numpy as np
    import onnxruntime as ort
    import yaml
    config = yaml.safe_load((model / "inference.yml").read_text())
    assert config["Preprocess"] == [
        {"interp": 2, "keep_ratio": False, "target_size": [800, 800], "type": "Resize"},
        {"mean": [0., 0., 0.], "norm_type": "none", "std": [1., 1., 1.], "type": "NormalizeImage"},
        {"type": "Permute"}]
    options = ort.SessionOptions()
    options.intra_op_num_threads = options.inter_op_num_threads = 1
    options.enable_cpu_mem_arena = False
    session = ort.InferenceSession(str(model / "inference.onnx"), sess_options=options,
                                   providers=["CPUExecutionProvider"])
    names = {item.name for item in session.get_inputs()}
    assert names <= {"image", "im_shape", "scale_factor"} and "image" in names

    def predict(path):
        # Match pinned PaddleX Resize(interp=2), RGB, scale=1/255, zero mean/unit std.
        image = cv2.cvtColor(cv2.imread(str(path)), cv2.COLOR_BGR2RGB)
        height, width = image.shape[:2]
        tensor = cv2.resize(image, (800, 800), interpolation=cv2.INTER_CUBIC).astype(np.float32) / 255.0
        values = {"image": tensor.transpose(2, 0, 1)[None],
                  "im_shape": np.array([[800, 800]], dtype=np.float32),
                  "scale_factor": np.array([[800 / height, 800 / width]], dtype=np.float32)}
        outputs = session.run(None, {name: values[name] for name in names})
        rows = outputs[0]
        assert rows.ndim == 2 and rows.shape[1] == 6 and len(rows) <= 10000
        boxes = []
        for row in rows:
            label, score, *bounds = row.tolist()
            if score >= config["draw_threshold"]:
                assert label == int(label) and 0 <= int(label) < len(config["label_list"])
                boxes.append({"cls_id": int(label), "label": config["label_list"][int(label)],
                              "score": score, "coordinate": bounds})
        return [{"res": {"boxes": boxes}, "raw_outputs": [value.tolist() for value in outputs],
                 "postprocess": "pinned draw_threshold only; no layout NMS or gold selection"}]
    return predict


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


def child(model, case, output, name):
    # Public diagnostic only. The enclosing systemd cgroup also bounds RSS/PIDs
    # and denies network. Do not mistake these exploratory limits for acceptance.
    resource.setrlimit(resource.RLIMIT_CPU, (45, 45))
    resource.setrlimit(resource.RLIMIT_NOFILE, (256, 256))
    resource.setrlimit(resource.RLIMIT_FSIZE, (64 * 1024 * 1024,) * 2)
    verify(model)
    started = time.monotonic()
    import pymupdf
    if name.endswith("_onnx"):
        predict = onnx_detector(model)
    else:
        from paddlex import create_predictor
        from paddlex.inference.utils.pp_option import PaddlePredictorOption
        options = PaddlePredictorOption(name, run_mode="paddle", cpu_threads=1)
        detector = create_predictor(model_name=name, model_dir=str(model), device="cpu", pp_option=options)
        def predict(path):
            return [item.json for item in detector.predict(str(path), batch_size=1, layout_nms=True)]
    loaded = time.monotonic()
    raw = Path(f"tests/fixtures/scanned_pdf/pdf/{case}.pdf").read_bytes()
    pages = []
    with pymupdf.open(stream=raw, filetype="pdf") as document:
        for page in document:
            raster = page.get_pixmap(dpi=300, colorspace=pymupdf.csRGB, alpha=False)
            image = output / f"{case}-{page.number + 1}.png"
            raster.save(image)
            begin = time.monotonic()
            result = predict(image)
            pages.append({"page": page.number + 1, "original_rect": list(page.rect),
                          "raster_size": [raster.width, raster.height], "dpi": 300,
                          "raster_sha256": hashlib.sha256(raster.samples).hexdigest(),
                          "inference_seconds": time.monotonic() - begin,
                          "raw_predictions": result})
            image.unlink()
    report = {"case": case, "original_sha256": hashlib.sha256(raw).hexdigest(),
              "load_seconds": loaded - started, "total_seconds": time.monotonic() - started,
              "peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss, "pages": pages}
    (output / f"{case}.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))


def main():
    global REVISION, FILES
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("--candidate", choices=["PP-DocLayout-S", "PP-DocLayout-L", "PP-DocLayout_plus-L_onnx"], default="PP-DocLayout-S")
    parser.add_argument("--case", choices=["F02", "F06", "F07", "F08", "F11", "F12", "F13", "F14"])
    args = parser.parse_args()
    if args.candidate == "PP-DocLayout-L":
        REVISION, FILES = L_REVISION, L_FILES
    elif args.candidate == "PP-DocLayout_plus-L_onnx":
        REVISION, FILES = ONNX_REVISION, ONNX_FILES
    args.output.mkdir(parents=True, exist_ok=True)
    if args.prepare:
        args.model.mkdir(parents=True, exist_ok=False)
        for name, (size, _) in FILES.items():
            endpoint = "resolve" if len(FILES[name][1]) == 64 else "raw"
            url = f"https://huggingface.co/PaddlePaddle/{args.candidate}/{endpoint}/{REVISION}/{name}"
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
        manifest = {"repository": "PaddlePaddle/" + args.candidate, "revision": REVISION,
                    "license_declaration": "Apache-2.0 in pinned model card; candidate only",
                    "files": verify(args.model)}
        (args.output / "model-manifest.json").write_text(json.dumps(manifest, indent=2))
        print(json.dumps(manifest), flush=True)
        return
    if args.case:
        child(args.model, args.case, args.output, args.candidate)
        return
    report = {"schema": "layout-model-diagnostic/v1", "candidate": args.candidate, "model_files": verify(args.model),
              "scope": "raw candidate regions only; no gold input, no OCR/projection modification",
              "cases": {}}
    for case in ("F02", "F06", "F07", "F08", "F11", "F12", "F13", "F14"):
        try:
            process = subprocess.run([sys.executable, str(Path(__file__).resolve()),
                "--model", str(args.model), "--output", str(args.output), "--case", case,
                "--candidate", args.candidate],
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
