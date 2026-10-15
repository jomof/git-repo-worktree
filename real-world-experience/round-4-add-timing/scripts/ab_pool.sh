#!/bin/bash
# A/B: does an empty pool restore the fast first-add timing on ~/projects/main?
S=/usr/local/google/home/jomof/.gemini/jetski/brain/191b6333-bed5-4830-a2b9-34c6e004a9ab/scratch/main
T=/usr/local/google/home/jomof/projects/main-wt-report
cd /usr/local/google/home/jomof/projects/main || exit 1
load() { echo "$(cut -d' ' -f1-3 /proc/loadavg)"; }
/usr/bin/time -f 'remove7 %e s' git repo-worktree remove --json main-wt-report > /dev/null 2> $S/remove7.err; tail -1 $S/remove7.err
/usr/bin/time -f 'pool clear %e s' git repo-worktree pool clear --json > $S/poolclear.json 2> $S/poolclear.err; tail -1 $S/poolclear.err
echo "spares after clear: $(git repo-worktree pool list --json | jq '[.result.spares[]?]|length')"
echo "add8 pre-load: $(load)"
/usr/bin/time -v -o $S/add8.time git repo-worktree add --json -v $T > $S/add8.json 2> $S/add8.stderr
echo "add8 exit=$? post-load: $(load)"
rg '^phase (snapshot|projects|repo-bridge|total): [0-9]' $S/add8.stderr | tr '\n' ' '; echo
rg 'Elapsed|Percent' $S/add8.time | tr '\n' ' '; echo
