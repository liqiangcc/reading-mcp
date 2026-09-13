"""Candidate private OCR root archive. No install, service switch, or source download.

Absolute symlinks retain *rooted* semantics; never resolve them against the host.
Only a reviewed outer digest authorizes reconstruction into a new destination.
"""
import argparse
import ctypes
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import tarfile
import tempfile

SCHEMA = 'ocr-private-runtime-archive/v1'
MAX_TOTAL = 1024 * 1024 * 1024
MAX_FILE = 256 * 1024 * 1024
MAX_MANIFEST = 32 * 1024 * 1024
EMPTY_DIRS = {'tmp', 'run', 'dev', 'proc', 'sys', 'opt/ocr-smoke'}
OMIT = {'opt/ocr-bootstrap'}
PROVENANCE = ('engine-manifest.json', 'python-manifest.json', 'requirements.lock', 'apt-source-uris.txt')
NOTICE_ROOT = 'usr/share/doc/reading-mcp-ocr'


def sha_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(65536), b''):
            digest.update(chunk)
    return digest.hexdigest()


def safe_path(name):
    if (not isinstance(name, str) or not name or '\0' in name or len(name.encode()) > 4096
            or PurePosixPath(name).is_absolute() or '..' in PurePosixPath(name).parts
            or str(PurePosixPath(name)) != name or name == '.'):
        raise ValueError('unsafe runtime archive path')
    return name


def rooted_target(name, target):
    if not isinstance(target, str) or not target or '\0' in target or len(target.encode()) > 4096:
        raise ValueError('invalid runtime symlink')
    parts = [] if target.startswith('/') else list(PurePosixPath(name).parent.parts)
    for part in PurePosixPath(target).parts:
        if part in ('/', '.'):
            continue
        if part == '..':
            if not parts:
                raise ValueError('runtime symlink escapes root')
            parts.pop()
        else:
            parts.append(part)
    return '/'.join(parts)


def validate(manifest):
    if manifest.get('schema') != SCHEMA or not re.fullmatch('[0-9a-f]{40}', manifest.get('source_sha', '')):
        raise ValueError('invalid runtime manifest identity')
    entries = manifest['entries']
    if not isinstance(entries, list) or not 0 < len(entries) <= 50000:
        raise ValueError('runtime inventory count limit')
    paths = [safe_path(item['path']) for item in entries]
    if paths != sorted(set(paths)):
        raise ValueError('runtime inventory must be unique and sorted')
    indexed = dict(zip(paths, entries))
    total = 0
    for name, item in indexed.items():
        for parent in PurePosixPath(name).parents:
            if str(parent) != '.' and indexed.get(str(parent), {}).get('kind') != 'directory':
                raise ValueError('runtime entry has non-directory ancestor')
        kind = item['kind']
        keys = {'path', 'kind'}
        if kind in ('file', 'directory'):
            keys.add('mode')
            if type(item['mode']) is not int or not 0 <= item['mode'] <= 0o7777:
                raise ValueError('invalid runtime mode')
        if kind == 'file':
            keys |= {'bytes', 'sha256'}
            if type(item['bytes']) is not int or not 0 <= item['bytes'] <= MAX_FILE:
                raise ValueError('runtime file size limit')
            if not re.fullmatch('[0-9a-f]{64}', item['sha256']):
                raise ValueError('invalid runtime file hash')
            total += item['bytes']
        elif kind == 'symlink':
            keys.add('target')
            rooted_target(name, item['target'])
        elif kind != 'directory':
            raise ValueError('runtime special files forbidden')
        if set(item) != keys:
            raise ValueError('unexpected runtime inventory fields')
    if type(manifest['regular_file_bytes']) is not int or total > MAX_TOTAL or manifest['regular_file_bytes'] != total:
        raise ValueError('runtime total size mismatch or limit')
    expected_provenance = {}
    for name in PROVENANCE:
        record = indexed.get(f'{NOTICE_ROOT}/{name}', {})
        if record.get('kind') != 'file':
            raise ValueError('runtime provenance missing')
        expected_provenance[name] = record['sha256']
    if manifest['provenance'] != expected_provenance:
        raise ValueError('runtime provenance identity mismatch')
    return entries


