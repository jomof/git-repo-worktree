# Round 7 response: what changed, what was declined

Response to [`report.md`](report.md). Everything marked **changed** ships in
`git-repo-worktree` 2.3.0. Each change has a regression test that fails
without it.

## Summary

| Finding | Outcome |
| :--- | :--- |
| F1 fsmonitor daemons wedge `add` | **Changed**: the tool's Git commands never start daemons; daemons in a tree are stopped before it is moved, removed, or returned to the pool; maintenance stops daemons on spares. |
| F2a inherited descriptors | **Changed**: every descriptor above stderr is close-on-exec from startup, so no child (fuse-overlayfs, `pool maintain`, Git, hooks) inherits the caller's. |
| F2b lock descriptor leak | **Changed**: locks are a plain `flock` on one open file; nothing is cloned or leaked. |
| F3 frozen trees | **Declined for now** (see below); `--full` is not a fast isolation option. |
| F4 pinned starting revisions | **Changed**: `add --manifest <file>` and `pinned_digest`. `--from-manifest` / `--at-upstream` declined. |
| F5 outdated performance report | **Changed**: marked historical, with links to current numbers. |
| F6 untracked files demote overlays | **Changed**: untracked source files are hidden in the overlay instead of forcing a full copy; large fallbacks warn; trash deletion waits until every pool has a spare. Smallest-first refill declined. |

## F1. fsmonitor daemons

**Changed.**

- The tool appends `core.fsmonitor=false` to the `GIT_CONFIG_COUNT`
  configuration of every Git command it runs (after any the caller set), so
  `pool fill`, `add`, `remove`, pool return, and maintenance never start a
  daemon. Site hooks get the caller's original configuration back, so a hook
  that runs Git behaves as the user configured it.
- Before a worktree is moved (`move`, pool return, taking a spare) or deleted
  (`remove`, discarding a spare), the tool runs `git fsmonitor--daemon stop`
  in it when the worktree's administrative directory holds a daemon socket,
  covering daemons the user or their tools started inside a tree. Each
  maintenance pass does the same for every spare. `remove` and `pool
  maintain` record the count as `fsmonitor_stopped` in history.
- A user's own `git status` inside a tree still starts a daemon if
  `core.fsmonitor=true`; that is their configuration and is left alone.

**Declined:** the per-project stall timeout (F1.4). The cause is fixed, and a
generic "no progress" detector for checkouts that legitimately take minutes
would mostly add false alarms. Revisit if another hang appears.

**Correction to the report:** on macOS, a daemon whose worktree is moved shuts
itself down, so only the daemons the tool itself started (all of the leaked
ones in the survey) were the problem there; Linux behavior was not reproduced
locally.

Existing leaked daemons on `jomof-specialist` are not killed by the upgrade.
Stop them once with `git -C <path> fsmonitor--daemon stop` or `pkill -f
'fsmonitor--daemon run'`; after that, maintenance keeps spares clear.

## F2. File descriptors

**F2a changed** as proposed, but in one place: `main()` marks descriptors 3
and up close-on-exec (`close_range(CLOSE_RANGE_CLOEXEC)` on Linux, `/dev/fd`
elsewhere), which covers every process the tool starts, not just the three
listed. Already-running fuse-overlayfs daemons keep what they inherited until
remounted (`repair --mounts-only` after unmounting, or a reboot).

**F2b changed**, using `libc::flock` directly rather than `File::try_lock`,
which would raise the minimum Rust version from 1.75 to 1.89. A waiting
command now holds exactly one descriptor however long it waits.

## F3. Frozen trees

**Declined for now.**

- **`--full` / `--isolated`:** `--full` is not fast. On `studio-main` the
  three `add --full` runs in the round 7 history took 90–145 s (median 110 s)
  against a 15 s median for 141 ordinary adds, and every such tree writes its
  ~20 overlaid projects (about 82 GB apparent) in full. Documenting it as the
  isolation option would point `evalbox` at the expensive path it is trying to
  avoid, so no alias and no new docs.
- **`source_drift` in `status` (F3a)** and **content-pinned lower snapshots
  (F3c)** are reasonable but larger; not in this release. For a byte-stable
  baseline today, combine `add --manifest` (F4) with a source client nobody
  edits, or `materialize` the specific overlaid projects the evaluation reads.

## F4. Pinned starting revisions

**Changed.**

```sh
repo manifest -r -o baseline.xml            # once, in any client at the wanted commits
git repo-worktree add --manifest baseline.xml --json /path/to/worker-1
```

- Every project the file lists starts at its revision; projects it omits start
  at the source's checked-out commit (as `add` does without `--manifest`).
- Projects whose pinned commit differs from the source checkout become
  worktrees, taken from the pool when possible, instead of overlays.
- Nothing is fetched. Before creating anything, `add` refuses a file that names
  a branch or tag (`E_MANIFEST_UNPINNED`, `error.details.unpinned`) or a commit
  missing from the local object stores (`E_MISSING_REVISIONS`,
  `error.details.missing[]` with `project` and `revision`).
- `pinned_digest` (in `list --json` and the `add`/`restore` result) is a hash
  of every populated project's path and pinned commit: equal for trees that
  started from the same commits. It is named for what it covers: starting
  commits, not current contents.

**Declined:** `--from-manifest` / `--at-upstream`. Write the pinned manifest
you want (for example `repo manifest -r` in a reference client synced to
upstream) and pass it with `--manifest`; that keeps `add` free of any notion of
"upstream".

## F5. Outdated performance report

**Changed:** `real-world-performance-studio-main.md` now opens with a banner
marking it as the v1 baseline and linking to current numbers.

## F6. Untracked files demote overlays

**Changed.**

- When the source's tracked files match the pinned commit, its untracked
  files no longer prevent an overlay. Each untracked path (for example
  `prebuilts/studio/sdk/linux/environments/`) is hidden with a whiteout in the
  overlay's upper directory, so the tree starts as a clean checkout of the
  commit, and `status` and `remove` do not count the whiteouts as changes. If
  the source later gains untracked files, they appear in the overlay, as any
  change to the live view does. Modified tracked files still force a worktree.
- The pool's rule for which projects need spares follows suit: a fixed-depth
  project with only untracked files in its source is overlaid, so its spares
  are orphaned and `pool clear --orphaned` (or maintenance) deletes them. On
  `studio-main` that should release the ~22.5 GB of `sdk/linux` spares.
- When a project over 1 GiB is checked out in full because it could not be
  shared or overlaid, `add` prints a warning with the project, its size, and
  the reason (not for `--manifest` pins that differ from the source, where a
  worktree is expected).
- Maintenance now deletes removed trees' files after the first refill pass
  (every pooled project at one spare), not before.

**Declined:** refilling smallest projects first. Population runs projects in
parallel, so an `add`'s time is set by its slowest missing spare; filling the
largest first still minimizes the next `add`. With `sdk/linux` overlaid again,
the head-of-line blocking in the report goes away.

## Not tool problems

Agreed on all four (`core.checkStat=minimal`, the per-worker `env.bazelrc`
rewrite, kernel overlayfs in a user namespace, canonical mount paths); no
changes.
