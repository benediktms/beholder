#!/usr/bin/env bash
set -euo pipefail

root="$(pwd)"
state="$(mktemp -d /tmp/beholder-integration-test.XXXXXX)"
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

start_daemon() {
    target/debug/beholderd >>"$state/beholderd.log" 2>&1 &
    daemon_pid=$!
    for _ in {1..50}; do
        target/debug/beholder daemon status >/dev/null 2>&1 && return
        if ! kill -0 "$daemon_pid" 2>/dev/null; then
            cat "$state/beholderd.log" >&2
            exit 1
        fi
        sleep 0.1
    done
    target/debug/beholder daemon status >/dev/null
}

echo 'Building Beholder binaries...' >&2
bash "$root/scripts/test-daemon-handover.sh"
echo 'Starting isolated beholderd...' >&2
start_daemon
[[ -S "$socket" ]] || { echo "daemon socket not found at $socket" >&2; exit 1; }
mkdir -p "$state/contracts"
xxd -r -p "$root/scripts/fixtures/pricing.descriptor.hex" "$state/contracts/pricing.descriptor.bin"
xxd -r -p "$root/scripts/fixtures/grpc-matrix.descriptor.hex" "$state/contracts/grpc-matrix.descriptor.bin"
mkdir -p "$state/elixir/lib"
cp "$root/scripts/fixtures/integration-test/elixir/smoke.ex.fixture" "$state/elixir/lib/smoke.ex"
cp "$root/scripts/fixtures/integration-test/elixir/grpc.pb.ex.fixture" "$state/elixir/lib/grpc.pb.ex"
git -C "$state/elixir" init -q
git -C "$state/elixir" config user.name 'Beholder Integration Test'
git -C "$state/elixir" config user.email 'integration-test@beholder.local'
ssh-keygen -q -t ed25519 -N '' -f "$state/signing-key"
git -C "$state/elixir" config gpg.format ssh
git -C "$state/elixir" config gpg.ssh.program "$(command -v ssh-keygen)"
git -C "$state/elixir" config user.signingkey "$state/signing-key"
git -C "$state/elixir" config commit.gpgsign true
git -C "$state/elixir" add lib
git -C "$state/elixir" commit -qm 'Add Elixir integration-test fixture'
git -C "$state/elixir" remote add origin https://github.com/example/beholder-elixir-smoke.git
mkdir -p "$state/rust/src"
printf '[package]\nname = "beholder-rust-smoke"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\ntonic = "0.14"\n' >"$state/rust/Cargo.toml"
cp "$root/scripts/fixtures/integration-test/rust/protocol.rs.fixture" "$state/rust/src/protocol.rs"
cp "$root/scripts/fixtures/integration-test/rust/generated.rs.fixture" "$state/rust/src/generated.rs"
cp "$root/scripts/fixtures/integration-test/rust/client.rs.fixture" "$state/rust/src/client.rs"
cp "$root/scripts/fixtures/integration-test/rust/server.rs.fixture" "$state/rust/src/server.rs"
git -C "$state/rust" init -q
git -C "$state/rust" config user.name 'Beholder Integration Test'
git -C "$state/rust" config user.email 'integration-test@beholder.local'
git -C "$state/rust" config gpg.format ssh
git -C "$state/rust" config gpg.ssh.program "$(command -v ssh-keygen)"
git -C "$state/rust" config user.signingkey "$state/signing-key"
git -C "$state/rust" config commit.gpgsign true
git -C "$state/rust" add Cargo.toml src
git -C "$state/rust" commit -qm 'Add Rust integration-test fixture'
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
caller="repo://$repository/rust/crates/daemon/src/main/run_daemon"
callee="repo://$repository/rust/crates/daemon-client/src/lib/state_dir"
echo 'Waiting for automatic Beholder indexing...' >&2
result=''
for _ in {1..600}; do
    result="$(target/debug/beholder context --json --workspace main "$caller" 2>/dev/null || true)"
    if grep -Fq "$callee" <<<"$result" && grep -Fq '"stale":false' <<<"$result"; then
        break
    fi
    sleep 0.1
done
if ! grep -Fq "$callee" <<<"$result"; then
    printf 'automatic indexing did not produce %s in context:\n%s\n' "$callee" "$result" >&2
    exit 1
