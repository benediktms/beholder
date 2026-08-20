#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$(mktemp -d "${TMPDIR:-/tmp}/beholder-handover.XXXXXX")"
export BEHOLDER_STATE_DIR="$state"
cli="$root/target/debug/beholder"

cleanup() {
    exit_status=$?
    "$cli" daemon stop >/dev/null 2>&1 || true
    if (( exit_status != 0 )); then
        for log in "$state/daemon"/*.log; do
            [[ -f "$log" ]] && sed -n '1,160p' "$log" >&2
        done
    fi
    rm -rf "$state"
    return "$exit_status"
}
trap cleanup EXIT

cargo build --quiet -p beholder-cli -p beholder-daemon -p beholder-worker-rust
mkdir -p "$state/daemon"

# Model the old daemon's socket-to-lock shutdown window without timing the race.
python3 - "$state/daemon/beholderd.pid" "$state/lock-ready" <<'PY' &
import fcntl
import pathlib
import sys
import time

with open(sys.argv[1], "w") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    pathlib.Path(sys.argv[2]).touch()
    time.sleep(0.25)
PY
lock_holder=$!
while [[ ! -e "$state/lock-ready" ]]; do sleep 0.01; done
"$cli" daemon start >/dev/null
wait "$lock_holder"
"$cli" daemon status >/dev/null
"$cli" daemon stop >/dev/null

# Concurrent starts may race, but only the process owning the daemon may say it started.
for attempt in 1 2 3; do
    gate="$state/start-$attempt"
    first="$state/first-$attempt"
    second="$state/second-$attempt"
    (while [[ ! -e "$gate" ]]; do sleep 0.001; done; "$cli" daemon start >"$first" 2>&1) &
    first_pid=$!
    (while [[ ! -e "$gate" ]]; do sleep 0.001; done; "$cli" daemon start >"$second" 2>&1) &
    second_pid=$!
    touch "$gate"
    wait "$first_pid" || true
    wait "$second_pid" || true

    [[ "$(grep -l '^started (pid ' "$first" "$second" | wc -l | tr -d ' ')" == 1 ]]
    grep -Eq '^(started|already running) \(pid |beholderd exited with' "$first"
    grep -Eq '^(started|already running) \(pid |beholderd exited with' "$second"
    "$cli" daemon status >/dev/null
    "$cli" daemon stop >/dev/null
done
