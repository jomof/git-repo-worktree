#!/bin/bash
# Repeats remove -> warm add cycles on ~/projects/main, recording load per sample.
S=/usr/local/google/home/jomof/.gemini/jetski/brain/191b6333-bed5-4830-a2b9-34c6e004a9ab/scratch/main
T=/usr/local/google/home/jomof/projects/main-wt-report
cd /usr/local/google/home/jomof/projects/main || exit 1
load() { echo "$(cut -d' ' -f1-3 /proc/loadavg) io:$(head -1 /proc/pressure/io | cut -d' ' -f2-3)"; }
for i in ${CYCLES:-3 4}; do
  echo "remove$((i-1)) pre-load: $(load)"
  /usr/bin/time -f '%e s %P cpu' -o $S/remove$((i-1)).time git repo-worktree remove --json main-wt-report > $S/remove$((i-1)).json 2>&1
  echo "remove$((i-1)): $(cat $S/remove$((i-1)).time) returned=$(jq -c '.result.returned_to_pool | length' $S/remove$((i-1)).json)"
  echo "add$i pre-load: $(load)"
  /usr/bin/time -v -o $S/add$i.time git repo-worktree add --json -v $T > $S/add$i.json 2> $S/add$i.stderr
  echo "add$i exit=$? post-load: $(load)"
  rg '^phase (snapshot|projects|repo-bridge|total): [0-9]' $S/add$i.stderr | tr '\n' ' '; echo
  rg 'Elapsed|Percent' $S/add$i.time | tr '\n' ' '; echo
done
