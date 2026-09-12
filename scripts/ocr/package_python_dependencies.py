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
import zipfile


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
    subprocess.run([sys.executable, "-m", "pip", "download", "--only-binary=:all:",
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
            if not notices:
                raise ValueError("wheel lacks bundled license notice: " + name)
        records.append({"name": name, "version": version, "file": "wheels/" + wheel.name,
                        "sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
                        "bytes": wheel.stat().st_size, "notices_in_wheel": notices})
    if observed != expected:
        raise ValueError("downloaded wheel set is not the complete pinned input")
    lock = "".join(f"{r['name']}=={r['version']} --hash=sha256:{r['sha256']}\n"
                   for r in sorted(records, key=lambda r: r["name"]))
    (args.output / "requirements.lock").write_text(lock)
    manifest = {"schema": "ocr-python-component/v1", "status": "candidate component, not complete OCR runtime",
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
