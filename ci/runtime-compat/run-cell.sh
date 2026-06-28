#!/usr/bin/env bash
# Run one matrix cell in one context and print a RESULT line.
# Usage: run-cell.sh <fixture-dir> <shell|launchd>
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"

FIXTURE="$(cd "$1" && pwd)"
CONTEXT="$2"
: "${ADJ_BIN:?set ADJ_BIN to the adj binary path}"

# shellcheck source=/dev/null
source "$FIXTURE/cell.env"   # MANAGER RUNTIME PIN EXPECT_SHELL EXPECT_LAUNCHD LAUNCHD_EXTRA_PATH(optional)

if [ "$CONTEXT" = "shell" ]; then
  EXPECT="$EXPECT_SHELL"; EXTRA=""
else
  EXPECT="$EXPECT_LAUNCHD"; EXTRA="${LAUNCHD_EXTRA_PATH:-}"
fi

trap stop_daemon EXIT
start_daemon "$CONTEXT" "$EXTRA"
"$ADJ_BIN" add "$FIXTURE" >/dev/null

OBSERVED_RAW="$(fetch_version || echo "BOOT_FAILED")"
OBSERVED="$(printf '%s' "$OBSERVED_RAW" | awk '{print $2}')"   # second field = version

assert_expectation "$EXPECT" "$OBSERVED" "$PIN"
echo "RESULT manager=$MANAGER context=$CONTEXT pin=$PIN observed=${OBSERVED:-none} expect=$EXPECT status=$RESULT_STATUS"
[ "$RESULT_STATUS" = "pass" ]
