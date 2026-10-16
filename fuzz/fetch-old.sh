#!/bin/sh
# Downloads earlier releases for GRW_FUZZ_OLD_BINS and prints the value to
# use. Usage: fuzz/fetch-old.sh <target-triple> <directory> [tags...]

set -eu

target=$1
directory=$2
shift 2
[ "$#" -gt 0 ] || set -- v1.0.0 v2.0.0 v2.1.0 v2.1.1 v2.1.2
mkdir -p "$directory"
pairs=""
for tag in "$@"; do
    destination="$directory/$tag"
    if [ ! -x "$destination/git-repo-worktree" ]; then
        mkdir -p "$destination"
        gh release download "$tag" -R jomof/git-repo-worktree \
            --pattern "git-repo-worktree-$target.tar.gz" -O - | tar -xzf - -C "$destination"
        found=$(find "$destination" -name git-repo-worktree -type f | head -n 1)
        [ "$found" = "$destination/git-repo-worktree" ] || mv "$found" "$destination/git-repo-worktree"
    fi
    pairs="$pairs${pairs:+,}$tag=$destination/git-repo-worktree"
done
echo "$pairs"
