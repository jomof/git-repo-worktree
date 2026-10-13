# git-repo-worktree

`git-repo-worktree` creates and manages cheap parallel images of an entire Google
`repo` client. Projects normally become linked Git worktrees at the revision
captured by `repo manifest -r`; large fixed-depth projects may be shared from
the source checkout for speed. Manifest `copyfile` and `linkfile` directives
are replayed into the new tree, and each tree carries a synthesized `.repo`
directory so `repo status`, `repo start`, `repo branches`, `repo diff`,
`repo forall`, `repo upload`, and `repo sync -l` work inside it.

The source client's uncommitted work is never copied into a new worktree.
Created trees are useful for hotfixes, reviews, parallel coding agents, and
disposable builds.

## Install

The runtime requires Git 2.31 or newer and, for `add` only, repo 2.30 or newer.
Linux and macOS are supported.

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/jomof/git-repo-worktree/releases/latest/download/install.sh |
  sh
```

The installer selects the macOS or Linux archive for the current x86-64 or
ARM64 machine, verifies its SHA-256 checksum, and atomically installs it in
`~/.local/bin`, with manual pages in `~/.local/share/man` so
`git repo-worktree --help` works. If that directory is not the
`git-repo-worktree` Git would find, it configures a global `repo-worktree` Git
alias pointing at the installed binary. It never replaces a conflicting existing
alias. Git runs a `git-repo-worktree` found on `PATH` before any alias, so the
installer warns when another copy (for example an older `cargo install` in
`~/.cargo/bin`) would shadow the one it installed.

Rerun the same command to upgrade. To pin a release or choose another
directory, pass installer arguments after `sh -s --`:

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/jomof/git-repo-worktree/releases/download/v1.0.0/install.sh |
  sh -s -- --version v1.0.0 --install-dir "$HOME/bin"
```

Use `--no-git-alias` if the chosen directory is already managed on `PATH`.
The traditional Rust installers remain supported:

```sh
cargo binstall git-repo-worktree
# or
cargo install git-repo-worktree
```

Git dispatches `git repo-worktree` to the installed `git-repo-worktree`
binary. An optional shorter alias is:

```sh
git config --global alias.rwt '!git-repo-worktree'
```

Prebuilt release archives contain the binary, license, README, `SKILL.md`,
generated man page, and bash, zsh, and fish completions. GNU Linux archives
retain the layout expected by `cargo-binstall`; the curl installer selects
static musl Linux archives.

## Start with an inventory

Run commands inside a repo client, or pass `-C <client-path>`.

```sh
git repo-worktree list
git repo-worktree list --porcelain -z
git repo-worktree list --json
```

Every command accepts `--json`, `-q`, `-v`, and `-C`. JSON mode emits exactly one
versioned envelope on stdout. `list --json` summarizes each tree
(`project_counts` by mode, `failed_projects`, `checked_out_branches`); add `-v` for
every project record, or use `status --json <tree>`, whose `.result` is the tree
itself. Exit code 3 means a usable but incomplete `add` or `repair`; exit code 4
means a lock stayed busy past `--lock-timeout` or a concurrent repo sync was
detected, and is suitable for retry.

Several agents can work in one client at once. Read-only commands (`list`,
`status`, `report`, `defaults list`, `pool list`) never wait. Commands that change
a tree wait for that tree only, printing who holds it, for up to
`--lock-timeout` seconds (default 300, or `GIT_REPO_WORKTREE_LOCK_TIMEOUT`).
Creating or removing different trees proceeds in parallel.

## Create and resume

Create a detached tree pinned to the current snapshot:

```sh
git repo-worktree add ../aosp-hotfix
git repo-worktree add --json --name agent-1 ../work/agent-1
```

Every project starts on a detached `HEAD` at the commit that project currently has
checked out in the source client, including local commits and topic branches
checked out there. `add` never fetches and never resets to the remote branch;
run `git checkout --detach <remote>/<branch>` in a project to start it from
upstream instead.

