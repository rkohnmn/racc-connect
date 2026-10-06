#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

for package in racc-proto racc-net racc-topology racc-session racc-telemetry racc-capture racc-encode racc-decode racc-input racc-clipboard racc-identity racc-core racc-testkit; do
    tree="$(cargo tree --prefix none --package "$package")"
    if printf '%s\n' "$tree" | grep -Eq '^racc-(app|host-agent)([[:space:]]|$)'; then
        printf 'Forbidden application dependency found in library dependency tree: %s\n' "$package" >&2
        exit 1
    fi
done

printf 'Layering check passed for 13 library crates.\n'