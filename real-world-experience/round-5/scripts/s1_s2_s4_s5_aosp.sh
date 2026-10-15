#!/bin/bash
set -eu

R5=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-5
D="$R5/data/main"
SCRIPTS="$R5/scripts"
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

for t in main-wt-r5 main-wt-r5-S4a main-wt-r5-S4b main-wt-r5-S4c; do
  if [ -d "/usr/local/google/home/jomof/projects/$t" ]; then
    GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 git -C "$MAIN" repo-worktree remove --no-pool --force --json "$t" >/dev/null 2>&1 || true
  fi
done

echo "=== Clearing AOSP pool before S1/S2 ==="
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-S2-init-clear" \
  git -C "$MAIN" repo-worktree pool clear --json > "$D/S2-init-pool-clear.json" 2> "$D/S2-init-pool-clear.stderr"

# S2 rotating order: 8, 16, 32, then 16, 32, 8, then 32, 8, 16
JOBS_ORDER=(8 16 32 16 32 8 32 8 16)
WT="/usr/local/google/home/jomof/projects/main-wt-r5"

for idx in "${!JOBS_ORDER[@]}"; do
  run_num=$((idx + 1))
  j="${JOBS_ORDER[$idx]}"
  label="S2-r${run_num}-j${j}"
  echo "=== Running $label (-j $j) at $(date -u +%FT%TZ) ==="
  wait_preconditions "$label"

  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-${label}" \
    bash "$SCRIPTS/sample2.sh" "$D/${label}" "$WT" -j "$j"

  rg 'Elapsed|User time|System time|Percent of CPU' "$D/${label}.time" | tr '\n' ' '; echo

  # Remove with --no-pool (first 2 also serve as S5 --no-pool timings, and we record all 9)
  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-${label}-rm-nopool" \
    /usr/bin/time -v -o "$D/${label}-rm-nopool.time" \
    git -C "$MAIN" repo-worktree remove --no-pool --json main-wt-r5 \
    > "$D/${label}-rm-nopool.json" 2> "$D/${label}-rm-nopool.stderr"
done

echo "=== S4: Pool on AOSP with maintenance ==="
wait_preconditions "S4-add1"
WT_A="/usr/local/google/home/jomof/projects/main-wt-r5-S4a"
WT_B="/usr/local/google/home/jomof/projects/main-wt-r5-S4b"
WT_C="/usr/local/google/home/jomof/projects/main-wt-r5-S4c"

echo "--- S4.1: First add with maintenance ON (spawns background pool maintain) ---"
GIT_REPO_WORKTREE_SESSION="R5-S4-add1" \
  bash "$SCRIPTS/sample2.sh" "$D/S4-add1" "$WT_A" -j 32

sleep 2
maint_active=0
if pgrep -f "[g]it-repo-worktree.*pool maintain" >/dev/null 2>&1; then
  maint_active=1
fi
echo "[S4] maintain_active_before_add2=$maint_active" | tee "$D/S4-maintain-active.txt"

echo "--- S4.2: Second add started WHILE pool maintain is running ---"
GIT_REPO_WORKTREE_SESSION="R5-S4-add2-during-maintain" \
  bash "$SCRIPTS/sample2.sh" "$D/S4-add2" "$WT_B" -j 32

echo "--- Waiting for background pool maintain to finish ---"
start_w=$(date +%s)
wait_idle_maintain
end_w=$(date +%s)
echo "[S4] waited $((end_w - start_w))s after S4-add2 for pool maintain to finish" | tee -a "$D/S4-maintain-active.txt"
jq -c 'select(.command=="pool maintain")' "$HIST" | tail -2 > "$D/S4-maintain-events.json"
cat "$D/S4-maintain-events.json"

GIT_REPO_WORKTREE_SESSION="R5-S4-pool-list" git -C "$MAIN" repo-worktree pool list > "$D/S4-pool-list-after-maintain.txt"
GIT_REPO_WORKTREE_SESSION="R5-S4-pool-list" git -C "$MAIN" repo-worktree pool list --json > "$D/S4-pool-list-after-maintain.json"
cat "$D/S4-pool-list-after-maintain.txt"

echo "--- S4.3: Third add after pool maintain finished (warm pool depth 2) ---"
wait_preconditions "S4-add3-warm"
GIT_REPO_WORKTREE_SESSION="R5-S4-add3-warm" \
  bash "$SCRIPTS/sample2.sh" "$D/S4-add3-warm" "$WT_C" -j 32

echo "=== S5: Timed removes (2 default, 2 --no-pool) ==="
# Remove S4b and S4c with default (returns to pool / trims via maintenance)
# First clear pool so default remove1 and remove2 both return all 14 projects!
wait_idle_maintain
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-S5-prep" \
  git -C "$MAIN" repo-worktree pool clear --json >/dev/null 2>&1

echo "--- S5 default remove 1 (main-wt-r5-S4a) ---"
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-S5-rm-default-1" \
  /usr/bin/time -v -o "$D/S5-rm-default-1.time" \
  git -C "$MAIN" repo-worktree remove --json main-wt-r5-S4a \
  > "$D/S5-rm-default-1.json" 2> "$D/S5-rm-default-1.stderr"

echo "--- S5 default remove 2 (main-wt-r5-S4b) ---"
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-S5-rm-default-2" \
  /usr/bin/time -v -o "$D/S5-rm-default-2.time" \
  git -C "$MAIN" repo-worktree remove --json main-wt-r5-S4b \
  > "$D/S5-rm-default-2.json" 2> "$D/S5-rm-default-2.stderr"

echo "--- S5 --no-pool remove 1 (main-wt-r5-S4c) ---"
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-S5-rm-nopool-1" \
  /usr/bin/time -v -o "$D/S5-rm-nopool-1.time" \
  git -C "$MAIN" repo-worktree remove --no-pool --json main-wt-r5-S4c \
  > "$D/S5-rm-nopool-1.json" 2> "$D/S5-rm-nopool-1.stderr"

echo "--- Cleaning AOSP pool at end of S5 ---"
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-S5-final-clear" \
  git -C "$MAIN" repo-worktree pool clear --json > "$D/S5-final-pool-clear.json" 2> "$D/S5-final-pool-clear.stderr"