Create a branch only where you will change code. `-b` requires a scope:

```sh
git repo-worktree add -b topic/name -p tools/adt/idea ../work/topic
git repo-worktree add -b topic/name --all-projects ../work/everything
```

`-p/--project` is repeatable and accepts any path inside a project. Created
branches track the project's manifest upstream (`branch.<name>.remote` and
`branch.<name>.merge`), as `repo start` does, so `repo upload` works. Inside a
tree you can also use `repo start <branch> <project>`.

Use `--submodules` to populate submodules; without it, projects containing
`.gitmodules` are reported but left unpopulated. Use `--git-only` for a tree
without the synthesized `.repo` directory.

Use `-v` to see phase start/completion timings and each project completion as soon
as its journal batch is durable. Each project's `populate_ms` in `list --json`
records how long its population took. Project checkouts and snapshot Git-directory
discovery use `-j/--jobs` workers (default: logical CPUs, capped at 32). On
storage-bound clients, a smaller value such as `-j 4` or `-j 8` can be faster.

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

Without a default, projects that are slow to check out (at least 30,000 tracked
files or 2 GiB) use `pool`, and fixed-depth projects use `overlay` where
available. A fixed-depth project whose source checkout is too dirty to share
falls back to a pooled worktree. `defaults worktree <project>` opts a project
out of both.

Two modes keep large projects fast without sharing the source's files:

* `pool`: the project is an ordinary isolated worktree, taken from a pool of
  pre-populated spares when one is available. `add` moves a spare into the tree
  with `git worktree move` and checks out the pinned commit, which rewrites only
  files that differ. `remove` returns a clean pool project to the pool (after
  `git clean -ffdx`) instead of deleting it, keeping up to `pool size` spares
  per project (default 2). Branched projects use spares too. The first tree pays
  the full checkout; `git repo-worktree pool fill` creates spares ahead of time
  for every pooled project, and `pool list` (alias `pool status`) and
  `pool clear` manage them. Spares must be on the same filesystem as the tree
  and cost a full checkout of disk each.
* `overlay` (Linux, requires `fuse-overlayfs`): the project is a real linked
  worktree whose files are a copy-on-write view of the clean source checkout.
  Creation takes well under a second regardless of size, writes never reach the
  source, paths stay inside the tree, and an untouched overlay is known clean
  without scanning it. Because the view is live, a source checkout that later
  changes branch shows up as modifications in the overlay. Mounts do not
  survive a reboot; `repair` remounts them. `materialize` converts a clean
  overlay to a normal checkout. Without FUSE, overlay projects fall back to
  worktrees.
Defaults are stored under `.repo/repo-worktrees`, and each `add` captures its
active defaults so later changes affect only future trees.

For worktree projects, Git's native parallel checkout is enabled and ready
projects are started largest-first using pinned tracked-file sizes. Projects
over 500 MiB are limited to three concurrent checkouts by default; use
`--heavy-jobs N` to tune storage pressure. On a clean, non-sparse source at the
pinned SHA, the tool may clone file extents with filesystem reflinks and verify
the result with Git. Any ambiguity falls back to normal checkout. Use
`--no-reflink` as a diagnostic escape hatch.

Creation is journaled. If it is interrupted or partially fails, resolve the reported
problem and resume without re-pinning:

```sh
git repo-worktree add --continue <name-or-path>
```

The original `snapshot.xml` remains authoritative for the lifetime of the tree.
If `repo manifest -r` omits a checkout listed in `.repo/project.list`—as happens
with active `notdefault` groups—the tool pins that checkout from its on-disk `HEAD`,
recovers its copy/link directives from the active source manifests, and persists the
supplement into `snapshot.xml` before creation begins.
Only actual parent/child project paths constrain scheduling, so unrelated projects
can run immediately even when their paths have different depths. Completion records
are coalesced into small batches and `fsync`ed before progress is reported or
dependent projects are released; an interrupted batch is safely replayed by
`add --continue`.
An interrupted optimized population is never accepted merely because its
`HEAD` is correct: resume repairs the unfinished worktree with a parallel
`reset --hard` before marking it done.