fi
if ! grep -Fq '"stale":false' <<<"$result"; then
    printf 'automatic indexing did not reach current freshness:\n%s\n' "$result" >&2
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

    context=''
    for _ in {1..1200}; do
        context="$(target/debug/beholder context --json --workspace main "$operation" 2>/dev/null || true)"
        grep -Fq '"stale":false' <<<"$context" && break
        sleep 0.1
    done
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
if ! grep -Eq 'revision [0-9]+' <<<"$raw"; then
    printf 'expected a revision in raw cross-language trace:\n%s\n' "$raw" >&2
    exit 1
fi
for expected in 'src/generated.rs' 'lib/grpc.pb.ex' 'confidence 1.00' 'stale=false'; do
    if ! grep -Fq "$expected" <<<"$raw"; then
        printf 'expected %s in raw cross-language trace:\n%s\n' "$expected" "$raw" >&2
        exit 1
    fi
done

echo 'Checking closed inspect output...' >&2
if ! target/debug/beholder inspect grpc-bindings --database "$state/daemon/beholder.db" | head -n 1 >/dev/null; then
    echo 'inspect grpc-bindings did not handle a closed output pipe cleanly' >&2
    exit 1
fi

echo 'Checking contract removal and restoration...' >&2
target/debug/beholder workspace register main "$root" "$state/contracts" "$state/rust" "$state/elixir" \
    --protobuf-descriptor "$state/contracts/pricing.descriptor.bin" >/dev/null
bindings=''
for _ in {1..600}; do
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
for _ in {1..600}; do
    result="$(target/debug/beholder context --json --workspace main \
        'grpc://phase5.v1.Bridge/RustToElixir' 2>/dev/null || true)"
    grep -Fq '"kind":"binds_contract"' <<<"$result" && grep -Fq '"stale":false' <<<"$result" && break
    sleep 0.1
done
if ! grep -Fq '"kind":"binds_contract"' <<<"$result" || ! grep -Fq '"stale":false' <<<"$result"; then
    printf 'restoring the contract did not resolve gRPC bindings:\n%s\n' "$result" >&2
    exit 1
fi
echo 'Checking run_daemon -> state_dir...' >&2
echo 'Checking state_dir impact reaches run_daemon...' >&2
for _ in {1..600}; do
    result="$(target/debug/beholder impact --json --max-hops 1 --workspace main "$callee")"
    grep -Fq "$caller" <<<"$result" && break
    sleep 0.1
done
if ! grep -Fq "$caller" <<<"$result"; then
    printf 'expected %s in impact result:\n%s\n' "$caller" "$result" >&2
    exit 1
fi
echo 'Checking why run_daemon reaches state_dir...' >&2
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

echo 'Checking manual enrichment and daemon-kill recovery...' >&2
rust_repository='github.com/example/beholder-rust-smoke'
printf '%s\n' 'fn manual_enrichment_recovery() {}' >"$state/rust/src/manual_enrichment.rs"
index_submission="$(target/debug/beholder index "$rust_repository" --workspace main)"
index_job="$(awk '$1 == "enqueued" { print $2; exit }' <<<"$index_submission")"
enrichment_submission="$(target/debug/beholder enrich "$rust_repository" --workspace main --only rust)"
enrichment_job="$(awk '$1 == "enqueued" { print $2; exit }' <<<"$enrichment_submission")"
if [[ -z "$index_job" || -z "$enrichment_job" ]] || \
    ! grep -Fq "prerequisite $index_job" <<<"$enrichment_submission"; then
    printf 'manual enrichment did not expose its durable prerequisite and job:\n%s\n%s\n' \
        "$index_submission" "$enrichment_submission" >&2
    exit 1
fi
kill -KILL "$daemon_pid"
wait "$daemon_pid" 2>/dev/null || true
daemon_pid=''
start_daemon
for _ in {1..600}; do
    enrichment_result="$(target/debug/beholder job get "$enrichment_job" 2>&1 || true)"
    grep -Fq 'status: Completed' <<<"$enrichment_result" && break
    if grep -Fq 'status: Failed' <<<"$enrichment_result"; then
        break
    fi
    sleep 0.1
done
if ! grep -Fq 'status: Completed' <<<"$enrichment_result" || \
    ! grep -Fq '/worker:rust' <<<"$enrichment_result" || \
    ! grep -Fq 'result:' <<<"$enrichment_result"; then
    printf 'persisted enrichment did not recover after daemon kill:\n%s\n' \
        "$enrichment_result" >&2
    exit 1
