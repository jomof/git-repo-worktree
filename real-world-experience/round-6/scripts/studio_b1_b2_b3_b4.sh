#!/bin/bash
set -eu

R6=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-6
D="$R6/data/studio-main"
SCRIPTS="$R6/scripts"
STUDIO=/usr/local/google/home/jomof/projects/studio-main
HIST="$STUDIO/.repo/repo-worktrees/history.jsonl"

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
    if [ "$dirty_kb" -lt 1048576 ]; then
      break
    fi
    sleep 1
  done
  local dirty_kb=$(awk '/^Dirty:/ {print $2}' /proc/meminfo)
  local loadavg=$(cut -d' ' -f1-4 /proc/loadavg)
  echo "[$label] pre-run Dirty=${dirty_kb}kB loadavg=${loadavg}" | tee "$D/${label}.precond"
}

cd "$STUDIO"
wait_idle_maintain

# Clean up any leftover test trees
for t in studio-wt-r6-B1 studio-wt-r6-B studio-wt-r6-C studio-wt-r6-D studio-wt-r6-E studio-wt-r6-B3 studio-wt-r6-B4; do
  if [ -d "/usr/local/google/home/jomof/projects/$t" ]; then
    GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 git -C "$STUDIO" repo-worktree remove --no-pool --force --json "$t" >/dev/null 2>&1 || true
  fi
done

echo "=== B1: Warm add with estimate cache (5 runs) ==="
WT_B1="/usr/local/google/home/jomof/projects/studio-wt-r6-B1"

for i in $(seq 1 5); do
  label="B1-r${i}"
  echo "--- B1 run $i at $(date -u +%FT%TZ) ---"
  wait_preconditions "$label"

  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-${label}" \
    /usr/bin/time -v -o "$D/${label}.time" \
    git -C "$STUDIO" repo-worktree add -v "$WT_B1" \
    > "$D/${label}.stdout" 2> "$D/${label}.stderr"

  rg 'Elapsed|User time|System time|Percent of CPU' "$D/${label}.time" | tr '\n' ' '; echo
  grep -E '^phase .*: [0-9]' "$D/${label}.stderr"

  # Remove with default remove (returns spares to pool)
  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-${label}-rm" \
    /usr/bin/time -v -o "$D/${label}-rm.time" \
    git -C "$STUDIO" repo-worktree remove --json "$WT_B1" \
    > "$D/${label}-rm.json" 2> "$D/${label}-rm.stderr"
done

echo "=== Pool state after B1 ==="
git -C "$STUDIO" repo-worktree pool list > "$D/B1-pool-list-after.txt"
cat "$D/B1-pool-list-after.txt"

echo "=== B2: Burst with maintenance (B, C, D back-to-back, then E right after D) ==="
WT_B="/usr/local/google/home/jomof/projects/studio-wt-r6-B"
WT_C="/usr/local/google/home/jomof/projects/studio-wt-r6-C"
WT_D="/usr/local/google/home/jomof/projects/studio-wt-r6-D"
WT_E="/usr/local/google/home/jomof/projects/studio-wt-r6-E"

echo "--- B2: add B ---"
GIT_REPO_WORKTREE_SESSION="R6-B2-burst-B" \
  /usr/bin/time -v -o "$D/B2-burst-B.time" \
  git -C "$STUDIO" repo-worktree add -v "$WT_B" \
  > "$D/B2-burst-B.stdout" 2> "$D/B2-burst-B.stderr"

echo "--- B2: add C ---"
GIT_REPO_WORKTREE_SESSION="R6-B2-burst-C" \
  /usr/bin/time -v -o "$D/B2-burst-C.time" \
  git -C "$STUDIO" repo-worktree add -v "$WT_C" \
  > "$D/B2-burst-C.stdout" 2> "$D/B2-burst-C.stderr"

echo "--- B2: add D ---"
GIT_REPO_WORKTREE_SESSION="R6-B2-burst-D" \
  /usr/bin/time -v -o "$D/B2-burst-D.time" \
  git -C "$STUDIO" repo-worktree add -v "$WT_D" \
  > "$D/B2-burst-D.stdout" 2> "$D/B2-burst-D.stderr"

echo "--- B2: add E (immediately after D) ---"
GIT_REPO_WORKTREE_SESSION="R6-B2-burst-E" \
  /usr/bin/time -v -o "$D/B2-burst-E.time" \
  git -C "$STUDIO" repo-worktree add -v "$WT_E" \
  > "$D/B2-burst-E.stdout" 2> "$D/B2-burst-E.stderr"

