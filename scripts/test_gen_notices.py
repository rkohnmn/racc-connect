import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("gen-notices.py")
WORKSPACE_ROOT = SCRIPT.parents[1]


class NoticeGeneratorTests(unittest.TestCase):
    def fixture(self, root: Path, license: str | None = "MIT") -> Path:
        source = root / "crate"
        source.mkdir()
        (source / "Cargo.toml").write_text("[package]\nname='fixture'\n", encoding="utf-8")
        (source / "LICENSE-MIT").write_text("Fixture license text.\n", encoding="utf-8")
        packages = [
            {"id": "app-id", "name": "racc-app", "version": "0.1.0", "source": None, "manifest_path": str(root / "app.toml"), "license": None, "license_file": None},
            {"id": "dep-id", "name": "fixture-dep", "version": "1.2.3", "source": "registry+fixture", "manifest_path": str(source / "Cargo.toml"), "license": license, "license_file": None},
        ]
        metadata = {"packages": packages, "resolve": {"nodes": [
            {"id": "app-id", "deps": [{"pkg": "dep-id", "dep_kinds": [{"kind": None}]}]},
            {"id": "dep-id", "deps": []},
        ]}}
        path = root / "metadata.json"
        path.write_text(json.dumps(metadata), encoding="utf-8")
        return path

    def test_fixture_inventory_includes_package_and_available_license_text(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-notices-test-") as directory:
            root = Path(directory)
            metadata = self.fixture(root)
            output = root / "notices.md"
            subprocess.run([sys.executable, str(SCRIPT), "--metadata-file", str(metadata), "--output", str(output)], check=True)
            text = output.read_text(encoding="utf-8")
            self.assertIn("fixture-dep", text)
            self.assertIn("Fixture license text.", text)
            self.assertIn("Icon artwork", text)

    def test_missing_license_fails_check(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-notices-test-") as directory:
            metadata = self.fixture(Path(directory), "Unknown-License-9")
            result = subprocess.run([sys.executable, str(SCRIPT), "--metadata-file", str(metadata), "--check"], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Unrecognized dependency licenses", result.stderr)

    def test_generation_is_byte_reproducible_for_same_metadata(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-notices-test-") as directory:
            root = Path(directory)
            metadata = self.fixture(root)
            outputs = [root / "one.md", root / "two.md"]
            for output in outputs:
                subprocess.run([sys.executable, str(SCRIPT), "--metadata-file", str(metadata), "--output", str(output)], check=True)
            digests = [hashlib.sha256(output.read_bytes()).hexdigest() for output in outputs]
            self.assertEqual(digests[0], digests[1])

    def test_check_passes_when_notice_matches_fixture_inputs(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-notices-test-") as directory:
            root = Path(directory)
            metadata = self.fixture(root)
            assets = root / "ASSETS.md"
            assets.write_text("# Assets\n\n## Raccoon mark\nOriginal fixture artwork.\n", encoding="utf-8")
            output = root / "notices.md"
            common = ["--metadata-file", str(metadata), "--assets-file", str(assets), "--output", str(output)]
            subprocess.run([sys.executable, str(SCRIPT), *common], check=True)
            subprocess.run([sys.executable, str(SCRIPT), *common, "--check"], check=True)
            self.assertIn("Original fixture artwork.", output.read_text(encoding="utf-8"))

    def test_check_rejects_changed_dependency_metadata(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-notices-test-") as directory:
            root = Path(directory)
            metadata = self.fixture(root)
            output = root / "notices.md"
            subprocess.run([sys.executable, str(SCRIPT), "--metadata-file", str(metadata), "--output", str(output)], check=True)
            data = json.loads(metadata.read_text(encoding="utf-8"))
            next(package for package in data["packages"] if package["name"] == "fixture-dep")["version"] = "2.0.0"
            metadata.write_text(json.dumps(data), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--metadata-file", str(metadata), "--output", str(output), "--check"],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is stale", result.stderr)

    def test_check_rejects_changed_artwork_attribution(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-notices-test-") as directory:
            root = Path(directory)
            metadata = self.fixture(root)
            assets = root / "ASSETS.md"
            assets.write_text("# Assets\n\n## Raccoon mark\nOriginal fixture artwork.\n", encoding="utf-8")
            output = root / "notices.md"
            common = ["--metadata-file", str(metadata), "--assets-file", str(assets), "--output", str(output)]
            subprocess.run([sys.executable, str(SCRIPT), *common], check=True)
            assets.write_text("# Assets\n\n## Raccoon mark\nUpdated fixture attribution.\n", encoding="utf-8")
            result = subprocess.run([sys.executable, str(SCRIPT), *common, "--check"], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is stale", result.stderr)


if __name__ == "__main__":
    unittest.main()
