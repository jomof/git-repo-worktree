# git-repo-worktree — Fast default `add` (Spec Addendum)

**Status:** Normative addendum to `spec.md` v1.0-draft.

This addendum makes the common `add` path fast on clients dominated by large,
fixed-depth prebuilts. A project explicitly carrying a positive
`clone-depth` may be represented by an absolute symlink to the source
checkout. All other projects remain real linked Git worktrees.

The tool selects the mode automatically and conservatively. The user does not
need to opt in, inspect sizes, or label projects beyond the existing manifest.

## 1. Population modes and strategies

Every non-skipped project has one population mode:

* `worktree`: an isolated linked Git worktree at the pinned SHA.
* `symlink-shared`: an absolute symlink to the corresponding source checkout.

A worktree-mode project records a population strategy, `reflink` or
`checkout`. A shared project has no worktree strategy.

Sharing is live, not copy-on-write: a write below a shared directory also
writes to the source checkout. `add` must therefore print one calm warning
when it creates any shared projects:

```text
warning: sharing N clone-depth projects with the source checkout; materialize one before editing it
```

With `-v`, list each shared project path. `--quiet` suppresses the warning.

## 2. Mode selection

At pin time, parse each snapshot `<project>` element's `clone-depth`
attribute. A positive decimal integer makes the project a sharing candidate.
Missing, zero, negative, or malformed values do not; malformed values produce
a warning, with project details only at `-v`.

A candidate becomes `symlink-shared` only if every condition below holds:

1. Neither `--full` nor a matching `--full-project <ppath>` forces a
   worktree.
2. `-b/--branch` was not used.
3. The project path is not a prefix-directory parent of another non-skipped
   project.
4. No copyfile or linkfile destination is equal to or below the project path.
5. The source checkout exists, its `HEAD` exactly equals the pinned SHA, its
   complete status including untracked files is clean, and sparse checkout is
   disabled.
6. If `--submodules` is active, the pinned project does not contain
   `.gitmodules`.

Failure of any condition selects `worktree`; it does not fail `add`. `-v`
may report the demotion reason.

If every non-skipped project is a sharing candidate, disable sharing for the
whole run. This treats client-wide shallow initialization as a storage policy,
not an instruction to alias the entire client. Report the guardrail only at
`-v`.

Supplemented projects inherit `clone-depth` from their active manifest
declaration, and the augmented snapshot preserves the attribute.

## 3. CLI

`add` gains:

* `--full`: force every project to use mode `worktree`.
* `--full-project <ppath>`: force one project to use mode `worktree`;
  repeatable. An unknown path is a usage error (exit 2).
* `--no-reflink`: disable reflink attempts for worktree projects.
* `--heavy-jobs <n>`: allow at most `n` projects larger than 500 MiB to
  populate concurrently; positive, default 3.

All are rejected with `--continue`; their recorded values govern resume.

A new command converts shared projects in place:

```text
git repo-worktree materialize [-j N] <tree> <ppath>...
```

Every named path must be a recorded `symlink-shared` project before mutation.
Otherwise fail with `E_NOT_SHARED` and exit 1. Materialization is allowed on a
locked tree; the resulting Git worktree must immediately receive the tree's
recorded lock.

## 4. Shared-project population and durability

For each shared project, journal step `project-share`:

1. Create parent directories below the tree root.
2. Create `<tree>/<ppath>` as an absolute symlink to
   `<source-root>/<ppath>`.
3. An existing symlink with the exact target succeeds. Replace a different
   symlink. Never replace a non-symlink; fail that project with
   `dest-occupied`.
4. Durably append the completion record before updating `meta.json`.

Completion records may be journaled in batches. Each batch is one
append/write followed by sync; metadata may mark a project done only after
the batch containing its completion is durable. A crash before that point
replays the idempotent operation. This batching rule also applies to
worktree-project and directive completion records.

## 5. Lifecycle safety

Commands must distinguish shared projects from worktrees:

* `dirty`, `remove`, `lock`, `unlock`, Git worktree repair, and Git worktree
  prune skip shared projects.
* Recursive tree removal must delete shared symlink inodes and must never
  traverse them.
