#!/bin/bash
# Stop hook: at the end of each Claude turn, rebuild and relaunch the app.
# - Skips if nothing changed since the last build and the app is already up.
# - On a build error, exits 2 so Claude sees the errors and keeps going.
# - Kills the running instance (including one started via `cargo run`).
set -u
cd "$(dirname "$0")/../.." || exit 0
APP=sample_manager
BIN=target/debug/$APP

input=$(cat)
# Already continuing because of this hook: don't loop forever on a broken build.
if echo "$input" | grep -q '"stop_hook_active": *true'; then exit 0; fi

running() { pgrep -x "$APP" >/dev/null; }

if [ -x "$BIN" ] && running && \
   [ -z "$(find src Cargo.toml Cargo.lock -newer "$BIN" -print -quit 2>/dev/null)" ]; then
  exit 0
fi

if ! out=$(cargo build --color never 2>&1); then
  echo "cargo build failed after this turn — fix it:" >&2
  echo "$out" | grep -E '^(error|warning: unused)|-->' -A4 | head -80 >&2
  exit 2
fi

if running; then
  pkill -x "$APP"
  for _ in $(seq 20); do running || break; sleep 0.1; done
  running && pkill -9 -x "$APP"
fi

nohup "$BIN" >/dev/null 2>&1 &
disown
echo '{"systemMessage": "Rebuilt and relaunched sample_manager"}'
