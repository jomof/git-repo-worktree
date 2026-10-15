#!/bin/sh
# Runs the fuzzer on Linux in Docker, with FUSE overlays, background
# maintenance, and crash injection. Findings are written to fuzz/findings/.
#
# Usage: fuzz/run-linux.sh [workers] [examples-per-worker]
# Needs GRW_TEST_REPO_URL (or /tmp/git-repo-src) naming a git-repo checkout.

set -eu

workers=${1:-4}
examples=${2:-40}
root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
repo_src=${GRW_TEST_REPO_URL:-/tmp/git-repo-src}
mkdir -p "$root/fuzz/findings"

docker build -q -t git-repo-worktree-linux-test -f "$root/scripts/linux-test.Dockerfile" "$root/scripts" >/dev/null
docker build -q -t git-repo-worktree-fuzz -f "$root/fuzz/Dockerfile" "$root/fuzz" >/dev/null
docker volume create git-repo-worktree-linux-work >/dev/null

docker run --rm \
    --device /dev/fuse --cap-add SYS_ADMIN --security-opt apparmor=unconfined \
    -v "$root:/src:ro" -v git-repo-worktree-linux-work:/work \
    -v "$repo_src:/git-repo-src:ro" -v "$root/fuzz/findings:/findings" \
    -e GRW_FUZZ_STEPS="${GRW_FUZZ_STEPS:-15}" \
    -e GRW_FUZZ_MAINTENANCE="${GRW_FUZZ_MAINTENANCE:-1}" \
    -e GRW_FUZZ_CRASH="${GRW_FUZZ_CRASH:-1}" \
    git-repo-worktree-fuzz sh -c '
        set -e
        rm -rf /work/src /work/fuzz && mkdir -p /work/src /work/fuzz
        tar -C /src --exclude=./target --exclude=./.git -cf - . | tar -C /work/src -xf -
        rm -rf /work/git-repo && git clone -q /git-repo-src /work/git-repo
        cd /work/src
        CARGO_TARGET_DIR=/work/fuzz-target cargo build -q --features test-hooks
        for worker in $(seq 1 "$1"); do
            GRW_BIN=/work/fuzz-target/debug/git-repo-worktree GRW_TEST_REPO_URL=/work/git-repo \
                GRW_FUZZ_WORKER="linux$worker" GRW_FUZZ_BASE="/work/fuzz/$worker" \
                GRW_FUZZ_FINDINGS=/findings \
                python3 fuzz/harness.py "$2" > "/work/fuzz/$worker.out" 2>&1 &
        done
        wait
        for worker in $(seq 1 "$1"); do
            echo "== worker $worker"
            grep -E "overlay=|FINDING|reproduction|no findings|^after [0-9]+s" "/work/fuzz/$worker.out" | head -12
        done
    ' sh "$workers" "$examples"
