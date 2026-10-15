# Round 5: Pool Maintenance and AOSP Stall Attribution

**Audience:** the orchestrating agent on the Linux workstation with
`~/projects/studio-main` and `~/projects/main` (AOSP).
**Build under test:** `main` at or after `a9c1499`
(`git repo-worktree version` prints the commit). Install it the way round 4
did, and check that no older copy shadows it (`which -a git-repo-worktree`).
**Method:** orchestrator measurements, not blindfold scenarios. Reuse round 4's
scripts in [`round-4-add-timing/scripts/`](./round-4-add-timing/scripts/) where
they fit; new scripts go in `round-5/scripts/`.
**Deliverable:** `real-world-experience/round-5/` (§6).

## 1. What changed since round 4

Fixes for [`round-4-add-timing/report.md`](./round-4-add-timing/report.md):

* **Orphaned spares.** A project keeps spares only if a default `add` would
  take them: it has a `pool` default, or it has no default, takes at least
  1.5 s to check out by estimate, and is not a fixed-depth project with a
  clean source (those are overlaid). `pool list` now shows spares per
  project, each orphaned project with its reason and estimated size, and the
  pool's **depth** (how many back-to-back adds take every pooled project from
  the pool). `pool clear --orphaned` deletes orphaned spares. `remove` no
  longer returns orphaned projects, and `pool fill` no longer tops them up.
* **Background refill.** After `add` and after `remove` (without `--no-pool`),
  a detached `pool maintain` runs under `nice -n 19` and `ionice -c 3`. It
  deletes orphaned spares, trims pools above `pool size`, and refills every
  pooled project to `pool size` (default 2), one spare per project at a time.
  It never fills the last 10% (at most 50 GB) of the filesystem. It records a
  `pool maintain` event in `history.jsonl` when it finishes.
  `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` turns it off.
* **Scheduler.** `add` now starts independent projects before it fsyncs each
  completion batch, and rewrites `meta.json` at most once a second instead of
  after every batch.
* **Instrumentation in `history.jsonl`:**
  * `add` phases `snapshot-repo-manifest`, `snapshot-git-dirs`,
    `snapshot-estimates`, `snapshot-preflight` (the parts of `snapshot`), and
    `projects-journal-sync` and `projects-meta-write` (time the scheduler
    spent syncing);
  * per-project `queue_ms` (wait for a worker after becoming ready);
  * `system_start` and `system_end` on `add`, `remove`, and `pool maintain`
    (load average and `/proc/pressure/{cpu,io,memory}` averages);
  * one `reflink` field instead of 971 identical per-project notes;
  * on `remove`, `pruned_git_dirs` (now only overlay projects and failed
    removals are pruned).

## 2. Setup

1. Record the starting state for both clients: `git repo-worktree list --json`,
   `pool list --json`, `df -B1 /`, `du -sx .repo/repo-worktrees/.pool`.
2. Tag every command with `GIT_REPO_WORKTREE_SESSION=R5-<step>`.
3. Keep the round 4 source-integrity check (`snap()` before and after) for
   both clients.

## 3. studio-main: pool

### P1. Orphan report and cleanup

1. `git repo-worktree pool list` and `pool list --json`. Compare the orphaned
   projects and reasons with round 4's dead-spare table (expect the nine
   overlay or quick projects: `prebuilts/tools`, `intellij-sdk`, `jdk`,
   `tools/external/gradle`, `sdk/remote`, `buildbenchmarks`, `clang`,
   `remoteexecution-client`, `external/dexter`). Note any project you think is
   misclassified, and why.
2. Time `pool clear --orphaned --json` in the foreground. Record reclaimed disk
   (`df` and `du -sx .pool` before and after) and the time it took.
3. `pool list` again: record depth and spares per pooled project.

Do P1 before anything else: otherwise the first `add` starts background
maintenance that deletes the same 132 GB at idle priority while you measure.

### P2. Background refill and bursts

1. With maintenance on (default), `add` tree A and wait for its
   `pool maintain` event in `history.jsonl` (`jq 'select(.command=="pool maintain")'`).
   Record how long maintenance ran, what it filled, and `pool list` depth
   afterwards (expect 2).
2. **Burst:** add B, C, D back-to-back without waiting. Expect B and C to
   take spares and D to miss. Record each `add` total, pool hits, and
   `queue_ms` of the slowest projects.
3. **Spaced:** remove B, C, D; wait for maintenance to finish; then add three
   trees two minutes apart. Expect three pool hits.
4. Remove everything; confirm maintenance trims the pool back to `pool size`
   and that `pool list` reports no orphans.

### P3. Cost of background refill to the user

1. Add a tree and immediately run the round 3 Bazel build in it
   (`//tools/base/common:tools.common`) while maintenance refills. Record the
   build time.
