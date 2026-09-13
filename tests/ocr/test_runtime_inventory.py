import hashlib
import importlib.util
import json
import os
from pathlib import Path
import unittest


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


BASE = Path(__file__).parents[2]
archive_tests = load('archive_inventory_fixture', BASE / 'tests/ocr/test_runtime_archive.py')
worker = load('inventory_worker', BASE / 'src/parsing/pdf_layout_worker.py')


class RuntimeInventoryTests(unittest.TestCase):
    def setUp(self):
        fixture = archive_tests.RuntimeArchiveTests()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.base = fixture.base
        self.root = self.base / 'installed'
        self.manifest = fixture.output / 'runtime-manifest.json'
        archive_tests.runtime.unpack(fixture.archive, fixture.result['archive_sha256'], self.root, archive_tests.SOURCE)

    def verify(self):
        return worker.verify_runtime_package(self.root, self.manifest)

    def test_exact_installed_inventory_produces_actual_manifest_identity(self):
        identity = self.verify()
        self.assertEqual(identity, {'schema': 'ocr-private-runtime-archive/v1',
            'source_sha': archive_tests.SOURCE, 'python_path': '/opt/ocr-python/bin/python',
            'manifest_sha256': hashlib.sha256(self.manifest.read_bytes()).hexdigest()})
        self.assertEqual(os.readlink(self.root / 'usr/bin/python3'), '/usr/bin/python')

    def test_file_bytes_mode_and_symlink_changes_fail(self):
        path = self.root / 'usr/bin/python'
        original = path.read_bytes()
        path.write_bytes(b'X' + original[1:])
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            self.verify()
        path.write_bytes(original)
        path.chmod(0o644)
        with self.assertRaisesRegex(ValueError, 'mode mismatch'):
            self.verify()
        path.chmod(0o755)
        link = self.root / 'usr/bin/python3'
        link.unlink()
        link.symlink_to('/outside-runtime')
        with self.assertRaisesRegex(ValueError, 'symlink mismatch'):
            self.verify()

    def test_unlisted_and_missing_files_fail(self):
        extra = self.root / 'usr/bin/injected.py'
        extra.write_bytes(b'unlisted module')
        with self.assertRaisesRegex(ValueError, 'unlisted runtime entry'):
            self.verify()
        extra.unlink()
        (self.root / 'usr/bin/python').unlink()
        with self.assertRaises(FileNotFoundError):
            self.verify()

    def test_file_and_parent_symlinks_are_not_followed(self):
        path = self.root / 'usr/bin/python'
        path.unlink()
        path.symlink_to('/outside-runtime')
        with self.assertRaises((ValueError, OSError)):
            self.verify()
        parent = self.root / 'usr'
        parent.rename(self.base / 'outside-usr')
        parent.symlink_to('../outside-usr')
        with self.assertRaisesRegex(ValueError, 'mode mismatch|directory mismatch'):
            self.verify()
        # Even a matching-looking manifest cannot authorize a '..' path.
        manifest = json.loads(self.manifest.read_bytes())
        manifest['entries'][0]['path'] = '../outside'
        self.manifest.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'inventory path'):
            self.verify()

    def test_duplicate_json_and_special_manifest_rejected(self):
        encoded = self.manifest.read_text()
        self.manifest.write_text('{"schema":"duplicate",' + encoded[1:])
        with self.assertRaisesRegex(ValueError, 'duplicate runtime manifest field'):
            self.verify()
        self.manifest.unlink()
        os.mkfifo(self.manifest)
        with self.assertRaisesRegex(ValueError, 'bounded regular file'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
