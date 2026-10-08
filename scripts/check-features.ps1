$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

foreach ($package in @('racc-app', 'racc-host-agent')) {
    $tree = & cargo tree --edges normal,build,features --target all --package $package
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    if (($tree -join "`n") -match 'racc-core feature "loopback-test"') {
        Write-Error "Forbidden loopback-test feature enabled in shipped binary dependency tree: $package"
        exit 1
    }
    if (($tree -join "`n") -match 'racc-net feature "test-bind"') {
        Write-Error "Forbidden test-bind feature enabled in shipped binary dependency tree: $package"
        exit 1
    }
}
Write-Output 'Feature gate check passed for racc-app and racc-host-agent.'
