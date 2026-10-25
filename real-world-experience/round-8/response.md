# Round 8 response: what changed in 3.0.0

Response to [`report.md`](report.md), [`worktree-rca.md`](worktree-rca.md),
and the owner's notes from the disk-space cleanup. Everything below ships in
`git-repo-worktree` 3.0.0, with a regression test for each fix.

## Trees start at the manifest revision (R8-1; breaking)

The RCA called the `v4-package` incident agent error. The owner's conclusion
was that the default invited it, and the default changed:

- `add` starts every project on a detached HEAD at the revision the manifest
  names, as already fetched: the commit `repo sync -l` would check out
  (`refs/remotes/m/<branch>`, which repo keeps pointing at the upstream branch
  or a pinned commit). No network is used. A developer's local branch (`mother`
  in `tools/vendor/google`) no longer leaks into any tree.
- `add` prints, and returns in `source_differs`, the projects whose source
  checkout is elsewhere:

  ```text
  note: 1 project(s) start at the manifest revision, not the source's checkout (pass --from-source to keep the source's commits):
    tools/vendor/google: source is on branch mother at f0d61ab633 (6 local commit(s)); tree starts at f491491880
  ```

- `--from-source` restores the old behavior for the cases that need the
  source's local commits. Trees record how they started in `start`
  (`manifest`, `source`, `pinned`, or `archive`).
- Pool spares are filled at the manifest revision, so default adds keep
  taking them without a checkout, and spares no longer carry a developer's
  local commits.

**For agent briefs:** drop the "immediately `git checkout --detach
goog/studio-main`" step. Ask for `--from-source` only when the work must build
on commits that exist only in the source client.

This is why the release is 3.0.0: scripts that relied on trees inheriting the
source's checked-out commits need `--from-source`.

## Archives (R8-2, and the owner's "agents didn't recover")

Every archive made under 2.3.0 expired at once, so no removed tree could be
restored; that is why agents found nothing to come back to.

- **Retention:** the newest archive (the one a `remove` just made) is exempt
  from the size budget and no longer counts against it. Before, one archive
  over 2 GiB expired itself and every older archive. Older archives still
  expire oldest first beyond the budget, and all expire after 30 days.
- **Honest output:** if an archive ever does expire in the same `remove`, the
  output says it cannot be restored instead of naming a `restore` command.
- **Size:** unique bytes now exclude everything reachable from the commit the
  tree started at, not just from branches, remote-tracking refs, and tags.
- **Large untracked files:** untracked files of 100 MiB or more (build
  outputs, `.deb`s, extracted SDKs) are no longer archived. `remove` lists
  them, the archive records them under `dropped` (kind `large-untracked`), and
  they never cause a refusal. This also keeps `remove` from writing gigabytes
  into object stores while you are trying to free disk space.
- **Finding a removed tree:** a command naming a tree that no longer exists
  but has an archive now says so and gives the `restore` command
  (`E_NO_SUCH_TREE` message).

## `remove --idle` (R8-3; owner point 1)

- Activity is decided from cheap signals first (tree metadata and each
  project's `HEAD`, index, and reflog timestamps), and `git status` runs only on
  trees those leave looking idle, skipping overlays with nothing written and
  stopping as soon as recent work is found. `/proc` is read once per run, not
  once per tree. On a 12-tree test client, `--idle --dry-run` went from 0.15 s
  to 0.01 s; on `studio-main` most of the 4–12 minutes was `git status` across
  every project of every tree.
- `--idle` without a value means 24h, and `--force` and `--discard` now
  combine with it (each applies per tree).
- Each tree prints as it is removed, so a long run shows progress.

**Unmountable overlays:** when an overlay with changes cannot be remounted
(for example its registration was pruned by an older version), the refusal now
says so and names `--force` and `--discard`, instead of suggesting
`repair --mounts-only`, which had just failed. An unmounted overlay with no
changes was already archived without complaint.

**Nested Git repositories** (`prefab-arm*`) still refuse: their contents
would be lost. `--force` archives the rest; with `--idle --force` that is now
one command.

## Background deletion (owner point 2; R8-5.1)

Deletion moved out of pool maintenance, where it ran at idle disk priority,
one tree at a time, paused whenever any tree command ran (an 88-minute sweep,
27 minutes of it paused):

- `remove` starts a detached `pool empty-trash` that deletes at normal disk
  priority (reduced CPU priority), four trees at a time, and is never paused.
- `remove --wait` deletes the tree's files before returning.
- Because maintenance no longer deletes, its `fill` and `trash` phase timings
  can no longer overlap.

## `prune` (R8-4)

We could not reproduce the overlay explanation (the test fixture forces
worktrees), but the failure fits a `gitdir` file recorded as a relative path,
which newer Git can write. Registrations are now matched after resolving
relative paths, for worktree and overlay projects alike, and `prune` unlocks
the overlay registrations of pruned trees so `git worktree prune` can reap
them.

## Smaller items

- `status` with no tree outside a tree now says `git repo-worktree list`
  shows the trees (R8-5.2).
- The v2.3.0 fixes the report verified (overlay whiteouts, fsmonitor, file
  descriptors) are unchanged.

## Not changed

- **`add --upstream`** (R8-1 proposal 2): superseded by the new default.
- **Refusing nested Git repositories:** kept; `--force` is the explicit way
  to drop them.
