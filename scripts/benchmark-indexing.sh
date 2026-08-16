#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 2 ]] || { echo "usage: $0 <workers> <repository-path-list>" >&2; exit 2; }
[[ "$1" =~ ^[1-9][0-9]*$ ]] || { echo 'workers must be a positive integer' >&2; exit 2; }

export BEHOLDER_INDEX_WORKERS="$1"
export BEHOLDER_INDEX_BENCH_REPOSITORIES="$2"
test_name='indexing::scheduler::tests::benchmark_indexing'

cargo test --release -p beholder-daemon "$test_name" --no-run
if [[ "$(uname)" == 'Darwin' ]]; then
    /usr/bin/time -l cargo test --release -p beholder-daemon "$test_name" -- --ignored --exact --nocapture
else
    /usr/bin/time -v cargo test --release -p beholder-daemon "$test_name" -- --ignored --exact --nocapture
fi
