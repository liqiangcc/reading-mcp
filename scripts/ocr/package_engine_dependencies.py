"""Resolve an empty-system Debian closure on hosted Ubuntu; no host installation."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


ROOTS = ["tesseract-ocr=5.3.4-1build5", "libtesseract5=5.3.4-1build5",
         "liblept5=1.82.0-3build4", "tesseract-ocr-eng=1:4.1.0-2",
         "tesseract-ocr-chi-sim=1:4.1.0-2"]


def run(args, **kwargs):
    return subprocess.run(args, check=True, timeout=240, **kwargs)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    debs = output / "debs"
    debs.mkdir()
    rootfs = output / "rootfs"
    rootfs.mkdir()
    # Ubuntu 24.04 packages assume the usr-merged base filesystem. Extracting
    # .deb data does not execute base-files maintainer scripts; preserve these
    # explicit assembly inputs rather than resolving ELF paths on the host.
    aliases = {"bin": "usr/bin", "sbin": "usr/sbin", "lib": "usr/lib", "lib64": "usr/lib64"}
    for name, target in aliases.items():
        (rootfs / target).mkdir(parents=True, exist_ok=True)
        (rootfs / name).symlink_to(target, target_is_directory=True)
    with tempfile.TemporaryDirectory(prefix="ocr-apt-empty-state-") as directory:
        status = Path(directory) / "status"
        status.write_text("")
        command = ["apt-get", "-o", "Debug::NoLocking=1", "-o", f"Dir::State::status={status}",
                   "-o", f"Dir::Cache::archives={debs}", "--assume-yes",
                   "--no-install-recommends", "--download-only"]
        sources = run(command + ["--print-uris", "install", *ROOTS],
                      text=True, capture_output=True).stdout
        (output / "apt-source-uris.txt").write_text(sources)
        run(command + ["install", *ROOTS])
    records = []
    for deb in sorted(debs.glob("*.deb")):
        fields = run(["dpkg-deb", "--show", "--showformat=${Package}\n${Version}\n${Architecture}\n", str(deb)],
                     text=True, capture_output=True).stdout.splitlines()
        if len(fields) != 3:
            raise ValueError("invalid Debian package metadata")
        package, version, arch = fields
        records.append({"package": package, "version": version, "architecture": arch,
                        "file": "debs/" + deb.name, "bytes": deb.stat().st_size,
                        "sha256": hashlib.sha256(deb.read_bytes()).hexdigest()})
        run(["dpkg-deb", "--extract", str(deb), str(rootfs)])
    versions = {r["package"]: r["version"] for r in records}
    for root in ROOTS:
        package, version = root.split("=", 1)
        if versions.get(package) != version:
            raise ValueError("missing or changed pinned root package: " + package)
    # Distribution notices must survive extraction; keep complete .deb files too.
    for record in records:
        notice = rootfs / "usr/share/doc" / record["package"] / "copyright"
        if not notice.is_file():
            raise ValueError("distribution copyright absent: " + record["package"])
        record["copyright_sha256"] = hashlib.sha256(notice.read_bytes()).hexdigest()
    files = []
    symlinks = []
    for path in sorted(rootfs.rglob("*")):
        if path.is_symlink():
            symlinks.append({"path": str(path.relative_to(rootfs)), "target": str(path.readlink())})
        elif path.is_file():
            files.append({"path": str(path.relative_to(rootfs)), "bytes": path.stat().st_size,
                          "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
    manifest = {"schema": "ocr-engine-component/v1", "status": "candidate component, not deployment",
                "roots": ROOTS, "resolver": "apt empty dpkg status, no recommends",
                "sources_sha256": hashlib.sha256(sources.encode()).hexdigest(),
                "packages": records, "files": files, "symlinks": symlinks,
                "ubuntu_usr_merge_aliases": aliases,
                "deb_bytes": sum(r["bytes"] for r in records),
                "extracted_regular_file_bytes": sum(f["bytes"] for f in files)}
    encoded = json.dumps(manifest, indent=2) + "\n"
    (output / "manifest.json").write_text(encoded)
    print(encoded, flush=True)


if __name__ == "__main__":
    main()
