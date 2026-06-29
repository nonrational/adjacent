# shellcheck shell=bash
# Helpers for the runtime-manager compatibility harness.
# Requires: ADJ_BIN (absolute path to the adj binary).

set -uo pipefail

start_daemon() {
  # $1 = context: "shell" | "launchd"
  # $2 = LAUNCHD_EXTRA_PATH (may be empty)
  local context="$1" extra="$2"
  ADJACENT_HOME="$(mktemp -d)"
  export ADJACENT_HOME
  rm -f "$ADJACENT_HOME/proxy.port"

  if [ "$context" = "launchd" ]; then
    local minimal="/usr/bin:/bin:/usr/sbin:/sbin"
    [ -n "$extra" ] && minimal="${minimal}:${extra}"
    env -i \
      HOME="$HOME" \
      ADJACENT_HOME="$ADJACENT_HOME" \
      ADJACENT_PROXY_PORT=0 \
      ADJACENT_HTTPS_PORT=0 \
      PATH="$minimal" \
      "$ADJ_BIN" daemon &
  else
    ADJACENT_PROXY_PORT=0 ADJACENT_HTTPS_PORT=0 "$ADJ_BIN" daemon &
  fi
  DAEMON_PID=$!

  # Wait for the daemon to publish its proxy port (race-free discovery).
  local tries=0
  while [ ! -s "$ADJACENT_HOME/proxy.port" ]; do
    tries=$((tries + 1))
    if [ "$tries" -gt 100 ]; then
      echo "daemon never wrote proxy.port" >&2
      return 1
    fi
    sleep 0.1
  done
  PROXY_PORT="$(cat "$ADJACENT_HOME/proxy.port")"
}

stop_daemon() {
  # Preserve the script's pending exit status: this runs from an EXIT trap, and
  # bash would otherwise make the trap's last command (wait on the SIGTERMed
  # daemon, status 143) the script's exit code — masking pass(0)/fail(1).
  local status=$?
  [ -n "${DAEMON_PID:-}" ] && kill "$DAEMON_PID" 2>/dev/null
  # || true: set -e is active in the EXIT trap; without it, wait's 143 exit
  # status would abort the function before return "$status" executes.
  wait "$DAEMON_PID" 2>/dev/null || true
  [ -n "${ADJACENT_HOME:-}" ] && rm -rf "$ADJACENT_HOME"
  return "$status"
}

# Request app.adj.ac through the proxy; first hit lazy-boots the app.
fetch_version() {
  curl -fsS --max-time 90 -H "Host: app.adj.ac" "http://127.0.0.1:${PROXY_PORT}/"
}

# assert_expectation <expect> <observed> <pin> -> sets RESULT_STATUS
assert_expectation() {
  local expect="$1" observed="$2" pin="$3"
  case "$expect" in
    resolved)
      [ "$observed" = "$pin" ] && RESULT_STATUS=pass || RESULT_STATUS=fail ;;
    fallback)
      [ "$observed" != "$pin" ] && RESULT_STATUS=pass || RESULT_STATUS=fail ;;
    record)
      RESULT_STATUS=pass ;;
    *)
      echo "unknown expectation: $expect" >&2; RESULT_STATUS=fail ;;
  esac
}
