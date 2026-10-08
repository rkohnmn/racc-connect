$ErrorActionPreference = 'Stop'
$python = Get-Command python -ErrorAction SilentlyContinue
if ($null -eq $python) { throw 'Python 3 is required to generate notices.' }
& $python.Source (Join-Path $PSScriptRoot 'gen-notices.py') @args
exit $LASTEXITCODE
