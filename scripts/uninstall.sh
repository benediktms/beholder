#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="${BEHOLDER_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}"
cli="$root/target/release/beholder"

if [[ ! -x "$cli" ]]; then
    cli="$install_dir/beholder"
fi
if [[ -x "$cli" ]]; then
    "$cli" daemon uninstall
else
    echo 'beholder is not built or installed; skipping daemon uninstall' >&2
fi
rm -f \
    "$install_dir/beholder" \
    "$install_dir/beholderd" \
    "$install_dir/beholder-worker-rust" \
    "$install_dir/beholder-worker-elixir"
