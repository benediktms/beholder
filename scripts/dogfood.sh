#!/usr/bin/env bash
set -euo pipefail

root="$(pwd)"
state="$(mktemp -d "${TMPDIR:-/tmp}/beholder-dogfood.XXXXXX")"
export BEHOLDER_STATE_DIR="$state"
export RUST_LOG="${RUST_LOG:-info,beholderd=debug}"
socket="$state/daemon/beholder.sock"
daemon_pid=''

cleanup() {
    exit_status=$?
    target/debug/beholder daemon stop >/dev/null 2>&1 || true
    if [[ -n "$daemon_pid" ]]; then
        wait "$daemon_pid" 2>/dev/null || true
    fi
    for _ in {1..100}; do
        [[ ! -s "$state/daemon/beholderd.pid" ]] && break
        sleep 0.05
    done
    if [[ -s "$state/daemon/beholderd.pid" ]]; then
        echo 'isolated beholderd did not stop' >&2
        exit_status=1
    fi
    if (( exit_status != 0 )); then
        echo '--- isolated daemon logs ---' >&2
        for log in "$state/beholderd.log" "$state/daemon"/beholderd.*.log; do
            [[ -f "$log" ]] && cat "$log" >&2
        done
    fi
    rm -rf "$state"
    return "$exit_status"
}
trap cleanup EXIT

echo 'Building beholder and beholderd...' >&2
cargo build -p beholder-cli -p beholder-daemon
echo 'Starting isolated beholderd...' >&2
target/debug/beholderd >"$state/beholderd.log" 2>&1 &
daemon_pid=$!
for _ in {1..50}; do
    target/debug/beholder daemon status >/dev/null 2>&1 && break
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        cat "$state/beholderd.log" >&2
        exit 1
    fi
    sleep 0.1
done
target/debug/beholder daemon status >/dev/null
[[ -S "$socket" ]] || { echo "daemon socket not found at $socket" >&2; exit 1; }
target/debug/beholder workspace register main "$root" >/dev/null

repository="$(basename "$root")"
if remote="$(git -C "$root" remote get-url origin 2>/dev/null)"; then
    repository="${remote#*://}"
    repository="${repository#*@}"
    repository="${repository/:/\/}"
    repository="${repository%.git}"
fi
caller="repo://$repository/rust/crates/daemon/src/main/main"
callee="repo://$repository/rust/crates/daemon-client/src/lib/state_dir"
echo 'Waiting for automatic Beholder indexing...' >&2
result=''
for _ in {1..100}; do
    result="$(target/debug/beholder context --json --workspace main "$caller" 2>/dev/null || true)"
    grep -Fq "$callee" <<<"$result" && break
    sleep 0.1
done
if ! grep -Fq "$callee" <<<"$result"; then
    printf 'automatic indexing did not produce %s in context:\n%s\n' "$callee" "$result" >&2
    exit 1
fi
echo 'Checking main -> state_dir...' >&2
echo 'Checking state_dir impact reaches main...' >&2
result="$(target/debug/beholder impact --json --workspace main "$callee")"
if ! grep -Fq "$caller" <<<"$result"; then
    printf 'expected %s in impact result:\n%s\n' "$caller" "$result" >&2
    exit 1
fi
echo 'Checking why main reaches state_dir...' >&2
result="$(target/debug/beholder why --json --workspace main "$caller" "$callee")"
if ! grep -Fq "$callee" <<<"$result"; then
    printf 'expected %s in why result:\n%s\n' "$callee" "$result" >&2
    exit 1
fi
if ! grep -Fq '"schema":"beholder.why.v1"' <<<"$result"; then
    printf 'why result did not use the versioned JSON contract:\n%s\n' "$result" >&2
    exit 1
fi
echo 'Checking completed revision...' >&2
revision="$(target/debug/beholder inspect revisions --database "$state/daemon/beholder.db")"
if ! grep -Fq '"main"' <<<"$revision"; then
    printf 'expected main revision:\n%s\n' "$revision" >&2
    exit 1
fi
echo 'Stopping daemon and inspecting traces...' >&2
target/debug/beholder daemon stop >/dev/null
wait "$daemon_pid"
daemon_pid=''
[[ ! -e "$socket" ]] || { echo "daemon socket was not removed: $socket" >&2; exit 1; }
trace_file="$(find "$state/daemon" -maxdepth 1 -name 'beholderd.*.log' -print | sort | tail -n 1)"
if [[ -z "$trace_file" ]]; then
    echo 'daemon produced no structured trace file' >&2
    exit 1
fi
if grep -Eq '"level":"(WARN|ERROR)"' "$trace_file"; then
    echo 'daemon trace contains warnings or errors:' >&2
    cat "$trace_file" >&2
    exit 1
fi
for expected in 'daemon started' 'workspace indexed' 'facts_inserted' 'rpc.context' 'daemon stopped'; do
    if ! grep -Fq "$expected" "$trace_file"; then
        printf 'daemon trace is missing %s:\n' "$expected" >&2
        cat "$trace_file" >&2
        exit 1
    fi
done
trace_events="$(wc -l <"$trace_file" | tr -d ' ')"
echo "Trace inspection passed: $trace_events events, no warnings or errors" >&2
echo "dogfood smoke passed: indexed Beholder and resolved main -> state_dir" >&2
