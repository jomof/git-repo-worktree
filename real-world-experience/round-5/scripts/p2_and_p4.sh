#!/bin/bash
set -eu

R5=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-5
D="$R5/data/studio-main"
STUDIO=/usr/local/google/home/jomof/projects/studio-main
HIST="$STUDIO/.repo/repo-worktrees/history.jsonl"

wait_for_maintain() {
  local label=$1
  local start_wait=$(date +%s)
  sleep 1
  while pgrep -f "[g]it-repo-worktree.*pool maintain" >/dev/null 2>&1; do
    sleep 1
  done
  local end_wait=$(date +%s)
  echo "[$label] waited $((end_wait - start_wait))s for background pool maintain to exit"
  jq -c 'select(.command=="pool maintain")' "$HIST" | tail -1 | tee "$D/${label}-maintain-event.json"
}

cleanup_existing() {
  for t in A B C D S1 S2 S3; do
    if [ -d "/usr/local/google/home/jomof/projects/studio-wt-r5-$t" ]; then
      GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 git -C "$STUDIO" repo-worktree remove --json --force "studio-wt-r5-$t" >/dev/null 2>&1 || true
    fi
  done
}

cleanup_existing

echo "=== P2.1: Add tree A with maintenance on, wait for pool maintain ==="
df -B1 / > "$D/P2-df-before-A.txt"
GIT_REPO_WORKTREE_SESSION=R5-P2-A /usr/bin/time -v -o "$D/P2-add-A.time" \
  git -C "$STUDIO" repo-worktree add --json -v /usr/local/google/home/jomof/projects/studio-wt-r5-A \
  > "$D/P2-add-A.json" 2> "$D/P2-add-A.stderr"
df -B1 / > "$D/P2-df-immediately-after-A.txt"

wait_for_maintain "P2-after-A"
df -B1 / > "$D/P2-df-after-A-maintain.txt"
GIT_REPO_WORKTREE_SESSION=R5-P2-A git -C "$STUDIO" repo-worktree pool list > "$D/P2-pool-list-after-A.txt"
GIT_REPO_WORKTREE_SESSION=R5-P2-A git -C "$STUDIO" repo-worktree pool list --json > "$D/P2-pool-list-after-A.json"
cat "$D/P2-pool-list-after-A.txt"

echo "=== P2.2: Burst add B, C, D back-to-back without waiting ==="
df -B1 / > "$D/P2-df-before-burst.txt"
for t in B C D; do
  echo "--- Burst add $t ---"
  GIT_REPO_WORKTREE_SESSION="R5-P2-burst-$t" /usr/bin/time -v -o "$D/P2-burst-add-$t.time" \
    git -C "$STUDIO" repo-worktree add --json -v "/usr/local/google/home/jomof/projects/studio-wt-r5-$t" \
    > "$D/P2-burst-add-$t.json" 2> "$D/P2-burst-add-$t.stderr"
  df -B1 / > "$D/P2-df-after-burst-$t.txt"
  rg '^phase .*: [0-9]' "$D/P2-burst-add-$t.stderr" | tr '\n' ' '; echo
done
wait_for_maintain "P2-after-burst"
GIT_REPO_WORKTREE_SESSION=R5-P2-burst git -C "$STUDIO" repo-worktree pool list > "$D/P2-pool-list-after-burst.txt"

echo "=== P2.3a: Remove B, C, D and wait for maintenance ==="
for t in B C D; do
  GIT_REPO_WORKTREE_SESSION="R5-P2-rm-$t" /usr/bin/time -v -o "$D/P2-rm-$t.time" \
    git -C "$STUDIO" repo-worktree remove --json "studio-wt-r5-$t" \
    > "$D/P2-rm-$t.json" 2> "$D/P2-rm-$t.stderr"
done
wait_for_maintain "P2-after-rm-BCD"
GIT_REPO_WORKTREE_SESSION=R5-P2-rm git -C "$STUDIO" repo-worktree pool list > "$D/P2-pool-list-before-spaced.txt"
GIT_REPO_WORKTREE_SESSION=R5-P2-rm git -C "$STUDIO" repo-worktree pool list --json > "$D/P2-pool-list-before-spaced.json"
cat "$D/P2-pool-list-before-spaced.txt"

echo "=== P2.3b: Spaced adds S1, S2, S3 two minutes apart ==="
for idx in 1 2 3; do
  t="S$idx"
  if [ "$idx" -gt 1 ]; then
    echo "Sleeping 120s before $t..."
    sleep 120
  fi
  echo "--- Spaced add $t at $(date -u +%FT%TZ) ---"
  GIT_REPO_WORKTREE_SESSION="R5-P2-spaced-$t" /usr/bin/time -v -o "$D/P2-spaced-add-$t.time" \
    git -C "$STUDIO" repo-worktree add --json -v "/usr/local/google/home/jomof/projects/studio-wt-r5-$t" \
    > "$D/P2-spaced-add-$t.json" 2> "$D/P2-spaced-add-$t.stderr"
  rg '^phase .*: [0-9]' "$D/P2-spaced-add-$t.stderr" | tr '\n' ' '; echo
done
wait_for_maintain "P2-after-spaced"

echo "=== P2.4 & P4: Remove A, S1, S2, S3; wait for maintenance; check pool & disk ==="
for t in A S1 S2 S3; do
  GIT_REPO_WORKTREE_SESSION="R5-P2-rm-$t" /usr/bin/time -v -o "$D/P2-rm-$t.time" \
    git -C "$STUDIO" repo-worktree remove --json "studio-wt-r5-$t" \
    > "$D/P2-rm-$t.json" 2> "$D/P2-rm-$t.stderr"
done
wait_for_maintain "P2-after-rm-all"

GIT_REPO_WORKTREE_SESSION=R5-P4 git -C "$STUDIO" repo-worktree pool list > "$D/P4-pool-list-after-P2.txt"
GIT_REPO_WORKTREE_SESSION=R5-P4 git -C "$STUDIO" repo-worktree pool list --json > "$D/P4-pool-list-after-P2.json"
du -sx "$STUDIO/.repo/repo-worktrees/.pool" > "$D/P4-du-pool-after-P2.txt"
(cd "$STUDIO/.repo/repo-worktrees/.pool" && du -sx *) > "$D/P4-du-pool-per-project-after-P2.txt"
df -B1 / > "$D/P4-df-after-P2.txt"

echo "--- P4 pool list & du -sx .pool after P2 ---"
cat "$D/P4-pool-list-after-P2.txt"
cat "$D/P4-du-pool-after-P2.txt"
cat "$D/P4-du-pool-per-project-after-P2.txt"
