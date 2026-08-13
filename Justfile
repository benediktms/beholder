# Show available commands.
default:
    @just --list

# Lint every crate.
[group('checks')]
lint:
    moon run beholder:lint

# Test every crate.
[group('checks')]
test:
    moon run beholder:test

# Run the end-to-end dogfood smoke test.
[group('manual')]
smoke:
    moon run beholder:smoke

# Check Rust formatting without changing files.
[group('formatting')]
format-check:
    moon run beholder:format-check

# Format every Rust crate.
[group('formatting')]
format:
    moon run beholder:format
