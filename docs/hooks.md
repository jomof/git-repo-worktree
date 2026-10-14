# Site hooks

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
never fails the command. A hook skipped with `--no-hooks` is recorded as
`skipped`. `add --continue` and `repair` rerun `post-add` until it
succeeds, so installing a hook and running `repair` applies it to existing
trees. `git repo-worktree hooks list` shows what is installed; `--no-hooks` or
`GIT_REPO_WORKTREE_NO_HOOKS=1` skips them. Hooks must not leave background
processes attached to their output.

`pre-remove` runs only once removal is certain: a removal refused for dirty
projects or a lock does not run it.

## Example: per-tree Bazel configuration

A client whose Bazel setup script writes a per-workspace `env.bazelrc` (with
its own `output_base`) might use:

```sh
#!/bin/sh
# post-add: give each tree its own env.bazelrc.
exec ./build/bazel/env/setup
```

```sh
#!/bin/sh
# pre-remove: stop this tree's Bazel server, if one is running, without starting one.
ob=$(sed -n 's/^startup --output_base=//p' env.bazelrc 2>/dev/null | tail -n 1)
[ -n "$ob" ] && [ -f "$ob/server/server.pid.txt" ] || exit 0
exec bazel shutdown
```

With these hooks in Android Studio's `studio-main`, `post-add` took about
100 ms and `pre-remove` about 500 ms with a live Bazel server.
