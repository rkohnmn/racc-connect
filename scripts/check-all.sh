#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-layering.sh
scripts/check-features.sh
python3 scripts/gen-notices.py --check
python3 scripts/test_gen_notices.py
python3 tools/icons/test_generate_icons.py
python3 scripts/test_package_artifacts.py
python3 scripts/test_write_sha256.py
python3 scripts/test_hygiene_check.py
python3 scripts/hygiene_check.py

if command -v cargo-deny >/dev/null 2>&1; then
    # Check both supported release targets without changing the repository policy.
    deny_status=0
    cargo deny --target x86_64-pc-windows-msvc check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked || deny_status=$?
    cargo deny --target x86_64-apple-darwin check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked || deny_status=$?
    if [ "$deny_status" -ne 0 ]; then exit "$deny_status"; fi
else
    printf 'NOTICE: cargo-deny is not installed; skipped cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked.\n'
fi
