#!/usr/bin/env bash
# Human-run removal of the current user's Racc Connect LaunchAgents.
set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/install-launch-agents.sh" --disable
