# git-repo-worktree — repo fidelity, scoped branches, inspection, pool and overlay (Spec Addendum)

**Status:** Normative addendum to `spec.md` v1.0-draft and `spec-fast-add.md`.
It responds to the field reports in `real-world-experience/`.

Where this addendum conflicts with `spec.md`, this addendum wins. It retires
non-goal 1 ("No `.repo` in created trees"), replaces policy P1, amends P4 and
P6, and raises the registry `format_version` to 3.

## 1. Metadata

Registry and breadcrumb `format_version` is 3. Readers accept 1, 2, and 3 and
reject newer values with `E_FORMAT_TOO_NEW`. Older binaries reject version 3
because they cannot safely remove trees with bridge links, overlay mounts, or
pooled projects.

New tree fields, all optional when reading older metadata:

```json
"branch_projects": ["tools/adt/idea"] | null,
"repo_bridge": null | {
  "state": "pending" | "done" | "failed" | "unsupported",
  "reason": null | "<text>",
  "manifests_git_dir": "/abs/client/.repo/manifests.git" | null,
  "manifests_branch": "repo-worktree/<tree>" | null
}
```

New project fields: `remote` and `upstream` (the `refs/...` a new branch
tracks), and `populate_ms` (duration of the last successful population), all
nullable. New tree field: `no_overlay` (default `false`). Project `mode` gains `"overlay"`; `strategy` gains
`"pool"` and `"fuse-overlayfs"`.

`branch_projects: null` with a non-null `branch` means every project
(trees created before scoping). A tree is complete when, in addition to the
existing rules, `repo_bridge` is null or in state `done` or `unsupported`.

## 2. Discovery

Walking up from the working directory, a directory containing a
`.repo-worktree` breadcrumb is checked before `.repo` at the same level. Commands
that accept a tree context proceed with the breadcrumb's source client. `add`
inside a tree fails with `E_NO_REPO_CLIENT`, naming the tree.

## 3. The `.repo` bridge

### 3.1 When

`add` builds the bridge unless `--git-only` is given. The phase runs after
directives and submodules and before locking, is journaled as step
`repo-bridge` with the resulting state, and is idempotent. `add --continue`,
`materialize` (when it converted a project), and `repair` rebuild it.

The bridge is `unsupported` (tree remains complete and git-only) when the source
has no `.repo/repo/main.py` or the snapshot contains `<submanifest>`. Any other
failure records state `failed` with a reason and leaves the tree `incomplete`.

### 3.2 Layout

`<tree>/.repo/` is a real directory containing:

* `repo` → absolute symlink to `<source>/.repo/repo`.
* Legacy source layout: `project-objects/` is a real directory with, for each
  included project, `<name>.git` → absolute symlink to
  `<source>/.repo/project-objects/<name>.git` (`repo gc` deletes every store in
  this directory that the tree's manifest does not list, so exposing the
  source's whole directory would let it delete shared projects' stores), and
  for every included project `projects/<path>.git` → absolute symlink to
  the project's worktree admin directory
  `<git_common_dir>/worktrees/<id>`. The project's `<tree>/<path>/.git` is
  replaced, atomically, by a relative symlink to
  `<tree>/.repo/projects/<path>.git`.
* Worktree source layout (`repo init --worktree`): `worktrees/` is a real
  directory with `<name>.git` → absolute symlink to
  `<source>/.repo/worktrees/<name>.git` for each included project. Projects keep
  Git's gitfiles.

A whole-directory link left by an older version is replaced by the real
directory when the bridge is rebuilt (for example by `repair`).
* `manifests/`: a linked worktree of `<source>/.repo/manifests.git` at the
  source manifest checkout's `HEAD`, on branch `repo-worktree/<tree>` (created
  with `-B`). The branch's `remote` and `merge` are copied from the source
  manifest branch; repo derives the manifest revision from them. If the source
  manifest checkout is detached or has no tracking configuration, the tree's is
  detached. If the source has no `manifests.git`, `manifests` is a symlink to
  the source's. `manifests/.git` becomes `../manifests.git`, and
  `manifests.git` links to its admin directory.
* `manifest.xml`: the source client's manifest selection — a copy of
  `<source>/.repo/manifest.xml`, or, when that is a symlink into `manifests/`,
  an equivalent `<include name="…"/>` — so repo in the tree resolves the same
  manifest from the tree's own manifest checkout. When the source has no
  readable `manifest.xml`, the pinned snapshot is generated instead (§3.3).
