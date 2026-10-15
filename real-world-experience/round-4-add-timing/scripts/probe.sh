#!/bin/bash
# Once per second: latency of a tiny git worktree add+remove vs. concurrent
# jetski-hub `git status --untracked-files=all` processes and run queue.
SRC=/usr/local/google/home/jomof/projects/main/tools/apksig
D=/usr/local/google/home/jomof/projects/rws-probe
for t in $(seq 1 ${SECS:-90}); do
  gs=$(pgrep -fc '[g]it --no-optional-locks status')
  gscpu=$(ps -eo pcpu=,args= | awk '/[g]it --no-optional-locks status/ {s+=$1} END {print s+0}')
  run=$(awk '{print $4}' /proc/loadavg | cut -d/ -f1)
  s=$(date +%s%N)
  git -C $SRC worktree add -q --detach $D HEAD >/dev/null 2>&1
  m=$(( ($(date +%s%N) - s) / 1000000 ))
  git -C $SRC worktree remove --force $D >/dev/null 2>&1
  echo "$t add_ms=$m gitstatus_procs=$gs gitstatus_cpu=$gscpu runnable=$run"
  sleep 1
done
