#!/bin/bash
# Samples git-repo-worktree child processes once per second during an add.
S=/usr/local/google/home/jomof/.gemini/jetski/brain/191b6333-bed5-4830-a2b9-34c6e004a9ab/scratch/main
cd /usr/local/google/home/jomof/projects/main || exit 1
git repo-worktree remove --json main-wt-report > $S/remove4.json 2>&1
git repo-worktree add -v /usr/local/google/home/jomof/projects/main-wt-report > /dev/null 2> $S/add5.stderr &
ADD=$!
: > $S/add5.samples
for t in $(seq 1 240); do
  kill -0 $ADD 2>/dev/null || break
  echo "t=$t $(tail -1 $S/add5.stderr | cut -c1-80)" >> $S/add5.samples
  for c in $(pgrep -P $(pgrep -P $ADD | head -1) 2>/dev/null) $(pgrep -P $ADD); do
    ps -o stat=,etimes=,args= -p $c 2>/dev/null | cut -c1-160
    cat /proc/$c/wchan 2>/dev/null; echo
  done >> $S/add5.samples
  cat /proc/$(pgrep -P $ADD | head -1 || echo $ADD)/stack 2>/dev/null | head -3 >> $S/add5.samples
  sleep 1
done
wait $ADD
rg '^phase .*: [0-9]' $S/add5.stderr
