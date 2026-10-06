$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$packages = @(
    'rd-proto', 'rd-net', 'rd-topology', 'rd-session', 'rd-telemetry',
    'rd-capture', 'rd-encode', 'rd-decode', 'rd-input', 'rd-clipboard',
    'rd-identity', 'rd-core', 'rd-testkit'
)
foreach ($package in $packages) {
    $tree = & cargo tree --prefix none --package $package
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    if ($tree -match '^rd-(app|host-agent)(\s|$)') {
        Write-Error "Forbidden application dependency found in library dependency tree: $package"
        exit 1
    }
}
Write-Output 'Layering check passed for 13 library crates.'