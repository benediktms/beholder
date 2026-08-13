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

# ── Manual ────────────────────────────────────────────────────────────────────

# Run the end-to-end dogfood smoke test.
[group('manual')]
smoke:
    moon run beholder:smoke
