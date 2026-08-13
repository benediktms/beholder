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
