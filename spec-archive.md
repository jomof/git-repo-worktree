# git-repo-worktree — archived trees (Spec Addendum)

**Status:** Implemented in 2.2.0. Normative addendum to `spec.md`, `spec-v2.md`,
and `spec-fast-add.md`; where it conflicts with them, it wins.

## 0. Problem and contract

Agents leave trees behind: work finished but not cleaned up, chats that are
quiet but may resume, chats that will not. Some of those trees hold work
(uncommitted changes, unbranched commits) that `remove` correctly refuses to
discard, so nobody can clean up without judging whether that work matters.

This addendum changes the contract:

> **Removing a tree never loses information.** `remove` records the tree's
> state in an *archive* before deleting it, and `restore` recreates the tree
> from that archive: the same project commits, branches, staged changes,
> working-tree changes, and untracked files. Archives expire under a
> retention policy, like Git's reflog.

So cleanup only has to answer "is anything using this tree right now?", not
"does this work matter?". A returning agent whose tree is gone runs
`git repo-worktree restore <name-or-path>` and continues.

Non-goals: archiving ignored files (build outputs, caches); archiving state
outside Git (running processes, Bazel output bases); syncing archives across
machines.

## 1. What an archive holds

A tree's state is, per worktree-backed project: `HEAD`, the checked-out branch
if any, the index, and the working tree (modified, deleted, and untracked
files that are not ignored). Plus, per tree: its name, path, creation options,
and its manifest checkout's commit.

Not archived: ignored files; shared (`symlink-shared`) projects, which belong
to the source; skipped projects; projects the tree never populated.

### 1.1 Per-project objects

For each project whose mode is `worktree` or `overlay`, not skipped, and whose
checkout exists, in the project's own repository (the shared object store):

1. `H` = `HEAD` commit. `B` = branch from `symbolic-ref -q HEAD`, or none.
2. `I` = the index as a tree (`git write-tree`).
3. `W` = the working tree as a tree: copy the index to a temporary index file,
   `git add -A` into it with pathspec exclusions for the tree's projects
   nested inside this one, and `git write-tree`. `W` respects `.gitignore`,
   includes untracked files, and omits deleted files.
4. If `I` and `W` both equal `H^{tree}`, the project's archive commit `A` is
   `H` itself. Otherwise `A` = `git commit-tree W -p H -p I'` with message
   `repo-worktree archive <id> <path>`, where `I'` = `git commit-tree I -p H`.
5. Ref `refs/repo-worktree/archive/<id>` → `A` in the project's repository
   when `A` differs from `H`, `H` has unbranched commits, or `H` differs from
   the commit the tree was created at. It keeps `H`, any unbranched commits
   behind it, `I`, and `W` reachable. A project still at its starting commit
   with no changes gets no ref, so archiving a clean tree writes nothing to
   its repositories.

Restoring sets `HEAD` to `H`, the index to `I`, and the working tree to `W`,
which reproduces staged, unstaged, deleted, and untracked state exactly
(unlike `git stash`, which does not round-trip every combination).

### 1.2 Per-tree record

`.repo/repo-worktrees/.archive/<id>/archive.json` plus a copy of the tree's
`snapshot.xml` with each project's revision set to its `H`. `<id>` is
`<tree-name>-<UTC yyyymmddThhmmssZ>`, made unique with a counter. Users refer
to archives by id or as `<tree>@{n}`, newest first, like `stash@{n}`.

```json
{
  "format_version": 1,
  "id": "agent-1-20261019T185012Z",
  "tree": "agent-1",
  "path": "/abs/path/of/the/tree",
  "archived_at": "2026-10-19T18:50:12Z",
  "source_root": "/abs/source/client",
  "archived_unix": 1792435812,
  "tool_version": "2.2.0",
  "reason": "remove" | "remove --idle" | "prune",
  "keep": false,
  "options": { "branch": null, "branch_projects": null, "submodules": false,
               "full": false, "full_projects": [], "default_modes": {},
               "no_overlay": false, "git_only": false },
  "manifests": { "branch": "repo-worktree/agent-1", "commit": "<sha>" } | null,
  "projects": [
    { "path": "tools/base", "name": "platform/tools/base", "mode": "worktree",
      "git_common_dir": "/abs/.repo/projects/tools/base.git",
      "pinned": "<sha the tree was created at>",
      "head": "<H>", "branch": "topic" | null, "archive": "<A>" | null,
      "index": "<I>" | null, "worktree": "<W>" | null, "referenced": true,
      "modified": 2, "untracked": 1, "unbranched_commits": 3,
      "bytes": 18234 }
  ],
  "bytes": 18234,
  "dropped": []
}
```

`bytes` per project is `git rev-list --objects --disk-usage A --not --branches
--remotes --tags` (objects only this archive keeps alive); the tree's `bytes`
is their sum. `dropped` lists state that `--force` let the archive omit
(§2.2).

