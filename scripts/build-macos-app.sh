# Build an installable .app bundle on macOS. Never run this on another OS.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then echo 'Run this script on macOS.' >&2; exit 2; fi
cd "$ROOT"
VERSION="$(cargo metadata --no-deps --format-version 1 --locked --offline | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["version"] for p in m["packages"] if p["name"]=="racc-app"))')"
TARGET="x86_64-apple-darwin"
cargo build --release --locked --target "$TARGET" -p racc-app -p racc-host-agent
APP="$ROOT/dist/Racc Connect.app"
CONTENTS="$APP/Contents"
rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$ROOT/target/$TARGET/release/racc-app" "$CONTENTS/MacOS/racc-app"
cp "$ROOT/target/$TARGET/release/racc-host-agent" "$CONTENTS/MacOS/racc-host-agent"
cp "$ROOT/assets/icons/racc-connect.icns" "$CONTENTS/Resources/racc-connect.icns"
cp "$ROOT/assets/icons/racc-menubar-template.png" "$CONTENTS/Resources/racc-menubar-template.png"
cp "$ROOT/THIRD_PARTY_LICENSES.md" "$CONTENTS/Resources/THIRD_PARTY_LICENSES.md"
cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.racc.connect</string>
<key>CFBundleName</key><string>Racc Connect</string>
<key>CFBundleDisplayName</key><string>Racc Connect</string>
<key>CFBundleExecutable</key><string>racc-app</string>
<key>CFBundleIconFile</key><string>racc-connect</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$VERSION</string>
<key>CFBundleVersion</key><string>$VERSION</string>
<key>LSUIElement</key><false/>
<key>NSScreenCaptureUsageDescription</key><string>Racc Connect needs Screen Recording access to share this Mac's display.</string>
<key>NSAccessibilityUsageDescription</key><string>Racc Connect needs Accessibility access to send remote keyboard and mouse input.</string>
</dict></plist>
PLIST
# Ad-hoc signing supports local testing only and does not use credentials or enable notarization.
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
plutil -lint "$CONTENTS/Info.plist"
APP_ARCHIVE="$ROOT/dist/racc-connect-$VERSION-macos-x64-app.zip"
mkdir -p "$ROOT/dist"
rm -f "$APP_ARCHIVE"
ditto -c -k --keepParent --sequesterRsrc "$APP" "$APP_ARCHIVE"
python3 scripts/write_sha256.py "$APP_ARCHIVE"
printf 'Created %s and %s for version %s (ad-hoc signed; personal testing only).\n' "$APP" "$APP_ARCHIVE" "$VERSION"
