import importlib.util
import io
import json
from pathlib import Path
import tarfile
import unittest

spec = importlib.util.spec_from_file_location("component", Path(__file__).resolve().parents[2] / "scripts/ocr/verify_engine_component.py")
component = importlib.util.module_from_spec(spec)
spec.loader.exec_module(component)


class ArchiveTests(unittest.TestCase):
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