The manifest repository gets a ref `refs/repo-worktree/archive/<id>` to the
tree's manifest checkout commit, when the tree has one.

The record is written before any ref, with `"complete": false`, and rewritten
with `"complete": true` once every ref exists. Listing and selection ignore
incomplete records; retention deletes incomplete records older than an hour,
with their refs, and reserved directories that never got a record. A capture
killed at any point therefore never leaves refs that nothing lists.

## 2. `remove`

### 2.1 Behavior

`remove <tree>` archives, then removes, in this order:

1. Locked trees still need `--force --force` (unchanged).
2. Inspect projects (unchanged), which now feeds the archive instead of
   refusing on dirty projects.
3. Remount any overlay project that is unmounted or dead (as
   `repair --mounts-only`). If that fails and the overlay has changes, the
   project is `unverifiable` (§2.2).
4. Capture the archive (§1). Objects and refs are written before anything is
   deleted, and the archive record is written last, atomically. A failure
   here aborts the removal with nothing deleted.
5. `pre-remove` hook, unmount, pool return, deletion (unchanged). Pool return
   runs after capture because it cleans the project.

Every removal is archived, clean or not: a clean tree's archive is a few
kilobytes of metadata, and it lets a returning agent restore exactly the
commits it was on even after the source client moved.

Flags:

* `--discard`: do not archive. The only way to discard unsaved work. Dirty
  projects are then deleted as `--force` deletes them today.
* `--force`: still needed only to proceed past a busy overlay (lazy unmount)
  and, twice, for locked trees. It no longer discards anything.
* `--no-pool`, `-j`: unchanged.

`remove` no longer returns `E_DIRTY`; §2.2 replaces it.

The result adds `archive`: `{ "id", "bytes", "projects_with_changes" }`, or
null with `--discard`. History records `archive_id`, `archive_bytes`, and
phase `archive`.

### 2.2 State an archive cannot hold

`remove` refuses with `E_ARCHIVE_INCOMPLETE` (exit 1), listing each item and
why, when a project has:

