# Show available commands.
default:
    @just --list

# Lint every crate.
lint:
    moon run :lint

# Test every crate.
test:
    moon run :test

# Run the end-to-end dogfood smoke test.
smoke:
    moon run beholder-cli:dogfood
