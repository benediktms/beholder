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
mkdir -p "$state/contracts"
xxd -r -p "$root/scripts/fixtures/pricing.descriptor.hex" "$state/contracts/pricing.descriptor.bin"
mkdir -p "$state/elixir/lib"
printf '%s\n' \
    'defmodule Beholder.Macro do' \
    '  defmacro __using__(_) do' \
    '    quote do' \
    '      def generated(value), do: Beholder.Helper.work(value)' \
    '    end' \
    '  end' \
    'end' \
    'defmodule Beholder.Worker do' \
    '  @callback work(term()) :: term()' \
    'end' \
    'defmodule Beholder.Payload do' \
    '  defstruct [:value]' \
    'end' \
    'defmodule Beholder do' \
    '  defmodule Smoke do' \
    '    use Beholder.Macro, mode: :strict' \
    '    @behaviour Beholder.Worker' \
    '    alias Beholder.Payload' \
    '    import External.Helpers, only: [help: 1]' \
    '    require External.Macros, as: Macros' \
    '    def indexed(value), do: helper(%Payload{value: value})' \
    '    defp helper(value), do: value' \
    '    def work(value), do: value' \
    '  end' \
    'end' \
    'defprotocol Beholder.Renderable do' \
    '  def render(value)' \
    'end' \
    'defimpl Beholder.Renderable, for: Beholder.Payload do' \
    '  def render(value), do: value.value' \
    'end' \
    'defmodule Beholder.Helper do' \
    '  def work(value), do: value' \
    'end' >"$state/elixir/lib/smoke.ex"
git -C "$state/elixir" init -q
git -C "$state/elixir" config user.name 'Beholder Smoke'
git -C "$state/elixir" config user.email 'smoke@beholder.local'
git -C "$state/elixir" add lib/smoke.ex
git -C "$state/elixir" commit -qm 'Add Elixir smoke fixture'
git -C "$state/elixir" remote add origin https://github.com/example/beholder-elixir-smoke.git
target/debug/beholder workspace register main "$root" "$state/contracts" "$state/elixir" \
    --protobuf-descriptor "$state/contracts/pricing.descriptor.bin" >/dev/null

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
for _ in {1..600}; do
    result="$(target/debug/beholder context --json --workspace main "$caller" 2>/dev/null || true)"
    grep -Fq "$callee" <<<"$result" && break
    sleep 0.1
done
if ! grep -Fq "$callee" <<<"$result"; then
    printf 'automatic indexing did not produce %s in context:\n%s\n' "$callee" "$result" >&2
    exit 1
fi
echo 'Checking Elixir module and function indexing...' >&2
elixir_module='repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Smoke'
result="$(target/debug/beholder context --json --workspace main "$elixir_module")"
for expected in \
    '"kind":"namespace"' \
    'Beholder.Smoke/indexed/1' \
    '"name":"indexed/1"' \
    'Beholder.Smoke/generated/1' \
    '"origin":"generated"' \
    '"source":"generated"' \
    'Beholder.Macro' \
    '"kind":"uses"' \
    'elixir-module://External.Helpers' \
    '"kind":"imports"' \
    'elixir-module://External.Macros' \
    '"kind":"requires"' \
    'Beholder.Worker' \
    '"kind":"implements"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in Elixir context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
result="$(target/debug/beholder context --json --workspace main \
    'repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Payload')"
for expected in 'Beholder.Payload/field/value' '"kind":"field_of"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in Elixir struct context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
result="$(target/debug/beholder context --json --workspace main \
    'repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Renderable')"
for expected in 'Beholder.Renderable.Beholder.Payload' '"kind":"implements"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in Elixir protocol implementation context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
result="$(target/debug/beholder context --json --workspace main \
    'repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Smoke/indexed/1')"
for expected in 'Beholder.Smoke/helper/1' '"kind":"calls"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in local Elixir call context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
result="$(target/debug/beholder context --json --workspace main \
    'repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Smoke/generated/1')"
for expected in 'Beholder.Helper/work/1' '"kind":"calls"' '"source":"generated"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in generated Elixir call context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
macro_result="$(target/debug/beholder context --json --workspace main \
    'repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Macro')"
if grep -Fq '/generated/1' <<<"$macro_result"; then
    printf 'quoted macro expansion leaked into direct definitions:\n%s\n' "$macro_result" >&2
    exit 1
fi
echo 'Checking Protobuf descriptor indexing...' >&2
result="$(target/debug/beholder context --json --workspace main \
    'grpc://pricing.v1.Pricing/GetQuote')"
for expected in '"kind":"rpc"' 'proto-message://pricing.v1.Request' '"kind":"request_type"' '"source":"descriptor"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in Protobuf context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
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
if ! grep -Fq '"schema":"beholder.why.v2"' <<<"$result"; then
    printf 'why result did not use the versioned JSON contract:\n%s\n' "$result" >&2
    exit 1
fi
echo 'Checking completed revision...' >&2
revision="$(target/debug/beholder inspect revisions --database "$state/daemon/beholder.db")"
if ! grep -Fq '"main"' <<<"$revision"; then
    printf 'expected main revision:\n%s\n' "$revision" >&2
    exit 1
fi
echo 'Garbage collecting obsolete semantic states...' >&2
gc_result="$(target/debug/beholder cache gc)"
if ! grep -Eq '^removed [0-9]+ repository states' <<<"$gc_result"; then
    printf 'unexpected garbage collection result:\n%s\n' "$gc_result" >&2
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
for expected in \
    'daemon started' \
    'workspace indexed' \
    'facts_inserted' \
    'rpc.context' \
    'semantic store garbage collected' \
    'elixir.macro_expansion_incomplete' \
    'rust.receiver_method_resolution_unavailable' \
    'frontend analysis limitations' \
    'daemon stopped'; do
    if ! grep -Fq "$expected" "$trace_file"; then
        printf 'daemon trace is missing %s:\n' "$expected" >&2
        cat "$trace_file" >&2
        exit 1
    fi
done
trace_events="$(wc -l <"$trace_file" | tr -d ' ')"
echo "Trace inspection passed: $trace_events events, no warnings or errors" >&2
echo "dogfood smoke passed: indexed Rust, Elixir, and Protobuf semantics" >&2
