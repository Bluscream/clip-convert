#!/usr/bin/env bash
# Measures the idle cost of a built binary: CPU burned while doing nothing, RSS,
# and open file descriptors.
#
# A tray app sits running for an entire session, so waste here is invisible until
# it starts stealing cores from something that matters. All three numbers should
# be flat and near zero.
set -euo pipefail

BINARY="${1:?usage: probe.sh /path/to/clip-convert}"
SAMPLE_SECONDS="${LCC_PROBE_SECONDS:-15}"

CONFIG_DIR="$(mktemp -d)"
trap 'rm -rf "$CONFIG_DIR"' EXIT

# An isolated config, so the probe never touches the user's real settings.
XDG_CONFIG_HOME="$CONFIG_DIR" "$BINARY" >"$CONFIG_DIR/out.log" 2>&1 &
PID=$!
trap 'kill "$PID" 2>/dev/null || true; rm -rf "$CONFIG_DIR"' EXIT

sleep 5
if ! kill -0 "$PID" 2>/dev/null; then
    echo "the binary exited immediately; output was:" >&2
    cat "$CONFIG_DIR/out.log" >&2
    exit 1
fi

read_jiffies() { awk '{print $14+$15}' "/proc/$1/stat"; }

BEFORE=$(read_jiffies "$PID")
FD_BEFORE=$(ls "/proc/$PID/fd" | wc -l)
sleep "$SAMPLE_SECONDS"
AFTER=$(read_jiffies "$PID")
FD_AFTER=$(ls "/proc/$PID/fd" | wc -l)

TICKS=$(getconf CLK_TCK)
USED=$(( AFTER - BEFORE ))
PERCENT=$(awk -v u="$USED" -v t="$TICKS" -v s="$SAMPLE_SECONDS" 'BEGIN{printf "%.2f", (u/t)/s*100}')
RSS_KB=$(awk '/VmRSS/{print $2}' "/proc/$PID/status")
THREADS=$(awk '/Threads/{print $2}' "/proc/$PID/status")

echo "idle CPU:    ${PERCENT}% of a core over ${SAMPLE_SECONDS}s"
echo "RSS:         $(( RSS_KB / 1024 )) MB"
echo "threads:     ${THREADS}"
echo "descriptors: ${FD_BEFORE} -> ${FD_AFTER}"

if [[ "$FD_AFTER" -gt "$FD_BEFORE" ]]; then
    echo "WARNING: descriptor count grew while idle; something is leaking." >&2
fi
awk -v p="$PERCENT" 'BEGIN{ if (p+0 > 2.0) { print "WARNING: idle CPU above 2% of a core." > "/dev/stderr" } }'
