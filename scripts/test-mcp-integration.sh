#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="$(mktemp -d "${TMPDIR:-/tmp}/beholder-mcp-integration.XXXXXX")"
export BEHOLDER_STATE_DIR="$state"
daemon_pid=''

cleanup() {
    exit_status=$?
    "$root/target/debug/beholder" daemon stop >/dev/null 2>&1 || true
    if [[ -n "$daemon_pid" ]]; then
        wait "$daemon_pid" 2>/dev/null || true
    fi
    if (( exit_status != 0 )); then
        [[ -f "$state/beholderd.log" ]] && sed -n '1,200p' "$state/beholderd.log" >&2
    fi
    rm -rf "$state"
    return "$exit_status"
}
trap cleanup EXIT

cargo build --quiet \
    -p beholder-cli \
    -p beholder-daemon \
    -p beholder-mcp \
    -p beholder-worker-rust

repository="$state/repository"
mkdir -p "$repository/src"
printf '[package]\nname = "beholder-mcp-smoke"\nversion = "0.1.0"\nedition = "2024"\n' \
    >"$repository/Cargo.toml"
printf 'pub fn caller() { helper(); }\npub fn helper() {}\n' >"$repository/src/lib.rs"
git -C "$repository" init -q
git -C "$repository" config user.name 'Beholder MCP Integration Test'
git -C "$repository" config user.email 'integration-test@beholder.local'
git -C "$repository" add Cargo.toml src/lib.rs
git -C "$repository" -c commit.gpgsign=false commit -qm 'Add MCP integration fixture'
git -C "$repository" remote add origin https://github.com/example/beholder-mcp-smoke.git

"$root/target/debug/beholderd" >"$state/beholderd.log" 2>&1 &
daemon_pid=$!
for _ in {1..100}; do
    "$root/target/debug/beholder" daemon status >/dev/null 2>&1 && break
    kill -0 "$daemon_pid" 2>/dev/null
    sleep 0.05
done
"$root/target/debug/beholder" daemon status >/dev/null
"$root/target/debug/beholder" workspace register mcp-smoke "$repository" >/dev/null

caller='repo://github.com/example/beholder-mcp-smoke/rust/lib/caller'
helper='repo://github.com/example/beholder-mcp-smoke/rust/lib/helper'
context=''
for _ in {1..600}; do
    context="$("$root/target/debug/beholder" context --json --workspace mcp-smoke "$caller" 2>/dev/null || true)"
    if grep -Fq "$helper" <<<"$context" && grep -Fq '"stale":false' <<<"$context"; then
        break
    fi
    sleep 0.1
done
if ! grep -Fq "$helper" <<<"$context"; then
    printf 'controlled daemon did not index the MCP fixture:\n%s\n' "$context" >&2
    exit 1
fi

python3 "$root/scripts/mcp-smoke.py" \
    "$root/target/debug/beholder-mcp" mcp-smoke caller "$caller" "$helper"
