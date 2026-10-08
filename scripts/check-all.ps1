$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

& (Join-Path $PSScriptRoot 'check-layering.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

& (Join-Path $PSScriptRoot 'check-features.ps1')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    # Check both supported release targets without changing the repository policy.
    $denyStatus = 0
    cargo deny --target x86_64-pc-windows-msvc check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked
    if ($LASTEXITCODE -ne 0) { $denyStatus = $LASTEXITCODE }
    cargo deny --target x86_64-apple-darwin check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked
    if ($LASTEXITCODE -ne 0) { $denyStatus = $LASTEXITCODE }
    if ($denyStatus -ne 0) { exit $denyStatus }
} else {
    Write-Output 'NOTICE: cargo-deny is not installed; skipped cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked.'
}
