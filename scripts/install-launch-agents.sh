# Install or remove the per-user Racc Connect app and host LaunchAgents on macOS.
# This script mutates only the current user's LaunchAgents and is intended for human use.
set -euo pipefail
if [[ "$(uname -s)" != Darwin ]]; then echo 'Run this script on macOS.' >&2; exit 2; fi
MODE="${1:-}"
APP_PATH="${2:-}"
HOST_PATH="${3:-}"
APP_LABEL='com.racc.connect'
HOST_LABEL='com.racc.connect.host-agent'
AGENTS="$HOME/Library/LaunchAgents"
if [[ "$MODE" == --disable ]]; then
  launchctl bootout "gui/$(id -u)" "$AGENTS/$APP_LABEL.plist" 2>/dev/null || true
  launchctl bootout "gui/$(id -u)" "$AGENTS/$HOST_LABEL.plist" 2>/dev/null || true
  rm -f "$AGENTS/$APP_LABEL.plist" "$AGENTS/$HOST_LABEL.plist"
  echo 'Removed Racc Connect app and host LaunchAgents.'
  exit 0
fi
if [[ "$MODE" == --host-only ]]; then
  HOST_PATH="${2:-}"
elif [[ "$MODE" != --enable || -z "$APP_PATH" || -z "$HOST_PATH" ]]; then
  echo 'Usage: install-launch-agents.sh --enable <racc-app-path> <racc-host-agent-path> | --host-only <racc-host-agent-path> | --disable' >&2
  exit 2
fi
if [[ ( "$MODE" == --enable && ! -x "$APP_PATH" ) || ! -x "$HOST_PATH" ]]; then
  echo 'The requested executable paths must exist and be executable.' >&2
  exit 2
fi
mkdir -p "$AGENTS"
write_agent() {
  local label="$1" executable="$2" plist="$3" keep_alive="$4" mode="${5:-}"
  defaults write "$plist" Label -string "$label"
  if [[ -n "$mode" ]]; then
    defaults write "$plist" ProgramArguments -array "$executable" "$mode"
  else
    defaults write "$plist" ProgramArguments -array "$executable"
  fi
  defaults write "$plist" RunAtLoad -bool true
  defaults write "$plist" KeepAlive -bool "$keep_alive"
  plutil -lint "$plist"
  launchctl bootout "gui/$(id -u)" "$plist" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$plist"
}
if [[ "$MODE" == --enable ]]; then
  write_agent "$APP_LABEL" "$APP_PATH" "$AGENTS/$APP_LABEL.plist" false
fi
write_agent "$HOST_LABEL" "$HOST_PATH" "$AGENTS/$HOST_LABEL.plist" true host
if [[ "$MODE" == --enable ]]; then
  echo 'Installed the current-user app and supervised host LaunchAgents. The host runs only in the logged-in GUI session.'
else
  echo 'Installed the supervised host LaunchAgent. App autostart remains controlled by Settings.'
fi
