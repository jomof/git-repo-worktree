#!/bin/bash
set -eu

R5=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-5
D="$R5/data/studio-main"
STUDIO=/usr/local/google/home/jomof/projects/studio-main

wait_idle_maintain() {
  sleep 1
  while pgrep -f "[g]it-repo-worktree.*pool maintain" >/dev/null 2>&1; do
    sleep 1
  done
}

run_p3_case() {
  local mode=$1
  local rep=$2
  local label="P3-${mode}-${rep}"
  local wt="/usr/local/google/home/jomof/projects/studio-wt-r5-${label}"

  wait_idle_maintain
  if [ -d "$wt" ]; then
    GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 git -C "$STUDIO" repo-worktree remove --json --force "studio-wt-r5-${label}" >/dev/null 2>&1 || true
  fi

  echo "=== Running $label (pool depth before: $(git -C "$STUDIO" repo-worktree pool list --json | jq '.result.depth')) ==="
  vmstat 5 > "$D/${label}.vmstat" &
  local vm_pid=$!
  iostat -x 5 > "$D/${label}.iostat" 2>/dev/null &
  local io_pid=$!

  if [ "$mode" = "maint" ]; then
    GIT_REPO_WORKTREE_SESSION="R5-${label}" /usr/bin/time -v -o "$D/${label}-add.time" \
      git -C "$STUDIO" repo-worktree add --json -v "$wt" \
      > "$D/${label}-add.json" 2> "$D/${label}-add.stderr"
  else
    GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1 GIT_REPO_WORKTREE_SESSION="R5-${label}" /usr/bin/time -v -o "$D/${label}-add.time" \
      git -C "$STUDIO" repo-worktree add --json -v "$wt" \
      > "$D/${label}-add.json" 2> "$D/${label}-add.stderr"
  fi

  local maintain_running_at_build_start=0
  if pgrep -f "[g]it-repo-worktree.*pool maintain" >/dev/null 2>&1; then
    maintain_running_at_build_start=1
  fi
  echo "[$label] maintain_running_at_build_start=$maintain_running_at_build_start"

  (
    cd "$wt"
    /usr/bin/time -v -o "$D/${label}-bazel.time" \
      bazel build //tools/base/common:tools.common \
      > "$D/${label}-bazel.stdout" 2> "$D/${label}-bazel.stderr"
  )

  local maintain_running_at_build_end=0
  if pgrep -f "[g]it-repo-worktree.*pool maintain" >/dev/null 2>&1; then
    maintain_running_at_build_end=1
  fi
  echo "[$label] maintain_running_at_build_end=$maintain_running_at_build_end"
  rg 'Elapsed|User time|System time|Percent of CPU' "$D/${label}-bazel.time" | tr '\n' ' '; echo

  kill "$vm_pid" "$io_pid" 2>/dev/null || true
  wait_idle_maintain

  GIT_REPO_WORKTREE_SESSION="R5-${label}-rm" git -C "$STUDIO" repo-worktree remove --json --force "studio-wt-r5-${label}" \
    > "$D/${label}-rm.json" 2> "$D/${label}-rm.stderr"
  wait_idle_maintain
}

run_p3_case maint 1
run_p3_case nomaint 1
run_p3_case maint 2
run_p3_case nomaint 2
