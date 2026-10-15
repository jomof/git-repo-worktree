#!/bin/bash
set -eu

R6=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-6
D="$R6/data/main"
SCRIPTS="$R6/scripts"
MAIN=/usr/local/google/home/jomof/projects/main
HIST="$MAIN/.repo/repo-worktrees/history.jsonl"

wait_idle_maintain() {
  sleep 1
  while pgrep -f "[g]it-repo-worktree.*pool maintain" >/dev/null 2>&1; do
    sleep 1
  done
}

wait_preconditions() {
  local label=$1
  for attempt in $(seq 1 90); do
    local dirty_kb=$(awk '/^Dirty:/ {print $2}' /proc/meminfo)
    local load1=$(awk '{print $1}' /proc/loadavg)
    local load_ok=$(awk -v l="$load1" 'BEGIN {print (l < 40.0) ? 1 : 0}')
    if [ "$dirty_kb" -lt 1048576 ] && [ "$load_ok" -eq 1 ]; then
      break
    fi
    sleep 1
  done
  local dirty_kb=$(awk '/^Dirty:/ {print $2}' /proc/meminfo)
  local writeback_kb=$(awk '/^Writeback:/ {print $2}' /proc/meminfo)
  local loadavg=$(cut -d' ' -f1-4 /proc/loadavg)
  echo "[$label] pre-run Dirty=${dirty_kb}kB Writeback=${writeback_kb}kB loadavg=${loadavg}" | tee "$D/${label}.precond"
}

cd "$MAIN"
wait_idle_maintain

WT="/usr/local/google/home/jomof/projects/main-wt-r6"
if [ -d "$WT" ]; then
  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 git -C "$MAIN" repo-worktree remove --no-pool --force --json "$WT" >/dev/null 2>&1 || true
fi

echo "=== Clearing AOSP pool before A1 (§3 A1) ==="
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-A1-init-clear" \
  git -C "$MAIN" repo-worktree pool clear --json > "$D/A1-init-pool-clear.json" 2> "$D/A1-init-pool-clear.stderr"

# A1 rotating order: 8, 16, 32, then 16, 32, 8, then 32, 8, 16
JOBS_ORDER=(8 16 32 16 32 8 32 8 16)

for idx in "${!JOBS_ORDER[@]}"; do
  run_num=$((idx + 1))
  j="${JOBS_ORDER[$idx]}"
  label="A1-r${run_num}-j${j}"
  echo "=== Running $label (-j $j) at $(date -u +%FT%TZ) ==="
  wait_preconditions "$label"

  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-${label}" \
    bash "$SCRIPTS/sample2.sh" "$D/${label}" "$WT" -j "$j"

  rg 'Elapsed|User time|System time|Percent of CPU' "$D/${label}.time" | tr '\n' ' '; echo

  # Remove with --no-pool
  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-${label}-rm-nopool" \
    /usr/bin/time -v -o "$D/${label}-rm-nopool.time" \
    git -C "$MAIN" repo-worktree remove --no-pool --json "$WT" \
    > "$D/${label}-rm-nopool.json" 2> "$D/${label}-rm-nopool.stderr"
done

echo "=== A1 and A2 completed successfully at $(date -u +%FT%TZ) ==="
