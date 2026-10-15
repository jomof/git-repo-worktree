#!/bin/bash
set -eu

R6=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-6
mkdir -p "$R6/data/studio-main" "$R6/data/main" "$R6/scripts"

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
} > "$R6/data/environment.txt" 2>&1

echo "=== 1. Snapshotting source integrity (before) ==="
snap_studio "$R6/data/studio-main/snap-before.txt"
snap_main "$R6/data/main/snap-before.txt"

echo "=== 2. Recording starting state for main ==="
GIT_REPO_WORKTREE_SESSION=R6-setup git -C "$MAIN" repo-worktree list --json > "$R6/data/main/setup-list.json"
GIT_REPO_WORKTREE_SESSION=R6-setup git -C "$MAIN" repo-worktree pool list > "$R6/data/main/setup-pool-list.txt"
GIT_REPO_WORKTREE_SESSION=R6-setup git -C "$MAIN" repo-worktree pool list --json > "$R6/data/main/setup-pool-list.json"
df -B1 / > "$R6/data/main/setup-df.txt"
du -sx "$MAIN/.repo/repo-worktrees/.pool" > "$R6/data/main/setup-du-pool.txt"

echo "=== 3. Recording starting state for studio-main ==="
GIT_REPO_WORKTREE_SESSION=R6-setup git -C "$STUDIO" repo-worktree list --json > "$R6/data/studio-main/setup-list.json"
GIT_REPO_WORKTREE_SESSION=R6-setup git -C "$STUDIO" repo-worktree pool list > "$R6/data/studio-main/setup-pool-list.txt"
GIT_REPO_WORKTREE_SESSION=R6-setup git -C "$STUDIO" repo-worktree pool list --json > "$R6/data/studio-main/setup-pool-list.json"
df -B1 / > "$R6/data/studio-main/setup-df.txt"
du -sx "$STUDIO/.repo/repo-worktrees/.pool" > "$R6/data/studio-main/setup-du-pool.txt"

echo "=== 4. Deleting estimates.json in both clients once at start (§2.4) ==="
rm -f "$MAIN/.repo/repo-worktrees/estimates.json" "$STUDIO/.repo/repo-worktrees/estimates.json"
echo "estimates.json deleted in both clients."
