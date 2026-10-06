#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

for package in rd-proto rd-net rd-topology rd-session rd-telemetry rd-capture rd-encode rd-decode rd-input rd-clipboard rd-identity rd-core rd-testkit; do
    tree="$(cargo tree --prefix none --package "$package")"
    if printf '%s\n' "$tree" | grep -Eq '^rd-(app|host-agent)([[:space:]]|$)'; then
        printf 'Forbidden application dependency found in library dependency tree: %s\n' "$package" >&2
        exit 1
    fi
done

printf 'Layering check passed for 13 library crates.\n'