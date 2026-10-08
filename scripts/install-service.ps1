# Register the Windows host service. This script does not configure the firewall.
# Run elevated for a real install. --dry-run only inspects and prints planned actions.
# Usage:
#   .\scripts\install-service.ps1 --binary-path .\target\release\racc-host-agent.exe
#   .\scripts\install-service.ps1 --binary-path C:\path\racc-host-agent.exe --dry-run
# The binary is registered with the `service` subcommand. The release path is explicit
# because this script must not guess which build the operator intends to install.

$ErrorActionPreference = 'Stop'

$serviceName = 'RaccConnectHost'
$displayName = 'Racc Connect Host Agent'
$description = 'Background host agent for Racc Connect.'
$binaryPathArgument = $null
$dryRun = $false

for ($index = 0; $index -lt $args.Count; $index++) {
    switch -CaseSensitive ($args[$index]) {
        '--dry-run' {
            $dryRun = $true
        }
        '--binary-path' {
            if (($index + 1) -ge $args.Count -or $args[$index + 1].StartsWith('--')) {
                throw 'Usage: install-service.ps1 --binary-path <racc-host-agent.exe> [--dry-run]'
            }
            $index++
            $binaryPathArgument = $args[$index]
        }
        default {
            throw "Unknown argument '$($args[$index])'. Usage: install-service.ps1 --binary-path <racc-host-agent.exe> [--dry-run]"
        }
    }
}

if ([string]::IsNullOrWhiteSpace($binaryPathArgument)) {
    throw 'A binary path is required. Usage: install-service.ps1 --binary-path <racc-host-agent.exe> [--dry-run]'
}
if ($env:OS -ne 'Windows_NT') {
    throw 'This script is intended for Windows.'
}

$binaryPath = [System.IO.Path]::GetFullPath($binaryPathArgument)
if ([System.IO.Path]::GetFileName($binaryPath) -ine 'racc-host-agent.exe') {
    throw 'The binary path must name racc-host-agent.exe.'
}
if (-not $dryRun -and -not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "The host-agent executable does not exist: $binaryPath"
}

$quotedImagePath = '"{0}" service' -f $binaryPath
$existingService = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction SilentlyContinue

if ($dryRun) {
    if ($null -eq $existingService) {
        Write-Host "DRY RUN: Would create service '$serviceName' as LocalSystem with automatic startup."
    } else {
        Write-Host "DRY RUN: Would update service '$serviceName' to the requested executable and automatic startup."
    }
    Write-Host "DRY RUN: ImagePath: $quotedImagePath"
    Write-Host 'DRY RUN: Would configure SCM recovery actions to restart after 5, 15, and 60 seconds.'
    Write-Host 'No service configuration was changed.'
    if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
        Write-Host 'Note: executable is not present; dry-run does not require a built release binary.'
    }
    exit 0
}

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Install requires an elevated PowerShell terminal. Re-run PowerShell as Administrator.'
}

if ($null -eq $existingService) {
    New-Service -Name $serviceName `
        -BinaryPathName $quotedImagePath `
        -DisplayName $displayName `
        -Description $description `
        -StartupType Automatic | Out-Null
} else {
    # Keep the service registered in place; updating the image path does not restart
    # an already-running process. The operator can restart it after reviewing the change.
    & sc.exe config $serviceName "binPath= $quotedImagePath" 'start= auto' 'obj= LocalSystem' | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "sc.exe config failed with exit code $LASTEXITCODE."
    }
    & sc.exe description $serviceName $description | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "sc.exe description failed with exit code $LASTEXITCODE."
    }
}

# Create and update both paths receive the same recovery policy.
& sc.exe failure $serviceName 'reset= 86400' 'actions= restart/5000/restart/15000/restart/60000' | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw "sc.exe failure configuration failed with exit code $LASTEXITCODE."
}
& sc.exe failureflag $serviceName '1' | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw "sc.exe failureflag configuration failed with exit code $LASTEXITCODE."
}

Write-Host "Service '$serviceName' is registered for automatic startup as LocalSystem."
Write-Host "ImagePath: $quotedImagePath"
Write-Host 'The service was not started or restarted. Restart it after confirming the service subcommand is available in this build.'
