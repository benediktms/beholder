#!/usr/bin/env bash
set -euo pipefail

root="$(pwd)"
state="$(mktemp -d "${TMPDIR:-/tmp}/beholder-dogfood.XXXXXX")"
# ponytail: PID-derived port can collide; accept BEHOLDER_ADDRESS when parallel dogfood makes that real.
export BEHOLDER_ADDRESS="${BEHOLDER_ADDRESS:-127.0.0.1:$((49152 + $$ % 10000))}"
export BEHOLDER_STATE_DIR="$state"
daemon_pid=""

cleanup() {
    if [[ -n "$daemon_pid" ]]; then
        target/debug/beholder daemon stop >/dev/null 2>&1 || true
        kill "$daemon_pid" >/dev/null 2>&1 || true
        wait "$daemon_pid" 2>/dev/null || true
    fi
    rm -rf "$state"
}
trap cleanup EXIT

echo 'Building beholder and beholderd...' >&2
cargo build -p beholder-cli -p beholder-daemon
echo 'Starting isolated beholderd...' >&2
target/debug/beholderd >"$state/daemon.log" 2>&1 &
daemon_pid=$!

for _ in {1..200}; do
    if target/debug/beholder daemon status >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
        cat "$state/daemon.log" >&2
        exit 1
    fi
    sleep 0.05
done
target/debug/beholder daemon status >/dev/null
target/debug/beholder workspace register main "$root" >/dev/null

echo 'Indexing Beholder...' >&2
target/debug/beholder index-rust-workspace main >/dev/null

caller='repo://beholder/rust/crates/daemon/src/main/main'
callee='repo://beholder/rust/crates/daemon-client/src/lib/state_dir'
echo 'Checking main -> state_dir...' >&2
result="$(target/debug/beholder context --workspace main "$caller")"
if ! grep -Fq "$callee" <<<"$result"; then
    printf 'expected %s in context:\n%s\n' "$callee" "$result" >&2
    exit 1
fi
echo 'Checking why main reaches state_dir...' >&2
result="$(target/debug/beholder why --workspace main "$caller" "$callee")"
if ! grep -Fq "$callee" <<<"$result"; then
    printf 'expected %s in why result:\n%s\n' "$callee" "$result" >&2
    exit 1
fi
echo 'Checking completed revision...' >&2
revision="$(target/debug/beholder inspect revisions --database "$state/daemon/beholder.db")"
if ! grep -Fq '"main"' <<<"$revision"; then
    printf 'expected main revision:\n%s\n' "$revision" >&2
    exit 1
fi
echo "dogfood smoke passed: indexed Beholder and resolved main -> state_dir" >&2
