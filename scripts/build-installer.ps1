param([string]$CompilerPath)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

if (-not $env:RACC_VERSION) {
    $metadata = & cargo metadata --no-deps --format-version 1 --locked --offline
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed.' }
    $workspace = $metadata | ConvertFrom-Json
    $env:RACC_VERSION = ($workspace.packages | Where-Object name -EQ 'racc-app' | Select-Object -First 1).version
}
$stage = Join-Path (Get-Location) "dist\racc-connect-$($env:RACC_VERSION)-windows-x64"
if (-not (Test-Path -LiteralPath $stage -PathType Container)) { throw "Build the portable release stage first: $stage" }
if (-not $CompilerPath) {
    $candidate = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($null -eq $candidate) { throw 'Install Inno Setup 6 yourself, then pass -CompilerPath to ISCC.exe. The installer compiler is not installed by this script.' }
    $CompilerPath = $candidate.Source
}
& $CompilerPath 'packaging\windows\racc-connect.iss'
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed with exit code $LASTEXITCODE." }

$installer = Join-Path (Get-Location) "dist\racc-connect-$($env:RACC_VERSION)-setup-windows-x64.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw "Inno Setup succeeded but the installer artifact was not found: $installer" }
python scripts/write_sha256.py $installer
if ($LASTEXITCODE -ne 0) { throw "Could not write installer SHA-256 sidecar (exit code $LASTEXITCODE)." }
