# Toggle per-user Windows app autostart. The host service has its own startup policy.
$ErrorActionPreference = 'Stop'
$enable = $false
$disable = $false
$appPath = $null
for ($index = 0; $index -lt $args.Count; $index++) {
    switch -CaseSensitive ($args[$index]) {
        '--enable' { $enable = $true }
        '--disable' { $disable = $true }
        '--app-path' { if ($index + 1 -ge $args.Count) { throw 'Missing value for --app-path' }; $index++; $appPath = $args[$index] }
        default { throw "Unknown argument '$($args[$index])'. Usage: set-app-autostart.ps1 (--enable --app-path <exe> | --disable)" }
    }
}
if ($env:OS -ne 'Windows_NT') { throw 'This script is intended for Windows.' }
if ($enable -eq $disable) { throw 'Specify exactly one of --enable or --disable.' }
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$name = 'RaccConnect'
if ($enable) {
    if ([string]::IsNullOrWhiteSpace($appPath)) { throw '--app-path is required with --enable.' }
    $appPath = [System.IO.Path]::GetFullPath($appPath)
    if (-not (Test-Path -LiteralPath $appPath -PathType Leaf)) { throw "App executable not found: $appPath" }
    $quoted = '"{0}"' -f $appPath.Replace('"', '\"')
    New-Item -Path $key -Force | Out-Null
    New-ItemProperty -Path $key -Name $name -Value $quoted -PropertyType String -Force | Out-Null
    Write-Host 'App autostart is enabled for the current user.'
} else {
    Remove-ItemProperty -Path $key -Name $name -ErrorAction SilentlyContinue
    Write-Host 'App autostart is disabled for the current user.'
}
