#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="${BEHOLDER_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}"

mkdir -p "$install_dir"
ln -sf "$root/target/release/beholder" "$install_dir/beholder"
ln -sf "$root/target/release/beholderd" "$install_dir/beholderd"
ln -sf "$root/target/release/beholder-worker-rust" "$install_dir/beholder-worker-rust"
ln -sf "$root/target/release/beholder-graph-ui" "$install_dir/beholder-graph-ui"
ln -sf "$root/workers/elixir/beholder-worker-elixir" "$install_dir/beholder-worker-elixir"
ln -sf "$root/workers/typescript/beholder-worker-typescript" "$install_dir/beholder-worker-typescript"
BEHOLDER_DAEMON_PATH="$install_dir/beholderd" \
    "$root/target/release/beholder" daemon install
