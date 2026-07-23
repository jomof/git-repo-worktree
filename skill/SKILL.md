---
name: git-repo-worktree
description: Create, inspect, resume, repair, move, lock, prune, and remove isolated whole-tree Git worktrees for Google repo-managed clients. Use when an agent needs a clean pinned checkout of every synced project without disturbing the source client, needs to inventory or recover existing whole-tree worktrees, or must safely clean up an agent worktree.
---

# Manage repo-wide worktrees

Use `git repo-worktree` from inside a Google `repo` client to create and manage a tree containing one linked Git worktree per synced manifest project. Treat created trees as Git-only: plain `git` works inside each project, but `repo` does not.

## Inspect before acting

Run:

```sh
git repo-worktree list --json
```

Read `result.trees` before creating another tree. Reuse a suitable `complete` tree, resume a `creating` or `incomplete` tree, or choose a unique name and destination.

Treat exit code 4 (`E_LOCKED` or `E_SYNC_ACTIVE`) as retryable with backoff. Diagnose other nonzero exits. Exit code 3 (`E_PARTIAL`) leaves useful metadata in `result`; inspect failed projects and reasons.

## Create an isolated tree

Create a detached, manifest-pinned tree:

```sh
git repo-worktree add --json --name agent-task ../work/agent-task
```

The source client's dirty state is neither copied nor modified. Work only in the returned `result.path`. Each project starts detached at the SHA captured by `repo manifest -r`.

Use `-b <branch>` only when the same topic branch should be created in every project. A branch collision produces a partial tree; resolve the conflicting branch and resume.

Use `--submodules` only when submodules must be populated. They are intentionally omitted by default.

## Resume safely

Resume without taking a new snapshot:

```sh
git repo-worktree add --continue --json <name-or-path>
```

Never delete or replace `snapshot.xml` to force a new pin. The original snapshot is authoritative for the tree's lifetime. Failed partial-clone fetches can be retried with the same command after connectivity or credentials are restored.

## Protect and relocate

Lock a long-lived tree:

```sh
git repo-worktree lock --reason "reference tree" <tree>
```

Unlock it before normal removal. A locked tree requires two `--force` flags for removal.

Move only within one filesystem:

```sh
git repo-worktree move <tree> <new-path>
```

Use the command instead of `mv`; it repairs every project's Git worktree registration. If a supported move was interrupted, run `git repo-worktree repair`.

## Repair and prune

Repair registered paths and manifest directives:

```sh
git repo-worktree repair --json [<tree>...]
```

Repair does not adopt a tree moved manually. Move it back to its registered path first, then use the supported `move` command.

Preview and remove registry records for trees deleted outside the tool:

```sh
git repo-worktree prune --dry-run
git repo-worktree prune
```

## Remove deliberately

Check for work worth keeping, then remove:

```sh
git repo-worktree remove <tree>
```

The command refuses dirty projects. Use one `--force` only when discarding dirty work is intentional. Use `--force --force` only to discard a locked tree as well.

Created trees contain no `.repo` directory and cannot run repo commands. The tool does not transport dirty state, does not support Windows or cross-filesystem moves, and populates submodules only with `--submodules`.