* `move` leaves absolute shared targets unchanged.
* `repair` verifies every shared path. It recreates a missing link and
  replaces a wrong symlink, but never deletes or replaces a real file or
  directory.
* Submodule population never runs through a shared project.
* A directive destination inside a shared project is forbidden and fails
  with `dest-in-shared`. Mode selection normally prevents this by demoting
  that project. A directive source inside a shared project is permitted.

`materialize` unlinks only the exact expected shared symlink, creates the
recorded pinned worktree through the normal worktree strategy, durably
journals `project-materialize`, changes the mode to `worktree`, applies a
tree lock when present, and retries directives. If interrupted after unlink,
retry treats the absent destination or a partial `.git` worktree as resumable.
It never replaces an unexpected real path.

## 6. Metadata and compatibility

Registry `format_version` becomes 2. Older binaries must reject v2 entries
because their lifecycle operations do not understand shared paths. A v2
reader accepts v1 metadata with these defaults:

```json
{
  "project": {
    "mode": "worktree",
    "clone_depth": null,
    "strategy": null,
    "est_bytes": null
  },
  "tree": {
    "full": false,
    "full_projects": [],
    "no_reflink": false,
    "heavy_jobs": 3
  }
}
```

Newly created v2 project records use:

```json
"mode": "worktree" | "symlink-shared",
"clone_depth": 1 | null,
"strategy": "reflink" | "checkout" | null,
"est_bytes": 1234 | null
```

Human `list` appends `(<n> shared)` when nonzero. Porcelain output includes
`shared <n>` immediately after `skipped`. JSON exposes the metadata directly.

## 7. Worktree performance

Worktree-mode projects retain the safe optimizations below.

### 7.1 Tracked-size scheduling

Compute `est_bytes` from the pinned tree with
`git --git-dir=<common> ls-tree -rlz <sha>`. Failures produce null and do not
fail `add`. Preserve parent-before-child dependencies; among ready projects
dispatch largest first. Limit projects over 500 MiB to `heavy_jobs`, while
allowing ready light work to fill remaining `-j` slots.

### 7.2 Parallel checkout

All checkout and resume-reset commands receive, without changing repository
configuration:

```text
-c checkout.workers=8 -c checkout.thresholdForParallelism=100
```

### 7.3 Conservative reflink

Reflink may run only when the source checkout exists at the exact pinned
HEAD, is non-sparse, and has an empty full porcelain status. It populates from
the pinned tree, preserves Git modes and symlinks, then rechecks the source
and validates the destination against the pinned index. Any uncertain
precondition falls back to checkout. Failure after worktree registration is
repaired with pinned `reset --hard`. A failed filesystem pair is memoized for
the run.

An unfinished destination already containing `.git` is always reset hard to
the pinned SHA; HEAD equality alone never proves completion.

## 8. User and agent guidance

README and the embedded skill must explain both modes without alarm:

* Most projects are isolated worktrees.
* Explicit fixed-depth projects may be shared for speed and are effectively
  read-only from the created tree.
* Before editing a project, agents inspect `list --json`; when its mode is
  `symlink-shared`, they run `materialize <tree> <project-path>`.
* `--full` creates an entirely materialized tree when needed.

## 9. Required tests

1. Positive, absent, and malformed `clone-depth` parsing, including
   supplemented snapshot preservation.
2. Default selection, `--full`, repeatable `--full-project`, client-wide
   guardrail, branch, nested-parent, directive-destination, dirty/wrong-HEAD,
   sparse, and submodule demotions.
3. Shared links are absolute and exact; resume is idempotent; unexpected
   real paths are preserved.
4. Removing a tree deletes the links while every source file remains;
   lifecycle Git commands do not operate through shared links.
5. Repair restores missing/wrong links without replacing real data.
6. Materialize creates a clean pinned worktree, resumes partial work,
   preserves locking, retries directives, and rejects non-shared paths.
7. Existing tracked-size, dependency scheduler, heavy throttle, checkout,
   reflink, source-race, and resume tests continue to pass.
8. v1 metadata loads with worktree defaults; v2 metadata is rejected by
   readers whose supported format is 1.

— end of addendum —
