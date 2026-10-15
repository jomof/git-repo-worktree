#!/bin/bash
set -eu

R6=/usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-6
D_STUDIO="$R6/data/studio-main"
D_MAIN="$R6/data/main"
STUDIO=/usr/local/google/home/jomof/projects/studio-main
MAIN=/usr/local/google/home/jomof/projects/main

snap_studio() {
  local out=$1
  while IFS= read -r p; do
    [ -z "$p" ] && continue
    head_sha=$(git -C "$STUDIO/$p" rev-parse HEAD 2>/dev/null || echo "MISSING")
    dirty=$(git -C "$STUDIO/$p" status --porcelain=v1 -uno 2>/dev/null | wc -l)
    echo "$p $head_sha dirty_tracked=$dirty"
  done < "$STUDIO/.repo/project.list" > "$out"
}

snap_main() {
  local out=$1
  while IFS= read -r p; do
    [ -z "$p" ] && continue
    head_sha=$(git -C "$MAIN/$p" rev-parse HEAD 2>/dev/null || echo "MISSING")
    echo "$p $head_sha"
  done < "$MAIN/.repo/project.list" > "$out"
  for kp in frameworks/base cts build/make build/soong bionic art system/core; do
    dirty=$(git -C "$MAIN/$kp" status --porcelain=v1 2>/dev/null | wc -l)
    echo "# key_status $kp dirty=$dirty" >> "$out"
  done
}

echo "=== 1. Capturing post-test source integrity snapshots ==="
snap_studio "$D_STUDIO/snap-after.txt"
diff -u "$D_STUDIO/snap-before.txt" "$D_STUDIO/snap-after.txt" > "$D_STUDIO/snap-diff.txt" || true
if [ -s "$D_STUDIO/snap-diff.txt" ]; then
  echo "WARNING: studio-main source checkout changed!"
  cat "$D_STUDIO/snap-diff.txt"
else
  echo "studio-main snap-diff: CLEAN (0 diffs)"
fi

snap_main "$D_MAIN/snap-after.txt"
diff -u "$D_MAIN/snap-before.txt" "$D_MAIN/snap-after.txt" > "$D_MAIN/snap-diff.txt" || true
if [ -s "$D_MAIN/snap-diff.txt" ]; then
  echo "WARNING: main source checkout changed!"
  cat "$D_MAIN/snap-diff.txt"
else
  echo "main snap-diff: CLEAN (0 diffs)"
fi

echo "=== 2. Capturing final tree lists and reports ==="
git -C "$STUDIO" repo-worktree list --json > "$D_STUDIO/final-list.json"
git -C "$STUDIO" repo-worktree report > "$D_STUDIO/report.txt"

git -C "$MAIN" repo-worktree list --json > "$D_MAIN/final-list.json"
git -C "$MAIN" repo-worktree report > "$D_MAIN/report.txt"

echo "=== 3. Exporting history ==="
cp "$STUDIO/.repo/repo-worktrees/history.jsonl" "$D_STUDIO/history-full.jsonl"
jq -c 'select((.session // "" | startswith("R6-")) or (.command=="pool maintain" and .ts >= "2026-10-15T03:57:00Z"))' \
  "$D_STUDIO/history-full.jsonl" > "$D_STUDIO/history-r6.jsonl"

cp "$MAIN/.repo/repo-worktrees/history.jsonl" "$D_MAIN/history-full.jsonl"
jq -c 'select((.session // "" | startswith("R6-")) or (.command=="pool maintain" and .ts >= "2026-10-15T03:57:00Z"))' \
  "$D_MAIN/history-full.jsonl" > "$D_MAIN/history-r6.jsonl"

echo "Done collecting final state and history."
