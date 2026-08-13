#!/usr/bin/env bash
set -euo pipefail

root="$(pwd)"
state="$(mktemp -d "${TMPDIR:-/tmp}/beholder-dogfood.XXXXXX")"
# ponytail: PID-derived port can collide; accept BEHOLDER_ADDRESS when parallel dogfood makes that real.
export BEHOLDER_ADDRESS="${BEHOLDER_ADDRESS:-127.0.0.1:$((49152 + $$ % 10000))}"
export BEHOLDER_STATE_DIR="$state"
daemon_pid=''

cleanup() {
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
        return 1
    fi
    rm -rf "$state"
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
    result="$(target/debug/beholder context --workspace main "$caller" 2>/dev/null || true)"
    grep -Fq "$callee" <<<"$result" && break
    sleep 0.1
done
if ! grep -Fq "$callee" <<<"$result"; then
    printf 'automatic indexing did not produce %s in context:\n%s\n' "$callee" "$result" >&2
    exit 1
fi
echo 'Checking main -> state_dir...' >&2
echo 'Checking state_dir impact reaches main...' >&2
result="$(target/debug/beholder impact --workspace main "$callee")"
if ! grep -Fq "$caller" <<<"$result"; then
    printf 'expected %s in impact result:\n%s\n' "$caller" "$result" >&2
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