* an operation in progress (`rebase-merge/`, `rebase-apply/`, `MERGE_HEAD`,
  `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `BISECT_LOG`) or unmerged index entries;
* a populated submodule with uncommitted changes of its own;
* an untracked Git repository nested in the project that is not one of the
  tree's projects (only its commit would be archived);
* projects inside the tree that `repo sync` added and the tool does not manage
  (their object stores live inside the tree and would be deleted with it);
* a project whose state cannot be read (`unverifiable`).

`--force` archives everything else and records each omission in `dropped`
(the in-progress operation's state, the submodule's changes, the unmanaged
project), so the loss is explicit and visible in `archive show`. `--discard`
skips archiving entirely.

### 2.3 Idle trees

`remove --idle <duration> [--dry-run]` (no tree argument) removes, with
archives, every tree that:

* is not locked, and its state is `complete` or `incomplete` (not `creating`,
  `moving`, or `removing`);
* has no process using it (working directory inside it, or arguments naming
  it; the same check `remove` reports as `lingering_processes`), not counting
  the tree's own `fuse-overlayfs` daemons;
* has had no activity for at least `<duration>` (`30m`, `24h`, `7d`).

*Activity* is the newest of: the mtimes of each project's per-worktree
`HEAD`, `index`, and `logs/HEAD`; the mtimes of files `git status` reports as
modified or untracked; and the tree's `meta.json`. The result lists `removed`
(each with its archive) and `skipped` (each with its reason). `--dry-run`
removes nothing.

`status --json <tree>` reports `last_activity_unix`, `idle_secs`, `processes`,
and `locked` so an agent can make the same judgement for one tree. Inspections
run `git status` with `GIT_OPTIONAL_LOCKS=0`, so `status` and `remove --idle`
never refresh an index and make a tree look active.

## 3. `restore`

```sh
git repo-worktree restore <archive-id | tree-name | tree-path> [<path>] [--name <name>] [--keep]
```

The selector matches an archive id, then the newest archive of a tree name,
then the newest archive whose recorded path equals the given path (agents
remember paths). `<path>` defaults to the recorded path; `--name` defaults to
the recorded name.

1. Refuse if the name is taken (`E_NAME_EXISTS`) or the path is not empty
   (`E_DEST_NOT_EMPTY`); the message names `--name` and `<path>`.
2. Create the tree as `add` does, with the recorded options, from the
   archive's `snapshot.xml` (each project pinned to its `H`) instead of
   `repo manifest -r`. Population uses the pool and overlays as usual.
3. For each project whose `A` is not `H`: `git read-tree -u --reset W`, then
   `git read-tree I`. This sets the working tree to `W` and the index to `I`
   without changing `HEAD`.
4. Branches: if `B` no longer exists, recreate it at `H`. If `B` points at
   `H`, check it out with `git switch` (same commit, so index and working tree
   are kept). Otherwise, or if `B` is checked out in another worktree, leave
   `HEAD` detached at `H` and report why in `reason`.
5. Point the tree's manifest branch at the recorded manifest commit when it
   still exists.
6. Run the `post-add` hook (unchanged `add` behavior).
7. On full success, delete the archive (refs and record) unless `--keep`, like
   `git stash pop`. If any project fails, keep the archive, leave the tree
   `incomplete` with per-project reasons, and exit 3.

The result is the tree summary plus `restored`: per project
`{ "path", "changes_applied", "branch", "branch_restored", "reason" }`.

Restore never moves a tree to the source's current commits; afterwards it is
an ordinary tree, so `repo sync` updates it.

## 4. Managing archives

```sh
git repo-worktree archive list [--json]          # id, tree, path, archived_at, bytes, changes, keep, expires_at
git repo-worktree archive show <id|tree@{n}>     # per-project changes (git diff --stat H..A), dropped items
git repo-worktree archive drop <id|tree@{n}>...  # delete refs and record
git repo-worktree archive keep <id|tree@{n}>     # exempt from expiry; `--unkeep` reverses
git repo-worktree archive prune [--dry-run]      # apply retention now
git repo-worktree archive policy [--expire <duration>] [--budget <size>]
```

`list --json` and `status` show live trees only. `add` to a path that matches
an archive's recorded path prints a hint naming `restore`.

## 5. Retention

Defaults, stored in `defaults.json` as `archive_expire` and `archive_budget`:

* Archives older than **30 days** expire.
* If the sum of unexpired archives' `bytes` exceeds **2 GB**, the oldest
  expire until it does not.
* Kept archives never expire and do not count against the budget.

Expiry deletes the archive's refs and record. Git's normal `gc` (with its own
two-week grace period for unreachable objects) later frees the space.
`remove` applies retention after each removal (it only reads the small
records and deletes refs of expired archives); `archive prune` applies it on
demand.

## 6. `prune`

`prune` (trees whose directories were deleted by hand) records an archive per
pruned tree before reaping its registrations. Working files are gone, so the
archive holds only `H` and `B` per project; it still keeps unbranched commits
reachable, which `prune` loses today. Reason `prune`.

## 7. Errors

* `E_ARCHIVE_INCOMPLETE` (exit 1): §2.2. Details list items as
  `{ "project", "kind", "detail" }` with kinds `in-progress`, `conflicts`,
  `submodule-changes`, `embedded-repository`, `unmanaged-project`, and
  `unverifiable`.
* `E_ARCHIVE` (exit 1): writing the archive failed; nothing was removed.
* `E_NO_SUCH_ARCHIVE` (exit 1): no archive matches the selector.
* At restore, a project whose archive objects are missing (for example its
  object store was deleted by `repo gc` after it left the manifest) fails
  that project with reason `archive objects missing`; the archive is kept.

## 8. Skill and help text

The skill's "Clean up" section becomes:

> Removing a tree is safe: `remove` archives its commits and changes, and
> `restore` brings them back. To clean up, remove any tree you are not using;
> `git repo-worktree remove --idle 24h --dry-run --json` lists idle trees. Lock
> a tree you want kept indefinitely. If your tree's directory is gone, run
> `git repo-worktree restore <name-or-path>` from the source client and
> continue; check `restored[].branch_restored`.

`remove --help` states that removal archives and names `--discard`.

## 9. Required tests

1. Round trip: staged, unstaged, deleted, untracked, renamed, and
   mode-changed files plus unbranched commits survive `remove` → `restore`
   exactly (`git status --porcelain=v2` and file contents match).
2. Branch restore: kept when free; detached with a reason when moved,
   deleted, or checked out elsewhere.
3. Restore pins recorded commits after the source and upstream moved.
4. Overlay and pooled projects round-trip; restore uses the pool.
5. `E_ARCHIVE_INCOMPLETE` for each §2.2 case; `--force` records `dropped`;
   `--discard` archives nothing.
6. `remove --idle` skips locked, in-use, and recently active trees.
7. Retention by age and by budget; kept archives survive.
8. `prune` archives heads of hand-deleted trees.
9. Name and path conflicts at restore; restore by id, name, and path.
10. Missing archive objects fail only the affected project.
11. Fuzzer: `remove` → `restore` as an operation pair whose invariant is that
    every project's `HEAD`, index, and working tree match the pre-removal
    state.

## 10. Decisions

1. Restore deletes the archive on success, like `git stash pop`; `--keep`
   retains it. A failed restore always keeps it.
2. Retention defaults: 30 days and 2 GiB.
3. In-progress operations are refused; `--force` archives the rest and
   records the omission in `dropped`. Archiving rebase state files is not
   planned.
4. Open: whether `remove --idle` should also count the tree's last history
   event as activity, and whether ignored files should be archivable on
   request (`--include-ignored` with a size cap).
