"""Verify a reviewed archive digest, then reconstruct its offline engine tree."""
import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tempfile


def digest(data):
    return hashlib.sha256(data).hexdigest()


def safe_name(name):
    path = PurePosixPath(name)
    if not name or path.is_absolute() or ".." in path.parts or str(path) != name.rstrip("/"):
        raise ValueError("unsafe archive path")
    return str(path)


def apply_runtime_assembly(root, assembly):
    # .deb extraction intentionally does not run maintainer scripts. The
    # optional private Python runtime needs the distribution's POSIX shell link
    # explicitly, not a dependency on the host's /bin/sh.
    if not assembly:
        return
    if assembly != {"posix_shell": {"path": "usr/bin/sh", "target": "dash"}}:
        raise ValueError("unsupported private runtime assembly")
    if not (root / 'usr/bin/dash').is_file():
        raise ValueError("private POSIX shell executable missing")
    link = root / 'usr/bin/sh'
    if link.is_symlink():
        if str(link.readlink()) != 'dash':
            raise ValueError("private POSIX shell link mismatch")
    elif link.exists():
        raise ValueError("refuse to replace an existing POSIX shell file")
    else:
        link.symlink_to('dash')


def verify_archive(raw, expected_sha256):
    if not re.fullmatch(r"[0-9a-f]{64}", expected_sha256) or digest(raw) != expected_sha256:
        raise ValueError("archive digest mismatch")
    contents = {}
    seen = set()
    total = 0
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as archive:
        for member in archive:
            name = safe_name(member.name)
            if name in seen:
                raise ValueError("duplicate archive member")
            seen.add(name)
            if member.isdir():
                continue
            if not member.isfile():
                raise ValueError("archive links and special files forbidden")
            total += member.size
            if member.size > 128 * 1024 * 1024 or total > 512 * 1024 * 1024:
                raise ValueError("component archive size limit")
            contents[name] = archive.extractfile(member).read()
    manifest = json.loads(contents["manifest.json"])
    if manifest["schema"] != "ocr-engine-component/v1":
        raise ValueError("unsupported engine component schema")
    if digest(contents["apt-source-uris.txt"]) != manifest["sources_sha256"]:
        raise ValueError("source inventory digest mismatch")
    expected = {"manifest.json", "apt-source-uris.txt"}
    packages = set()
    for record in manifest["packages"]:
        name = safe_name(record["file"])
        if not name.startswith("debs/") or not name.endswith(".deb") or name in expected or record["package"] in packages:
            raise ValueError("duplicate or invalid package record")
        expected.add(name)
        packages.add(record["package"])
        data = contents[name]
        if len(data) != record["bytes"] or digest(data) != record["sha256"]:
            raise ValueError("package digest mismatch")
    if set(contents) != expected or not packages:
        raise ValueError("archive inventory mismatch")
    return manifest, contents


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.archive.stat().st_size > 512 * 1024 * 1024:
        raise ValueError("compressed archive too large")
    manifest, contents = verify_archive(args.archive.read_bytes(), args.sha256)
    aliases = {"bin": "usr/bin", "sbin": "usr/sbin", "lib": "usr/lib", "lib64": "usr/lib64"}
    if manifest["ubuntu_usr_merge_aliases"] != aliases:
        raise ValueError("unsupported filesystem assembly")
    args.output.mkdir(parents=True, exist_ok=False)
    for name, target in aliases.items():
        (args.output / target).mkdir(parents=True, exist_ok=True)
        (args.output / name).symlink_to(target, target_is_directory=True)
    with tempfile.TemporaryDirectory(prefix="verified-ocr-debs-") as directory:
        for index, record in enumerate(manifest["packages"]):
            deb = Path(directory) / f"{index}.deb"
            deb.write_bytes(contents[record["file"]])
            subprocess.run(["dpkg-deb", "--extract", str(deb), str(args.output)], check=True, timeout=30)
    apply_runtime_assembly(args.output, manifest.get('private_runtime_assembly', {}))
    files, links = [], []
    for path in sorted(args.output.rglob("*")):
        name = str(path.relative_to(args.output))
        if path.is_symlink():
            links.append({"path": name, "target": str(path.readlink())})
        elif path.is_file():
            files.append({"path": name, "bytes": path.stat().st_size, "sha256": digest(path.read_bytes())})
    if files != manifest["files"] or links != manifest["symlinks"]:
        raise ValueError("reconstructed filesystem does not match frozen manifest")
    print(json.dumps({"verified_archive_sha256": args.sha256, "packages": len(manifest["packages"]),
                      "files": len(files), "symlinks": len(links), "reconstruction": "exact"}))


if __name__ == "__main__":
    main()
