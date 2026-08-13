# Beholder developer workflow. Just is the human-facing entry point, Moon
# orchestrates repository tasks, and Cargo remains authoritative for Rust.

default:
    @just --list

# ── Checks ───────────────────────────────────────────────────────────────────

# Run all repository formatting, linting, and test checks.
[group('checks')]
check:
    moon run beholder:lint
    moon run beholder:test

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

# Build, link both binaries into ~/.local/bin, and load the user daemon.
[group('install')]
[unix]
install:
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
