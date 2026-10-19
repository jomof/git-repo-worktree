# git-repo-worktree

**Fresh isolated Google `repo` clients at agent speed.**

```console
$ cd ~/projects/studio-main
$ time git repo-worktree add ~/projects/studio-feature
git repo-worktree add ~/projects/studio-feature  14.21s user 20.94s system 648% cpu 5.418 total

$ cd ~/projects/studio-feature
$ bazel build //tools/base/common:tools.common
...
INFO: Elapsed time: 16.337s
INFO: 150 processes: 113 remote cache hit, 37 internal.
INFO: Build completed successfully
```

Work in it like any repo client:

```sh
repo start fix-lint tools/base         # branch tracks the manifest upstream
# ...edit, build, git commit...
repo sync                              # fetch and move the tree to the latest upstream
cd tools/base
git rebase -i @{upstream}              # tidy up your commits
repo upload --cbr .                    # send them for review
```

When you're done, clean up:

```console
$ cd ~/projects/studio-main
$ git repo-worktree remove studio-feature    # archives unsaved work first
$ git repo-worktree restore studio-feature   # changed your mind? it's back
```

Branches you create outlive the tree, and your original client's files are
never touched. Run one tree per hotfix, per code review, or per coding agent,
side by side.

## Android Studio, by the numbers

Android Studio's `studio-main` client (56 projects, about 660,000 files) on a
96-core Linux workstation with ext4, starting from a clean source client:

| | `repo init` + `repo sync -j 8` | `git repo-worktree add` |
|---|---|---|
| Time to a new client | 16 min 50 s | **5.1 s** typical, 4.0 s best (about 15 s the very first time) |
| Disk per client | 141 GB | **about 11 GB** |

A Bazel build of `//tools/base/common:tools.common` in a brand-new tree took
16.3 s, with 113 of its 150 actions served from the remote cache.

## Why it's fast

* **Linked Git worktrees.** Every tree shares the source client's object store,
  so nothing is cloned or fetched.
* **Copy-on-write overlays.** On Linux with `fuse-overlayfs`, large prebuilt
  projects appear in milliseconds and use no disk until you write to them.
* **A pool of warm spares.** The biggest source projects are recycled between
  trees, so only the files that differ are rewritten.
* **Parallel everything.** Projects are populated concurrently, largest first,
  with Git's parallel checkout and filesystem reflinks where available.

## Install it

```sh
cargo install git-repo-worktree
```

Requires Git 2.31+ and repo 2.30+ on Linux or macOS, and a stable Rust
toolchain to build. See [Installing](docs/install.md) for prebuilt binaries via
`cargo binstall`, mise, ubi, eget, or a standalone install script.

## Hooks if you need them

If every new tree needs machine-local setup (build environment, IDE settings)
or something to stop before it is deleted (build servers, daemons), add hooks
once per source client. Both run in the tree's root directory:

```sh
cd ~/projects/studio-main
mkdir -p .repo/repo-worktrees/hooks
cat > .repo/repo-worktrees/hooks/post-add <<'EOF'
#!/bin/sh
# Runs after a tree is created.
exec ./path/to/your/setup-script
EOF
cat > .repo/repo-worktrees/hooks/pre-remove <<'EOF'
#!/bin/sh
# Runs before a tree is removed.
exec ./path/to/your/teardown-script
EOF
chmod +x .repo/repo-worktrees/hooks/post-add .repo/repo-worktrees/hooks/pre-remove
```

See [Site hooks](docs/hooks.md) for the environment they get and a Bazel example.

## Repo commands in a tree

These work: `status`, `diff`, `branches`, `info`, `list`, `grep`, `overview`,
`forall`, `manifest`, `start`, `checkout`, `rebase`, `cherry-pick`, `download`,
`upload`, `prune`, and `sync`.

**Avoid in a tree:** `repo init`, `repo wipe`, `repo gc`, `repo sync --force-sync`.

## For coding agents

Give your agents the bundled skill so each one works in its own tree:

```sh
git repo-worktree skill add --scope user
```

## Learn more

* [Installing](docs/install.md)
* [Using git-repo-worktree](docs/usage.md): creating, resuming, inspecting,
  moving, and removing trees; JSON output; usage history
* [How trees are populated](docs/population.md): worktree, pool, overlay, and
  shared modes, and per-project defaults
* [Repo commands inside a tree](docs/repo-commands.md)
* [Site hooks](docs/hooks.md)
* [Agent skill](docs/agent-skill.md)
* [Development](docs/development.md)
