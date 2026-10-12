#!/bin/sh
# Runs the test suite in a Linux container with FUSE available, so overlay
# mode and other Linux-only behavior can be tested from macOS.
#
# Usage: scripts/linux-test.sh [cargo test arguments...]
#        scripts/linux-test.sh --shell
#
# Build output and test temporaries live on a named volume (ext4 inside the
# Docker VM, no reflink support), never on the bind-mounted source tree,
# whose inode and device semantics differ from a local Linux filesystem.

set -eu

image=git-repo-worktree-linux-test
volume=git-repo-worktree-linux-work
root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)

docker build -q -t "$image" -f "$root/scripts/linux-test.Dockerfile" "$root/scripts" >/dev/null
docker volume create "$volume" >/dev/null

repo_mount=""
if [ -n "${GRW_TEST_REPO_URL:-}" ] && [ -d "$GRW_TEST_REPO_URL" ]; then
    repo_mount="-v $GRW_TEST_REPO_URL:/git-repo-src:ro"
fi

interactive=""
if [ "${1:-}" = "--shell" ]; then
    shift
    interactive="-it"
    command="bash"
else
    command='set -e
        rm -rf /work/src /work/tmp && mkdir -p /work/src /work/tmp
        tar -C /src --exclude=./target --exclude=./.git -cf - . | tar -C /work/src -xf -
        if [ -d /git-repo-src ]; then
            rm -rf /work/git-repo && git clone -q /git-repo-src /work/git-repo
            export GRW_TEST_REPO_URL=/work/git-repo
        fi
        cd /work/src
        CARGO_TARGET_DIR=/work/target TMPDIR=/work/tmp cargo test --all-features "$@"'
fi

# shellcheck disable=SC2086
docker run --rm $interactive \
    --device /dev/fuse --cap-add SYS_ADMIN --security-opt apparmor=unconfined \
    -v "$root:/src:ro" -v "$volume:/work" $repo_mount \
    -e GRW_TEST_REQUIRE_OVERLAY=1 \
    "$image" sh -c "$command" sh "$@"
