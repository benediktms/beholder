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
xxd -r -p "$root/scripts/fixtures/grpc-matrix.descriptor.hex" "$state/contracts/grpc-matrix.descriptor.bin"
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
    '    def classify(:none), do: :none' \
    '    def classify(%Beholder.PatternPayload{}), do: :pattern' \
    '    defp helper(value), do: value' \
    '    def work(value), do: value' \
    '  end' \
    'end' \
    'defprotocol Beholder.Renderable do' \
    '  def render(value)' \
    'end' \
    'defmodule Beholder.Implementations do' \
    '  defimpl Beholder.Renderable, for: Beholder.Payload do' \
    '    def render(value), do: value.value' \
    '  end' \
    'end' \
    'defmodule Beholder.Helper do' \
    '  def work(value), do: value' \
    'end' >"$state/elixir/lib/smoke.ex"
printf '%s\n' \
    'defmodule Phase5.V1.Bridge.Service do' \
    '  use GRPC.Service, name: "phase5.v1.Bridge"' \
    '  rpc :RustToElixir, Phase5.V1.Request, Phase5.V1.Response' \
    '  rpc :ElixirToRust, Phase5.V1.Request, Phase5.V1.Response' \
    'end' \
    'defmodule Phase5.V1.Bridge.Stub do' \
    '  use GRPC.Stub, service: Phase5.V1.Bridge.Service' \
    'end' \
    'defmodule Phase5.Client do' \
    '  alias Phase5.V1.Bridge.Stub' \
    '  def elixir_to_rust(channel, request), do: Stub.elixir_to_rust(channel, request)' \
    'end' \
    'defmodule Phase5.Server do' \
    '  alias Phase5.V1.Bridge.Service' \
    '  use GRPC.Server, service: Service' \
    '  def rust_to_elixir(request, stream), do: {request, stream}' \
    'end' >"$state/elixir/lib/grpc.pb.ex"
git -C "$state/elixir" init -q
git -C "$state/elixir" config user.name 'Beholder Smoke'
git -C "$state/elixir" config user.email 'smoke@beholder.local'
ssh-keygen -q -t ed25519 -N '' -f "$state/signing-key"
git -C "$state/elixir" config gpg.format ssh
git -C "$state/elixir" config gpg.ssh.program "$(command -v ssh-keygen)"
git -C "$state/elixir" config user.signingkey "$state/signing-key"
git -C "$state/elixir" config commit.gpgsign true
git -C "$state/elixir" add lib
git -C "$state/elixir" commit -qm 'Add Elixir smoke fixture'
git -C "$state/elixir" remote add origin https://github.com/example/beholder-elixir-smoke.git
mkdir -p "$state/rust/src"
printf '%s\n' 'tonic::include_proto!("phase5.v1");' >"$state/rust/src/protocol.rs"
printf '%s\n' \
    'mod bridge_client {' \
    '  pub struct BridgeClient<T>(T);' \
    '  impl<T> BridgeClient<T> {' \
    '    pub async fn rust_to_elixir(&mut self) {}' \
    '    pub async fn elixir_to_rust(&mut self) {}' \
    '  }' \
    '}' >"$state/rust/src/generated.rs"
printf '%s\n' \
    'use contract::bridge_client::BridgeClient;' \
    'async fn rust_to_elixir() {' \
    '  let mut client = BridgeClient::new();' \
    '  client.rust_to_elixir().await;' \
    '}' >"$state/rust/src/client.rs"
printf '%s\n' \
    'use contract::bridge_server::{Bridge, BridgeServer};' \
    'struct RustHandler;' \
    'impl Bridge for RustHandler {' \
    '  async fn elixir_to_rust(&self) {}' \
    '}' >"$state/rust/src/server.rs"
git -C "$state/rust" init -q
git -C "$state/rust" config user.name 'Beholder Smoke'
git -C "$state/rust" config user.email 'smoke@beholder.local'
git -C "$state/rust" config gpg.format ssh
git -C "$state/rust" config gpg.ssh.program "$(command -v ssh-keygen)"
git -C "$state/rust" config user.signingkey "$state/signing-key"
git -C "$state/rust" config commit.gpgsign true
git -C "$state/rust" add src
git -C "$state/rust" commit -qm 'Add Rust smoke fixture'
git -C "$state/rust" remote add origin https://github.com/example/beholder-rust-smoke.git
target/debug/beholder workspace register main "$root" "$state/contracts" "$state/rust" "$state/elixir" \
    --protobuf-descriptor "$state/contracts/pricing.descriptor.bin" \
    --protobuf-descriptor "$state/contracts/grpc-matrix.descriptor.bin" >/dev/null

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
    'repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Smoke/classify/1')"
for expected in 'elixir-module://Beholder.PatternPayload' '"kind":"uses"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in Elixir struct pattern context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
result="$(target/debug/beholder context --json --workspace main \
    'repo://github.com/example/beholder-elixir-smoke/elixir/Beholder.Implementations')"
for expected in 'Beholder.Renderable.Beholder.Payload' '"kind":"defines"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in nested protocol implementation context:\n%s\n' "$expected" "$result" >&2
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
    'proto-method://pricing.v1.Pricing/GetQuote')"
