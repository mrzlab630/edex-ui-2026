#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT_DIR/rust"

MODE="interactive"
if [[ "${1-}" == "--smoke" ]]; then
  MODE="smoke"
fi

BOOTSTRAP_ROOT="${EDEX_TUI_BOOTSTRAP_ROOT:-$ROOT_DIR}"
BOOTSTRAP_TMUX="${EDEX_TUI_BOOTSTRAP_TMUX:-1}"
MASTER_PASSPHRASE="${EDEX_CORE_MASTER_PASSPHRASE:-test-passphrase}"

RUNTIME_DIR="$(mktemp -d /tmp/edex-ui-2026-tui.XXXXXX)"
SOCKET_PATH="$RUNTIME_DIR/daemon.sock"
STATE_DB="$RUNTIME_DIR/state.sqlite3"
HISTORY_DB="$RUNTIME_DIR/history.sqlite3"
DAEMON_LOG="$RUNTIME_DIR/runtime-daemon.log"

daemon_pid=""

cleanup() {
  if [[ -n "$daemon_pid" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
    kill "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  rm -rf "$RUNTIME_DIR"
}

trap cleanup EXIT INT TERM

cd "$RUST_DIR"

echo "Launching runtime-daemon..."
(
  export EDEX_CORE_SOCKET="$SOCKET_PATH"
  export EDEX_CORE_STATE_DB="$STATE_DB"
  export EDEX_CORE_HISTORY_DB="$HISTORY_DB"
  export EDEX_CORE_MASTER_PASSPHRASE="$MASTER_PASSPHRASE"
  cargo run -p runtime-daemon --bin runtime-daemon
) >"$DAEMON_LOG" 2>&1 &
daemon_pid="$!"

echo "Waiting for socket: $SOCKET_PATH"
for _ in $(seq 1 100); do
  if [[ -S "$SOCKET_PATH" ]]; then
    break
  fi

  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    echo "runtime-daemon exited early" >&2
    cat "$DAEMON_LOG" >&2
    exit 1
  fi

  sleep 0.1
done

if [[ ! -S "$SOCKET_PATH" ]]; then
  echo "Timed out waiting for daemon socket" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
fi

export EDEX_CORE_SOCKET="$SOCKET_PATH"
export EDEX_TUI_BOOTSTRAP_ROOT="$BOOTSTRAP_ROOT"
export EDEX_TUI_BOOTSTRAP_TMUX="$BOOTSTRAP_TMUX"

if [[ "$MODE" == "smoke" ]]; then
  echo "Running tui-client --smoke"
  cargo run -p tui-client -- --smoke
else
  echo "Running tui-client"
  cargo run -p tui-client
fi
