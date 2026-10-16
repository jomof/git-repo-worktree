#!/bin/sh
# Runs four fuzz workers on this machine, one per configuration: plain world
# with old releases and faults, partial clone, worktree layout, and random
# kills. Findings are written to fuzz/findings/.
#
# Usage: fuzz/run-local.sh [examples-per-worker]
# Environment: GRW_BIN (a --features test-hooks build), PYTHON (with
# Hypothesis), GRW_FUZZ_TARGET (release triple for old binaries).

set -eu

examples=${1:-20}
root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
bin=${GRW_BIN:?set GRW_BIN to a --features test-hooks build}
python=${PYTHON:-python3}
target=${GRW_FUZZ_TARGET:-aarch64-apple-darwin}
old=$("$root/fuzz/fetch-old.sh" "$target" "${TMPDIR:-/tmp}/grw-old")

export GRW_BIN="$bin" GRW_FUZZ_STEPS="${GRW_FUZZ_STEPS:-12}" GRW_FUZZ_MAINTENANCE=1 GRW_FUZZ_CRASH=1
cd "$root"
GRW_FUZZ_WORKER=1 GRW_FUZZ_FAULTS=1 GRW_FUZZ_OLD_BINS="$old" \
    "$python" fuzz/harness.py "$examples" > /tmp/grw-fuzz-1.out 2>&1 &
GRW_FUZZ_WORKER=2 GRW_FUZZ_WORLD=partial "$python" fuzz/harness.py "$examples" > /tmp/grw-fuzz-2.out 2>&1 &
GRW_FUZZ_WORKER=3 GRW_FUZZ_WORLD=worktree "$python" fuzz/harness.py "$examples" > /tmp/grw-fuzz-3.out 2>&1 &
GRW_FUZZ_WORKER=4 GRW_FUZZ_KILL=1 "$python" fuzz/harness.py "$examples" > /tmp/grw-fuzz-4.out 2>&1 &
wait
for worker in 1 2 3 4; do
    echo "== worker $worker"
    grep -E "world=|FINDING|reproduction|no findings|^after [0-9]+s|Traceback|Error:" "/tmp/grw-fuzz-$worker.out" | cut -c1-260 | head -10
done
