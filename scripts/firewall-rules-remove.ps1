# Removes only the two named Racc Connect firewall rules. Human-run and elevated.
$ErrorActionPreference = 'Stop'
$dryRun = $false
foreach ($argument in $args) {
    if ($argument -ceq '--dry-run') { $dryRun = $true } else { throw "Unknown argument '$argument'. Usage: firewall-rules-remove.ps1 [--dry-run]" }
}
if ($env:OS -ne 'Windows_NT') { throw 'This script is intended for Windows.' }
$names = @('Racc Connect host TCP 47473', 'Racc Connect viewer UDP')
if ($dryRun) {
    foreach ($name in $names) { Write-Host "DRY RUN: Would remove firewall rule '$name' if present." }
    Write-Host 'DRY RUN: No firewall rules were changed.'
    exit 0
}
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [System.Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Firewall removal requires an elevated PowerShell terminal.' }
foreach ($name in $names) {
    Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    Write-Host "Removed firewall rule '$name' if it was present."
}
