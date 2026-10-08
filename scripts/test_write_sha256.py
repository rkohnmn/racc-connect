import hashlib
import tempfile
import unittest
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parents[1]

try:
    from .write_sha256 import write_sidecars
except ImportError:
    from write_sha256 import write_sidecars


class Sha256SidecarTests(unittest.TestCase):
    def test_sidecar_uses_stable_basename_format_and_digest(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-sha256-test-") as directory:
            artifact = Path(directory) / "racc-connect-test.zip"
            artifact.write_bytes(b"release artifact\x00payload")

            sidecar, digest = write_sidecars([artifact])[0]

            expected = hashlib.sha256(artifact.read_bytes()).hexdigest()
            self.assertEqual(digest, expected)
            self.assertEqual(sidecar.name, "racc-connect-test.zip.sha256")
            self.assertEqual(sidecar.read_bytes(), f"{expected}  {artifact.name}\n".encode("ascii"))

            first = sidecar.read_bytes()
            write_sidecars([artifact])
            self.assertEqual(sidecar.read_bytes(), first)

    def test_sidecar_is_updated_when_artifact_changes(self):
        with tempfile.TemporaryDirectory(dir=WORKSPACE_ROOT, prefix=".m10-sha256-test-") as directory:
            artifact = Path(directory) / "installer.exe"
            artifact.write_bytes(b"first")
            sidecar, first_digest = write_sidecars([artifact])[0]

            artifact.write_bytes(b"second")
            _, second_digest = write_sidecars([artifact])[0]

            self.assertNotEqual(first_digest, second_digest)
            self.assertIn(second_digest.encode("ascii"), sidecar.read_bytes())


if __name__ == "__main__":
    unittest.main()