* `local_manifests/`: a symlink to each `*.xml` in the source's
  `local_manifests/` (stale links are removed) plus a generated
  `zz-git-repo-worktree.xml`, sorted last, with
  `<remove-project name path optional="true"/>` for every snapshot project the
  tree does not include.
* `project.list`: sorted paths of the included projects, newline-terminated.

The tree's projects are still populated at the snapshot's pinned revisions;
repo commands run later see the live manifest, so `repo sync` moves the tree to
upstream like any client.

Included projects are those with mode `worktree` or `overlay` in state `done`.
Shared, skipped, and failed projects are excluded so repo never acts on them.

Each admin directory gets relative links that repo reads directly and Git
ignores for linked worktrees: `config`, `objects`, `packed-refs`, `hooks`,
`rr-cache`, `shallow`, and `description` → `../../<name>` (when present in the
common directory; `packed-refs` always). Unless the common directory uses
reftable, `refs/` is a real directory holding `heads`, `remotes`, `tags`,
`published`, and every other non-per-worktree namespace present in the common
`refs/`, each linked to `../../../refs/<name>`. `refs/published` is created in
the common directory if missing. `bisect`, `worktree`, and `rewritten` are never
linked: Git stores them per worktree. `info` is never linked because
`info/sparse-checkout` is per worktree. Existing real entries are never
replaced.

### 3.3 Generated manifest (fallback)

Used only when the source has no readable `manifest.xml`. Serialize the
snapshot's root `<manifest>` children, dropping comments, and:

* keep `<project>` only for included paths (path, or name when path is absent);
* keep `<extend-project>` and `<repo-hooks>` only when they name an included
  project;
* drop `<superproject>`, `<remove-project>`, `<include>`, and `<submanifest>`;
* keep every other element (remotes, defaults, notices, annotations).

### 3.4 Lifecycle

Git's `worktree remove`, `move`, and `repair` require a gitfile. Before those
commands the tool rewrites a bridge `.git` symlink to `gitdir: <admin>` and,
when the worktree still exists afterwards, restores the original link.

`remove` removes the manifest worktree with `--force --force`, prunes the
manifest repository, and deletes the per-tree manifest branch. `prune` does the
same for trees whose directories are gone. When the tree is locked, project
`worktree remove` receives a second `--force`. `move` repairs the manifest
worktree registration.

### 3.5 What works

Inside a tree: `repo status`, `start`, `branches`, `diff`, `forall`, `info`,
`abandon`, `checkout`, `prune`, `upload`, `rebase`, and `sync` with or without
`-l`, `-n`, and `--rebase`. They behave as in an ordinary client of the same
manifest: `repo sync` moves detached projects to upstream and rebases topic
branches (published branches only with `--rebase`). They must leave shared
projects, their `.git`, the source's checkouts, and the source's manifest
checkout untouched. Projects new upstream are cloned by repo as ordinary
projects inside the tree and are not tracked by the tool.

