import hashlib
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

GENERATOR = Path(__file__).with_name("generate_icons.py")
CHECKED_IN = GENERATOR.parent.parent.parent / "assets" / "icons"


class IconGeneratorTests(unittest.TestCase):
    def test_outputs_are_reproducible_and_have_expected_container_headers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            outputs = []
            for name in ("first", "second"):
                output = root / name
                subprocess.run([sys.executable, str(GENERATOR), "--output", str(output)], check=True)
                outputs.append(output)
            files_a = sorted(path.name for path in outputs[0].iterdir())
            files_b = sorted(path.name for path in outputs[1].iterdir())
            self.assertEqual(files_a, files_b)
            for name in files_a:
                self.assertEqual(hashlib.sha256((outputs[0] / name).read_bytes()).digest(), hashlib.sha256((outputs[1] / name).read_bytes()).digest())
            self.assertEqual((outputs[0] / "racc-connect.ico").read_bytes()[:4], b"\x00\x00\x01\x00")
            self.assertEqual((outputs[0] / "racc-connect.icns").read_bytes()[:4], b"icns")
            png = (outputs[0] / "racc-connect-64.png").read_bytes()
            self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")
            self.assertEqual(struct.unpack(">II", png[16:24]), (64, 64))
            self.assertTrue((outputs[0] / "racc-tray-connected.png").exists())
            self.assertTrue((outputs[0] / "racc-tray-disconnected.png").exists())
            raw_tray = (outputs[0] / "racc-tray-disconnected.rgba").read_bytes()
            self.assertEqual(len(raw_tray), 32 * 32 * 4)
            self.assertNotEqual(raw_tray, bytes(len(raw_tray)))
            connected_tray = (outputs[0] / "racc-tray-connected.rgba").read_bytes()
            self.assertEqual(len(connected_tray), 32 * 32 * 4)
            self.assertNotEqual(connected_tray, raw_tray)
            template_tray = (outputs[0] / "racc-menubar-template.rgba").read_bytes()
            self.assertEqual(len(template_tray), 32 * 32 * 4)
            self.assertNotEqual(template_tray, raw_tray)
            self.assertTrue((outputs[0] / "racc-menubar-template.png").exists())

    def test_checked_in_assets_match_generator_output(self):
        with tempfile.TemporaryDirectory() as directory:
            generated = Path(directory) / "icons"
            subprocess.run([sys.executable, str(GENERATOR), "--output", str(generated)], check=True)
            generated_files = {path.name: path.read_bytes() for path in generated.iterdir() if path.is_file()}
            checked_in_files = {path.name: path.read_bytes() for path in CHECKED_IN.iterdir() if path.is_file()}
            self.assertEqual(set(generated_files), set(checked_in_files))
            self.assertEqual(generated_files, checked_in_files)


if __name__ == "__main__":
    unittest.main()
