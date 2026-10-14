# git-repo-worktree

**A second copy of your entire Google `repo` client, in seconds.**

`git repo-worktree add ../feature` creates a new, fully isolated tree with every
project of your repo client at the commits you have checked out right now.
Build in it, branch in it, `repo sync` it, `repo upload` from it, then throw it
away. Your original client is never touched. Run one tree per hotfix, per code
review, or per coding agent, side by side.

## Why it's fast

* **Linked Git worktrees.** Every tree shares the source client's object store,
  so nothing is cloned or fetched.
* **Copy-on-write overlays.** On Linux with `fuse-overlayfs`, large prebuilt
  projects appear in milliseconds and use no disk until you write to them.
* **A pool of warm spares.** The biggest source projects are recycled between
  trees, so only the files that differ are rewritten.
* **Parallel everything.** Projects are populated concurrently, largest first,
  with Git's parallel checkout and filesystem reflinks where available.

## Android Studio, by the numbers

Measured on Android Studio's `studio-main` client (56 projects, about 660,000
files and 88 GB) on a 96-core Linux workstation with ext4:

| | Checking out every project | `git repo-worktree add` |
|---|---|---|
| Create a tree | 1 min 50 s | **6.4 s** (32 s the very first time) |
| Disk per tree | 88 GB | **21 GB** |

A Bazel build of `//tools/base/common:tools.common` in a brand-new tree took
16.3 s, with 113 of its 150 actions served from the remote cache.

## Get started

```sh
cargo binstall git-repo-worktree     # or: cargo install git-repo-worktree

# Optional, once per client: run your own setup in every new tree and
# cleanup before one is removed. Both run in the tree's root directory.
cd ~/projects/studio-main
mkdir -p .repo/repo-worktrees/hooks
cat > .repo/repo-worktrees/hooks/post-add <<'EOF'
#!/bin/sh
# Generate machine-local config the new tree needs (build env, IDE settings).
exec ./path/to/your/setup-script
EOF
cat > .repo/repo-worktrees/hooks/pre-remove <<'EOF'
#!/bin/sh
# Stop anything started from this tree (build servers, daemons).
exec ./path/to/your/teardown-script
EOF
chmod +x .repo/repo-worktrees/hooks/post-add .repo/repo-worktrees/hooks/pre-remove
```

Requires Git 2.31+ and repo 2.30+ on Linux or macOS. See
[Installing](docs/install.md) for mise, ubi, eget, and a standalone install
script.

## A day in a tree

```console
$ cd ~/projects/studio-main
$ time git repo-worktree add ../studio-feature
note: 19 project(s) are copy-on-write overlays of the source checkout; `repair` remounts them after a reboot
created studio-feature at /home/you/projects/studio-feature: 56 projects, 0 skipped, 0 failed

real    0m6.427s

$ cd ../studio-feature
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

cd ~/projects/studio-main
git repo-worktree remove studio-feature   # your branch stays; the tree goes
```

These repo commands work in a tree: `status`, `diff`, `branches`, `info`,
`list`, `grep`, `overview`, `forall`, `manifest`, `start`, `checkout`,
`rebase`, `cherry-pick`, `download`, `upload`, `prune`, and `sync`.

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
