---
name: git-repo-worktree
description: >-
  Create and manage fast whole-tree Git worktrees for Google repo-managed
  clients (AOSP-style multi-project checkouts with a .repo directory). Use when
  Codex needs a disposable second checkout of an entire repo client for parallel
  work, hotfixes, review, experiments, or clean builds; when users mention
  repo-wide worktrees, AOSP or manifest-checkout isolation, working without
  disturbing an active client, configuring per-project population defaults, or
  managing an existing git-repo-worktree. Also use before modifying a repo
  client concurrently used by another session, even if the user does not
  explicitly request a worktree. Prefer this over copying a repo client, syncing
  a disposable second client, or creating worktrees project by project.
---

# git-repo-worktree

Create parallel directory trees for every active project of a `repo` client,
with manifest `<copyfile>`/`<linkfile>` directives replayed. Most projects are
linked `git worktree`s at pinned SHAs. Explicit fixed-depth projects may be
shared from the source checkout until they need editing. Think `git worktree`,
but for the entire multi-project tree at once.

Key properties to keep in mind:

- **Starting revisions:** every project starts on a **detached HEAD at the
  commit currently checked out in the source client** — including local
  commits or a topic branch checked out there. `add` never fetches and never
  resets to the remote branch. To start a project from upstream, run
  `git -C <tree>/<project> checkout --detach <remote>/<branch>` there.
- **repo works inside a tree.** The tree has its own `.repo`, so `repo status`,
  `repo start`, `repo branches`, `repo diff`, `repo forall`, `repo upload`, and
  `repo sync -l` act on the tree, not the source.
- Operations are **journaled and resumable** — a failed or interrupted `add`
  is resumed with `add --continue`, never by deleting and starting over.

## Before you start: verify the environment

Run these checks; do not assume.

```bash
git repo-worktree version --json
git repo-worktree list --json
```

Treat a nonzero `version` exit as a missing or broken installation. Ask before
installing; the default installation needs Git, curl, and network access:

```bash
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf https://github.com/jomof/git-repo-worktree/releases/latest/download/install.sh | sh
```

The installer verifies the release checksum, installs manual pages, and warns
when an older copy elsewhere on PATH would shadow the new one. Use the
structured `list` error to distinguish `E_NO_REPO_CLIENT` from retryable lock
contention. If there is no `.repo` directory anywhere above the working
directory, this skill does not apply — for a single Git repository use plain
`git worktree` instead.

For flag help use `git repo-worktree <subcommand> -h`. `git repo-worktree --help`
opens the manual page, which exists only when the installer placed it on
`MANPATH`.

## Core workflows

Always pass `--json` when you are an agent: stdout becomes exactly one JSON
document with `ok`, `exit_code`, `error` (`code`, `message`, optional
`details`), and a `result` object; human progress goes to stderr. Parse JSON —
do not scrape the human output. `result` is `null` when a command fails.

Result shapes:

- `list --json`: `.result.trees[]`, one summary per tree.
- `add`, `repair <tree>`, `materialize`: `.result` is one tree summary
  (`name`, `path`, `state`, `project_counts`, `failed_projects`,
  `failed_directives`, `checked_out_branches`, `hooks`, `repo_bridge`). Add `-v`
  for full per-project records.
- `status --json <tree>`: `.result.projects[]` holds per-project state.
  Example: `jq '.result.projects[] | select(.dirty) | {path, reasons}'`.

Before creating anything, inspect `list --json`: reuse a suitable `complete`
tree, resume a `creating` or `incomplete` tree, or choose a unique name and
destination.

### Create a parallel tree

```bash
git repo-worktree add --json --name mywork /absolute/path/outside-client/mywork
```

- Run from anywhere inside the source client (or use `-C <client-dir>`).
- The destination must be a new or empty directory **outside** the client.
- Read `.result.state`: `"complete"` means every project succeeded.
- Projects containing `.gitmodules` are left unpopulated by default; pass
  `--submodules` if the task needs them.
