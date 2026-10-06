#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-layering.sh

if command -v cargo-deny >/dev/null 2>&1; then
    cargo deny check --warn advisories
else
    printf 'NOTICE: cargo-deny is not installed; skipped cargo deny check --warn advisories.\n'
fi