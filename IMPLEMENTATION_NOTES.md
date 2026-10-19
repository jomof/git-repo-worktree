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

## Round 2 changes

- Locks: round 2 recorded 28 `E_LOCKED` failures among three parallel agents
  because `add` and `remove` held the registry lock for their whole 30–40 s run
  and every lock attempt failed immediately. Readers now take no lock, writers
  wait, and tree operations lock only their tree. `fd-lock` guards borrow their
  lock, so each waiting attempt leaks one small boxed lock to obtain a
  `'static` guard; a process makes at most a few thousand attempts.
- Pool selection counts tracked files: checkout costs about 70 microseconds per
  file on ext4 at studio-main scale, so file count predicts cost far better
  than bytes (`external/jetbrains/kotlin`, 290 MB, took 6.5 s; `external/dexter`,
  460 MB, took 1.6 s).
- `remove` reports processes still using the tree instead of stopping them:
  only the user knows whether a server or editor should keep running, and the
  tool must not know about specific build systems.
- Hooks are local files in the source client rather than manifest-provided
  `<repo-hooks>`: they encode one machine's setup, need no approval because the
  owner installed them, and studio-main has no manifest hook to extend. Hook
  failures are reported but never fail `add`: the tree itself is complete, and
  `repair` retries the hook.

## Round 3 changes

- Pooling uses an estimated checkout time (60 µs per file plus 500 MB/s, at
  least 1.5 s) fitted to round-3 cold checkouts: it keeps the six projects
  pooled before and adds `tools/base`, `tools/vendor/google`, and
  `tools/vendor/intellij/cidr`, which had become the slowest projects in a
  warm `add`, at about 1.7 GB per spare set.
- `add`, `repair`, and `materialize` return tree summaries by default: a
  full 56-project tree is about 25 KB, which truncated agents' tool output.

## Live manifest in trees

- Trees select the source's manifest (with a generated local manifest removing
  projects they do not own) instead of a pinned snapshot manifest, so
  `repo sync` updates a tree like any client. Field use showed trees outliving
  their starting point: work under review needs rebasing onto upstream, and a
  pinned manifest made `repo sync` a no-op. Creation still pins to the source's
  checked-out commits; the snapshot remains the tool's record for resume and
  repair.

## Round 4 changes

- Spares are kept only for projects a default `add` would take from the pool,
  judged with current defaults and the source checkout. Round 4 found 132 GB
  of studio-main's 171 GB pool held by nine projects no `add` pooled: spares
  returned from `--full` trees (whose prebuilts are worktrees and looked
  poolable by checkout time alone), and spares `pool fill` kept topping up
  because old trees without file counts fell back to the 500 MiB rule. The
  rule's inputs are recorded beside each pool so it needs no registered tree.
- Pool depth came only from `remove`, so "27 spares" meant one per pooled
  project, and the second and third adds of a burst missed (40 s, 60 s versus
  4.8 s). A detached `pool maintain` now refills to the pool size after `add`
  and `remove`, at idle priority, filling every project to one spare before any
  gets two. It runs `nice`/`ionice` as programs rather than calling
  `setpriority`/`ioprio_set` to avoid a libc dependency; `ionice -t` keeps a
  refused priority change from stopping the run. A disk reserve (10%, at most
  50 GB) keeps background refills from filling the disk unasked.
- AOSP adds stalled machine-wide (1,084 projects, 155–243 s at about one busy
  CPU). Sampling showed the scheduler thread sometimes blocked in ext4 journal
  commits and, in about a third of samples, only 1–4 git children running
  while hundreds of projects were ready. The scheduler now starts independent
  projects before syncing each 10 ms batch and rewrites `meta.json` (hundreds
  of kilobytes for AOSP, previously after every batch) at most once a second. The
  root cause is outside the tool; `queue_ms`, the `snapshot-*` and
  `projects-journal-sync` phases, and load and pressure snapshots in history
  are there to attribute it next time.
- `remove` prunes only Git directories that may hold stale registrations
  (overlay projects, failed `git worktree remove`), not every project's.

## Round 5 changes

- Project completion records are journaled by a writer thread that syncs at
  most once a second, instead of the scheduler syncing every 10 ms batch.
  Round 5 showed the scheduler spending 46–93% of AOSP's population phase in
  the journal and `meta.json` syncs, each of which waits for an ext4 journal
  commit, while 60–150 ms checkouts finished and their slots sat empty. Git
  does not sync the files it checks out, so syncing a completion record never
  made the project itself survive power loss; resume already replays an
  unrecorded project idempotently.
- Size estimates are cached by commit in `estimates.json`. They were 1.3–1.8 s
  of studio-main's 4.4–5 s warm `add`, and a commit's tree never changes.
- Background maintenance stops its running refill (`SIGSTOP` to the git
  process group) while any tree lock is held. Round 5's `add` during an AOSP
  refill took 98 s instead of about 30 s, throttled by the refill's buffered
  writes: I/O priority orders block requests but not dirty page cache.
  Stopping the process stops new dirty pages; writes already buffered still
  drain. Tree locks are the signal because every operation that writes
  heavily holds one.
- Maintenance keeps refilling the slowest projects first, although round 5
  suggested the quickest first: an `add` waits for its slowest project, so
  pooling the quick ones first would not shorten the next `add`.
- The default `-j` is unchanged. Round 5's `-j 8/16/32` matrix was measured
  with the scheduler blocked on syncs, and summed per-project time doubled
  from `-j 8` to `-j 32`, so the disk saturated well below 32 workers. It
  needs re-measuring now that slots are refilled promptly.

