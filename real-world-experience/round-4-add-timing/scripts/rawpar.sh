#!/bin/bash
# Raw-git parallel reproduction: worktree add+checkout of the N smallest
# worktree-mode projects at the tool's parallelism, outside the tool.
S=/usr/local/google/home/jomof/.gemini/jetski/brain/191b6333-bed5-4830-a2b9-34c6e004a9ab/scratch/main
SRC=/usr/local/google/home/jomof/projects/main
DST=/usr/local/google/home/jomof/projects/rws-rawpar
N=${N:-300}
P=${P:-96}
jq -r 'select(.tree=="main-wt-report" and .command=="add") | .projects[] | select(.strategy=="checkout") | "\(.bytes) \(.path)"' \
  $SRC/.repo/repo-worktrees/history.jsonl | tail -957 | sort -n | head -$N | cut -d' ' -f2 > $S/rawpar.list
mkdir -p $DST
one() {
  local p=$1 s e
  s=$(date +%s%N)
  git $EXTRA -c checkout.workers=8 -c checkout.thresholdForParallelism=100 -C "$SRC/$p" \
    worktree add -q --detach "$DST/$p" HEAD >/dev/null 2>&1
  e=$(date +%s%N)
  echo "$(( (e - s) / 1000000 )) $p"
}
export -f one; export SRC DST EXTRA
start=$(date +%s%N)
xargs -P $P -I{} bash -c 'one {}' < $S/rawpar.list > $S/rawpar.ms
echo "wall=$(( ($(date +%s%N) - start) / 1000000 ))ms n=$(wc -l < $S/rawpar.ms) P=$P load=$(cut -d' ' -f1 /proc/loadavg)"
sort -n $S/rawpar.ms | awk '{a[NR]=$1} END {print "p50="a[int(NR/2)]"ms p90="a[int(NR*0.9)]"ms max="a[NR]"ms"}'
# cleanup
while read -r p; do git -C "$SRC/$p" worktree remove --force "$DST/$p" 2>/dev/null; done < $S/rawpar.list
rm -rf "$DST"
echo "cleanup done"
