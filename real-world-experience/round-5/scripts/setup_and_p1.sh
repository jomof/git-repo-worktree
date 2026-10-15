#!/bin/bash
set -eu

R5=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-5
mkdir -p "$R5/data/studio-main" "$R5/data/main" "$R5/scripts"

STUDIO=/usr/local/google/home/jomof/projects/studio-main
MAIN=/usr/local/google/home/jomof/projects/main

snap_studio() {
  local out=$1
  while IFS= read -r p; do
    [ -z "$p" ] && continue
    head_sha=$(git -C "$STUDIO/$p" rev-parse HEAD 2>/dev/null || echo "MISSING")
    dirty=$(git -C "$STUDIO/$p" status --porcelain=v1 -uno 2>/dev/null | wc -l)
    echo "$p $head_sha dirty_tracked=$dirty"
  done < "$STUDIO/.repo/project.list" > "$out"
}

snap_main() {
  local out=$1
  while IFS= read -r p; do
    [ -z "$p" ] && continue
    head_sha=$(git -C "$MAIN/$p" rev-parse HEAD 2>/dev/null || echo "MISSING")
    echo "$p $head_sha"
  done < "$MAIN/.repo/project.list" > "$out"
  for kp in frameworks/base cts build/make build/soong bionic art system/core; do
    dirty=$(git -C "$MAIN/$kp" status --porcelain=v1 2>/dev/null | wc -l)
    echo "# key_status $kp dirty=$dirty" >> "$out"
  done
}

{
  echo "# captured $(date -u +%FT%TZ)"
  git repo-worktree version
  which -a git-repo-worktree
  uname -a
  nproc
  free -g
  df -B1 /
  findmnt -T / -o TARGET,FSTYPE,OPTIONS
  git --version
  repo --version 2>&1 | head -2
  fuse-overlayfs --version 2>&1 | head -1
  uptime
  cat /proc/pressure/cpu /proc/pressure/io /proc/pressure/memory
} > "$R5/data/environment.txt" 2>&1

echo "=== 1. Snapshotting source integrity (before) ==="
snap_studio "$R5/data/studio-main/snap-before.txt"
snap_main "$R5/data/main/snap-before.txt"

echo "=== 2. Recording starting state for main ==="
GIT_REPO_WORKTREE_SESSION=R5-setup git -C "$MAIN" repo-worktree list --json > "$R5/data/main/setup-list.json"
GIT_REPO_WORKTREE_SESSION=R5-setup git -C "$MAIN" repo-worktree pool list > "$R5/data/main/setup-pool-list.txt"
GIT_REPO_WORKTREE_SESSION=R5-setup git -C "$MAIN" repo-worktree pool list --json > "$R5/data/main/setup-pool-list.json"
df -B1 / > "$R5/data/main/setup-df.txt"
du -sx "$MAIN/.repo/repo-worktrees/.pool" > "$R5/data/main/setup-du-pool.txt"

echo "=== 3. Recording starting state & P1 before for studio-main ==="
GIT_REPO_WORKTREE_SESSION=R5-setup git -C "$STUDIO" repo-worktree list --json > "$R5/data/studio-main/setup-list.json"
GIT_REPO_WORKTREE_SESSION=R5-P1 git -C "$STUDIO" repo-worktree pool list > "$R5/data/studio-main/P1-pool-list-before.txt"
GIT_REPO_WORKTREE_SESSION=R5-P1 git -C "$STUDIO" repo-worktree pool list --json > "$R5/data/studio-main/P1-pool-list-before.json"
df -B1 / > "$R5/data/studio-main/P1-df-before.txt"
du -sx "$STUDIO/.repo/repo-worktrees/.pool" > "$R5/data/studio-main/P1-du-pool-before.txt"

echo "--- P1 pool list (before) ---"
cat "$R5/data/studio-main/P1-pool-list-before.txt"

echo "=== 4. Running P1: pool clear --orphaned ==="
GIT_REPO_WORKTREE_SESSION=R5-P1 /usr/bin/time -v -o "$R5/data/studio-main/P1-clear-orphaned.time" \
  git -C "$STUDIO" repo-worktree pool clear --orphaned --json \
  > "$R5/data/studio-main/P1-clear-orphaned.json" \
  2> "$R5/data/studio-main/P1-clear-orphaned.stderr"

df -B1 / > "$R5/data/studio-main/P1-df-after.txt"
du -sx "$STUDIO/.repo/repo-worktrees/.pool" > "$R5/data/studio-main/P1-du-pool-after.txt"

GIT_REPO_WORKTREE_SESSION=R5-P1 git -C "$STUDIO" repo-worktree pool list > "$R5/data/studio-main/P1-pool-list-after.txt"
GIT_REPO_WORKTREE_SESSION=R5-P1 git -C "$STUDIO" repo-worktree pool list --json > "$R5/data/studio-main/P1-pool-list-after.json"

echo "--- P1 stderr & timing ---"
cat "$R5/data/studio-main/P1-clear-orphaned.stderr"
rg 'Elapsed|User time|System time|Percent of CPU' "$R5/data/studio-main/P1-clear-orphaned.time"
echo "--- P1 du -sx .pool (KB) before vs after ---"
cat "$R5/data/studio-main/P1-du-pool-before.txt"
cat "$R5/data/studio-main/P1-du-pool-after.txt"
echo "--- P1 df -B1 / before vs after ---"
cat "$R5/data/studio-main/P1-df-before.txt"
cat "$R5/data/studio-main/P1-df-after.txt"
echo "--- P1 pool list (after) ---"
cat "$R5/data/studio-main/P1-pool-list-after.txt"