echo "B, C, D, E added. Waiting for background pool maintain to finish..."
wait_idle_maintain

echo "Extracting B2 maintenance events..."
jq -c 'select(.command=="pool maintain")' "$HIST" | tail -4 > "$D/B2-maintain-events.json"
cat "$D/B2-maintain-events.json"

echo "Cleaning up B2 trees (B, C, D, E)..."
for wt in "$WT_B" "$WT_C" "$WT_D" "$WT_E"; do
  GIT_REPO_WORKTREE_SESSION="R6-B2-rm" git -C "$STUDIO" repo-worktree remove --force --json "$wt" >/dev/null 2>&1 || true
done
wait_idle_maintain

echo "=== Pool state after B2 cleanup (should be depth 2) ==="
git -C "$STUDIO" repo-worktree pool list > "$D/B2-pool-list-after.txt"
cat "$D/B2-pool-list-after.txt"

echo "=== B3: remove on a full pool (repeat 3 times) ==="
WT_B3="/usr/local/google/home/jomof/projects/studio-wt-r6-B3"

for i in $(seq 1 3); do
  label="B3-r${i}"
  echo "--- B3 run $i: add with maintenance OFF ---"
  GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-${label}-add" \
    /usr/bin/time -v -o "$D/${label}-add.time" \
    git -C "$STUDIO" repo-worktree add -v "$WT_B3" \
    > "$D/${label}-add.stdout" 2> "$D/${label}-add.stderr"

  echo "--- B3 run $i: remove with maintenance ON (pool depth returns to 2) ---"
  GIT_REPO_WORKTREE_SESSION="R6-${label}-rm" \
    /usr/bin/time -v -o "$D/${label}-rm.time" \
    git -C "$STUDIO" repo-worktree remove --json "$WT_B3" \
    > "$D/${label}-rm.json" 2> "$D/${label}-rm.stderr"

  wait_idle_maintain
done

echo "=== B4: Bazel build regression check (once maint ON, once maint OFF) ==="
WT_B4="/usr/local/google/home/jomof/projects/studio-wt-r6-B4"

echo "--- B4.1: add with maintenance ON ---"
GIT_REPO_WORKTREE_SESSION="R6-B4-maint-add" \
  /usr/bin/time -v -o "$D/B4-maint-add.time" \
  git -C "$STUDIO" repo-worktree add -v "$WT_B4" \
  > "$D/B4-maint-add.stdout" 2> "$D/B4-maint-add.stderr"

echo "--- B4.1: bazel build //tools/base/common:tools.common ---"
(
  cd "$WT_B4"
  /usr/bin/time -v -o "$D/B4-maint-bazel.time" \
    bazel build //tools/base/common:tools.common \
    > "$D/B4-maint-bazel.stdout" 2> "$D/B4-maint-bazel.stderr"
)
wait_idle_maintain

echo "--- B4.1: remove B4 tree ---"
GIT_REPO_WORKTREE_SESSION="R6-B4-maint-rm" \
  git -C "$STUDIO" repo-worktree remove --force --json "$WT_B4" >/dev/null 2>&1 || true
wait_idle_maintain

echo "--- B4.2: add with maintenance OFF ---"
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-B4-nomaint-add" \
  /usr/bin/time -v -o "$D/B4-nomaint-add.time" \
  git -C "$STUDIO" repo-worktree add -v "$WT_B4" \
  > "$D/B4-nomaint-add.stdout" 2> "$D/B4-nomaint-add.stderr"

echo "--- B4.2: bazel build //tools/base/common:tools.common ---"
(
  cd "$WT_B4"
  /usr/bin/time -v -o "$D/B4-nomaint-bazel.time" \
    bazel build //tools/base/common:tools.common \
    > "$D/B4-nomaint-bazel.stdout" 2> "$D/B4-nomaint-bazel.stderr"
)

echo "--- B4.2: remove B4 tree ---"
GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R6-B4-nomaint-rm" \
  git -C "$STUDIO" repo-worktree remove --force --json "$WT_B4" >/dev/null 2>&1 || true
wait_idle_maintain

echo "=== Final pool list on studio-main ==="
git -C "$STUDIO" repo-worktree pool list > "$D/final-pool-list.txt"
git -C "$STUDIO" repo-worktree pool list --json > "$D/final-pool-list.json"
cat "$D/final-pool-list.txt"

echo "=== Section 4 completed at $(date -u +%FT%TZ) ==="
