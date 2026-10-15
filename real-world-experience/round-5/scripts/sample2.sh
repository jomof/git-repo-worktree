#!/bin/bash
# usage: sample2.sh <out-prefix> <tree-path> [add args...]
out=$1; tree=$2; shift 2
/usr/bin/time -v -o "$out.time" git-repo-worktree add -v "$@" "$tree" > /dev/null 2> "$out.stderr" &
root=$!
: > "$out.samples"
vmstat 1 > "$out.vmstat" & vm=$!
while kill -0 "$root" 2>/dev/null; do
  pids=$(ps -e -o pid=,ppid= | awk -v root="$root" '
    { parent[$1] = $2 }
    END {
      for (p in parent) {
        q = p
        while ((q in parent) && q != root && q > 1) q = parent[q]
        if (q == root) printf "%s,", p
      }
      print root
    }')
  {
    echo "t=$(date +%s.%N) $(tail -1 "$out.stderr" | cut -c1-80)"
    cut -d' ' -f1-4 /proc/loadavg
    for r in cpu io memory; do echo "$r $(head -1 /proc/pressure/$r)"; done
    grep -E '^(Dirty|Writeback):' /proc/meminfo | tr -s ' ' | tr '\n' ' '; echo
    ps -L -o stat=,wchan:32=,comm= -p "$pids" 2>/dev/null | sort | uniq -c | sort -rn | head -25
  } >> "$out.samples"
  sleep 1
done
wait "$root"
rc=$?
kill "$vm" 2>/dev/null || true
grep -E '^phase .*: [0-9]' "$out.stderr"
exit "$rc"
