# Human-run, elevated firewall setup. This script is safe to inspect and accepts --dry-run.
# It never widens the peer address scope beyond Tailscale IPv4/IPv6.
$ErrorActionPreference = 'Stop'
$agentPath = $null
$appPath = $null
$dryRun = $false
for ($index = 0; $index -lt $args.Count; $index++) {
    switch -CaseSensitive ($args[$index]) {
        '--agent-path' { if ($index + 1 -ge $args.Count) { throw 'Missing value for --agent-path' }; $index++; $agentPath = $args[$index] }
        '--app-path' { if ($index + 1 -ge $args.Count) { throw 'Missing value for --app-path' }; $index++; $appPath = $args[$index] }
        '--dry-run' { $dryRun = $true }
        default { throw "Unknown argument '$($args[$index])'. Usage: firewall-rules.ps1 --agent-path <exe> --app-path <exe> [--dry-run]" }
    }
}
if ([string]::IsNullOrWhiteSpace($agentPath) -or [string]::IsNullOrWhiteSpace($appPath)) { throw 'Both --agent-path and --app-path are required.' }
if ($env:OS -ne 'Windows_NT') { throw 'This script is intended for Windows.' }
$agentPath = [System.IO.Path]::GetFullPath($agentPath)
$appPath = [System.IO.Path]::GetFullPath($appPath)
if (-not $dryRun -and (-not (Test-Path -LiteralPath $agentPath -PathType Leaf) -or -not (Test-Path -LiteralPath $appPath -PathType Leaf))) { throw 'Both executable paths must exist.' }
$tailscaleRanges = @('100.64.0.0/10', 'fd7a:115c:a1e0::/48')
$rules = @(
    @{ Name = 'Racc Connect host TCP 47473'; Program = $agentPath; Protocol = 'TCP'; LocalPort = '47473' },
    @{ Name = 'Racc Connect viewer UDP'; Program = $appPath; Protocol = 'UDP'; LocalPort = 'Any' }
)
if ($dryRun) {
    foreach ($rule in $rules) { Write-Host "DRY RUN: Allow inbound $($rule.Protocol) local port $($rule.LocalPort) for $($rule.Program), remote addresses $($tailscaleRanges -join ', ') (all profiles)." }
    Write-Host 'DRY RUN: No firewall rules were changed.'
    exit 0
}
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [System.Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Firewall setup requires an elevated PowerShell terminal.' }
foreach ($rule in $rules) {
    Get-NetFirewallRule -DisplayName $rule.Name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    New-NetFirewallRule -DisplayName $rule.Name -Direction Inbound -Action Allow -Enabled True -Profile Any -Program $rule.Program -Protocol $rule.Protocol -LocalPort $rule.LocalPort -RemoteAddress $tailscaleRanges | Out-Null
    Write-Host "Created scoped firewall rule '$($rule.Name)'."
}
