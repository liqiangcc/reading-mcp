import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("component", Path(__file__).resolve().parents[2] / "scripts/ocr/verify_engine_component.py")
component = importlib.util.module_from_spec(spec)
spec.loader.exec_module(component)


class ArchiveTests(unittest.TestCase):
    def test_private_runtime_shell_assembly_is_explicit_and_non_overwriting(self):
        assembly = {"posix_shell": {"path": "usr/bin/sh", "target": "dash"}}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'usr/bin').mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, 'executable missing'):
                component.apply_runtime_assembly(root, assembly)
            (root / 'usr/bin/dash').write_bytes(b'fixed shell payload')
            component.apply_runtime_assembly(root, assembly)
            component.apply_runtime_assembly(root, assembly)
            self.assertEqual(str((root / 'usr/bin/sh').readlink()), 'dash')
            self.assertEqual((root / 'usr/bin/sh').read_bytes(), b'fixed shell payload')
            (root / 'usr/bin/sh').unlink()
            (root / 'usr/bin/sh').write_bytes(b'preserve this file')
            with self.assertRaisesRegex(ValueError, 'refuse to replace'):
                component.apply_runtime_assembly(root, assembly)
            self.assertEqual((root / 'usr/bin/sh').read_bytes(), b'preserve this file')
            with self.assertRaisesRegex(ValueError, 'unsupported'):
                component.apply_runtime_assembly(root, {'posix_shell': {'path': '../escape', 'target': 'dash'}})

    def archive(self, payload=b"deb", extra=None):
        manifest = {"schema": "ocr-engine-component/v1", "sources_sha256": component.digest(b"sources"),
                    "packages": [{"package": "engine", "file": "debs/engine.deb", "bytes": 3,
                                  "sha256": component.digest(b"deb")}]}
        entries = [("manifest.json", json.dumps(manifest).encode()),
                   ("apt-source-uris.txt", b"sources"), ("debs/engine.deb", payload)]
        if extra:
            entries.append(extra)
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            for name, data in entries:
                entry = tarfile.TarInfo(name)
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
        return output.getvalue()

    def test_valid_exact_archive(self):
        raw = self.archive()
        manifest, contents = component.verify_archive(raw, component.digest(raw))
        self.assertEqual(len(manifest["packages"]), 1)
        self.assertEqual(contents["debs/engine.deb"], b"deb")

    def test_changed_outer_archive_rejected(self):
        with self.assertRaisesRegex(ValueError, "archive digest mismatch"):
            component.verify_archive(self.archive(), "0" * 64)

    def test_changed_deb_rejected_even_with_matching_outer_digest(self):
        raw = self.archive(payload=b"bad")
        with self.assertRaisesRegex(ValueError, "package digest mismatch"):
            component.verify_archive(raw, component.digest(raw))

    def test_escaping_duplicate_and_unlisted_entries_rejected(self):
        for name, error in [("../escape", "unsafe"), ("debs/engine.deb", "duplicate"), ("debs/extra.deb", "inventory")]:
            with self.subTest(name=name):
                raw = self.archive(extra=(name, b"deb"))
                with self.assertRaisesRegex(ValueError, error):
                    component.verify_archive(raw, component.digest(raw))


if __name__ == "__main__":
    unittest.main()
