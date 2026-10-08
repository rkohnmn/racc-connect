"""Regression tests for files referenced by packaging manifests."""
from __future__ import annotations

import re
import unittest

from package_artifacts import ROOT, WINDOWS_SCRIPTS


class PackageManifestTests(unittest.TestCase):
    def test_windows_installer_uninstall_helpers_are_in_portable_stage(self):
        installer = (ROOT / "packaging/windows/racc-connect.iss").read_text(encoding="utf-8")
        uninstall_section = installer.split("[UninstallRun]", 1)[1].split("[", 1)[0]
        referenced = {
            f"scripts/{name}"
            for name in re.findall(r'\\scripts\\([^" ]+\.ps1)', uninstall_section)
        }
        self.assertTrue(referenced)
        self.assertTrue(referenced.issubset(set(WINDOWS_SCRIPTS)))

    def test_every_windows_packaging_script_exists(self):
        for relative in WINDOWS_SCRIPTS:
            with self.subTest(script=relative):
                self.assertTrue((ROOT / relative).is_file(), relative)


if __name__ == "__main__":
    unittest.main()