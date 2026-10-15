# Round 6: Verify the Round 5 Fixes

**Audience:** the orchestrating agent on the Linux workstation with
`~/projects/studio-main` and `~/projects/main` (AOSP).
**Build under test:** `main` at or after `72af7a8`
(`git repo-worktree version` prints the commit). Install it as in round 5 and
check that no older copy shadows it (`which -a git-repo-worktree`).
**Method:** orchestrator measurements, repeating round 5's procedures so the
numbers compare directly. Reuse
[`round-5/scripts/`](./round-5/scripts/) (`sample2.sh`, the precondition wait,
`summarize.py`), changing only paths and session names. New or changed
scripts go in `round-6/scripts/`.
**Deliverable:** `real-world-experience/round-6/` (§6).

## 1. What changed since round 5

Fixes for [`round-5/report.md`](./round-5/report.md):

* **Journal writer thread.** During project population, completion records
  and `meta.json` are written by a separate thread that syncs at most once a
  second. The scheduler no longer waits on any fsync, so a finished project's
  worker slot is refilled at once. `projects-journal-sync` and
  `projects-meta-write` now measure that thread's syncing, which no longer
  delays anything.
* **New `projects-idle-slot` phase:** worker-slot milliseconds between a
  project finishing and the scheduler starting the next one in that slot.
  Round 5 had no such measure; the scheduler's sync time was its proxy.
* **Size-estimate cache.** `.repo/repo-worktrees/estimates.json` maps each
  commit to its tracked size and file count. An `add` whose projects are at
  the same commits as the last `add` skips `git ls-tree` for them;
  `estimate_cache_hits` counts how many did.
* **Maintenance yields to tree operations.** `pool maintain` waits before each
  deletion or refill while any tree lock is held, and stops its running
  refill's process group (`SIGSTOP`) until no tree lock is held (`SIGCONT`).
  The new `paused` phase records the time spent waiting or stopped.
* **Cheaper `remove` on a full pool.** It skips the source-cleanliness check
  for projects whose pool is already full, and starts maintenance only when
  one of the tree's pooled projects is below the pool size.
* **Unchanged on purpose:** the default `-j`, and maintenance's
  slowest-project-first refill order. See `IMPLEMENTATION_NOTES.md`, "Round 5
  changes", for why.

## 2. Setup

1. Record the starting state of both clients as in round 5: `list --json`,
   `pool list --json`, `df -B1 /`, `du -sx .repo/repo-worktrees/.pool`.
2. Tag every command with `GIT_REPO_WORKTREE_SESSION=R6-<step>`.
3. Keep round 5's source-integrity snapshot (`snap-before.txt`/`snap-after.txt`)
   for both clients.
4. Delete `estimates.json` in both clients once at the start, so the first
   `add` of each section measures an empty cache.

## 3. AOSP (`~/projects/main`)

Use `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` and an empty pool
(`pool clear`) for A1 and A2, as in round 5's S2.

### A1. Cold `add` with the writer thread

Repeat round 5's S2 matrix exactly: `-j 8`, `-j 16`, `-j 32`, three runs each
in the same rotating order, with the same precondition wait (`Dirty` < 1 GB,
1-minute load average < 40, up to 90 s) and `remove --no-pool` after each run.
Sample every run with `sample2.sh`.

Report per run, next to round 5's matching run: total, `snapshot` and its four
parts, `projects`, `projects-idle-slot`, `projects-journal-sync`,
`projects-meta-write`, the sum of per-project `ms`, `estimate_cache_hits`,
%CPU, and the summed wait-channel histogram.

Expectations to confirm or refute: `projects-idle-slot` is small (well under
a second per worker), `projects` drops to roughly 12–15 s at `-j 16` and
`-j 32`, and `futex_do_wait` samples on `git-repo-worktree` threads mostly
disappear.

### A2. Size-estimate cache on AOSP

