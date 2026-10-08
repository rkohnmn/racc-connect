#Requires -Version 5.1
#Requires -RunAsAdministrator
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
    throw 'Run this from 64-bit PowerShell on 64-bit Windows 10.'
}
if (-not (Get-Command winget.exe -ErrorAction SilentlyContinue)) {
    throw 'Windows Package Manager (winget) is required. Install or update App Installer, then rerun this script.'
}

function Install-WingetPackage {
    param(
        [Parameter(Mandatory = $true)][string]$Id,
        [string]$Override
    )

    $listing = (& winget.exe list --id $Id --exact --source winget --accept-source-agreements --disable-interactivity 2>$null | Out-String)
    if ($LASTEXITCODE -eq 0 -and $listing -match [regex]::Escape($Id)) {
        Write-Host "Already installed: $Id"
        return
    }

    Write-Host "Installing $Id ..."
    $arguments = @('install', '--id', $Id, '--exact', '--source', 'winget',
        '--accept-source-agreements', '--accept-package-agreements', '--disable-interactivity')
    if ($Override) {
        $arguments += @('--override', $Override)
    }
    & winget.exe @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "winget failed to install $Id (exit code $LASTEXITCODE)."
    }
}

Write-Host 'Installing or checking build prerequisites. Tailscale will not be installed or changed.'
Install-WingetPackage -Id 'Git.Git'
Install-WingetPackage -Id 'Python.Python.3.13'
Install-WingetPackage -Id 'Rustlang.Rustup'
Install-WingetPackage -Id 'Kitware.CMake'
Install-WingetPackage -Id 'JRSoftware.InnoSetup'

$programFilesX86 = [Environment]::GetFolderPath('ProgramFilesX86')
$vswhere = Join-Path $programFilesX86 'Microsoft Visual Studio\Installer\vswhere.exe'
$vsSetup = Join-Path $programFilesX86 'Microsoft Visual Studio\Installer\setup.exe'
$vsInstall = $null
if (Test-Path -LiteralPath $vswhere) {
    $vsInstall = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
}
if (-not $vsInstall) {
    $anyVsInstall = $null
    if (Test-Path -LiteralPath $vswhere) {
        $anyVsInstall = (& $vswhere -latest -products '*' -property installationPath | Select-Object -First 1)
    }
    if ($anyVsInstall -and (Test-Path -LiteralPath $vsSetup)) {
        Write-Host 'Adding the Visual C++ workload and recommended Windows SDK to the existing Visual Studio installation ...'
        & $vsSetup modify --installPath $anyVsInstall --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --quiet --norestart --wait
        if ($LASTEXITCODE -ne 0) {
            throw "Visual Studio Installer failed to add the C++ workload (exit code $LASTEXITCODE)."
        }
    } else {
        Install-WingetPackage -Id 'Microsoft.VisualStudio.2022.BuildTools' -Override '--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --norestart'
    }
    if (Test-Path -LiteralPath $vswhere) {
        $vsInstall = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
    }
}
if (-not $vsInstall) {
    throw 'The Visual C++ Build Tools were not found. If setup requested a reboot, restart Windows and rerun setup-windows.ps1.'
}

# Make newly installed user and machine tools visible to this PowerShell process.
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$env:Path = "$env:Path;$machinePath;$userPath"
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path -LiteralPath $cargoBin) {
    $env:Path = "$cargoBin;$env:Path"
}

$python = Get-Command python.exe -ErrorAction SilentlyContinue
if ($python) {
    & $python.Source --version *> $null
    if ($LASTEXITCODE -ne 0) { $python = $null }
}
if (-not $python) {
    $pythonCandidates = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python313\python.exe'),
        (Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Python313\python.exe'),
        (Join-Path $programFilesX86 'Python313\python.exe')
    )
    foreach ($candidate in $pythonCandidates) {
        if (Test-Path -LiteralPath $candidate) {
            $python = Get-Item -LiteralPath $candidate
            $env:Path = "$(Split-Path -Parent $candidate);$env:Path"
            break
        }
    }
}
if (-not $python -or -not (Get-Command cargo.exe -ErrorAction SilentlyContinue)) {
    throw 'Python or Cargo is still unavailable. Close and reopen elevated PowerShell, then rerun setup-windows.ps1.'
}

$vsDevCmd = Join-Path $vsInstall 'Common7\Tools\VsDevCmd.bat'
$buildRelease = Join-Path $repoRoot 'scripts\build-release.ps1'
if (-not (Test-Path -LiteralPath $vsDevCmd)) {
    throw "Visual Studio developer environment was not found: $vsDevCmd"
}

