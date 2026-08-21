#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="${BEHOLDER_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}"

mkdir -p "$install_dir"
ln -sf "$root/target/release/beholder" "$install_dir/beholder"
ln -sf "$root/target/release/beholderd" "$install_dir/beholderd"
ln -sf "$root/target/release/beholder-worker-rust" "$install_dir/beholder-worker-rust"
BEHOLDER_DAEMON_PATH="$install_dir/beholderd" \
    "$root/target/release/beholder" daemon install
