#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

for package in racc-app racc-host-agent; do
    tree="$(cargo tree --edges normal,build,features --target all --package "$package")"
    if printf '%s\n' "$tree" | grep -Fq 'racc-core feature "loopback-test"'; then
        printf 'Forbidden loopback-test feature enabled in shipped binary dependency tree: %s\n' "$package" >&2
        exit 1
    fi
    if printf '%s\n' "$tree" | grep -Fq 'racc-net feature "test-bind"'; then
        printf 'Forbidden test-bind feature enabled in shipped binary dependency tree: %s\n' "$package" >&2
        exit 1
    fi
done

printf 'Feature gate check passed for racc-app and racc-host-agent.\n'
