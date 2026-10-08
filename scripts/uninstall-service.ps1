# Remove the Windows host service. This script does not configure the firewall.
# Run elevated for a real uninstall. --dry-run only inspects and prints planned actions.
# Usage:
#   .\scripts\uninstall-service.ps1
#   .\scripts\uninstall-service.ps1 --dry-run

$ErrorActionPreference = 'Stop'
$serviceName = 'RaccConnectHost'
$dryRun = $false

for ($index = 0; $index -lt $args.Count; $index++) {
    switch -CaseSensitive ($args[$index]) {
        '--dry-run' {
            $dryRun = $true
        }
        default {
            throw "Unknown argument '$($args[$index])'. Usage: uninstall-service.ps1 [--dry-run]"
        }
    }
}

if ($env:OS -ne 'Windows_NT') {
    throw 'This script is intended for Windows.'
}

$existingService = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction SilentlyContinue
if ($dryRun) {
    if ($null -eq $existingService) {
        Write-Host "DRY RUN: Service '$serviceName' is already absent; no action would be needed."
    } elseif ($existingService.State -eq 'Running') {
        Write-Host "DRY RUN: Would stop and remove service '$serviceName'."
    } else {
        Write-Host "DRY RUN: Would remove service '$serviceName'."
    }
    Write-Host 'No service configuration was changed.'
    exit 0
}

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Uninstall requires an elevated PowerShell terminal. Re-run PowerShell as Administrator.'
}

if ($null -eq $existingService) {
    Write-Host "Service '$serviceName' is already absent; nothing to remove."
    exit 0
}

if ($existingService.State -ne 'Stopped') {
    Stop-Service -Name $serviceName -Force -ErrorAction Stop
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 250
        $existingService = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction SilentlyContinue
        if ($null -eq $existingService) {
            Write-Host "Service '$serviceName' disappeared while stopping; removal is complete."
            exit 0
        }
    } while ($existingService.State -ne 'Stopped' -and [DateTime]::UtcNow -lt $deadline)

    if ($existingService.State -ne 'Stopped') {
        throw "Service '$serviceName' did not stop within 30 seconds (current state: $($existingService.State)). It was not deleted."
    }
}

& sc.exe delete $serviceName | Out-Host
if ($LASTEXITCODE -ne 0) {
    # A second invocation is a successful no-op when SCM has already removed it.
    $remainingService = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction SilentlyContinue
    if ($null -ne $remainingService) {
        throw "sc.exe delete failed with exit code $LASTEXITCODE."
    }
}

Write-Host "Service '$serviceName' has been removed. SCM may finish deleting it after open handles close."