- Let the default population policy run. Projects with a positive manifest
  `clone-depth` are shared with the source checkout: as copy-on-write
  `"overlay"` projects on Linux with FUSE, otherwise as `"symlink-shared"`.
  Large source projects may come from a pool of spare worktrees. All of these
  except `"symlink-shared"` are isolated and safe to edit.
- Use `--full` only when the whole tree must be independently writable, or
  repeat `--full-project <path>` for selected projects.

Project arguments (`--full-project`, `-p`, `materialize`, `defaults`, `detach`)
accept any path inside a project, such as `tools/adt/idea/android/src`; they
resolve to the enclosing project and report the resolution.

### Branches: only where you change code

Leave projects detached unless you will commit in them. To start with a branch
in specific projects:

```bash
git repo-worktree add --json -b my-topic -p tools/adt/idea -p tools/base /abs/path/mywork
```

`-b` without `-p` fails with `E_BRANCH_NEEDS_PROJECT_SCOPE`; `--all-projects`
branches every project and is rarely what you want. Created branches track the
manifest upstream like `repo start`, so `repo upload` works. Later, inside the
tree, `repo start <branch> <project>` or `git switch -c <branch>` in a project
does the same job.

### Site hooks

The client's owner may install hooks that run automatically: `post-add` after
a tree is complete (for example to generate local build configuration) and
`pre-remove` before deletion (for example to stop build servers). Check
`.result.hooks` in the `add` and `remove` output, or run
`git repo-worktree hooks list --json`. When `hooks.post-add.status` is `"ok"`,
do not repeat that setup by hand. When it is `"failed"`, `"timeout"`, or
`"skipped"`, read the warning on stderr, fix the cause, and run
`git repo-worktree repair <tree>` to run it again.

### Work inside the tree

Before editing a project, run `status --json <tree>` (or `list -v --json`) and
check that project's `"mode"`. If it is `"symlink-shared"`, materialize it
first:

```bash
git repo-worktree materialize --json <tree> <project-path>
```

Materialization is resumable, works on locked trees, and succeeds as a no-op
(`already_materialized`) when the project is already a worktree. Proceed when
the project is listed in `materialized` or `already_materialized` and not in
`failed_projects`.

Use Git and repo normally inside the tree. Prefer running repo commands from
the tree root or a worktree project: a shared project is physically the source
checkout, so a shell inside it is in the source client. `repo sync -l` returns
projects to the tree's pinned revisions. A networked `repo sync` fetches but also
keeps them pinned. Push or upload only when the user asks.

Working files and indexes are isolated, but each linked worktree shares its
project's objects, branches, and tags with the source checkout. Branches you
create are visible from the source and survive removing the tree.

### Check state

```bash
git repo-worktree status --json [<tree>]
```

Each `projects[]` entry reports `head`, `branch`, `modified`, `untracked`,
`unbranched_commits`, `dirty`, and `reasons`. `unbranched-commits` means
commits on a detached HEAD that no branch contains: create a branch
(`git branch <name>`) before removing the tree, or they are lost.

### Move work between the source and a tree

Git checks out a branch in only one worktree per project. If `git switch
<branch>` fails with "already used by worktree", release it where it is held:

```bash
git repo-worktree detach --json --branch <branch>        # search the source and every tree
git repo-worktree detach --json --source -p <project>    # or a specific location
```

`detach` only moves HEAD off the branch; files and the index are untouched.
`list --json` reports `checked_out_branches` per tree. Detach your branches
before finishing so others can check them out.

### Handle partial failure (exit code 3)

Exit 3 means the tree exists but is `incomplete`. Do not delete it. Inspect
and resume:

```bash
git repo-worktree list --json          # find the tree; read projects[] with state=="failed" and their reason
git repo-worktree add --continue --json mywork
```

Resume skips finished projects, repairs any interrupted optimized checkout with
`reset --hard` at its pinned SHA, and retries failures. A reason mentioning
partial clone / network means the source lazily fetches objects — retry when
network/auth is available. A `branch-exists` reason (from `-b`) means that
branch already exists in the named project; resolve it there, then continue.
If `repo_bridge.state` is `"failed"`, read `repo_bridge.reason`; git commands
still work and `repair` rebuilds the bridge.

