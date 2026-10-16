#!/bin/sh
# Runs the fuzzers on Linux in Docker, with FUSE overlays, background
# maintenance, and crash injection. Workers: (1) plain world with older
# releases and faults, (2) partial clones, (3) worktree layout, (4) a full
# disk on a small tmpfs, and (5) the race driver. Findings are written to
# fuzz/findings/.
#
# Usage: fuzz/run-linux.sh [examples-per-worker] [race-rounds]
# Needs GRW_TEST_REPO_URL (or /tmp/git-repo-src) naming a git-repo checkout,
# and gh for downloading the older releases.

set -eu

examples=${1:-30}
rounds=${2:-30}
root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
repo_src=${GRW_TEST_REPO_URL:-/tmp/git-repo-src}
mkdir -p "$root/fuzz/findings"

arch=$(docker info --format '{{.Architecture}}')
case "$arch" in
    aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
    *) target=x86_64-unknown-linux-musl ;;
esac
old_host="${TMPDIR:-/tmp}/grw-old-linux"
"$root/fuzz/fetch-old.sh" "$target" "$old_host" >/dev/null
old=""
for binary in "$old_host"/*/git-repo-worktree; do
    tag=$(basename "$(dirname "$binary")")
    old="$old${old:+,}$tag=/old/$tag/git-repo-worktree"
done

docker build -q -t git-repo-worktree-linux-test -f "$root/scripts/linux-test.Dockerfile" "$root/scripts" >/dev/null
docker build -q -t git-repo-worktree-fuzz -f "$root/fuzz/Dockerfile" "$root/fuzz" >/dev/null
docker volume create git-repo-worktree-linux-work >/dev/null

docker run --rm \
    --device /dev/fuse --cap-add SYS_ADMIN --security-opt apparmor=unconfined \
    --tmpfs /work-disk:rw,exec,size=700m,uid=1000,gid=1000 \
    -v "$root:/src:ro" -v git-repo-worktree-linux-work:/work \
    -v "$repo_src:/git-repo-src:ro" -v "$old_host:/old:ro" -v "$root/fuzz/findings:/findings" \
    -e GRW_FUZZ_STEPS="${GRW_FUZZ_STEPS:-15}" -e OLD_BINS="$old" \
    git-repo-worktree-fuzz sh -c '
        set -e
        rm -rf /work/src /work/fuzz && mkdir -p /work/src /work/fuzz
        tar -C /src --exclude=./target --exclude=./.git -cf - . | tar -C /work/src -xf -
        rm -rf /work/git-repo && git clone -q /git-repo-src /work/git-repo
        cd /work/src
        CARGO_TARGET_DIR=/work/fuzz-target cargo build -q --features test-hooks
        export GRW_BIN=/work/fuzz-target/debug/git-repo-worktree GRW_TEST_REPO_URL=/work/git-repo \
            GRW_FUZZ_FINDINGS=/findings GRW_FUZZ_MAINTENANCE=1 GRW_FUZZ_CRASH=1
        GRW_FUZZ_WORKER=linux1 GRW_FUZZ_BASE=/work/fuzz/1 GRW_FUZZ_FAULTS=1 GRW_FUZZ_OLD_BINS="$OLD_BINS" \
            python3 fuzz/harness.py "$1" > /work/fuzz/1.out 2>&1 &
        GRW_FUZZ_WORKER=linux2 GRW_FUZZ_BASE=/work/fuzz/2 GRW_FUZZ_WORLD=partial \
            python3 fuzz/harness.py "$1" > /work/fuzz/2.out 2>&1 &
        GRW_FUZZ_WORKER=linux3 GRW_FUZZ_BASE=/work/fuzz/3 GRW_FUZZ_WORLD=worktree \
            python3 fuzz/harness.py "$1" > /work/fuzz/3.out 2>&1 &
        GRW_FUZZ_WORKER=linux4 GRW_FUZZ_BASE=/work-disk/4 GRW_FUZZ_DISK=1 \
            python3 fuzz/harness.py "$1" > /work/fuzz/4.out 2>&1 &
        GRW_FUZZ_WORKER=linux5 GRW_FUZZ_BASE=/work/fuzz/5 \
            python3 fuzz/race.py "$2" > /work/fuzz/5.out 2>&1 &
        wait
        for worker in 1 2 3 4 5; do
            echo "== worker $worker"
            grep -E "world=|FINDING|reproduction|details:|no findings|finding\(s\)|^after [0-9]+s|Traceback|Error:" \
                "/work/fuzz/$worker.out" | cut -c1-260 | head -12
        done
    ' sh "$examples" "$rounds"