for expected in '"kind":"rpc"' 'proto-type://pricing.v1.Request' '"kind":"request_type"' '"source":"descriptor"'; do
    if ! grep -Fq "$expected" <<<"$result"; then
        printf 'expected %s in Protobuf context:\n%s\n' "$expected" "$result" >&2
        exit 1
    fi
done
check_grpc_path() {
    method="$1"
    client="$2"
    server="$3"
    operation="grpc://phase5.v1.Bridge/$method"
    contract="proto-method://phase5.v1.Bridge/$method"

    context="$(target/debug/beholder context --json --workspace main "$operation")"
    for expected in "$client" "$server" "$contract" '"kind":"calls_rpc"' \
        '"kind":"implemented_by"' '"kind":"binds_contract"' '"confidence":1.0' \
        '"stale":false'; do
        if ! grep -Fq "$expected" <<<"$context"; then
            printf 'expected %s in gRPC context:\n%s\n' "$expected" "$context" >&2
            exit 1
        fi
    done

    trace="$(target/debug/beholder trace --json --workspace main "$client" "$server")"
    for expected in '"schema":"beholder.trace.v2"' "$client" "$operation" "$server" \
        '"kind":"calls_rpc"' '"kind":"implemented_by"' '"stale":false'; do
        if ! grep -Fq "$expected" <<<"$trace"; then
            printf 'expected %s in cross-language trace:\n%s\n' "$expected" "$trace" >&2
            exit 1
        fi
    done
    if [[ "$trace" != "$(target/debug/beholder trace --json --workspace main "$client" "$server")" ]]; then
        echo 'cross-language trace JSON ordering was not stable' >&2
        exit 1
    fi

    why="$(target/debug/beholder why --json --workspace main "$client" "$server")"
    if ! grep -Fq '"schema":"beholder.why.v2"' <<<"$why" || ! grep -Fq "$server" <<<"$why"; then
        printf 'cross-language why did not resolve:\n%s\n' "$why" >&2
        exit 1
    fi
    impact="$(target/debug/beholder impact --json --workspace main "$contract")"
    for expected in "$client" "$server"; do
        if ! grep -Fq "$expected" <<<"$impact"; then
            printf 'contract impact did not reach %s:\n%s\n' "$expected" "$impact" >&2
            exit 1
        fi
    done
}

echo 'Checking cross-language gRPC resolution...' >&2
rust_client='repo://github.com/example/beholder-rust-smoke/rust/client/rust_to_elixir'
rust_server='repo://github.com/example/beholder-rust-smoke/rust/server/impl/Bridge-for-RustHandler/elixir_to_rust'
elixir_client='repo://github.com/example/beholder-elixir-smoke/elixir/Phase5.Client/elixir_to_rust/2'
elixir_server='repo://github.com/example/beholder-elixir-smoke/elixir/Phase5.Server/rust_to_elixir/2'
check_grpc_path 'RustToElixir' "$rust_client" "$elixir_server"
check_grpc_path 'ElixirToRust' "$elixir_client" "$rust_server"

compact="$(target/debug/beholder trace --workspace main "$rust_client" "$elixir_server")"
if grep -Fq 'generated.rs' <<<"$compact"; then
    printf 'compact trace leaked generated support:\n%s\n' "$compact" >&2
    exit 1
fi
raw="$(target/debug/beholder trace --raw --workspace main "$rust_client" "$elixir_server")"
for expected in 'src/generated.rs' 'lib/grpc.pb.ex' 'confidence 1.00' 'revision 1' 'stale=false'; do
    if ! grep -Fq "$expected" <<<"$raw"; then
        printf 'expected %s in raw cross-language trace:\n%s\n' "$expected" "$raw" >&2
        exit 1
    fi
done

echo 'Checking contract removal and restoration...' >&2
target/debug/beholder workspace register main "$root" "$state/contracts" "$state/rust" "$state/elixir" \
    --protobuf-descriptor "$state/contracts/pricing.descriptor.bin" >/dev/null
bindings=''
for _ in {1..100}; do
    bindings="$(target/debug/beholder inspect grpc-bindings --database "$state/daemon/beholder.db")"
    grep -Fq 'grpc.contract_unmatched' <<<"$bindings" && break
    sleep 0.1
done
if ! grep -Fq 'grpc.contract_unmatched' <<<"$bindings"; then
    printf 'removing the contract did not expose unresolved candidates:\n%s\n' "$bindings" >&2
    exit 1
fi
target/debug/beholder workspace register main "$root" "$state/contracts" "$state/rust" "$state/elixir" \
    --protobuf-descriptor "$state/contracts/pricing.descriptor.bin" \
    --protobuf-descriptor "$state/contracts/grpc-matrix.descriptor.bin" >/dev/null
for _ in {1..100}; do
    result="$(target/debug/beholder context --json --workspace main \
        'grpc://phase5.v1.Bridge/RustToElixir' 2>/dev/null || true)"
    grep -Fq '"kind":"binds_contract"' <<<"$result" && break
    sleep 0.1
done
if ! grep -Fq '"kind":"binds_contract"' <<<"$result"; then
    printf 'restoring the contract did not resolve gRPC bindings:\n%s\n' "$result" >&2
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