### Clean up

```bash
git repo-worktree remove --json mywork             # refuses if any project is dirty
git repo-worktree remove --force mywork            # discard dirty work knowingly
```

A refusal is `E_DIRTY` with every dirty project, and why, in
`error.details.dirty_projects`. After removing, read `lingering_processes`:
programs you started in the tree (build servers, watchers) keep running until
you stop them. Use `--force` only after explicit confirmation
that the listed work may be discarded. Locked trees need `--force --force` and
the same explicit confirmation. If a tree was deleted with `rm -rf` instead,
run `git repo-worktree prune` to reap the stale registration.

### Other operations

```bash
git repo-worktree list --json
git repo-worktree lock --reason "keep for release diffing" <tree>
git repo-worktree move <tree> <new-path>        # same filesystem only
git repo-worktree defaults list --json
git repo-worktree pool list --json             # alias: pool status
git repo-worktree repair [<tree>...]            # registrations, links, directives, overlays, .repo bridge
```

Persistent defaults are local source-client policy: `symlink`, `worktree`,
`pool` (worktrees taken from pre-populated spares), `overlay` (Linux
copy-on-write views of the source), and `auto`. Inspect them; change them only
when the user asks:

```bash
git repo-worktree defaults pool <project-path>...
git repo-worktree defaults auto <project-path>...
```

### Feedback about the tool

When asked how the tool is performing, or to report problems with it, run
`git repo-worktree report --json` and include its result: it summarizes this
client's local history of timings, errors, and usage mistakes. If the user
gives a label for a test run, export `GIT_REPO_WORKTREE_SESSION=<label>` before
running commands and use `report --session <label>`.

## Exit codes (uniform across subcommands)

| Code | Meaning | Agent action |
|------|---------|--------------|
| 0 | success | proceed |
| 1 | operational failure | read `error.code`/`error.message`/`error.details`, diagnose |
| 2 | CLI usage error | fix the command line |
| 3 | partial success, tree `incomplete` | `add --continue` (or `repair`) |
| 4 | a lock stayed busy past `--lock-timeout` (default 300 s), or an active `repo sync` was detected | retry later; the message names the process holding the lock; use `--force` only after confirming sync detection is stale or a false positive |

Common error codes: `E_NO_REPO_CLIENT`, `E_NO_SUCH_TREE`, `E_NAME_EXISTS`,
`E_DEST_NOT_EMPTY`, `E_DIRTY`, `E_TREE_LOCKED`, `E_LOCKED`, `E_SYNC_ACTIVE`,
`E_UNKNOWN_PROJECT`, `E_BRANCH_NEEDS_PROJECT_SCOPE`, `E_OVERLAY_BUSY`.

## Rules that prevent damage

- Prefer letting `add` create the destination; an existing directory must be
  empty. Never place a tree inside the source client or inside another tree.
- Never write into the source's `.repo/` or edit `.repo/repo-worktrees/` files
  by hand; the registry is the tool's. Do not run `repo init` inside a tree.
- Never re-create a failed tree from scratch when `add --continue` applies —
  recreation wastes the journal and repeats completed work.
- Treat `"mode": "symlink-shared"` projects as read-only because their files
  are the source checkout's files. Materialize a shared project before editing,
  building into, or running Git anywhere below its path.
- Before `remove`, check `status` for `unbranched-commits`; branch any work you
  need to keep.
- Do not `mv` a tree by hand; use `move`. If someone already did, `mv` it back
  and then use `move`.

## When NOT to use this tool

- Single git repository (no `.repo`): use `git worktree` directly.
- You need a tree that tracks a *different* manifest branch: create a second
  client with `repo init --reference` instead.
- Windows: unsupported.

For full flag reference on any subcommand, run
`git repo-worktree <subcommand> -h`.
