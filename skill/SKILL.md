---
name: git-repo-worktree
description: >-
  Create and manage isolated whole-tree Git worktrees for Google repo-managed
  clients (AOSP-style multi-project checkouts with a .repo directory). Use when
  Codex needs a disposable second checkout of an entire repo client for parallel
  work, hotfixes, review, experiments, or clean builds; when users mention
  repo-wide worktrees, AOSP or manifest-checkout isolation, working without
  disturbing an active client, or managing an existing git-repo-worktree. Also
  use before modifying a repo client concurrently used by another session, even
  if the user does not explicitly request a worktree. Prefer this over copying a
  repo client, syncing a disposable second client, or creating worktrees project
  by project.
---

# git-repo-worktree

Create parallel directory trees in which **every active project** of a `repo`
client is checked out as a linked `git worktree` at pinned SHAs, with manifest
`<copyfile>`/`<linkfile>` directives replayed. Think `git worktree`, but for
the entire multi-project tree at once. Trees are cheap (objects are shared),
isolate working files and indexes, and are disposable.

Key properties to keep in mind:

- Created trees are **git-only**: every project is a real git checkout, but
  `repo` commands do NOT work inside them. Use plain git there.
- All projects start on **detached HEAD** at the SHAs the source client had at
  creation time. Uncommitted changes in the source are never copied.
- Operations are **journaled and resumable** — a failed or interrupted `add`
  is resumed with `add --continue`, never by deleting and starting over.

## Before you start: verify the environment

Run these checks; do not assume.

```bash
git repo-worktree version --json
git repo-worktree list --json
```

Treat a nonzero `version` exit as a missing or broken installation. Ask before
installing; installation uses `cargo install git-repo-worktree`, needs a Rust
toolchain and network access, and requires `~/.cargo/bin` on PATH. Use the
structured `list` error to distinguish `E_NO_REPO_CLIENT` from retryable lock
contention. If there is no `.repo` directory anywhere above the working
directory, this skill does not apply — for a single Git repository use plain
`git worktree` instead.

## Core workflows

Always pass `--json` when you are an agent: stdout becomes exactly one JSON
document with `ok`, `exit_code`, `error.code`, and a full `result` object;
human progress goes to stderr. Parse JSON — do not scrape the human output.
Before creating anything, inspect `result.trees`: reuse a suitable `complete`
tree, resume a `creating` or `incomplete` tree, or choose a unique name and
destination.

### Create an isolated tree

```bash
git repo-worktree add --json --name mywork /absolute/path/outside-client/mywork
```

- Run from anywhere inside the source client (or use `-C <client-dir>`).
- The destination must be a new or empty directory **outside** the client.
- Read `.result.state`: `"complete"` means every project succeeded.
- Need a topic branch in every project from the start? Add `-b <branch>`.
- Projects containing `.gitmodules` are left unpopulated by default; pass
  `--submodules` if the task needs them.
- Let the default safe population optimizations run: every project still becomes
  an isolated real worktree. Ready projects start largest-first, projects over
  500 MiB are capped at three concurrent populations, and clean pinned source
  checkouts may use verified filesystem reflinks.
- On storage-constrained machines, lower heavy concurrency with
  `--heavy-jobs <n>`. Use `--no-reflink` only to diagnose filesystem or
  performance problems; it forces Git checkout for every project.

### Work inside the tree

Use ordinary Git commands per project directory. Create branches with
`git switch -c` and commit as the task requires; push only when the user asks.
Working files and indexes are isolated, but each linked worktree shares its
project's object database and refs with the source checkout. Branches and
commits are therefore visible from both trees even though edits do not change
the source working files.

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

### Clean up

```bash
git repo-worktree remove mywork             # refuses if any project is dirty
git repo-worktree remove --force mywork     # discard uncommitted work knowingly
```

Use `--force` only after explicit confirmation that dirty work may be
discarded. Locked trees need `--force --force` and the same explicit
confirmation. If a tree was deleted with `rm -rf` instead, run
`git repo-worktree prune` to reap the stale registration.

### Other operations

```bash
git repo-worktree list --json
git repo-worktree lock --reason "keep for release diffing" <tree>
git repo-worktree move <tree> <new-path>        # same filesystem only
git repo-worktree repair [<tree>...]            # repair registrations/directives
```

## Exit codes (uniform across subcommands)

| Code | Meaning | Agent action |
|------|---------|--------------|
| 0 | success | proceed |
| 1 | operational failure | read `error.code`/`error.message`, diagnose |
| 2 | CLI usage error | fix the command line |
| 3 | partial success, tree `incomplete` | `add --continue` (or `repair`) |
| 4 | lock contention or active `repo sync` detected | retry with backoff; use `--force` only after confirming sync detection is stale or a false positive |

## Rules that prevent damage

- Prefer letting `add` create the destination; an existing directory must be
  empty. Never place a tree inside the source client or inside another tree.
- Never write into `.repo/` or edit `.repo/repo-worktrees/` files by hand;
  the registry is the tool's. Use `list`/`repair`/`prune` instead.
- Never re-create a failed tree from scratch when `add --continue` applies —
  recreation wastes the journal and repeats completed work.
- Treat every created project as a real isolated worktree. Fast `add` never
  symlinks a project to the source client and never permits source-only dirty
  or untracked content to enter the tree.
- Never run `repo sync`, `repo start`, or any `repo` command inside a created
  tree; they will fail or act on the wrong client. Use git.
- Do not `mv` a tree by hand; use `move`. If someone already did, `mv` it back
  and then use `move`.

## When NOT to use this tool

- Single git repository (no `.repo`): use `git worktree` directly.
- You need `repo upload`/`repo sync` **inside** the new tree: not supported;
  create a second full client with `repo init --reference` instead.
- Windows: unsupported.

For full flag reference on any subcommand, run
`git repo-worktree <subcommand> --help`.
