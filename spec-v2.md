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
* Legacy source layout: `project-objects` → `<source>/.repo/project-objects`,
  and for every included project `projects/<path>.git` → absolute symlink to
  the project's worktree admin directory
  `<git_common_dir>/worktrees/<id>`. The project's `<tree>/<path>/.git` is
  replaced, atomically, by a relative symlink to
  `<tree>/.repo/projects/<path>.git`.
* Worktree source layout (`repo init --worktree`): `worktrees` → absolute
  symlink to `<source>/.repo/worktrees`. Projects keep Git's gitfiles.
* `manifests/`: a linked worktree of `<source>/.repo/manifests.git` at the
  source manifest checkout's `HEAD`, on branch `repo-worktree/<tree>` (created
  with `-B`). The branch's `remote` and `merge` are copied from the source
  manifest branch; repo derives the manifest revision from them. If the source
  manifest checkout is detached or has no tracking configuration, the tree's is
  detached. If the source has no `manifests.git`, `manifests` is a symlink to
  the source's. `manifests/.git` becomes `../manifests.git`, and
  `manifests.git` links to its admin directory.
* `manifest.xml`: generated from the pinned snapshot (§3.3).
* `project.list`: sorted paths of the included projects, newline-terminated.

No `local_manifests` directory is created; the snapshot already merged them.

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

### 3.3 Generated manifest

Serialize the snapshot's root `<manifest>` children, dropping comments, and:

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
`abandon`, `checkout`, `prune`, `upload`, and `sync -l`. `repo sync -l` returns
projects to the tree's pinned revisions and must leave shared projects, their
`.git`, and the source untouched. A networked `repo sync` in a tree updates only
the tree's manifest checkout, keeps projects pinned, and, as in any client, sets
each project's shared `refs/remotes/m/<branch>` to the revision it synced.

## 4. Scoped branches

`-b <branch>` requires `-p/--project <path>` (repeatable) or `--all-projects`;
otherwise exit 2 with `E_BRANCH_NEEDS_PROJECT_SCOPE` before anything is
created. `--project` and `--all-projects` require `-b` and conflict with each
other. Branch projects are resolved (§5) at pin time and recorded in
`branch_projects`.

Only branch projects receive the branch and are forced to mode `worktree`; all
other projects are detached and keep their normal mode selection. Pool spares
are not used for branch projects.

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

`list --json` adds `checked_out_branches: [{"project", "branch"}]` per tree,
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

A project uses the pool when its default is `pool`, or when it has no default,
no `clone-depth`, and `est_bytes` above 500 MiB (the heavy threshold of
`spec-fast-add.md` §8.1). Any other explicit default disables pooling. Pool use
keeps the project in mode `worktree` and adds strategy `pool`.
Spares are detached linked worktrees under
`.repo/repo-worktrees/.pool/<encoded-path>/<unique>`, where the encoding
replaces `%` with `%25` and `/` with `%2F`. The tree name `.pool` is reserved
(`E_BAD_NAME`).

On `add`, for a non-branch pool project, take a spare on the same filesystem as
the destination's parent, `git worktree move` it to the destination, and
`git checkout -q --detach <pin>` with parallel checkout. If no spare is usable
or the move fails, populate normally. A failed checkout after the move fails the
project; resume repairs it with `reset --hard`.

On `remove` (without `--no-pool`), a pool project that is a `done` worktree,
contains no other project, and is not dirty returns to the pool when it has
fewer than one spare: convert a bridge `.git` to a gitfile, unlock it,
`git worktree move` it into the pool, `git checkout -q --detach`, and
`git clean -ffdxq`. Failures are warnings; the project is then removed
normally. The result adds `returned_to_pool`.

`pool list`, `pool fill [<path>...] [--count N]` (default: pool-default projects,
one spare each), and `pool clear [<path>...]`. `fill` creates spares at the
source checkout's `HEAD` under a `.filling-` name without holding the registry
lock and publishes each with `git worktree move` under the lock, retrying lock
contention.

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

## 11. Installation and help

Release archives contain `git-repo-worktree.1` and one
`git-repo-worktree-<subcommand>.1` per subcommand. The installer installs them
into `--man-dir` (default: `<prefix>/share/man` when the install directory is
`<prefix>/bin`, else `~/.local/share/man`), skips them with `--no-man`, and
suggests `MANPATH` when `man -w` cannot find the page. After installing, it
warns when another `git-repo-worktree` earlier on `PATH` reports a different
version, because Git prefers `PATH` commands over aliases.

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
  mode, strategy, state, `ms`, `bytes`, `pooled`, reason), `notes` (population
  fallbacks), and `environment` (os, arch, kernel, cpus, git, repo, source and
  tree filesystem types, `overlay_unavailable`).
* `remove`: `tree`, `force`, `inspected`, `slow_inspections` (≥ 100 ms, at most
  20), `dirty` (project and reasons) on refusal, `returned_to_pool`, `projects`,
  and phases `inspect`, `unmount`, `pool-return`, `worktree-remove`, `delete`,
  `prune`.
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
   `fill`, and `clear`.
9. Linux with FUSE: overlay isolation, upper-only cleanliness, `repair`
   remount, `move`, `materialize`, dirty-source fallback, and repo commands in
   an overlay project.
10. History records argument shapes without values, sessions, phases,
    per-project timings, early usage errors, and honors
    `GIT_REPO_WORKTREE_NO_HISTORY`; `report` aggregates them.

— end of addendum —