fi

index_until_current() {
    local entity="$1"
    local context=''
    local output=''
    local job_id=''
    if ! output="$(target/debug/beholder index main 2>&1)"; then
        printf '%s' "$output"
        return 1
    fi
    job_id="$(awk '$1 == "enqueued" { print $2; exit }' <<<"$output")"
    for _ in {1..600}; do
        output="$(target/debug/beholder job get "$job_id" 2>&1)"
        if grep -Fq 'status: Completed' <<<"$output"; then
            context="$(target/debug/beholder context --json --workspace main "$entity" 2>&1 || true)"
            if grep -Fq "$entity" <<<"$context" && grep -Fq '"stale":false' <<<"$context"; then
                printf '%s' "$output"
                return 0
            fi
        fi
        if grep -Fq 'status: Failed' <<<"$output"; then
            printf '%s' "$output"
            return 1
        fi
        sleep 0.1
    done
    printf '%s\n%s' "$output" "$context"
    return 1
}

echo 'Checking a bad source does not abort workspace indexing...' >&2
recovery_source="$state/rust/src/recovery.rs"
printf '%s\n' 'fn broken() {' 'fn nested() {}' >"$recovery_source"
if ! reindex_output="$(index_until_current "$rust_client")"; then
    printf 'unrecoverable Rust source aborted workspace indexing:\n%s\n' "$reindex_output" >&2
    exit 1
fi
result="$(target/debug/beholder context --json --workspace main "$rust_client")"
if ! grep -Fq '"kind":"calls_rpc"' <<<"$result"; then
    printf 'bad Rust source removed valid sibling observations:\n%s\n' "$result" >&2
    exit 1
fi
printf '%s\n' 'fn repaired() {}' >"$recovery_source"
repaired='repo://github.com/example/beholder-rust-smoke/rust/recovery/repaired'
if ! repaired_output="$(index_until_current "$repaired")"; then
    printf 'repaired Rust source did not reach current indexing:\n%s\n' "$repaired_output" >&2
    exit 1
fi

echo 'Garbage collecting obsolete semantic states...' >&2
gc_result="$(target/debug/beholder cache gc)"
if ! grep -Eq '^queued [0-9]+ obsolete repository states for background cleanup$' <<<"$gc_result"; then
    printf 'unexpected garbage collection result:\n%s\n' "$gc_result" >&2
    exit 1
fi
for _ in {1..100}; do
    gc_status="$(target/debug/beholder cache gc --status)"
    grep -Fq 'idle · 0 obsolete repository states · 0 queued · 0 database pages reclaimable' \
        <<<"$gc_status" && break
    sleep 0.1
done
if ! grep -Fq \
    'idle · 0 obsolete repository states · 0 queued · 0 database pages reclaimable' \
    <<<"$gc_status"; then
    printf 'garbage collection did not finish:\n%s\n' "$gc_status" >&2
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
unexpected_errors="$(grep -E '"level":"(WARN|ERROR)"' "$trace_file" || true)"
unexpected_errors="$(grep -Fv 'rust.parse_recovery' <<<"$unexpected_errors" || true)"
unexpected_errors="$(grep -Fv \
    'workspace inputs changed during indexing; stale analysis was discarded' \
    <<<"$unexpected_errors" || true)"
unexpected_errors="$(grep -Fv \
    '"message":"job retry scheduled"' \
    <<<"$unexpected_errors" || true)"
unexpected_errors="$(grep -Fv \
    '"message":"index job attempt failed"' \
    <<<"$unexpected_errors" || true)"
unexpected_errors="$(grep -Fv \
    '"message":"interrupted job recovered"' \
    <<<"$unexpected_errors" || true)"
if [[ -n "$unexpected_errors" ]]; then
    echo 'daemon trace contains warnings or errors:' >&2
    printf '%s\n' "$unexpected_errors" >&2
    exit 1
fi
for expected in \
    'daemon started' \
    'workspace indexed' \
    'facts_inserted' \
    'rpc.context' \
    'semantic store garbage collection sweep completed' \
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
echo "integration test passed: indexed Rust, Elixir, and Protobuf semantics" >&2