From A1's history, compare `snapshot-estimates` on the first run (empty cache)
with later runs (expect `estimate_cache_hits` = 1,084 and `snapshot-estimates`
in the tens of milliseconds).

### A3. `add` during a background refill

Repeat round 5's S4 with maintenance on:

1. `pool clear`, then a cold `add` (tree S4a); this starts maintenance, which
   fills two spare sets.
2. Two seconds after S4a finishes, start a second cold `add` (tree S4b) while
   maintenance is still refilling. Sample it with `sample2.sh`, and also
   sample the refill's `git` processes once a second
   (`ps -o pid,stat,wchan:32,args -C git | grep '\.filling-'`) to confirm
   they show state `T` (stopped) while S4b runs and resume afterwards.
3. Wait for maintenance to finish (its `pool maintain` event in
   `history.jsonl`), then a warm `add` (tree S4c).

Report S4b's total and wait-channel histogram next to round 5's 97.7 s run
and A1's `-j 32` median. Report the maintenance event's `paused`, `fill`, and
total, and S4c next to round 5's 20.5 s warm `add`.

### A4. `remove`

Remove S4a, S4b (default) and S4c (`--no-pool`) as in round 5's S5. Report
the phases and whether maintenance was started (`pool_maintenance` in each
`remove` event).

## 4. studio-main

### B1. Warm `add` with the estimate cache

With the pool at depth 2 and maintenance off
(`GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1`), add and remove a tree five
times, waiting for `Dirty` < 1 GB between runs. Return the spares to the pool
on each `remove` (default `remove`). Report each `add`'s total,
`snapshot-estimates`, `estimate_cache_hits`, and `projects`, next to round
5's 4.4–5.3 s warm adds. Expect the first run to miss the cache and later runs
to hit all 56 projects.

### B2. Burst with maintenance

Repeat round 5's P2.2: from depth 2, add B, C, D back-to-back with
maintenance on. Record pool hits, totals, and each maintenance event's
`paused`. Then add a fourth tree E right after D finishes and record whether
E took any spares (maintenance should have started refilling once D's lock
was released).

### B3. `remove` on a full pool

With the pool at depth 2, add a tree with maintenance off, then remove it
with maintenance on. Since the pool is full, expect the `remove`'s
`pool-return` phase to be well under round 5's 0.9–2.3 s, and no
`pool maintain` event to follow. Repeat three times.

### B4. Bazel build (regression check)

Repeat round 5's P3 once with maintenance on and once off. The point is only
to confirm that pausing maintenance during tree operations changed nothing
for a build that runs after `add` has returned.

## 5. Questions to answer

1. Is scheduler idle time gone on AOSP? What are `projects-idle-slot` and
   `projects` now, per `-j`?
2. With the scheduler no longer the bottleneck, which `-j` is fastest, and is
   it stable across the three rotations? Should the default change?
3. What dominates the remaining AOSP `add` time (for example
   `snapshot-repo-manifest`, or per-project checkout time growing with `-j`),
   by the wait-channel histograms?
4. Does an `add` started during a refill now run at about normal speed, and
   were the refill's processes stopped while it ran? How much longer did the
   refill take as a result?
5. How much did the estimate cache save on each client?
6. Is `remove` on a full pool cheaper, with no spurious maintenance runs?
7. Anything wrong or surprising in the history fields, or in behavior.

## 6. Deliverable

`real-world-experience/round-6/`:

* `report.md`: findings first, then tables comparing each measurement with
  its round 5 counterpart, then answers to §5 with numbers. Keep quotes
  verbatim and mark anything reconstructed.
* `data/`: raw outputs, sampler files, `/usr/bin/time -v` files, both
  clients' `history.jsonl` filtered to `R6-*` sessions plus the full
  histories, `df`/`du` snapshots, and a `data/README.md` index.
* `scripts/`: every script used, with the machine paths it assumes.
* State left behind: remaining trees and each client's pool state.
