#!/bin/bash
# Build and install Racc Connect on the 2015 Intel Mac.
# Run from the repository root: ./setup-macos.sh
# Tailscale is intentionally not installed or modified.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "x86_64" ]]; then
  echo "This installer is for the 2015 Intel Mac (macOS x86_64)." >&2
  exit 1
fi

MACOS_VERSION="$(sw_vers -productVersion)"
MACOS_MAJOR="$(printf '%s' "$MACOS_VERSION" | cut -d '.' -f 1)"
if (( MACOS_MAJOR < 12 )); then
  echo "Racc Connect requires macOS 12 or later; found $MACOS_VERSION." >&2
  exit 1
fi

if ! xcode-select -p >/dev/null 2>&1; then
  echo "Starting Apple's Command Line Tools installer. Approve its macOS prompt, wait for it to finish, then run this script again."
  xcode-select --install || true
  exit 2
fi

TAILSCALE_CLI="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
if [[ ! -x "$TAILSCALE_CLI" ]]; then
  TAILSCALE_CLI="$(command -v tailscale || true)"
fi
if [[ -z "$TAILSCALE_CLI" ]]; then
  echo "Tailscale was not found. Install/sign in to Tailscale yourself, then rerun; this script never installs it." >&2
  exit 1
fi
if ! TAILSCALE_BE_CLI=1 "$TAILSCALE_CLI" status >/dev/null; then
  echo "Tailscale is installed but not reporting an active session. Sign in/start it, then rerun; this script will not change Tailscale."
fi

if ! command -v curl >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
  echo "Apple Command Line Tools should provide curl and Python 3; reinstall them, then rerun this script." >&2
  exit 1
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "Installing Rustup from the official Rust installer ..."
  installer="$(mktemp)"
  trap 'rm -f "$installer"' EXIT
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error https://sh.rustup.rs -o "$installer"
  sh "$installer" -y --profile minimal --default-toolchain none
  rm -f "$installer"
  trap - EXIT
fi

export PATH="$HOME/.cargo/bin:$PATH"
TOOLCHAIN="$(awk -F '"' '/^[[:space:]]*channel[[:space:]]*=/ { print $2; exit }' "$ROOT/rust-toolchain.toml")"
if [[ -z "$TOOLCHAIN" ]]; then
  echo "Could not read the pinned Rust channel from rust-toolchain.toml." >&2
  exit 1
fi
# Rustup's optional self-update can fail on flaky networks even when the pinned toolchain is
# already installed. Keep resolving the pinned toolchain while skipping only that self-update.
rustup toolchain install "$TOOLCHAIN" --component rustfmt --component clippy --no-self-update
rustup target add x86_64-apple-darwin --toolchain "$TOOLCHAIN"

cd "$ROOT"
echo "Building the Intel macOS app bundle for macOS $MACOS_VERSION ..."
bash scripts/build-macos-app.sh

BUILT_APP="$ROOT/dist/Racc Connect.app"
if [[ ! -d "$BUILT_APP" ]]; then
  echo "The macOS app bundle was not created: $BUILT_APP" >&2
  exit 1
fi

INSTALL_DIR="$HOME/Applications"
if [[ -L "$INSTALL_DIR" ]]; then
  echo "Refusing to install through a symbolic link at $INSTALL_DIR." >&2
  exit 1
fi
INSTALL_APP="$INSTALL_DIR/Racc Connect.app"
mkdir -p "$INSTALL_DIR"
if [[ -L "$INSTALL_APP" ]]; then
  echo "Refusing to replace a symbolic link at $INSTALL_APP." >&2
  exit 1
fi

# Preserve the app autostart choice across an update. The LaunchAgent's presence
# is the OS-level source of truth for whether Settings enabled startup.
APP_AUTOSTART_PLIST="$HOME/Library/LaunchAgents/com.racc.connect.plist"
if [[ -L "$APP_AUTOSTART_PLIST" ]]; then
  echo "Refusing to update through a symbolic link at $APP_AUTOSTART_PLIST." >&2
  exit 1
fi
APP_AUTOSTART_WAS_ENABLED=false
if [[ -f "$APP_AUTOSTART_PLIST" ]]; then APP_AUTOSTART_WAS_ENABLED=true; fi

# Stop previous per-user jobs before replacing their app bundle.
bash scripts/install-launch-agents.sh --disable
if [[ -e "$INSTALL_APP" ]]; then
  rm -rf "$INSTALL_APP"
fi
ditto "$BUILT_APP" "$INSTALL_APP"

APP_EXECUTABLE="$INSTALL_APP/Contents/MacOS/racc-app"
HOST_EXECUTABLE="$INSTALL_APP/Contents/MacOS/racc-host-agent"
if [[ "$APP_AUTOSTART_WAS_ENABLED" == true ]]; then
  bash scripts/install-launch-agents.sh --enable "$APP_EXECUTABLE" "$HOST_EXECUTABLE"
else
  bash scripts/install-launch-agents.sh --host-only "$HOST_EXECUTABLE"
fi
open -a "$INSTALL_APP"

echo
echo "macOS setup finished."
echo "App installed at: $INSTALL_APP"
if [[ "$APP_AUTOSTART_WAS_ENABLED" == true ]]; then
  echo "The host and app LaunchAgents are installed for the current macOS user."
else
  echo "The host LaunchAgent is installed for the current macOS user. App autostart is off until enabled in Settings."
fi
echo "Tailscale was not installed or changed."
echo "Grant Screen Recording and Accessibility in System Settings when prompted; macOS does not allow this script to grant those permissions."
echo "The Mac host stays at its 720p30 default until sustained performance is verified. See docs/HARDWARE.md."
