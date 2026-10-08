# Notarize an app bundle and create a DMG. Run manually on macOS with owner-owned credentials.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$(uname -s)" != Darwin ]]; then echo 'Run this script on macOS.' >&2; exit 2; fi
: "${DEVELOPER_ID_APPLICATION:?Set the Developer ID Application identity in your shell}"
: "${AC_NOTARY_PROFILE:?Set the notarytool keychain profile name in your shell}"
APP="${1:-$ROOT/dist/Racc Connect.app}"
if [[ ! -d "$APP" ]]; then echo "App bundle not found: $APP" >&2; exit 2; fi
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
ZIP="$ROOT/dist/racc-connect-$VERSION-macos-x64-notarize.zip"
DMG="$ROOT/dist/racc-connect-$VERSION-macos-x64.dmg"
# The identity and profile refer to credentials held by the human in Keychain; this script does not collect or print secrets.
codesign --force --options runtime --timestamp --sign "$DEVELOPER_ID_APPLICATION" "$APP"
codesign --verify --deep --strict "$APP"
ditto -c -k --keepParent --sequesterRsrc "$APP" "$ZIP"
xcrun notarytool submit "$ZIP" --keychain-profile "$AC_NOTARY_PROFILE" --wait
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
hdiutil create -volname 'Racc Connect' -srcfolder "$APP" -ov -format UDZO "$DMG"
shasum -a 256 "$DMG" > "$DMG.sha256"
printf 'Notarized output prepared: %s\n' "$DMG"