def inventory(root):
    entries = []
    def walk_error(error):
        raise error
    for directory, dirs, files in os.walk(root, followlinks=False, onerror=walk_error):
        for leaf in sorted(dirs + files):
            path = Path(directory) / leaf
            name = path.relative_to(root).as_posix()
            if name in OMIT:
                if leaf in dirs:
                    dirs.remove(leaf)
                continue
            info = path.lstat()
            record = {'path': name}
            if stat.S_ISLNK(info.st_mode):
                record.update(kind='symlink', target=os.readlink(path))
            elif stat.S_ISDIR(info.st_mode):
                record.update(kind='directory', mode=stat.S_IMODE(info.st_mode))
                if name in EMPTY_DIRS:
                    dirs.remove(leaf)
            elif stat.S_ISREG(info.st_mode):
                record.update(kind='file', mode=stat.S_IMODE(info.st_mode),
                              bytes=info.st_size, sha256=sha_file(path))
            else:
                raise ValueError('runtime special files forbidden')
            entries.append(record)
    return sorted(entries, key=lambda item: item['path'])


def build(root, output, source_sha, status='candidate'):
    if status not in ('candidate', 'formal-v1'):
        raise ValueError('unsupported runtime archive status')
    entries = inventory(root)
    indexed = {item['path']: item for item in entries}
    manifest = {'schema': SCHEMA, 'source_sha': source_sha,
        'status': status,
        'entries': entries, 'regular_file_bytes': sum(item.get('bytes', 0) for item in entries),
        'provenance': {name: indexed[f'{NOTICE_ROOT}/{name}']['sha256'] for name in PROVENANCE}}
    validate(manifest)
    encoded = json.dumps(manifest, sort_keys=True, separators=(',', ':')).encode()
    if len(encoded) > MAX_MANIFEST:
        raise ValueError('runtime manifest size limit')
    output.mkdir(parents=True, exist_ok=False)
    archive_path = output / 'ocr-private-runtime.tar.gz'
    with archive_path.open('xb') as raw, gzip.GzipFile(filename='', fileobj=raw, mode='wb', mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode='w|', format=tarfile.PAX_FORMAT) as archive:
            header = tarfile.TarInfo('runtime-manifest.json')
            header.size, header.mode = len(encoded), 0o644
            archive.addfile(header, io.BytesIO(encoded))
            for item in entries:
                header = tarfile.TarInfo('rootfs/' + item['path'])
                header.mode = item.get('mode', 0o777)
                if item['kind'] == 'directory':
                    header.type = tarfile.DIRTYPE
                    archive.addfile(header)
                elif item['kind'] == 'symlink':
                    header.type, header.linkname = tarfile.SYMTYPE, item['target']
                    archive.addfile(header)
                else:
                    header.size = item['bytes']
                    with (root / item['path']).open('rb') as payload:
                        archive.addfile(header, payload)
    (output / 'runtime-manifest.json').write_bytes(encoded)
    digest = sha_file(archive_path)
    (output / 'SHA256SUMS').write_text(f'{digest}  {archive_path.name}\n')
    return {'archive_sha256': digest, 'archive_bytes': archive_path.stat().st_size,
            'regular_file_bytes': manifest['regular_file_bytes'], 'entries': len(entries), 'source_sha': source_sha}


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def publish_new_directory(source, destination):
    # Linux deployment target: atomic no-replace, including an existing empty dir.
    libc = ctypes.CDLL(None, use_errno=True)
    rename = libc.renameat2
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    if rename(-100, os.fsencode(source), -100, os.fsencode(destination), 1) != 0:
        raise OSError(ctypes.get_errno(), 'runtime directory publication failed')


def unique_json_pairs(pairs):
    result = {}
    for name, value in pairs:
        if name in result:
            raise ValueError('duplicate runtime JSON field')
        result[name] = value
    return result


class ArchiveReader:
    def __init__(self, stream):
        self.stream = stream
        self.digest = hashlib.sha256()
        self.bytes = 0

    def read(self, size):
        data = self.stream.read(size)
        self.bytes += len(data)
        if self.bytes > MAX_TOTAL:
            raise ValueError('compressed runtime archive size limit')
        self.digest.update(data)
        return data


