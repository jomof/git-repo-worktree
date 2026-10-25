# How trees are populated

Most projects in a new tree are linked Git worktrees: they share the source
client's object store, so nothing is fetched or cloned, but each still needs its
files checked out. For very large clients that checkout dominates, so
`git-repo-worktree` chooses one of these modes per project:

| Mode | What the project is | Cost |
|---|---|---|
| `worktree` | an ordinary linked worktree, checked out fresh | proportional to files and bytes |
| `pool` | an ordinary linked worktree recycled from a pool of spares | rewrites only files that differ |
| `overlay` | a linked worktree whose files are a copy-on-write view of the source (Linux) | milliseconds, no disk until written |
| `symlink-shared` | a symlink to the clean source checkout (read-only) | instant |

## Defaults

By default, a project with an explicit positive manifest `clone-depth` is shared
with its clean, pinned source checkout instead of being copied. This avoids
copying large prebuilts on filesystems such as ext4 that lack reflinks. On
Linux with `fuse-overlayfs` installed, it becomes a copy-on-write overlay (see
`overlay` below), so editing it is safe. Elsewhere, or with `--no-overlay` or
`GIT_REPO_WORKTREE_NO_OVERLAY=1`, it is an absolute symlink to the source
checkout and `add` prints a short warning. Symlink-shared projects are live
views of the source and should be treated as read-only; convert one before
editing:

```sh
git repo-worktree materialize <tree> <project-path>
```

`--full` forces an entirely materialized tree, while repeatable
`--full-project <project-path>` forces selected projects. Project arguments to
`--full-project`, `-p`, `materialize`, `defaults`, and `detach` may name any path
inside a project, or a filesystem path relative to the current directory; they
resolve to the enclosing manifest project. Sharing is automatically disabled for
branched projects, nested parents, directive destinations,
dirty/wrong-HEAD/sparse sources, relevant submodule population, and clients
where every active project has `clone-depth`. `materialize` on a project that is
already a worktree succeeds and reports it under `already_materialized`.

Shared projects are excluded from the tree's `.repo` manifest, so repo commands
never touch them. Run repo commands from the tree root or from a worktree
project: a shell inside a shared project is physically in the source client.

Without a default, projects estimated to take at least 1.5 s to check out
(60 µs per tracked file plus 500 MB/s, fitted to studio-main on ext4) use
`pool`, and fixed-depth projects use `overlay` where available. A fixed-depth project whose source checkout is too dirty to share
falls back to a pooled worktree. `defaults worktree <project>` opts a project
out of both.

## Per-project defaults

Source-client-local defaults can opt individual active projects in or out for
future trees:

```sh
git repo-worktree defaults symlink prebuilts/example
git repo-worktree defaults worktree platform/app
git repo-worktree defaults pool tools/idea
git repo-worktree defaults overlay prebuilts/studio/sdk
git repo-worktree defaults auto platform/app
git repo-worktree defaults list
```

A `symlink` default makes the project a sharing candidate even without
`clone-depth`; it does not bypass any safety fallback. A `worktree` default
suppresses clone-depth sharing. `auto` returns to manifest-driven behavior.
Defaults are stored under `.repo/repo-worktrees`, and each `add` captures its
active defaults so later changes affect only future trees.

## Pool and overlay

Two modes keep large projects fast without sharing the source's files:

* `pool`: the project is an ordinary isolated worktree, taken from a pool of
  pre-populated spares when one is available. `add` moves a spare into the tree
  with `git worktree move` and checks out the pinned commit, which rewrites only
  files that differ. `remove` returns a clean pool project to the pool (after
  `git clean -ffdx`) instead of deleting it, keeping up to `pool size` spares
  per project (default 2). Branched projects use spares too. Spares must be on
  the same filesystem as the tree and cost a full checkout of disk each.
  See [Keeping the pool full](#keeping-the-pool-full).
* `overlay` (Linux, requires `fuse-overlayfs`): the project is a real linked
  worktree whose files are a copy-on-write view of the clean source checkout.
  Creation takes well under a second regardless of size, writes never reach the
  source, paths stay inside the tree, and an untouched overlay is known clean
  without scanning it. The source's tracked files must match the pinned
  commit; untracked files in the source (for example a directory runtime
  tools create) are hidden in the overlay, so the tree starts clean. Because
  the view is live, a source checkout that later changes branch shows up as
  modifications in the overlay. Mounts do not
  survive a reboot; `repair` remounts them. `materialize` converts a clean
  overlay to a normal checkout. Without FUSE, overlay projects fall back to
  worktrees.

## Keeping the pool full

After `add` and `remove`, a detached `pool maintain` runs in the background at
the lowest CPU and I/O priority. It refills every pooled project to `pool size`
spares, one spare per project at a time, so after the first tree a burst of
`pool size` adds all take spares. Spares are checked out at each project's
manifest revision, where a default `add` starts. The files of removed trees
are deleted separately, at normal disk priority, so refills never delay
freeing disk space. It also deletes *orphaned* spares: spares of
projects a default `add` would no longer take from the pool, for example
fixed-depth prebuilts that are now overlaid, projects quick enough to check
out directly, or projects whose default has changed. Maintenance pauses while
any `add`, `remove`, or other tree operation runs, so refills never compete
with them for the disk. Refills stop short of leaving less than 10% of the
filesystem (at most 50 GB) free.

```sh
git repo-worktree pool list             # spares per project, depth, orphaned spares and their size
git repo-worktree pool clear --orphaned # delete orphaned spares now
git repo-worktree pool fill             # fill the pool in the foreground
git repo-worktree pool size 1           # keep fewer spares per project
```

`pool list` reports the pool's *depth*: how many consecutive adds can take
every pooled project from the pool. Set `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1`
to stop background maintenance, for example while measuring pool misses;
`remove` then deletes tree files in the foreground.
To keep spares for a project the pool would otherwise orphan, give it a
`pool` default.

## Checkout speed

When a project larger than 1 GiB cannot be shared or overlaid (its source
checkout changed or has modified tracked files), `add` warns, naming the
project, its size, and the reason.

For worktree projects, Git's native parallel checkout is enabled and ready
projects are started largest-first using pinned tracked-file sizes. Projects
over 500 MiB are limited to three concurrent checkouts by default; use
`--heavy-jobs N` to tune storage pressure. On a clean, non-sparse source at the
pinned SHA, the tool may clone file extents with filesystem reflinks and verify
the result with Git. Any ambiguity falls back to normal checkout. Use
`--no-reflink` as a diagnostic escape hatch.
