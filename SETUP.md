# Install and prepare the three test machines

Run the matching root-level setup script once on each computer, using the same repository commit. The scripts build locally for that machine; they do not download an app binary from another computer. Tailscale must already be installed and signed in on all machines. These scripts check for it but never install, configure, or modify it.

## Windows PC #1 and PC #2

1. Connect the PC to the internet and confirm Tailscale is already signed in.
2. Open 64-bit PowerShell as Administrator in the repository folder.
3. Run:

~~~powershell
Set-ExecutionPolicy -Scope Process Bypass
.\setup-windows.ps1
~~~

The script uses winget to install Git, Python 3.13, Rustup, CMake, Visual Studio 2022 C++ Build Tools with the recommended Windows SDK, and Inno Setup 6. It builds the locked release, creates the installer, installs Racc Connect, registers and starts the host service, installs the Tailscale-range firewall rules, creates shortcuts, enables app startup at sign-in, and launches the app. It stops a running Racc Connect app and host service before upgrading them. A Windows restart may be required after Build Tools installation; rerun the script after restarting.

## 2015 Intel Mac

1. Connect the Mac to the internet and confirm Tailscale is already installed and signed in.
2. Open Terminal in the repository folder and run:

~~~sh
bash ./setup-macos.sh
~~~

The script checks for macOS 12+, installs the pinned Rust toolchain, builds the Intel app bundle, copies it to your user Applications folder, installs and starts the current user's app and host LaunchAgents, and opens the app. If Apple Command Line Tools are missing, macOS presents its installer; approve it, wait for completion, then rerun the script. The Mac's host profile remains at 720p30.

macOS does not let this script grant Screen Recording or Accessibility access. Approve those permissions in System Settings when asked. The bundle is ad-hoc signed for personal testing, not notarized.

## Current product limitation

The scripts install the current builds; they do not make unimplemented runtime features work. In particular, the app's Hosting toggle currently reports Unavailable, so use the installed host service for host startup and record whether streaming works. The scripts do not prove cross-machine operation. After setup, run the host/viewer checks in docs/HARDWARE.md and report results.