Not supported in a tree: `repo init` (rewrites the bridge), `repo wipe` and
`repo gc` (delete shared object stores; the per-project links above make `gc`
ignore the source's other projects and make `wipe` fail rather than delete
through a link), and `repo sync --force-sync` (deletes the real paths behind a
project's gitdir and checkout when it judges them inconsistent). `--help` and
the bundled skill list them.

## 3.6 Locking (supersedes `spec.md` §3 lock rules)

Metadata files are always replaced atomically, so read-only commands (`list`,
`status`, `report`, `defaults list`, `pool list`, `detach`) take no lock and
skip entries that disappear while being read.

Writers wait for advisory `flock` locks, polling every 100 ms up to the timeout
given by global `--lock-timeout <seconds>`, else
`GIT_REPO_WORKTREE_LOCK_TIMEOUT`, else 300; `0` fails immediately. After one
second of waiting they print, unless `-q`, which lock they wait for and the
holder recorded in the lock file (pid, command, start time), repeating every
30 s. A timeout fails with `E_LOCKED` (exit 4) naming the holder. Waiting time
is recorded as history phase `lock-wait`.

* Per-tree lock `<entry>/lock`: held for the whole operation by `add`
  (after reservation), `add --continue`, `remove`, `lock`, `unlock`, `move`,
  `materialize`, and `repair` (each selected tree). Metadata is re-read after
  acquiring it; a tree removed meanwhile fails with `E_NO_SUCH_TREE`. `prune`
  skips trees whose lock is busy.
* Registry lock `.repo/repo-worktrees/lock`: held only by `add`'s reservation
  step (quiescence check, destination validation, name check, entry creation,
  and acquiring the new tree's lock), by `defaults` changes, and by
  `pool size N`.
* Pool locks `.repo/repo-worktrees/.pool/<encoded-path>.lock`: held while a
  spare is selected and moved, returned, published by `fill`, or cleared.

## 4. Scoped branches

`-b <branch>` requires `-p/--project <path>` (repeatable) or `--all-projects`;
otherwise exit 2 with `E_BRANCH_NEEDS_PROJECT_SCOPE` before anything is
created. `--project` and `--all-projects` require `-b` and conflict with each
other. Branch projects are resolved (§5) at pin time and recorded in
`branch_projects`.

Only branch projects receive the branch and are forced to mode `worktree`; all
other projects are detached and keep their normal mode selection. A branch
project may take a pool spare: fail with `branch-exists` before claiming one if
the branch exists, then `git checkout -q -b <branch> <pin>` in the spare.

After creating the branch, set `branch.<name>.remote` to the project's remote
and `branch.<name>.merge` to its upstream, both computed at pin time:

* remote: the project's `remote`, else `<default remote>`;
* upstream, the first non-object-ID value of: project `dest-branch`, project
  `upstream`, project `revision`, `<default dest-branch>`, the remote's
  `revision`, `<default revision>`; prefixed with `refs/heads/` unless it
  starts with `refs/`. `<extend-project>` may override `remote`,
  `dest-branch`, and `upstream`.

If either value is unknown, no tracking is configured.

## 5. Project path resolution

`--full-project`, `-p/--project`, `materialize`, `defaults` (except `auto`),
`detach --project`, and `pool fill|clear` resolve each input to a manifest
project:

1. An input that is absolute or begins with `./` or `../` is a filesystem path.
   It is made absolute against the working directory and must lie inside the
   tree root or source root; the remainder is looked up.
2. Otherwise the input must be a safe relative path. Look it up.
3. If not found and the working directory is inside a root, look up the
   working directory's relative path joined with the input.

Lookup returns the exact project, else the longest project path that is a
component-wise prefix of the input. Inputs resolving to the same project are
deduplicated. Rewrites are reported on stderr (`resolved ... to project ...`);
`materialize --json` includes `resolved`.

Unresolved inputs fail with `E_UNKNOWN_PROJECT`: exit 2 for `add` flags, exit 1
elsewhere. `defaults auto` accepts manifest-style inputs verbatim so stale
entries can always be removed.

## 6. Inspection and `status`

A worktree-backed project is inspected with one
`git status --porcelain=v2 -z --branch --untracked-files=normal` plus, when
`HEAD` differs from the pinned SHA,
`git rev-list --count HEAD --not <pinned> --branches --remotes --tags`.
Overlay projects whose upper directory holds only `.git` skip `git status`.

The inspection records `head`, `branch`, `modified`, `untracked`,
`unbranched_commits`, `mounted` (overlay only), `error`, and `reasons` from
`modified`, `untracked`, `unbranched-commits`, `unverifiable`. A project is
dirty when any reason applies. Inspections run in parallel with `-j` workers.

`git repo-worktree status [<tree>]` (default: the tree containing the working
directory) prints dirty or branched projects (all with `-v`) and returns
`{name, path, state, dirty, repo_bridge, projects, shared}`.

`remove` without `--force` inspects every project and, when any is dirty, fails
with `E_DIRTY`, a message listing up to ten projects with their reasons, and
`error.details.dirty_projects` holding every dirty inspection.

Tree results — `list --json` per tree, and the results of `add`,
`add --continue`, `repair`, and `materialize` — omit `projects`, `copyfiles`, and
`linkfiles`, adding `project_counts` (non-skipped projects by mode), `skipped`,
`failed_projects` (`path`, `reason`), and `failed_directives` (`dest`,
`reason`); with `-v` they report full tree metadata. Either way they add
`checked_out_branches: [{"project", "branch"}]`,
read from each worktree's `HEAD` without spawning Git (falling back to
`git symbolic-ref` for reftable's placeholder `HEAD`).

The JSON error object gains an optional `details` member.

## 7. `detach`

```text
git repo-worktree detach [<tree> | --source] [--branch <b>] [-p <path>]... [-n]
```

Locations: `--source`, the named tree, every tree plus the source when only
`--branch` is given, or the tree containing the working directory. Anything
else is a usage error. For each selected project location on a branch (matching
`--branch` when given), run `git checkout -q --detach`, which keeps the index and
working tree. `--dry-run` reports without changing anything. The result is
`{detached: [{location, project, branch, path}], failed: [...], dry_run}`;
failures produce `E_PARTIAL` (exit 3). The registry lock is held exclusively.

## 8. `materialize`

Requested projects already in mode `worktree` are reported in
`already_materialized` and are not an error. `symlink-shared` projects convert
as before. Overlay projects convert only when Git reports no modified or
untracked files: unmount, restore `.git` from the upper directory, and populate
with `reset --hard`. Files that exist only in the overlay and are ignored by
Git are discarded. A refused overlay stays mounted and `done`. The result adds
`materialized` and `already_materialized`.

## 9. Pool mode

A project whose final mode is `worktree` uses the pool when its default is
`pool`, or when it has no default and its estimated checkout time,
`est_files × 60 µs + est_bytes ÷ 500 MB/s` (`est_files` counts tracked blobs
alongside `est_bytes`), is at least 1.5 s. Metadata without `est_files` falls back to `est_bytes` above 500 MiB.
`clone-depth` does not exclude a project: a fixed-depth project becomes a
worktree when its source is too dirty to share or overlay. Any other explicit
default disables pooling. Pool use adds strategy `pool`.
Spares are detached linked worktrees under
`.repo/repo-worktrees/.pool/<encoded-path>/<unique>`, where the encoding
replaces `%` with `%25` and `/` with `%2F`. The tree name `.pool` is reserved
(`E_BAD_NAME`).

On `add`, for a pool project, take a spare on the same filesystem as
the destination's parent, `git worktree move` it to the destination, and
`git checkout -q --detach <pin>` with parallel checkout. If no spare is usable
or the move fails, populate normally. A failed checkout after the move fails the
project; resume repairs it with `reset --hard`.

On `remove` (without `--no-pool`), a pool project that is a `done` worktree,
contains no other project, and is not dirty returns to the pool when it has
fewer spares than the pool size (`pool size [N]`, stored as `pool_spares` in
`defaults.json`, default 2): convert a bridge `.git` to a gitfile, unlock it,
`git worktree move` it into the pool, `git checkout -q --detach`, and
`git clean -ffdxq`. Candidates are returned in parallel with `-j` workers.
Failures are warnings; the project is then removed normally. The result adds `returned_to_pool`.

`pool list` (alias `pool status`), `pool size [N]`,
`pool fill [<path>...] [--count N]`, and `pool clear [<path>...] [--orphaned]`.
With no paths, `fill` covers the kept projects (below); `--count` defaults to
the pool size. `fill` creates spares at the
source checkout's `HEAD` under a `.filling-` name without holding the registry
lock and publishes each with `git worktree move` under the lock, retrying lock
contention.

### 9.1 Kept and orphaned spares

A project's spares are kept only if a default `add` would take them, judged
with the current `defaults.json` and source checkout. The project is *orphaned*
when the first of these applies:

1. It is not in `.repo/project.list`.
2. Its default is anything other than `pool` (a `pool` default always keeps).
3. Its source checkout is missing.
4. Its estimated checkout time is under 1.5 s.
5. It is a fixed-depth (shallow) clone, the client is not all fixed-depth,
   and its source checkout is clean (`git status --porcelain
   --untracked-files=all` is empty): a default `add` shares or overlays it.

Candidates are explicit `pool` defaults, projects with a pool directory, and
worktree projects pooled in any registered tree. The estimate uses facts
recorded beside the pool in `.pool/<encoded-path>.json` (`est_files`,
`est_bytes`, `shallow`), else a registered tree's metadata, else
`git ls-tree -rlz HEAD` and `git rev-parse --is-shallow-repository` in the
source checkout. `remove` returns a project to the pool only when it is kept,
and records its facts.

`pool list` reports, besides `spares` (each with `orphaned`), `projects`
(project, `spares`, `bytes_per_spare` ≈ `est_bytes + 4096 × est_files`, and
`orphaned` reason or null), `capacity`, `depth` (the fewest spares among kept
projects: how many consecutive adds take every pooled project from the pool;
null with no kept projects), `orphaned_spares`, and `orphaned_bytes`.
`pool clear --orphaned` deletes only orphaned projects' spares, intersected
with any given paths.

### 9.2 Background maintenance

After `add` (complete or partial) when the client has a `.pool` directory or
the tree has pooled projects, and after `remove` without `--no-pool` when any
of the tree's pooled projects has fewer spares than the pool size, start
`git-repo-worktree -C <source> -q pool maintain` detached: in its own process
group with null standard streams, under `nice -n 19` and
`ionice -c 3 -t` where those programs exist. The command records
`pool_maintenance: "started"` in its history. `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE`
set to a non-empty value other than `0` disables this.

`pool maintain` (hidden) exits at once, recording `skipped`, if another run
holds `.pool/.maintain.lock`. Otherwise, for up to five rounds, until a round
changes nothing: plan kept and orphaned projects; delete orphaned projects'
spares and facts; remove spares beyond the pool size; then refill kept
projects one spare at a time, bringing every kept project to one spare before
any gets two, slowest checkout first, with `checkout.workers=2`. A refill is
published only while the pool is below the pool size, and is skipped
(`deferred`) when it would leave less free space on the pool's filesystem
than 10% of its size, capped at 50 GB.

Maintenance yields to tree operations, detected by trying each registered
tree's `lock` without waiting. Before deleting a project's spares and before
starting a refill it waits, polling every 250 ms, while any tree lock is
held. A refill's `git worktree add` runs in its own process group, which is
sent `SIGSTOP` while any tree lock is held and `SIGCONT` once none is: idle
I/O priority does not keep its buffered writes from throttling a concurrent
`add`. History records `reaped`, `trimmed`, `filled`, `failed`, `deferred`,
`system_start`, `system_end`, and phases `plan`, `reap`, `fill`, and `paused`
(milliseconds spent waiting or stopped).

`remove` skips the cleanliness check of a project whose pool is already full.

## 10. Overlay mode (Linux)

A sharing candidate becomes an overlay candidate when its default is `overlay`,
or when it has no default (clone-depth inference) and overlay support is
available. `add --no-overlay`, or `GIT_REPO_WORKTREE_NO_OVERLAY` set to a
non-empty value other than `0`, disables overlays for the tree (recorded as
`no_overlay`; inferred candidates are then symlink-shared and explicit
`overlay` defaults demote to worktrees). `add` prints one note when it created
overlays.

An overlay candidate differs from a symlink candidate as follows: directive destinations inside it do not demote it, and it demotes
to `worktree` (reported without `-v`) when overlay support is unavailable —
not Linux, `fuse-overlayfs` or `fusermount3`/`fusermount` missing from `PATH`,
or no `/dev/fuse`. All other sharing demotions apply. At population time a
source that is no longer clean at the pin falls back to a worktree unless the
overlay was already started.

Population, idempotent at every step:

1. Create `<entry>/overlays/<encoded-path>/{upper,work}`.
2. `git -C <source>/<path> worktree add --no-checkout --detach <tree>/<path> <pin>`.
3. Copy the source checkout's index (`--git-path index`) into the admin
   directory, or `git read-tree <pin>` when `core.splitIndex` is true.
4. Move `<tree>/<path>/.git` to `upper/.git`.
5. `fuse-overlayfs -o lowerdir=<source>/<path>,upperdir=…,workdir=… <tree>/<path>`.
   Paths containing `,` or `:` are rejected.
6. `git update-index --no-fsmonitor --no-untracked-cache`, then verify `HEAD`.
   On failure, lazily unmount.

The overlay presents the source's stat data, so the copied index is trusted
without reading file contents; no `core.checkStat` or `st_dev` adjustment is
needed.

`remove`, `move`, and `materialize` unmount first with `fusermount -u`;
`remove --force` uses `-uz`. For `remove` and `move` a busy mount otherwise
fails with `E_OVERLAY_BUSY` before any state changes; `materialize` reports the
project as failed and leaves it mounted. `remove` deletes the admin directory's `locked` file
so the registration can be pruned. `repair` and `move` remount overlays whose
upper directory has a `.git`; a failed remount marks the project `failed`
(reason `remount: …`), and a later successful remount clears it. Bridge
operations on an unmounted overlay act on `upper/.git`, resolving its relative
link against the mount point.

## 10.1 Processes using a removed tree

On Linux, after removing worktrees and before deleting the tree directory,
`remove` scans `/proc` for other processes whose working directory is inside the
tree or with an argument equal to, ending with, or containing the tree path plus
`/`, excluding the tool itself and its ancestor processes (read from
`/proc/<pid>/stat`), whose command lines often name the tree. Up to 20 are reported as `lingering_processes: [{pid, command, reason}]`
(`reason` is `cwd` or `arguments`, `command` the first 120 bytes of the
arguments) and in a warning on stderr. They are never signalled.

## 10.2 Reflink probe

Before populating projects, unless `--no-reflink`, `add` reflinks one 4 KiB file
from the registry entry to the tree root. Failure marks the source/tree
filesystem pair unsupported for the run, so projects skip reflink attempts.

## 10.3 Site hooks

Hooks are files named `post-add` and `pre-remove` in
`.repo/repo-worktrees/hooks/` of the source client. They are skipped with the
global `--no-hooks` flag or `GIT_REPO_WORKTREE_NO_HOOKS` set to a non-empty value
other than `0`.

* `post-add` runs when a tree is complete: in `add` and `add --continue` after
  the bridge phase and before locking, and in `repair`. It does not run again
  once its recorded status is `ok`. `add` journals `hook-post-add` with the
  status.
* `pre-remove` runs in `remove` after the dirty and lock checks pass and before
  any overlay is unmounted or worktree removed, unless the tree is already in
  state `removing`. It also runs with `--force`.

A hook runs with the tree root as working directory, stdin from `/dev/null`,
stdout redirected to the tool's stderr, and environment `GIT_REPO_WORKTREE_EVENT`,
`GIT_REPO_WORKTREE_TREE`, `GIT_REPO_WORKTREE_NAME`, and
`GIT_REPO_WORKTREE_SOURCE`, in its own process group. After
`GIT_REPO_WORKTREE_HOOK_TIMEOUT` seconds (default 600) the group receives
`SIGTERM`, then `SIGKILL` 200 ms later. A non-executable file fails with a
reason suggesting `chmod +x`.

The result is recorded in tree metadata `hooks.<event>`
(`status`: `ok` | `failed` | `timeout` | `skipped`, `exit_code`, `ms`, `at`,
`reason`; `skipped` means the hook was installed but hooks were disabled, so
`repair` runs a skipped `post-add`),
returned in `add`, `repair`, and `remove` results, printed as a line on
stderr, and pushed to the history record as `hooks[]`. `report` summarizes
statuses and latency per event. Hook results never change exit codes.

`git repo-worktree hooks list` reports `directory`, `disabled`, and per event
`path`, `installed`, and `executable`.

## 11. Installation and help

Release archives contain `git-repo-worktree.1` and one
`git-repo-worktree-<subcommand>.1` per subcommand. The installer installs them
into `--man-dir` (default: `<prefix>/share/man` when the install directory is
`<prefix>/bin`, else `~/.local/share/man`), skips them with `--no-man`, and
suggests `MANPATH` when `man -w` cannot find the page. After installing, it
warns when the `git-repo-worktree` found on `PATH` is a different file whose
contents differ (`cmp`), because Git prefers `PATH` commands over aliases.

`version` prints `git-repo-worktree <version> (<commit>)` when built from a Git
checkout (the build script exports the 12-character commit as `GRW_COMMIT`);
`version --json` and history records include `commit`.

`add`'s help (and its manual page) states the starting-revision rule: projects
start detached at the source checkout's current commits, never at remote tips.

## 12. Usage history

Every invocation that identifies a source client whose registry exists appends
one JSON object line to `.repo/repo-worktrees/history.jsonl` (no `fsync`). When
the file exceeds 8 MiB it is atomically rewritten to its newest whole lines of
at most 4 MiB. `GIT_REPO_WORKTREE_NO_HISTORY` set to a non-empty value other
than `0` disables recording. History failures never change a command's result.

Every record has `v` (1), `ts`, `tool_version`, `command`, `args` (subcommand
and flag names only; `--flag=value` keeps `--flag`, short flags keep two
characters, other arguments are dropped), `ms`, `exit_code`, `error` (code or
null), `via_git` (`GIT_EXEC_PATH` present), optional `session`
(`GIT_REPO_WORKTREE_SESSION`), and optional `phases` (milliseconds per phase).
Usage errors rejected by the parser are recorded with `error: "E_USAGE"` when
the client can be discovered from `-C` or the working directory. Commands
add:

* `add`: `tree`, `state`, `skipped`, `repo_bridge` state, `projects` (path,
  mode, strategy, state, `ms`, `queue_ms` (time from becoming eligible, at the
  start of the phase or when its parent project finished, to starting; it
  includes waiting behind other projects), `bytes`, `pooled`, reason),
  `estimate_cache_hits` (projects whose size estimate came from
  `.repo/repo-worktrees/estimates.json`, which maps each commit to its tracked
  bytes and file count and is rewritten with the current snapshot's entries
  when any was missing), `notes` (per-project population
  fallbacks), `reflink` (a reason that applied to every project, such as an
  unsupported filesystem pair, recorded once instead of per project),
  `environment` (os, arch, kernel, cpus, git, repo, source and tree filesystem
  types, `overlay_unavailable`), and `system_start`/`system_end`. Phases add
  `snapshot-repo-manifest`, `snapshot-git-dirs`, `snapshot-estimates`, and
  `snapshot-preflight` (parts of `snapshot`), `projects-journal-sync` and
  `projects-meta-write` (time the journal writer thread spent syncing, off the
  scheduler's path), and `projects-idle-slot` (worker-slot milliseconds between
  a project finishing and the scheduler starting the next one in its slot;
  slots left empty because no work remained are not counted).
* `remove`: `tree`, `force`, `inspected`, `slow_inspections` (≥ 100 ms, at most
  20), `dirty` (project and reasons) on refusal, `returned_to_pool`, `projects`,
  `pruned_git_dirs`, `system_start`/`system_end`, and phases `inspect`,
  `unmount`, `pool-return`, `worktree-remove`, `delete`, `prune`.
* `system_start`/`system_end`: `loadavg` (1, 5, 15 minutes) and, on Linux,
  `pressure` from `/proc/pressure/{cpu,io,memory}` (`some`/`full` with
  `avg10`, `avg60`, `avg300`); null where unavailable.
* `status`: `inspected`, `slow_inspections`. `detach`: `detached`, `failed`.
* `materialize`: `projects` (path, previous mode, `ms`, ok, reason).
* `repair`: `trees` (name, `remount_failures`). `pool fill`: `spares`.

`git repo-worktree report [--session <label>] [--last <n>]` summarizes the
selected events and the live trees: per-command latency (count, p50, p90, max),
successes, error codes, `via_git` count, and per-phase latency; usage-error
shapes; `add` mode, strategy, failure, and note counts; pool hits and misses
among pooled projects; the 15 slowest projects by median population time;
`remove` dirty reasons and projects returned to the pool; the slowest
inspections; the most recent `add` environment; and `history_path`.

## 13. Required tests

1. Real-repo client: `repo start`, `branches` (current only in the tree),
   `forall`, `sync -l` (links preserved, shared project and source untouched),
   networked `repo sync` moving detached projects to upstream, `--rebase`
   replaying a published branch,
   `move`, locked `remove --force --force`, and complete cleanup of project
   registrations, the manifest worktree, and its branch.
2. Per-worktree refs written in a tree do not appear in the source.
3. Discovery from inside a tree, and refusal of `add` there.
4. Worktree-layout sources; `--git-only` trees.
5. Structured `E_DIRTY` including stranded detached commits; `status`.
6. Scoped `-b` with subpath resolution and tracking configuration; unscoped
   `-b` rejected; `checked_out_branches`; `detach --branch` and `--source`.
7. Subpath resolution for `--full-project` and `defaults`; idempotent
   `materialize`.
8. Pool reuse, return on `remove` with ignored files cleaned, `--no-pool`,
   `fill`, and `clear`; orphaned spares reported by `pool list`, skipped by
   `fill` and `remove`, deleted by `clear --orphaned` and `pool maintain`;
   maintenance refilling to the pool size, and running in the background
   after `add`.
9. Linux with FUSE: overlay isolation, upper-only cleanliness, `repair`
   remount, `move`, `materialize`, dirty-source fallback, and repo commands in
   an overlay project.
10. History records argument shapes without values, sessions, phases,
    per-project timings, early usage errors, and honors
    `GIT_REPO_WORKTREE_NO_HISTORY`; `report` aggregates them.

— end of addendum —
