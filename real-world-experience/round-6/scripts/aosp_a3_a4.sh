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

WT_A="/usr/local/google/home/jomof/projects/main-wt-r6-S4a"
WT_B="/usr/local/google/home/jomof/projects/main-wt-r6-S4b"
WT_C="/usr/local/google/home/jomof/projects/main-wt-r6-S4c"

for wt in "$WT_A" "$WT_B" "$WT_C"; do
  if [ -d "$wt" ]; then
    GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 git -C "$MAIN" repo-worktree remove --no-pool --force --json "$wt" >/dev/null 2>&1 || true
  fi
done

echo "=== A3.1: Clearing pool, then starting cold add S4a with maintenance ON ==="
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-A3-init-clear" \
  git -C "$MAIN" repo-worktree pool clear --json > "$D/A3-init-pool-clear.json" 2> "$D/A3-init-pool-clear.stderr"

wait_preconditions "A3-S4a"
GIT_REPO_WORKTREE_SESSION="R6-A3-S4a" \
  bash "$SCRIPTS/sample2.sh" "$D/A3-S4a" "$WT_A" -j 32

echo "S4a finished at $(date -u +%FT%TZ). Waiting 2s before starting S4b..."
sleep 2

maint_pid=$(pgrep -f "[g]it-repo-worktree.*pool maintain" || true)
echo "pool maintain pid after S4a: '$maint_pid'" | tee "$D/A3-S4-maintain-active.txt"

echo "=== A3.2: Starting cold add S4b WHILE pool maintain is running ==="
# Launch background sampler for refill git processes
: > "$D/A3-S4b-refill-git.samples"
(
  while true; do
    ts=$(date +%s.%N)
    matches=$(ps -o pid=,stat=,wchan:32=,comm=,args= -C git 2>/dev/null | grep '\.filling-' || true)
    if [ -n "$matches" ]; then
      echo "--- t=$ts ---" >> "$D/A3-S4b-refill-git.samples"
      echo "$matches" >> "$D/A3-S4b-refill-git.samples"
    fi
    sleep 1
  done
) &
refill_sampler_pid=$!

GIT_REPO_WORKTREE_SESSION="R6-A3-S4b-during-maintain" \
  bash "$SCRIPTS/sample2.sh" "$D/A3-S4b" "$WT_B" -j 32

echo "S4b finished at $(date -u +%FT%TZ). Sampling refill for 5 more seconds..."
sleep 5
kill "$refill_sampler_pid" 2>/dev/null || true

echo "=== Waiting for background pool maintain to complete ==="
start_w=$(date +%s)
wait_idle_maintain
end_w=$(date +%s)
echo "Waited $((end_w - start_w))s after S4b for pool maintain to finish" | tee -a "$D/A3-S4-maintain-active.txt"

# Extract pool maintain event
jq -c 'select(.command=="pool maintain")' "$HIST" | tail -2 > "$D/A3-S4-maintain-events.json"
cat "$D/A3-S4-maintain-events.json"

GIT_REPO_WORKTREE_SESSION="R6-A3-pool-list" git -C "$MAIN" repo-worktree pool list > "$D/A3-S4-pool-list-after-maintain.txt"
GIT_REPO_WORKTREE_SESSION="R6-A3-pool-list" git -C "$MAIN" repo-worktree pool list --json > "$D/A3-S4-pool-list-after-maintain.json"
cat "$D/A3-S4-pool-list-after-maintain.txt"

echo "=== A3.3: Warm add S4c after pool maintain finished ==="
wait_preconditions "A3-S4c-warm"
GIT_REPO_WORKTREE_SESSION="R6-A3-S4c-warm" \
  bash "$SCRIPTS/sample2.sh" "$D/A3-S4c-warm" "$WT_C" -j 32

echo "Waiting for S4c background refill to finish before A4 removes..."
wait_idle_maintain

echo "=== A4: Removes (S4a default, S4b default, S4c --no-pool) ==="
echo "--- A4.1 Remove S4a (default remove on full pool) ---"
GIT_REPO_WORKTREE_SESSION="R6-A4-rm-S4a-default" \
  /usr/bin/time -v -o "$D/A4-rm-S4a-default.time" \
  git -C "$MAIN" repo-worktree remove --json "$WT_A" \
  > "$D/A4-rm-S4a-default.json" 2> "$D/A4-rm-S4a-default.stderr"

echo "--- A4.2 Remove S4b (default remove on full pool) ---"
GIT_REPO_WORKTREE_SESSION="R6-A4-rm-S4b-default" \
  /usr/bin/time -v -o "$D/A4-rm-S4b-default.time" \
  git -C "$MAIN" repo-worktree remove --json "$WT_B" \
  > "$D/A4-rm-S4b-default.json" 2> "$D/A4-rm-S4b-default.stderr"

echo "--- A4.3 Remove S4c (--no-pool) ---"
GIT_REPO_WORKTREE_SESSION="R6-A4-rm-S4c-nopool" \
  /usr/bin/time -v -o "$D/A4-rm-S4c-nopool.time" \
  git -C "$MAIN" repo-worktree remove --no-pool --json "$WT_C" \
  > "$D/A4-rm-S4c-nopool.json" 2> "$D/A4-rm-S4c-nopool.stderr"

echo "--- A4.4 Final AOSP pool clear ---"
wait_idle_maintain
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-A4-final-clear" \
  git -C "$MAIN" repo-worktree pool clear --json > "$D/A4-final-pool-clear.json" 2> "$D/A4-final-pool-clear.stderr"

GIT_REPO_WORKTREE_SESSION="R6-A4-final-pool-list" git -C "$MAIN" repo-worktree pool list > "$D/final-pool-list.txt"
GIT_REPO_WORKTREE_SESSION="R6-A4-final-pool-list" git -C "$MAIN" repo-worktree pool list --json > "$D/final-pool-list.json"
cat "$D/final-pool-list.txt"

echo "=== A3 and A4 completed at $(date -u +%FT%TZ) ==="