## Manage trees

```sh
git repo-worktree status [<tree>]
git repo-worktree detach --branch my-feature
git repo-worktree lock --reason "release reference" ref-tree
git repo-worktree unlock ref-tree
git repo-worktree move ref-tree ../new-location
git repo-worktree materialize ref-tree prebuilts/example
git repo-worktree defaults list
git repo-worktree repair [<tree>...]
git repo-worktree prune --dry-run
git repo-worktree prune
git repo-worktree remove <tree>
```

`status` reports each project's HEAD, branch, and dirtiness. A project counts as
dirty when it has modified or untracked files, or commits on a detached `HEAD`
that no branch, remote-tracking ref, or tag contains; those commits would be
lost with the tree's reflog. Normal removal refuses dirty projects and returns
the full list under `error.details.dirty_projects` in JSON mode. One `--force`
deliberately discards dirty work. A locked tree is protected from prune and
requires `--force --force` for removal.

On Linux, `remove` warns about processes that still use the tree (working
directory inside it, or arguments naming it), such as a build server started
there, and lists them under `lingering_processes`. It does not stop them.

Git lets a branch be checked out in only one worktree per project. `detach`
releases branches in place (keeping the working tree and index): name a tree,
pass `--source`, or pass `--branch <name>` to find it anywhere. `list --json`
reports `checked_out_branches` for every tree.

`move` supports same-filesystem relocation and repairs every Git registration.
`repair` restores registrations, shared links, and missing manifest directives
at registered paths without replacing unexpected real data.

### If a tree was moved with `mv`

`repair` does not adopt manually renamed trees. If `mv ../aosp-hotfix ../hotfix2`
leaves the registry pointing at `../aosp-hotfix`, `repair` reports the old root as
prunable. Move the directory back to its registered path, then run
`git repo-worktree move ../aosp-hotfix ../hotfix2`. If the original directory was
deleted instead, use `prune` and create a new tree.

## Agent skill

The binary embeds [the agent skill](skill/SKILL.md) as its single source of truth:

```sh
git repo-worktree skill show
git repo-worktree skill list
git repo-worktree skill add --scope user
git repo-worktree skill add --scope project
git repo-worktree skill sync
git repo-worktree skill remove
```

Automatic installation only uses framework roots that already exist. It can create
the `skills/git-repo-worktree` children, but never invents `.agents`, `.claude`, or
`.gemini`. Use `--path <existing-skills-dir>` for another framework or set the
colon-separated `GRW_SKILL_DIRS`.

When both `.agents/skills` and `.gemini/skills` are eligible at the same scope,
`.agents/skills` is installed once as Gemini's interoperable alias. An explicit
`--target gemini-user` or `--target gemini-project` overrides that deduplication.

Antigravity's global skills location has changed across releases. This version uses
`~/.gemini/config/skills` only when Antigravity-specific state is present.
`git repo-worktree skill list` is the authoritative report of the exact path this
binary will use.

Installed copies carry ownership and version sentinels. Foreign files are never
overwritten or removed without `--force`; `skill sync` updates every sentinel-bearing
copy after a tool upgrade.

## Site hooks

Per-client setup that every new tree needs, such as generating machine-local
build configuration, belongs in hooks rather than in each user's or agent's
instructions. Hooks are executables in the source client:

```text
.repo/repo-worktrees/hooks/post-add     # after a tree is complete
.repo/repo-worktrees/hooks/pre-remove   # before a tree is deleted
```

