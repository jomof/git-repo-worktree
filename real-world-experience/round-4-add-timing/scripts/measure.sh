#!/bin/bash
# studio-main: add A, B (expected pool hits), C (expected miss); measure; remove all.
S=/usr/local/google/home/jomof/.gemini/jetski/brain/191b6333-bed5-4830-a2b9-34c6e004a9ab/scratch/studio
mkdir -p $S
cd /usr/local/google/home/jomof/projects/studio-main || exit 1
load() { echo "$(cut -d' ' -f1-3 /proc/loadavg)"; }
df -B1 --output=used / | tail -1 > $S/df0
echo "spares before: $(git repo-worktree pool list --json | jq '[.result.spares[]?]|length')"
for n in A B C; do
  T=/usr/local/google/home/jomof/projects/studio-wt-report-$n
  echo "add$n pre-load: $(load)"
  /usr/bin/time -v -o $S/add$n.time git repo-worktree add --json -v $T > $S/add$n.json 2> $S/add$n.stderr
  echo "add$n exit=$? $(rg '^phase (snapshot|projects|hook-post-add|total): [0-9]' $S/add$n.stderr | tr '\n' ' ')"
  rg 'Elapsed|Percent' $S/add$n.time | tr '\n' ' '; echo
  df -B1 --output=used / | tail -1 > $S/df-$n
  echo "disk delta vs start: $(( ($(cat $S/df-$n) - $(cat $S/df0)) / 1000000 )) MB"
done
T=/usr/local/google/home/jomof/projects/studio-wt-report-A
/usr/bin/time -f 'du -x %e s' du -sh -x $T 2>&1 | tr '\n' ' '; echo
du -sh $T 2>/dev/null | tail -1
echo "overlay mounts in A: $(findmnt -rn -t fuse.fuse-overlayfs -o TARGET | rg -c studio-wt-report-A)"
ls -la $T/env.bazelrc
/usr/bin/time -f 'rws status %e s' git repo-worktree status --json studio-wt-report-A 2>&1 >/dev/null | tail -1
/usr/bin/time -f 'repo status(tree) %e s' repo -C $T status -j32 2>&1 | tail -1 || (cd $T && /usr/bin/time -f 'repo status(tree) %e s' repo status -j32 2>&1 | tail -1)
(cd /usr/local/google/home/jomof/projects/studio-main && /usr/bin/time -f 'repo status(source) %e s' repo status -j32 2>&1 | tail -1)
for n in C B A; do
  /usr/bin/time -f "remove$n %e s" git repo-worktree remove --json studio-wt-report-$n > $S/remove$n.json 2> $S/remove$n.err
  tail -1 $S/remove$n.err; jq -c '.result | {returned: (.returned_to_pool|length), lingering: (.lingering_processes|length), hooks: (.hooks|keys)}' $S/remove$n.json
done
echo "disk delta after removes: $(( ($(df -B1 --output=used / | tail -1) - $(cat $S/df0)) / 1000000 )) MB"
echo "spares after: $(git repo-worktree pool list --json | jq '[.result.spares[]?]|length')"
