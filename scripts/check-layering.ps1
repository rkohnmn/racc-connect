$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$packages = @(
    'racc-proto', 'racc-net', 'racc-topology', 'racc-session', 'racc-telemetry',
    'racc-capture', 'racc-encode', 'racc-decode', 'racc-input', 'racc-clipboard',
    'racc-identity', 'racc-core', 'racc-testkit'
)
foreach ($package in $packages) {
    $tree = & cargo tree --prefix none --package $package
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    if ($tree -match '^racc-(app|host-agent)(\s|$)') {
        Write-Error "Forbidden application dependency found in library dependency tree: $package"
        exit 1
    }
}
Write-Output 'Layering check passed for 13 library crates.'