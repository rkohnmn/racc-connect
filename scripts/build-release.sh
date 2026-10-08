#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DRY_RUN=false
for argument in "$@"; do
  case "$argument" in
    --dry-run) DRY_RUN=true ;;
    *) printf 'Unknown argument: %s\nUsage: scripts/build-release.sh [--dry-run]\n' "$argument" >&2; exit 2 ;;
  esac
done

VERSION="$(cargo metadata --no-deps --format-version 1 --locked --offline | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["version"] for p in m["packages"] if p["name"]=="racc-app"))')"
case "$(uname -s)" in
  Darwin) PLATFORM=macos-x64 ;;
  *) printf 'Unsupported release host: %s (build releases on Windows or macOS)\n' "$(uname -s)" >&2; exit 2 ;;
esac

if [[ "$DRY_RUN" == true ]]; then
  python3 scripts/gen-notices.py --check
  python3 scripts/package_artifacts.py --version "$VERSION" --platform "$PLATFORM" --dry-run
  exit 0
fi
python3 scripts/gen-notices.py
cargo build --release --locked -p racc-app -p racc-host-agent
python3 scripts/package_artifacts.py --version "$VERSION" --platform "$PLATFORM"
