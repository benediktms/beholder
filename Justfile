# Show available commands.
default:
    @just --list

# Lint every crate.
lint:
    moon run beholder:lint

# Test every crate.
test:
    moon run beholder:test

# Run the end-to-end dogfood smoke test.
smoke:
    moon run beholder:smoke
