# Assets and attribution

## Raccoon mark

The icon is an original, deliberately simple raccoon-face glyph drawn for this project as SVG geometry. It uses a rounded blue-purple badge, a dark mask and pale face; connected and disconnected tray exports add a small green or gray state dot. No third-party emoji pixels, Apple artwork, other product logos, or copied wordmarks are included.

- Source master: `assets/icons/racc-connect.svg`
- Template source: `assets/icons/racc-menubar-template.svg`
- Generator: `tools/icons/generate_icons.py` (Python standard library only)
- Reproduce from repository root: `python tools/icons/generate_icons.py`
- Output includes PNG sizes 16, 32, 48, 64, 128, 256, 512 and 1024; `racc-connect.ico`; `racc-connect.icns`; monochrome menu-bar template PNG; connected/disconnected tray PNGs; and 32×32 straight RGBA buffers for native tray APIs: connected and disconnected Windows icons and the monochrome macOS template.
- Modifications: raster exports, ICO/ICNS containers, and state-dot variants are generated from the in-repository original design. No external artwork was modified.
- Reproducibility check: `python -m unittest tools.icons.test_generate_icons` generates two copies and compares SHA-256 for every output.

The design is an original stylized glyph, not Google's Noto Emoji artwork. This is the permitted fallback because the agent did not verify the exact license and source commit for the Noto Emoji raccoon during this run.

## Noto Emoji raccoon option — BLOCKED-HUMAN

Do not replace the original artwork with U+1F99D until the owner or a later run verifies the exact source asset, commit/version, and license files in the official [Google Noto Emoji repository](https://github.com/googlefonts/noto-emoji). Required follow-up: inspect the source SVG/PNG for U+1F99D and the repository's applicable license file at the selected commit, record the commit hash and exact license text path here, confirm the license permits the intended use, and update `THIRD_PARTY_LICENSES.md` with attribution. Never use Apple emoji artwork.

## Included binary outputs

- Windows: `racc-connect.ico`, `racc-connect-16.png`, `racc-connect-32.png`, `racc-connect-48.png`, `racc-connect-256.png`, `racc-tray-connected.png`, `racc-tray-disconnected.png`, and `racc-tray-disconnected.rgba`, and `racc-tray-connected.rgba`.
- macOS: `racc-connect.icns`, `racc-menubar-template.png`.
- Cross-platform source: both SVG masters and all PNG sizes.

Native tray registration uses the disconnected 32×32 RGBA buffer on Windows and the monochrome template buffer on macOS; see `docs/PACKAGING.md`.
