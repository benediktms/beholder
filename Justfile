# Beholder developer workflow. Just is the human-facing entry point, Moon
# orchestrates repository tasks, and Cargo remains authoritative for Rust.

default:
    @just --list

# ── Checks ───────────────────────────────────────────────────────────────────

# Run all repository formatting, linting, and test checks.
[group('checks')]
check:
    moon run :format-check :lint :test

alias c := check

# Lint every crate.
[group('checks')]
lint:
    moon run beholder:lint

alias l := lint

# Test every crate.
[group('checks')]
test:
    moon run beholder:test

alias t := test

# ── Formatting ───────────────────────────────────────────────────────────────

# Check Rust formatting without changing files.
[group('formatting')]
format-check:
    moon run beholder:format-check

# Format every Rust crate.
[group('formatting')]
format:
    moon run beholder:format

alias f := format

# ── Install ──────────────────────────────────────────────────────────────────

# Build and link the CLI, daemon, and compiler workers, then load the user daemon.
[group('install')]
[unix]
install:
    OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://localhost:4318}" \
    OTEL_EXPORTER_OTLP_LOGS_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://localhost:4318}/v1/logs" \
    OTEL_SERVICE_NAME="${OTEL_SERVICE_NAME:-beholderd}" \
    moon run beholder:install

# Unload the user daemon and remove both ~/.local/bin links.
[group('install')]
[unix]
uninstall:
    moon run beholder:uninstall

# ── Manual ────────────────────────────────────────────────────────────────────

# Run the end-to-end dogfood smoke test.
[group('manual')]
smoke:
    moon run beholder:smoke

# Benchmark cold and warm-frontend indexing with a bounded worker count.
[group('manual')]
index-bench workers repositories:
    moon run beholder:index-bench -- "{{workers}}" "{{repositories}}"

# Explain a Datalog query against the installed daemon database.
[group('manual')]
db-plan query:
    #!/usr/bin/env bash
    set -euo pipefail
    for attempt in 1 2 3; do
        if output="$(moon run beholder:db-plan -- "{{query}}" 2>&1)"; then
            printf '%s\n' "$output"
            exit 0
        fi
        [[ "$output" == *"database is locked"* ]] || break
        sleep 1
    done
    printf '%s\n' "$output" >&2
    exit 1

# Print the newest daemon trace segment (default: 100 lines).
[group('manual')]
logs lines="100":
    #!/usr/bin/env bash
    set -euo pipefail
    [[ "{{lines}}" =~ ^[1-9][0-9]*$ ]] || { echo 'line count must be a positive integer' >&2; exit 2; }
    state_base="${BEHOLDER_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/beholder}"
    log_dir="$state_base/daemon"
    log="$(find "$log_dir" -maxdepth 1 -name 'beholderd.*.log' -print 2>/dev/null | sort | tail -n 1)"
    [[ -n "$log" ]] || log="$log_dir/beholderd.log"
    [[ -f "$log" ]] || { echo "daemon log not found in $log_dir" >&2; exit 1; }
    tail -n "{{lines}}" "$log"