2. Repeat with `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` on the `add`.
3. Repeat each twice. Did idle-priority refill slow the build noticeably?
   Record `iostat -x 5` or `vmstat 5` alongside.

### P4. Disk

Record `du -sx .pool` after P2 (expect about two spare sets of the nine
pooled projects, roughly 2 × 22 GB) and the disk delta of one pool-hit `add`
against one miss.

## 4. AOSP (`~/projects/main`): stalls

Round 4 could not attribute the 155–243 s adds. This round measures where
the time goes. Use `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` for S1–S3 so
background refills do not mix into the measurements, and `pool clear` first
so every run is a plain checkout.

### S1. Sampler v2

Sample every **thread** of the tool and all of its descendants once a second,
including git's `checkout--worker` grandchildren, which round 4 missed:

```bash
#!/bin/bash
# usage: sample2.sh <out-prefix> <tree-path> [add args...]
out=$1; tree=$2; shift 2
git-repo-worktree add -v "$@" "$tree" > /dev/null 2> "$out.stderr" &
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
    ps -L -o stat=,wchan:32=,comm= -p "$pids" | sort | uniq -c | sort -rn | head -25
  } >> "$out.samples"
  sleep 1
done
kill "$vm"
grep -E '^phase .*: [0-9]' "$out.stderr"
```

It runs `git-repo-worktree` directly rather than through `git`, so the tool is
the root of the sampled process tree.

The `uniq -c` histogram of (state, wait channel, command) per second is the
key output. Names to look for:

* `balance_dirty_pages`: writers throttled because too much dirty page cache
  is waiting for writeback.
* `jbd2_log_wait_commit`, `start_this_handle`, `wait_transaction_locked`:
  waiting on the ext4 journal.
* `rwsem_down_*`, `mutex_lock*`: directory or inode lock contention.
* `futex_*`, `do_wait`, `pipe_*`: idle, waiting on another process.

### S2. Parallelism matrix

Run `add` at `-j 8`, `-j 16`, and `-j 32` (the default on this machine),
three runs each, in a rotating order (8, 16, 32, then 16, 32, 8, then 32, 8,
16). Before each run, wait until `Dirty` in `/proc/meminfo` is below 1 GB
and the 1-minute load average is below 40, and record both. Remove the tree
after each run with `--no-pool`.

Report per run: total, the `snapshot-*` phases, `projects`,
`projects-journal-sync`, `projects-meta-write`, the median and p90 of
per-project `ms` and `queue_ms`, %CPU, and the S1 wait-channel histogram
summed over the run.

### S3. Quiet-machine control (only with the user's permission)

If the user agrees, pause the background load for one `-j 32` run: the
Jetski hub's `git status` polling, the Cuttlefish VMs, and any Bazel build.
Do not stop anything the user has not approved. If they decline, skip S3 and
say so.

### S4. Pool on AOSP with maintenance

Run one `add` with maintenance on and let it refill (14 pooled projects,
about 19 GB per spare set). Record how long `pool maintain` took, whether it
deferred anything for disk space, and whether a second `add` started while
it ran was slower than an S2 `-j 32` run.

### S5. `remove`

Time `remove` (default, then `--no-pool`) twice each. Compare `prune` and
`pruned_git_dirs` with round 4's 0.2–32 s `prune`.

## 5. Questions to answer

1. Does `pool list` classify studio-main's spares correctly, and how much disk
   did `pool clear --orphaned` reclaim?
2. With background refill, how many adds of a burst hit the pool, and how
   soon after an `add` is the pool back to depth 2?
3. Does idle-priority refill slow a build or a concurrent `add` measurably?
4. On AOSP, which wait channels dominate the slow periods? Is it dirty-page
   throttling, the ext4 journal, lock contention, or something else?
5. Does lower `-j` make AOSP adds faster on this machine? Which value is best,
   and is the answer stable across runs?
6. How much of `snapshot` is `repo manifest -r`, git-dir discovery, size
   estimation, and preflight? Which part varies?
7. How much time does the scheduler now spend in `projects-journal-sync` and
   `projects-meta-write`, and do `queue_ms` values show projects waiting
   while workers sat idle?
8. Anything that looks wrong or surprising in the new history fields.

## 6. Deliverable

`real-world-experience/round-5/`:

* `report.md`: findings first, then the tables for §3 and §4, and answers to
  §5 with numbers. Keep raw quotes verbatim and mark anything reconstructed.
* `data/`: `pool list` outputs, sampler output, `/usr/bin/time -v` files,
  `history.jsonl` filtered to `R5-*` sessions plus each client's full
  history, `df`/`du` snapshots, and a `data/README.md` index as in round 4.
* `scripts/`: every script used, with the machine paths it assumes.
* State left behind: which trees remain, and the pool state of each client.