Each runs in the tree root with `GIT_REPO_WORKTREE_EVENT`,
`GIT_REPO_WORKTREE_TREE`, `GIT_REPO_WORKTREE_NAME`, and
`GIT_REPO_WORKTREE_SOURCE` set. Output goes to stderr. A failing or timed-out
hook (`GIT_REPO_WORKTREE_HOOK_TIMEOUT`, default 600 s; the hook's whole process
group is stopped) prints a warning and is recorded under the tree's `hooks`, but
never fails the command. `add --continue` and `repair` rerun `post-add` until it
succeeds, so installing a hook and running `repair` applies it to existing
trees. `git repo-worktree hooks list` shows what is installed; `--no-hooks` or
`GIT_REPO_WORKTREE_NO_HOOKS=1` skips them. Hooks must not leave background
processes attached to their output.

For example, a client whose Bazel setup script writes a per-workspace
`env.bazelrc` might use:

```sh
#!/bin/sh
# post-add
exec ./build/bazel/env/setup
```

```sh
#!/bin/sh
# pre-remove
[ -f env.bazelrc ] && bazel shutdown || true
```

## Usage history and `report`

Every invocation appends one JSON line to `.repo/repo-worktrees/history.jsonl` in
the source client: the command and flag names (never flag values or paths
outside the client), duration, exit and error codes, phase timings, and, for
`add`, `materialize`, `repair`, and `pool fill`, per-project modes, strategies,
sizes, and timings. `add` also records the OS, kernel, Git and repo versions,
CPU count, and filesystem types. The file never leaves the machine, is written
without `fsync`, and is trimmed to its newest half past 8 MiB.

```sh
git repo-worktree report                 # latency per command, errors, usage mistakes, slowest projects
git repo-worktree report --json --last 200
GIT_REPO_WORKTREE_SESSION=scenario-3 git repo-worktree add ../wt   # tag events
git repo-worktree report --session scenario-3
```

Set `GIT_REPO_WORKTREE_NO_HISTORY=1` to stop recording.

## Repo commands inside a tree

The tree's `.repo` links to the source's repo tool and object store, holds a
generated `manifest.xml` and `project.list` describing only the tree's own
worktree projects at their pinned revisions, and has its own manifest checkout
(a linked worktree on a per-tree branch `repo-worktree/<tree>`). Each project's
`.git` is a link to `.repo/projects/<path>.git`, which resolves to the project's
Git worktree directory, so repo and Git always agree on `HEAD` and the index.

* `repo sync -l` returns projects to the tree's pinned revisions and leaves
  shared projects and the source untouched.
* A networked `repo sync` in a tree fetches into the shared object store and
  updates only the tree's manifest checkout. The tree stays pinned to its
  snapshot. Like any sync, it also points each project's shared
  `refs/remotes/m/<branch>` pseudo-ref at the revision it synced, here the
  tree's pin; the next sync in the source resets them.
* Manifests with `<submanifest>` are not bridged; such trees are git-only.

## Limitations

No dirty-state transport into materialized worktrees, no Windows, no
cross-filesystem move, submodules opt-in. The tool never fetches proactively.
Besides its registry under `.repo/repo-worktrees`, it writes only Git's own
worktree registrations in the source (and links inside them for the `.repo`
bridge) and per-tree branches in the source manifest repository.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

The build script generates `git-repo-worktree.1` and one page per subcommand
with `clap_mangen`.

Some behavior needs more than the default test environment:

* Tests in `tests/repo_fidelity.rs` drive a real `repo` client. They need
  `repo` on `PATH` and `GRW_TEST_REPO_URL` naming a git-repo checkout to
  initialize from (`git clone https://gerrit.googlesource.com/git-repo`).
* Overlay tests run only on Linux with `fuse-overlayfs`. From macOS, run
  `scripts/linux-test.sh [cargo test args]` (requires Docker). It builds and tests
  in an Ubuntu container with FUSE enabled, as a non-root user, on an ext4
  volume without reflink support. Completion
scripts are generated from the same CLI definition using the `completions`
subcommand. See [RELEASING.md](RELEASING.md) for the GitHub-hosted release
process.