Push-Location $repoRoot
try {
    Write-Host 'Building the pinned Rust release and portable package ...'
    $powershellExe = Join-Path $PSHOME 'powershell.exe'
    $buildCommand = 'call "{0}" -arch=x64 -host_arch=x64 >nul && "{1}" -NoLogo -NoProfile -ExecutionPolicy Bypass -File "{2}"' -f $vsDevCmd, $powershellExe, $buildRelease
    & $env:ComSpec /d /s /c $buildCommand
    if ($LASTEXITCODE -ne 0) {
        throw "Release build failed (exit code $LASTEXITCODE)."
    }

    $metadataText = & cargo.exe metadata --no-deps --format-version 1 --locked --offline
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not read the locked workspace version after building.'
    }
    $metadata = ($metadataText -join [Environment]::NewLine) | ConvertFrom-Json
    $appPackage = $metadata.packages | Where-Object name -EQ 'racc-app' | Select-Object -First 1
    if (-not $appPackage) {
        throw 'Cargo metadata did not include racc-app.'
    }
    $env:RACC_VERSION = $appPackage.version

    $innoCandidates = @(
        (Join-Path $programFilesX86 'Inno Setup 6\ISCC.exe'),
        (Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    )
    $iscc = $innoCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $iscc) {
        $isccCommand = Get-Command ISCC.exe -ErrorAction SilentlyContinue
        if ($isccCommand) { $iscc = $isccCommand.Source }
    }
    if (-not $iscc) {
        throw 'Inno Setup was installed but ISCC.exe was not found. Reopen PowerShell and rerun setup-windows.ps1.'
    }

    Write-Host 'Compiling the Windows installer ...'
    & (Join-Path $repoRoot 'scripts\build-installer.ps1') -CompilerPath $iscc
    if ($LASTEXITCODE -ne 0) {
        throw "Installer compilation failed (exit code $LASTEXITCODE)."
    }

    $installerPath = Join-Path $repoRoot "dist\racc-connect-$($appPackage.version)-setup-windows-x64.exe"
    if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
        throw "The installer was not created: $installerPath"
    }

    # This install replaces the app build and restarts its host service.
    Get-Process -Name 'racc-app' -ErrorAction SilentlyContinue | Stop-Process -Force
    $oldService = Get-Service -Name 'RaccConnectHost' -ErrorAction SilentlyContinue
    if ($oldService -and $oldService.Status -ne 'Stopped') {
        Stop-Service -Name 'RaccConnectHost' -Force
        $oldService.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20))
    }

    Write-Host 'Installing the app, automatic host service, Tailscale-scoped firewall rules, shortcuts, and sign-in startup ...'
    $install = Start-Process -FilePath $installerPath -ArgumentList @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', '/TASKS=appautostart,desktopicon'
    ) -Wait -PassThru
    if ($install.ExitCode -ne 0) {
        throw "The Racc Connect installer failed (exit code $($install.ExitCode))."
    }

    $service = Get-Service -Name 'RaccConnectHost' -ErrorAction SilentlyContinue
    if (-not $service) {
        throw 'The installer completed but the Racc Connect host service was not registered. Review the installer output and docs/HARDWARE.md.'
    }
    if ($service.Status -ne 'Running') {
        Start-Service -Name 'RaccConnectHost'
        $service = Get-Service -Name 'RaccConnectHost'
    }
    if ($service.Status -ne 'Running') {
        throw 'The host service did not start. The app is installed; review Windows service events and docs/HARDWARE.md.'
    }

    $installedApp = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Racc Connect\racc-app.exe'
    if (-not (Test-Path -LiteralPath $installedApp -PathType Leaf)) {
        throw "The installer completed but the app executable is missing: $installedApp"
    }
    Start-Process -FilePath $installedApp

    $tailscale = Get-Command tailscale.exe -ErrorAction SilentlyContinue
    if (-not $tailscale) {
        $tailscaleCandidates = @(
            (Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Tailscale\tailscale.exe'),
            (Join-Path $programFilesX86 'Tailscale\tailscale.exe')
        )
        $tailscalePath = $tailscaleCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if ($tailscalePath) { $tailscale = Get-Item -LiteralPath $tailscalePath }
    }
    if ($tailscale) {
        & $tailscale.Source status
        if ($LASTEXITCODE -ne 0) {
            Write-Warning 'Tailscale is installed but did not report a healthy status. The setup script did not change it.'
        }
    } else {
        Write-Warning 'Tailscale CLI was not found. The setup script did not install or change Tailscale.'
    }

    Write-Host ''
    Write-Host 'Windows setup finished. App is installed and launched; host service is running.'
    Write-Host 'Review the host approval prompt in the app and follow docs/HARDWARE.md for real-device tests.'
} finally {
    Pop-Location
}
