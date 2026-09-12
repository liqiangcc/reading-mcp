"""Build the hash-locked Python component on a hosted runner, never production."""
import argparse
from email.parser import BytesParser
import hashlib
import json
from pathlib import Path
import platform
import re
import subprocess
import sys
import urllib.request
import zipfile


MISSING_WHEEL_NOTICES = {
    ("flatbuffers", "25.12.19"): (
        "https://raw.githubusercontent.com/google/flatbuffers/7e163021e59cca4f8e1e35a7c828b5c6b7915953/LICENSE",
        "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
    ),
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    requirements = root / "scripts/pdf-layout/requirements.txt"
    expected = {}
    for line in requirements.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        match = re.fullmatch(r"([A-Za-z0-9_.-]+)==([A-Za-z0-9_.+-]+)", line)
        if not match:
            raise ValueError("dependency input must contain exact versions only")
        name, version = match.groups()
        name = re.sub(r"[-_.]+", "-", name).lower()
        if name in expected:
            raise ValueError("duplicate dependency input")
        expected[name] = version
    args.output.mkdir(parents=True, exist_ok=False)
    wheels = args.output / "wheels"
    subprocess.run([sys.executable, "-m", "pip", "--isolated", "download", "--only-binary=:all:",
                    "--index-url", "https://pypi.org/simple",
                    "--dest", str(wheels), "--requirement", str(requirements)],
                   check=True, timeout=240)
    records = []
    observed = {}
    for wheel in sorted(wheels.iterdir()):
        if wheel.suffix != ".whl":
            raise ValueError("non-wheel dependency artifact")
        with zipfile.ZipFile(wheel) as archive:
            metadata_paths = [p for p in archive.namelist() if p.endswith(".dist-info/METADATA")]
            if len(metadata_paths) != 1:
                raise ValueError("wheel metadata is ambiguous")
            metadata = BytesParser().parsebytes(archive.read(metadata_paths[0]))
            name = re.sub(r"[-_.]+", "-", metadata["Name"]).lower()
            version = metadata["Version"]
            if name in observed or expected.get(name) != version:
                raise ValueError("resolved dependency is duplicate, unpinned, or a different version: " + name)
            observed[name] = version
            notices = [p for p in archive.namelist()
                       if re.search(r"(^|/)(licenses?|copying|copyright|notice)([./_-]|$)", p, re.I)]
        external_notice = None
        if not notices:
            source = MISSING_WHEEL_NOTICES.get((name, version))
            if source is None:
                raise ValueError("wheel lacks bundled license notice: " + name)
            url, expected_hash = source
            with urllib.request.urlopen(url, timeout=30) as response:
                notice = response.read(1024 * 1024 + 1)
            if len(notice) > 1024 * 1024 or hashlib.sha256(notice).hexdigest() != expected_hash:
                raise ValueError("pinned upstream license digest mismatch")
            notice_path = args.output / "notices" / f"{name}-{version}-LICENSE"
            notice_path.parent.mkdir(exist_ok=True)
            notice_path.write_bytes(notice)
            external_notice = {"file": str(notice_path.relative_to(args.output)),
                               "source": url, "sha256": expected_hash}
        records.append({"name": name, "version": version, "file": "wheels/" + wheel.name,
                        "sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
                        "bytes": wheel.stat().st_size, "notices_in_wheel": notices,
                        "external_notice": external_notice})
    if observed != expected:
        raise ValueError("downloaded wheel set is not the complete pinned input")
    lock = "".join(f"{r['name']}=={r['version']} --hash=sha256:{r['sha256']}\n"
                   for r in sorted(records, key=lambda r: r["name"]))
    (args.output / "requirements.lock").write_text(lock)
    manifest = {"schema": "ocr-python-component/v1", "status": "candidate component, not complete OCR runtime",
                "index": "https://pypi.org/simple",
                "python": sys.version, "cache_tag": sys.implementation.cache_tag,
                "platform": platform.platform(), "machine": platform.machine(),
                "input_sha256": hashlib.sha256(requirements.read_bytes()).hexdigest(),
                "lock_sha256": hashlib.sha256(lock.encode()).hexdigest(),
                "wheel_bytes": sum(r["bytes"] for r in records), "wheels": records}
    encoded = json.dumps(manifest, ensure_ascii=False, indent=2) + "\n"
    (args.output / "manifest.json").write_text(encoded)
    print(encoded, flush=True)


if __name__ == "__main__":
    main()
