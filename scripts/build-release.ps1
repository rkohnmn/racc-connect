param([switch]$DryRun)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$metadata = & cargo metadata --no-deps --format-version 1 --locked --offline
if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed.' }
$workspace = $metadata | ConvertFrom-Json
$app = $workspace.packages | Where-Object name -EQ 'racc-app' | Select-Object -First 1
if ($null -eq $app) { throw 'Cargo metadata did not return racc-app.' }
$version = $app.version
$platform = 'windows-x64'

if ($DryRun) {
    python scripts/gen-notices.py --check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    python scripts/package_artifacts.py --version $version --platform $platform --dry-run
    exit $LASTEXITCODE
}

python scripts/gen-notices.py
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --release --locked -p racc-app -p racc-host-agent
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
python scripts/package_artifacts.py --version $version --platform $platform
exit $LASTEXITCODE