def unpack(archive_path, expected_sha256, output, source_sha):
    if os.path.lexists(output):
        raise ValueError('runtime destination already exists')
    if not re.fullmatch('[0-9a-f]{64}', expected_sha256):
        raise ValueError('runtime archive digest mismatch or size limit')
    descriptor = os.open(archive_path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_NOFOLLOW)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_TOTAL:
            raise ValueError('runtime archive must be a bounded regular file')
        reader = ArchiveReader(source)
        while reader.read(65536):
            pass
        if reader.digest.hexdigest() != expected_sha256:
            raise ValueError('runtime archive digest mismatch')
        source.seek(0)
        # Authenticate again over the exact compressed bytes consumed while
        # extracting. Neither path substitution nor in-place changes may publish.
        return unpack_stream(ArchiveReader(source), expected_sha256, output, source_sha)


def unpack_stream(reader, expected_sha256, output, source_sha):
    # Parent is an explicit existing operator-owned directory; do not invent it.
    with tempfile.TemporaryDirectory(prefix='.ocr-runtime-stage-', dir=output.parent) as staging:
        root = Path(staging) / 'rootfs'
        root.mkdir()
        with tarfile.open(fileobj=reader, mode='r|gz') as archive:
            header = archive.next()
            if header is None or header.name != 'runtime-manifest.json' or not header.isfile() or header.size > MAX_MANIFEST:
                raise ValueError('runtime archive requires first manifest')
            manifest = json.loads(archive.extractfile(header).read(), object_pairs_hook=unique_json_pairs)
            entries = validate(manifest)
            if manifest['source_sha'] != source_sha:
                raise ValueError('runtime source SHA mismatch')
            links = []
            for item in entries:
                header = archive.next()
                if header is None or header.name != 'rootfs/' + item['path']:
                    raise ValueError('runtime archive inventory mismatch')
                path = root / item['path']
                if item['kind'] == 'directory' and header.isdir():
                    path.mkdir(mode=0o700)
                elif item['kind'] == 'symlink' and header.issym() and header.linkname == item['target']:
                    links.append((path, item['target']))
                elif item['kind'] == 'file' and header.isfile() and header.size == item['bytes']:
                    digest = hashlib.sha256()
                    with path.open('xb') as payload, archive.extractfile(header) as stream:
                        for chunk in iter(lambda: stream.read(65536), b''):
                            digest.update(chunk)
                            payload.write(chunk)
                        payload.flush()
                        os.fchmod(payload.fileno(), item['mode'])
                        os.fsync(payload.fileno())
                    if digest.hexdigest() != item['sha256']:
                        raise ValueError('runtime payload digest mismatch')
                else:
                    raise ValueError('runtime archive type or size mismatch')
                if item['kind'] != 'symlink' and header.mode != item['mode']:
                    raise ValueError('runtime archive mode mismatch')
            if archive.next() is not None:
                raise ValueError('runtime archive has unlisted member')
        while reader.read(65536):
            pass
        if reader.digest.hexdigest() != expected_sha256:
            raise ValueError('runtime archive changed during extraction')
        # No writes traverse archive symlinks. Absolute links are only meaningful
        # inside RootDirectory; neither validation nor extraction follows them.
        for path, target in links:
            path.symlink_to(target)
        for item in reversed(entries):
            if item['kind'] == 'directory':
                path = root / item['path']
                path.chmod(item['mode'])
                sync_directory(path)
        sync_directory(root)
        publish_new_directory(root, output)
        sync_directory(Path(staging))
        sync_directory(output.parent)
    return {'archive_sha256': expected_sha256, 'source_sha': source_sha,
            'regular_file_bytes': manifest['regular_file_bytes'], 'entries': len(entries),
            'reconstruction': 'all bytes, modes and symlink targets verified'}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('operation', choices=['build', 'unpack'])
    parser.add_argument('--root', type=Path)
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--sha256')
    parser.add_argument('--source-sha', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--status', choices=('candidate', 'formal-v1'), default='candidate')
    args = parser.parse_args()
    if args.operation == 'build':
        result = build(args.root, args.output, args.source_sha, args.status)
    else:
        result = unpack(args.archive, args.sha256, args.output, args.source_sha)
    print(json.dumps(result, sort_keys=True), flush=True)


if __name__ == '__main__':
    main()
