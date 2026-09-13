import copy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('runtime_archive', Path(__file__).parents[2] / 'scripts/ocr/runtime_archive.py')
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)
SOURCE = '1' * 40


class RuntimeArchiveTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.root = self.base / 'input'
        (self.root / 'usr/bin').mkdir(parents=True)
        (self.root / 'usr/bin/python').write_bytes(b'fixed synthetic interpreter bytes')
        (self.root / 'usr/bin/python').chmod(0o755)
        (self.root / 'bin').symlink_to('usr/bin')
        (self.root / 'usr/bin/python3').symlink_to('/usr/bin/python')
        notice = self.root / runtime.NOTICE_ROOT
        notice.mkdir(parents=True)
        for name in runtime.PROVENANCE:
            (notice / name).write_text('synthetic provenance ' + name)
        for name in ('tmp', 'opt/ocr-smoke', 'opt/ocr-bootstrap'):
            (self.root / name).mkdir(parents=True)
            (self.root / name / 'not-runtime').write_bytes(b'excluded bootstrap/test bytes')
        self.output = self.base / 'package'
        self.result = runtime.build(self.root, self.output, SOURCE)
        self.archive = self.output / 'ocr-private-runtime.tar.gz'

    def rewrite(self, change):
        result = self.base / 'changed.tar.gz'
        with tarfile.open(self.archive) as source, tarfile.open(result, 'w:gz') as target:
            for header in source:
                payload = source.extractfile(header).read() if header.isfile() else None
                header, payload = change(copy.copy(header), payload)
                if payload is not None:
                    header.size = len(payload)
                target.addfile(header, io.BytesIO(payload) if payload is not None else None)
        return result

    def reject(self, archive, message):
        target = self.base / 'rejected'
        with self.assertRaisesRegex((ValueError, OSError), message):
            runtime.unpack(archive, runtime.sha_file(archive), target, SOURCE)
        self.assertFalse(target.exists())
        self.assertFalse(list(self.base.glob('.ocr-runtime-stage-*')))

    def test_deterministic_archive_and_exact_round_trip(self):
        second = runtime.build(self.root, self.base / 'second-package', SOURCE)
        self.assertEqual(second, self.result)
        restored = self.base / 'restored'
        result = runtime.unpack(self.archive, self.result['archive_sha256'], restored, SOURCE)
        self.assertEqual(result['regular_file_bytes'], self.result['regular_file_bytes'])
        self.assertEqual(runtime.inventory(restored), runtime.inventory(self.root))
        self.assertEqual(os.readlink(restored / 'usr/bin/python3'), '/usr/bin/python')
        self.assertFalse((restored / 'opt/ocr-bootstrap').exists())
        self.assertEqual(list((restored / 'tmp').iterdir()), [])
        self.assertEqual(list((restored / 'opt/ocr-smoke').iterdir()), [])

    def test_payload_tamper_rejected_even_with_new_outer_digest(self):
        def change(header, payload):
            if header.name == 'rootfs/usr/bin/python':
                payload = b'X' + payload[1:]
            return header, payload
        self.reject(self.rewrite(change), 'payload digest mismatch')

    def test_wrong_archive_digest_and_source_never_publish(self):
        for digest, source, error in [('0' * 64, SOURCE, 'archive digest'),
                                      (self.result['archive_sha256'], '2' * 40, 'source SHA')]:
            with self.assertRaisesRegex(ValueError, error):
                runtime.unpack(self.archive, digest, self.base / 'rejected', source)
            self.assertFalse((self.base / 'rejected').exists())
            self.assertFalse(list(self.base.glob('.ocr-runtime-stage-*')))

    def test_changed_bytes_consumed_after_precheck_cannot_publish(self):
        changed_payload = b'Xixed synthetic interpreter bytes'
        def change(header, payload):
            if header.name == 'runtime-manifest.json':
                manifest = json.loads(payload)
                record = next(item for item in manifest['entries'] if item['path'] == 'usr/bin/python')
                record['sha256'] = hashlib.sha256(changed_payload).hexdigest()
                payload = json.dumps(manifest).encode()
            elif header.name == 'rootfs/usr/bin/python':
                payload = changed_payload
            return header, payload
        archive = self.rewrite(change)
        destination = self.base / 'changed-after-precheck'
        # Model a substitution after successful precheck. Inner content hashes
        # are self-consistent, but the actual consumed outer bytes are not pinned.
        with archive.open('rb') as stream, self.assertRaisesRegex(ValueError, 'changed during extraction'):
            runtime.unpack_stream(runtime.ArchiveReader(stream), self.result['archive_sha256'], destination, SOURCE)
        self.assertFalse(destination.exists())
        self.assertFalse(list(self.base.glob('.ocr-runtime-stage-*')))

    def test_member_escape_and_hardlink_rejected(self):
        for hardlink in (False, True):
            def change(header, payload):
                if header.name == 'rootfs/usr/bin/python':
                    if hardlink:
                        header.type, header.linkname, header.size = tarfile.LNKTYPE, '/outside', 0
                        payload = None
                    else:
                        header.name = '../outside'
                return header, payload
            self.reject(self.rewrite(change), 'inventory mismatch|type or size mismatch')
            self.assertFalse((self.base / 'outside').exists())

    def test_symlink_escape_and_write_through_link_rejected_in_manifest(self):
        original = json.loads((self.output / 'runtime-manifest.json').read_text())
        changed = copy.deepcopy(original)
        next(item for item in changed['entries'] if item['path'] == 'bin')['target'] = '../outside'
        with self.assertRaisesRegex(ValueError, 'symlink escapes root'):
            runtime.validate(changed)
        changed = copy.deepcopy(original)
        entry = copy.deepcopy(next(item for item in changed['entries'] if item['path'] == 'usr/bin/python'))
        entry['path'] = 'bin/injected'
        changed['entries'].append(entry)
        changed['entries'].sort(key=lambda item: item['path'])
        with self.assertRaisesRegex(ValueError, 'non-directory ancestor'):
            runtime.validate(changed)

    def test_existing_destination_and_publication_race_preserve_old_directory(self):
        destination = self.base / 'existing'
        destination.mkdir()
        (destination / 'keep').write_bytes(b'old runtime')
        with self.assertRaisesRegex(ValueError, 'already exists'):
            runtime.unpack(self.archive, self.result['archive_sha256'], destination, SOURCE)
        self.assertEqual((destination / 'keep').read_bytes(), b'old runtime')
        staging = self.base / 'racing-stage'
        staging.mkdir()
        empty_existing = self.base / 'empty-existing'
        empty_existing.mkdir()
        with self.assertRaises(OSError):
            runtime.publish_new_directory(staging, empty_existing)
        self.assertTrue(staging.is_dir())
        self.assertTrue(empty_existing.is_dir())


if __name__ == '__main__':
    unittest.main()
