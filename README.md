# git-repo-worktree

`git-repo-worktree` creates and manages cheap parallel images of an entire Google
`repo` client. Projects normally become linked Git worktrees at the revision
captured by `repo manifest -r`; large fixed-depth projects may be shared from
the source checkout for speed. Manifest `copyfile` and `linkfile` directives
are replayed into the new tree.

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
`~/.local/bin`. If that directory is not the `git-repo-worktree` Git would
find, it configures a global `repo-worktree` Git alias pointing at the
installed binary. It never replaces a conflicting existing alias.

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
versioned envelope on stdout. Exit code 3 means a usable but incomplete `add` or
`repair`; exit code 4 means lock contention or a detected concurrent repo sync and
is suitable for retry with backoff.

## Create and resume

Create a detached tree pinned to the current snapshot:

```sh
git repo-worktree add ../aosp-hotfix
git repo-worktree add --json --name agent-1 ../work/agent-1
```

Use `-b topic/name` to create the same branch independently in every project. Use
`--submodules` to populate submodules; without it, projects containing
`.gitmodules` are reported but left unpopulated.

Use `-v` to see phase start/completion timings and each project completion as soon
as its journal batch is durable. Project checkouts and snapshot Git-directory
discovery use `-j/--jobs` workers (default: logical CPUs, capped at 32). On
storage-bound clients, a smaller value such as `-j 4` or `-j 8` can be faster.

By default, a project with an explicit positive manifest `clone-depth` may be
an absolute symlink to its clean, pinned source checkout. This avoids copying
large prebuilts on filesystems such as ext4 that lack reflinks. `add` prints a
short warning when it uses this mode. Shared projects are live views of the
source and should be treated as read-only; convert one before editing:

```sh
git repo-worktree materialize <tree> <project-path>
```

`--full` forces an entirely materialized tree, while repeatable
`--full-project <project-path>` forces selected projects. Sharing is
automatically disabled for branch creation, nested parents, directive
destinations, dirty/wrong-HEAD/sparse sources, relevant submodule population,
and clients where every active project has `clone-depth`.

Source-client-local defaults can opt individual active projects in or out for
future trees:

```sh
git repo-worktree defaults symlink prebuilts/example
git repo-worktree defaults worktree platform/app
git repo-worktree defaults auto platform/app
git repo-worktree defaults list
```

A `symlink` default makes the project a sharing candidate even without
`clone-depth`; it does not bypass any safety fallback. A `worktree` default
suppresses clone-depth sharing. `auto` returns to manifest-driven behavior.
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

Normal removal refuses dirty projects. One `--force` deliberately discards dirty
work. A locked tree is protected from prune and requires `--force --force` for
removal.

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

## v1 limitations

The v1 limitations are: git-only trees (no repo commands inside), no dirty-state
transport into materialized worktrees, no Windows, no cross-filesystem move,
submodules opt-in.

The tool never synthesizes `.repo` in a created tree, never fetches proactively, and
never writes repo internals outside `.repo/repo-worktrees`.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

The build script generates `git-repo-worktree.1` with `clap_mangen`. Completion
scripts are generated from the same CLI definition using the `completions`
subcommand. See [RELEASING.md](RELEASING.md) for the GitHub-hosted release
process.