## Round 6 changes

- The scheduler starts the next project as soon as one finishes instead of
  waiting 10 ms to collect more completions; the window only existed to
  batch journal syncs, which the writer thread now does. Round 6 measured
  7.5, 11, and 26 ms of slot idle time per project at `-j 8`, `16`, and `32`.
  `projects-idle-slot` no longer counts slots left empty because only heavy
  projects remained, which made pool-miss adds look idle for 30 s.
- `remove` deletes files in the background. It was slower than `add` after
  round 5's fixes (27–45 s on AOSP, about 20 s of it deleting 1.35 million
  files; 33–46 s on studio-main with a full pool). It now deletes each
  project's worktree administrative directory directly, since that is all
  `git worktree remove` does besides deleting files, and renames the tree
  aside for maintenance to delete. The administrative directory is deleted
  only when its `gitdir` points back at the project, so a moved or foreign
  registration falls back to `git worktree remove`. Deletion runs `rm` as a
  child so maintenance can stop it during tree operations, with
  `--one-file-system` (`-x` on macOS) so an overlay that failed to unmount is
  never descended into. The same switch that turns maintenance off restores
  foreground deletion, so a disabled maintenance never strands files.

## `repair` post-mortem (asfp-cxx-lsp)

- `repair` and `move` no longer run `git worktree repair`. Besides the given
  path, Git checks every worktree of the repository and exits 1 when another
  tree's `.git` is a bridge symlink, so with two or more bridged trees every
  project's repair "failed". Those failures were written onto healthy
  projects, and the bridge, built from projects recorded as `done`, removed
  all of them, including the repo-hooks project, which made every repo
  command in the tree fail. The tool now rewrites the one admin `gitdir` link
  itself and sets project state from whether `HEAD` reads.
- The bridge includes projects by checking their `.git` on disk, refuses to
  write a bridge without any of the tree's projects, and runs `repo list -n`
  after writing, restoring the previous files if repo rejects them. A broken
  bridge must never be the result of a repair attempt.
- `status`, `detach`, and `remove`'s dirty check select projects by whether
  their checkout exists, not recorded state: a project wrongly recorded as
  failed was invisible to `status`, kept `detach` from releasing its branch,
  and skipped the dirty check before deletion.
- Dead overlay mounts are detected by opening the mount root: `stat` is served
  from the kernel's attribute cache after the daemon dies.

## Stateful fuzzing (`fuzz/`)

- `fuzz/harness.py` drives a tiny world with real repo clients through random
  upstream, manifest, source, tree, and failure operations (Hypothesis
  stateful testing) and compares trees with a fresh client. It found, beyond
  the multi-tree `repair` bug it was validated on:
  - `add` blocked by a `repo sync` of another client, and sync detection
    missing Homebrew's framework Python on macOS;
  - overlay registrations prunable whenever the overlay is unmounted, so a
    `git worktree prune` (including our own `remove` of another tree) after a
    reboot deleted them; overlay registrations are now locked;
  - a `linkfile` dropped from the manifest surviving `repo sync` in a tree,
    because repo did not know the tool had created it;
  - `remove` refusing a tree whose overlay daemon died, because its files
    could not be read; cleanliness now comes from the upper directory.
- The second growth added older releases creating trees (upgraded by
  `repair`), nested projects, submodules, a notdefault project from an
  included manifest, partial-clone and worktree-layout clients, a fake Gerrit
  with a recording pre-upload hook, rebase conflicts, shared-branch abandons,
  random kills, a full disk, read-only projects, unreachable remotes, and a
  separate race driver (`race.py`) for concurrent operations on one client.
  It found:
  - trees with nested projects never removable without `--force`: Git lists a
    nested repository as untracked in its parent;
  - submodules broken by `move` (their relative `core.worktree` and `.git`
    links), which Git's own `worktree move` refuses to attempt;
  - the second `repo sync` failing in worktree-layout trees, whose manifest
    checkout needs Git's gitfile and the shared `manifests.git`;
  - `repair` failing with an I/O error instead of `E_NO_SUCH_TREE` for a tree
    removed while it waited;
  - bridge builds failing when another tree wrote the shared manifest config
    at the same moment;
  - a lost maintenance request when one arrived while a run was finishing
    (found while triaging a harness bug that hid it).
- Each finding became an ordinary test in `tests/`; the harness stays a tool
  for finding the next ones, not part of the suite.

## Archived trees (`spec-archive.md`)

- `remove` archives before deleting, so cleanup never needs to judge whether
  work matters; `--discard` is the only destructive option. `restore`
  recreates the tree through `add` with the archived snapshot and commits.
- Index and working tree are stored as trees of a commit (parents: HEAD and an
  index commit) built with a temporary index and `git add -A`, so restore can
  set HEAD, index, and working tree exactly with two `read-tree` calls instead
  of replaying `git stash`, which does not round-trip every combination.
- Refs are written only for projects that need them (changes, unbranched
  commits, or a moved HEAD), so archiving a clean tree costs one status per
  project and no repository writes, and expiry deletes few refs.
- Projects with archived changes, a branch, or a moved HEAD are forced to
  worktree mode on restore: an overlay or shared checkout shows the source's
  files, not the archived commit's.
- Inspections run `git status` with `GIT_OPTIONAL_LOCKS=0` so that `status`
  does not rewrite indexes and make every tree look recently active to
  `remove --idle`.
