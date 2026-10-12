# Implementation notes

The specifications leave the following boundary cases open; these choices keep the
machine interface deterministic:

- `repair --json` returns the specified full tree object when exactly one tree is
  selected. When repairing all or multiple trees, `result` is
  `{"trees":[<full tree objects>]}`.
- The add-time repo version check reads the installed repo launcher's `VERSION`
  tuple. It never executes repo for version discovery, preserving the rule that add
  invokes repo only as `repo manifest -r`. If a custom launcher exposes no readable
  tuple, availability and compatibility are determined by the required manifest
  command.
- A normal snapshot-command or snapshot-parse failure removes the just-created empty
  registry entry so the same name can be retried. If the process is killed before
  pin completion, `add --continue` refuses the unpinned entry; `remove <name>` safely
  clears it. If a pinned snapshot exists but the process died before metadata or the
  breadcrumb was flushed, continuation reconstructs those pieces from the immutable
  snapshot without invoking repo again.
- Snapshot `git_common_dir` discovery uses the same bounded job count as project
  creation. Project creation uses prefix dependencies rather than global path-depth
  barriers, allowing unrelated deep paths to start immediately.
- `.repo/project.list` entries omitted by `repo manifest -r` are pinned from the
  corresponding checkout `HEAD`. Copy/link declarations are reconstructed by
  following the active `.repo/manifest.xml` include graph and applying sorted local
  manifests. The resulting pinned projects and directives are atomically appended
  to `snapshot.xml` before `pin done` is journaled, so crash recovery never re-pins
  them from a later source state.
- Project completions that arrive within a short coalescing window share one journal
  write and `fsync`. State changes, progress output, and dependent scheduling happen
  only after that durability boundary. If a process or machine fails before it,
  `add --continue` safely repeats the idempotent worktree check. A partial trailing
  journal record is truncated during recovery; malformed durable records still fail
  closed.

## `spec-v2.md` decisions

- The tree's manifest checkout is on a per-tree branch `repo-worktree/<tree>`,
  not detached. repo derives the manifest revision from the current manifest
  branch's `branch.<name>.merge`; a detached checkout makes `repo sync -l` fail
  with "revision refs/heads/master in manifests not found". The branch reuses
  the source manifest branch's `remote` and `merge` and is deleted on `remove`
  and `prune`.
- Shared projects are left out of the generated `manifest.xml` and
  `project.list`. Listing a project that repo does not see in `project.list`
  makes `repo sync` delete its checkout, and repo "repairs" a `.git` it does
  not own by replacing it, which for a shared project is the source's `.git`.
- New branches track, in order: project `dest-branch`, project `upstream`,
  symbolic project `revision`, then manifest defaults. `repo start` in a pinned
  client skips the project `upstream`; using it is closer to the project's real
  upstream when a project's revision differs from the default.
- Overlay projects copy the source checkout's index unmodified. Measured on
  ext4 under `fuse-overlayfs` with 40,000 files: `git status` hashes nothing
  (0.5 s cold, 23 ms warm) because the overlay reports the lower files' stat
  data and Git does not compare `st_dev` unless built with `USE_STDEV`. The
  field report's `core.checkStat=minimal`/`sd_dev` workaround is unnecessary,
  and `checkStat` would have had to be set in the source's shared config.
- `remove` returns at most one spare per pool project. `pool fill --count`
  can keep more.
- Concurrent-sync detection matches only processes whose program is `repo`, or
  a Python interpreter running `repo` or repo's `main.py`, with a `sync` or
  `init` argument. The previous substring match tripped on any process whose
  command line mentioned "repo sync", including agent shells.
- A bridge that cannot be built for a supported reason (no `.repo/repo`,
  `<submanifest>`) is `unsupported` and does not make the tree incomplete; any
  other bridge failure is `failed` and does, so `repair` retries it.
- Fast modes are the defaults so field use exercises them: inferred
  fixed-depth candidates become overlays where FUSE works, and heavy source
  projects use the pool. `defaults worktree` opts a project out of both;
  `--no-overlay` or `GIT_REPO_WORKTREE_NO_OVERLAY=1` turns overlays off per tree
  or per machine. Per-project `populate_ms` is recorded for the next round of
  measurements.
- The usage history is permanent rather than temporary instrumentation: an
  `add` record is a few kilobytes even for large clients, writes are unsynced
  appends, and the file is capped. Flag values are never recorded because they
  carry paths and branch names; flag names are enough to see which invocations
  agents get wrong.
