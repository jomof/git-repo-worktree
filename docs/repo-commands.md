# Repo commands inside a tree

The tree's `.repo` links to the source's repo tool and object store, selects
the same manifest as the source client, and has its own manifest checkout (a
linked worktree on a per-tree branch `repo-worktree/<tree>`). A generated local
manifest, `.repo/local_manifests/zz-git-repo-worktree.xml`, removes the projects
the tree does not own (shared, skipped, or not yet populated); the source's own
local manifests are linked alongside it. Each project's `.git` is a link to
`.repo/projects/<path>.git`, which resolves to the project's Git worktree
directory, so repo and Git always agree on `HEAD` and the index.

These work as in any client of the same manifest: `status`, `diff`, `branches`,
`info`, `list`, `grep`, `overview`, `forall`, `manifest`, `start`, `checkout`,
`rebase`, `cherry-pick`, `download`, `upload`, `prune`, and `sync` (with `-n`,
`-l`, `--rebase`).

A tree is created at the source's checked-out commits, but afterwards repo
treats it as an ordinary client:

* `repo sync` fetches into the shared object store, moves detached projects to
  the current upstream, rebases unpublished topic branches, and updates only the
  tree's own manifest checkout. The source client is not touched.
* A branch already uploaded for review is not rewritten by plain `repo sync`
  ("published (but not merged)"); use `repo sync --rebase` (or `repo rebase`) to
  replay it onto the new upstream, then `repo upload` the result.
* With several trees, fetch once (`repo sync -n` in the source or any tree)
  and run `repo sync -l --rebase` in each tree: the local half needs no network.
* `repo start <branch> <project>` starts the branch at the manifest upstream, as
  in any client; use `repo start --head` to branch from the tree's current
  commit.
* Shared (`symlink-shared`) projects are not synced; they keep following the
  source checkout. Overlay and worktree projects are synced like any other.
  Materialize a shared project, or sync the source, if the tree needs a newer
  version of it.
* A project added to the manifest upstream is cloned by `repo sync` as an
  ordinary repo project inside the tree; the tool does not manage it.
* Manifests with `<submanifest>` are not bridged; such trees are git-only.
* Branches are shared with the source and every other tree, so `repo abandon`
  and `repo prune` in a tree delete branches everywhere.

## Commands to avoid

| Command | Why | Instead |
|---|---|---|
| `repo init` | rewrites the tree's `.repo` | create a new tree |
| `repo wipe` | deletes object stores shared with the source | `git repo-worktree remove` |
| `repo gc` | cleans object stores shared with the source | run it in the source client |
| `repo sync --force-sync` | may delete a project's checkout and its registration | `git repo-worktree repair <tree>` |

The tree's `.repo/project-objects` holds a link per tree project rather than
the source's whole directory, so `repo gc` cannot see the source's other
projects and `repo wipe` refuses to delete through a link; trees created
before this layout get it from `git repo-worktree repair`.
