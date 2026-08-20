#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="${BEHOLDER_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}"

mkdir -p "$install_dir"
ln -sf "$root/target/release/beholder" "$install_dir/beholder"
ln -sf "$root/target/release/beholderd" "$install_dir/beholderd"
ln -sf "$root/target/release/beholder-worker-rust" "$install_dir/beholder-worker-rust"

if [[ -z "${OTEL_EXPORTER_OTLP_ENDPOINT:-}" \
    && -z "${OTEL_EXPORTER_OTLP_TRACES_ENDPOINT:-}" \
    && -z "${OTEL_EXPORTER_OTLP_LOGS_ENDPOINT:-}" ]]; then
    export OTEL_EXPORTER_OTLP_ENDPOINT='http://localhost:4318'
fi
export OTEL_SERVICE_NAME="${OTEL_SERVICE_NAME:-beholderd}"

BEHOLDER_DAEMON_PATH="$install_dir/beholderd" \
    "$root/target/release/beholder" daemon install
