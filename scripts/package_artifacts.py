#!/usr/bin/env python3
"""Build a stable portable directory and ZIP without machine timestamps."""
from __future__ import annotations

import argparse
import shutil
import zipfile
from pathlib import Path

try:
    from .write_sha256 import write_sidecars
except ImportError:
    from write_sha256 import write_sidecars

ROOT = Path(__file__).resolve().parents[1]
DOCS = ["README.md", "CHANGELOG.md", "THIRD_PARTY_LICENSES.md", "docs/USER_GUIDE.md", "docs/PACKAGING.md", "docs/ASSETS.md", "docs/LICENSE_OPTIONS.md"]
WINDOWS_SCRIPTS = ["scripts/install-service.ps1", "scripts/uninstall-service.ps1", "scripts/firewall-rules.ps1", "scripts/firewall-rules-remove.ps1", "scripts/set-app-autostart.ps1", "scripts/remove-app-autostart.ps1"]
MAC_SCRIPTS = ["scripts/build-macos-app.sh", "scripts/notarize-macos.sh", "scripts/install-launch-agents.sh", "scripts/uninstall-launch-agents.sh"]
MAC_SUPPORT_FILES = ["packaging/macos/com.racc.connect.host-agent.plist.in"]


def package(version: str, platform: str, binary_dir: Path, output: Path, dry_run: bool = False) -> tuple[Path, Path]:
    executable_suffix = ".exe" if platform.startswith("windows-") else ""
    names = [f"racc-app{executable_suffix}", f"racc-host-agent{executable_suffix}"]
    artifacts = [binary_dir / name for name in names]
    if dry_run:
        for path in artifacts:
            print(f"Would include binary: {path}")
    elif any(not path.is_file() for path in artifacts):
        missing = [str(path) for path in artifacts if not path.is_file()]
        raise FileNotFoundError("required release binaries are missing: " + ", ".join(missing))
    for relative in DOCS:
        if not (ROOT / relative).is_file():
            raise FileNotFoundError(f"required package document is missing: {relative}")
    if not dry_run and not (ROOT / "assets/icons/racc-connect.icns").is_file():
        raise FileNotFoundError("generated application icon assets are missing")
    if not version or any(character not in "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz.+-" for character in version):
        raise ValueError("version contains unsafe path characters")
    output = output.resolve()
    expected_output = (ROOT / "dist").resolve()
    if output != expected_output:
        raise ValueError(f"output must remain in the repository dist directory: {expected_output}")
    package_name = f"racc-connect-{version}-{platform}"
    stage = output / package_name
    if stage.resolve().parent != output:
        raise ValueError("staging directory escapes the dist directory")
    archive = output / f"{package_name}.zip"
    checksum = archive.with_name(archive.name + ".sha256")
    if dry_run:
        print(f"Would stage portable directory: {stage}")
        print(f"Would create deterministic ZIP: {archive}")
        print(f"Would write SHA-256 sidecar: {archive.name}.sha256")
        return archive, checksum

    output.mkdir(parents=True, exist_ok=True)
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    for binary in artifacts:
        shutil.copy2(binary, stage / binary.name)
    for relative in DOCS:
        shutil.copy2(ROOT / relative, stage / Path(relative).name)
    for script in WINDOWS_SCRIPTS if platform.startswith("windows-") else MAC_SCRIPTS if platform.startswith("macos-") else []:
        source = ROOT / script
        if source.is_file():
            destination = stage / "scripts" / source.name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
    if platform.startswith("macos-"):
        for relative in MAC_SUPPORT_FILES:
            source = ROOT / relative
            destination = stage / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
    icon_directory = stage / "assets" / "icons"
    icon_directory.mkdir(parents=True)
    for icon in sorted((ROOT / "assets/icons").iterdir()):
        if icon.is_file():
            shutil.copy2(icon, icon_directory / icon.name)

    temp_archive = archive.with_suffix(".zip.tmp")
    temp_archive.unlink(missing_ok=True)
    with zipfile.ZipFile(temp_archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zipped:
        for path in sorted(stage.rglob("*")):
            if path.is_file():
                relative = Path(package_name) / path.relative_to(stage)
                info = zipfile.ZipInfo(relative.as_posix(), date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED

                info.external_attr = (0o100644 & 0xFFFF) << 16
                zipped.writestr(info, path.read_bytes(), compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)
    temp_archive.replace(archive)
    print(f"Portable package: {archive} ({archive.stat().st_size} bytes)")
    write_sidecars([archive])
    return archive, checksum


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform", required=True, choices=("windows-x64", "macos-x64", "linux-x64"))
    parser.add_argument("--binary-dir", type=Path, default=ROOT / "target" / "release")
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    package(args.version, args.platform, args.binary_dir, args.output, args.dry_run)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
