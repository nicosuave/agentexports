#!/usr/bin/env bash
set -euo pipefail

payload="${1:-}"
if [[ -z "$payload" ]]; then
  exit 0
fi

agentexport_bin="${AGENTEXPORT_BIN:-/Users/nico/Code/agentexports/target/release/agentexport}"
if [[ ! -x "$agentexport_bin" ]]; then
  agentexport_bin="/Users/nico/Code/agentexports/target/debug/agentexport"
fi

if [[ -x "$agentexport_bin" ]]; then
  "$agentexport_bin" codex-notify "$payload" >/dev/null 2>&1 || true
fi

notify_script="/Users/nico/.codex/notify.py"
if [[ -f "$notify_script" ]]; then
  uv run "$notify_script" "$payload" >/dev/null 2>&1 || true
fi

exit 0
