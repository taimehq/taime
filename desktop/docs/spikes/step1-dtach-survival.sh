#!/usr/bin/env bash
# Step 1 (optional, non-blocking) — crash-resilience spike.
#
# Proves the plan's core premise: when the PTY master leaves the launching
# process, the workload SURVIVES the launcher's death. `dtach -n` holds the PTY
# master in a daemon reparented to launchd, so a counter started under it keeps
# running (and its output keeps flowing) after the subshell that launched it
# exits. This is exactly what the real `taime-session-daemon` does in production
# (setsid-detached spawn + dropped child) — `dtach` is NOT a product dependency;
# this only validates "master-outside-app survival" cheaply.
#
# Usage: ./step1-dtach-survival.sh   (requires `dtach`: brew install dtach)
set -euo pipefail

command -v dtach >/dev/null || { echo "dtach not installed (brew install dtach)"; exit 1; }

sock="$(mktemp -u "${TMPDIR:-/tmp}/taime-dtach-XXXXXX.sock")"
log="$(mktemp "${TMPDIR:-/tmp}/taime-dtach-XXXXXX.log")"
trap 'rm -f "$sock" "$log"' EXIT

# Launch a counter under dtach -n from a SUBSHELL that exits immediately. After
# the subshell is gone, the counter must keep running because dtach owns the PTY.
( dtach -n "$sock" bash -c 'for i in $(seq 1 20); do echo "tick $i ts=$(date +%s)"; sleep 0.3; done >> '"$log"' 2>&1' )
echo "launcher subshell has exited; the dtach'd counter should keep running…"

sleep 1
before="$(wc -l < "$log" | tr -d ' ')"
sleep 1
after="$(wc -l < "$log" | tr -d ' ')"

echo "log lines: before=$before after=$after"
if [ "$after" -gt "$before" ]; then
  echo "PASS: the process survived its launcher (master-outside-app survival proven)."
else
  echo "FAIL: output did not advance after the launcher exited."
  exit 1
fi
