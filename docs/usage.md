# Using git-repo-worktree

`git-repo-worktree` creates and manages cheap parallel images of an entire Google
`repo` client. Projects normally become linked Git worktrees at the revision
captured by `repo manifest -r`; large fixed-depth projects may be shared from
the source checkout for speed (see [How trees are populated](population.md)).
Manifest `copyfile` and `linkfile` directives are replayed into the new tree,
and each tree carries a synthesized `.repo` directory so repo commands work
inside it (see [Repo commands inside a tree](repo-commands.md)).

The source client's uncommitted work is never copied into a new worktree.
Created trees are useful for hotfixes, reviews, parallel coding agents, and
disposable builds.

## Start with an inventory

Run commands inside a repo client, or pass `-C <client-path>`.

```sh
git repo-worktree list
git repo-worktree list --porcelain -z
git repo-worktree list --json
```

Every command accepts `--json`, `-q`, `-v`, and `-C`. JSON mode emits exactly one
versioned envelope on stdout. Tree results (`list --json` per tree, and
`add`, `repair`, and `materialize`) summarize each tree with `project_counts` by
mode, `failed_projects`, `failed_directives`, `checked_out_branches`, and
`hooks`; add `-v` for every project record, or use `status --json <tree>` for
per-project state. Exit code 3 means a usable but incomplete `add` or `repair`; exit code 4
means a lock stayed busy past `--lock-timeout` or a concurrent repo sync was
detected, and is suitable for retry.

Several agents can work in one client at once. Read-only commands (`list`,
`status`, `report`, `defaults list`, `pool list`) never wait. Commands that change
a tree wait for that tree only, printing who holds it, for up to
`--lock-timeout` seconds (default 300, or `GIT_REPO_WORKTREE_LOCK_TIMEOUT`).
Creating or removing different trees proceeds in parallel.

## Create a tree

Create a detached tree pinned to the current snapshot:

```sh
git repo-worktree add ../aosp-hotfix
git repo-worktree add --json --name agent-1 ../work/agent-1
```

Every project starts on a detached `HEAD` at the commit that project currently has
checked out in the source client, including local commits and topic branches
checked out there. `add` never fetches and never resets to the remote branch;
run `git checkout --detach <remote>/<branch>` in a project to start it from
upstream instead, or run `repo sync` in the tree.

To start from recorded commits instead, pass a manifest with pinned
revisions, such as one written earlier with `repo manifest -r -o FILE`:

```sh
git repo-worktree add --manifest baseline.xml ../work/baseline
```

Every project the file lists starts at its revision; projects it omits start
at the source's checked-out commit. Projects whose pinned commit differs from
the source checkout become worktrees (taken from the pool when possible)
rather than shared views. `add` refuses, before creating anything, a file that
names a branch instead of a commit (`E_MANIFEST_UNPINNED`) or a commit not yet
in the local object stores (`E_MISSING_REVISIONS`, listing each project);
nothing is fetched. `list --json` and the `add` result report a
`pinned_digest` of every project's path and pinned commit, equal for trees that
started from the same commits.

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

Large projects are populated in faster ways than a plain checkout; see
[How trees are populated](population.md) for the modes, per-project defaults,
and how to materialize a shared project before editing it.

## Resume an interrupted add

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
are written by a separate thread and synced about once a second, so syncing
never holds up the next project; records lost to a crash are safely replayed
by `add --continue`.
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
git repo-worktree remove --idle 7d --dry-run
git repo-worktree restore <tree-name-or-path>
git repo-worktree archive list
```

`status` reports each project's HEAD, branch, and dirtiness. A project counts as
dirty when it has modified or untracked files, or commits on a detached `HEAD`
that no branch, remote-tracking ref, or tag contains; those commits would be
lost with the tree's reflog. `status --json` also reports `idle_secs` (time
since the last sign of work in the tree) and the `processes` using it.

## Remove and restore

`remove` never loses work. Before deleting a tree it archives each project's
commit, branch, index, and working tree (untracked files included, ignored
files not) in the project's own repository, under
`refs/repo-worktree/archive/<id>`, and `restore` recreates the tree from that
archive:

```sh
git repo-worktree remove my-feature           # archived as my-feature-20261019T185012Z
git repo-worktree restore my-feature          # same path, same commits, same changes
git repo-worktree restore my-feature@{1} /tmp/older --name older
```

`restore` accepts an archive id, `<tree>@{n}` (newest first), a tree name, or
the tree's former path. It creates the tree with the archived commits (not the
source's current ones) using the pool and overlays as `add` does, applies the
archived index and working tree, and checks out each archived branch again if
the branch still points at the archived commit; otherwise the project stays
detached and the result says why. The archive is deleted after a successful
restore unless `--keep`.

Some state cannot be archived: a rebase, merge, cherry-pick, or bisect in
progress, unresolved conflicts, a submodule's own uncommitted changes, and
projects `repo sync` added inside the tree. `remove` refuses those with
`E_ARCHIVE_INCOMPLETE`; `--force` archives everything else and records what
was dropped. `--discard` deletes without archiving. A locked tree is protected
from prune and requires `--force --force` for removal.

An interrupted `remove` leaves the tree in state `removing`; run `remove`
again to finish it. `repair` skips such trees (listing them in
`interrupted_removals`) rather than rebuilding a half-removed tree.

`remove --idle <duration>` removes, with archives, every tree that is
unlocked, not in use by a process (Linux), and idle for at least the duration;
`--dry-run` lists them. `prune` archives the commits of trees deleted by hand
before reaping them.

```sh
git repo-worktree archive list
git repo-worktree archive show my-feature
git repo-worktree archive keep my-feature     # never expire this one
git repo-worktree archive drop my-feature@{1}
git repo-worktree archive policy --expire 30d --budget 2G
```

Archives expire after 30 days, and the oldest expire first once the objects
they alone keep alive exceed 2 GiB; kept archives never expire. Expiry runs
after each `remove` and on `archive prune`. Projects left at their starting
commit with no changes need no ref, so archiving a clean tree writes nothing
to its repositories.

`remove` returns once the tree is unregistered and moved aside: its files are
deleted afterwards by background maintenance (see
[Keeping the pool full](population.md#keeping-the-pool-full)), so disk space
comes back shortly after the command finishes. The tree's path is free for a
new tree immediately. With `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1`, `remove`
deletes the files itself.

On Linux, `remove` warns about processes that still use the tree (working
directory inside it, or arguments naming it), such as a build server started
there, and lists them under `lingering_processes`. It does not stop them; a
[`pre-remove` hook](hooks.md) can.

Git lets a branch be checked out in only one worktree per project. `detach`
releases branches in place (keeping the working tree and index): name a tree,
pass `--source`, or pass `--branch <name>` to find it anywhere. `list --json`
reports `checked_out_branches` for every tree.

`move` supports same-filesystem relocation and repairs every Git registration.
`repair --mounts-only` only remounts overlay projects that are unmounted or
whose mount died (after a reboot or crash); `list --json` names them in
`overlays_unmounted`. `repair` restores registrations, shared links, and
missing manifest directives at registered paths without replacing unexpected
real data, and rebuilds the `.repo` bridge, keeping the previous one if repo
rejects the result.

### If a tree was moved with `mv`

`repair` does not adopt manually renamed trees. If `mv ../aosp-hotfix ../hotfix2`
leaves the registry pointing at `../aosp-hotfix`, `repair` reports the old root as
prunable. Move the directory back to its registered path, then run
`git repo-worktree move ../aosp-hotfix ../hotfix2`. If the original directory was
deleted instead, use `prune` and create a new tree.

## Usage history and `report`

Every invocation appends one JSON line to `.repo/repo-worktrees/history.jsonl` in
the source client: the command and flag names (never flag values or paths
outside the client), duration, exit and error codes, phase timings, and, for
`add`, `materialize`, `repair`, and `pool fill`, per-project modes, strategies,
sizes, and timings, including how long each project waited for a worker.
`add` also records the OS, kernel, Git and repo versions, CPU count, and
filesystem types. `add`, `remove`, and pool maintenance record the load
average and, on Linux, CPU, I/O, and memory pressure when they start and end,
so a slow run on a busy machine can be told apart from a slow tool. The file
never leaves the machine, is written without `fsync`, and is trimmed to its
newest half past 8 MiB.

```sh
git repo-worktree report                 # latency per command, errors, usage mistakes, slowest projects
git repo-worktree report --json --last 200
GIT_REPO_WORKTREE_SESSION=scenario-3 git repo-worktree add ../wt   # tag events
git repo-worktree report --session scenario-3
```

Set `GIT_REPO_WORKTREE_NO_HISTORY=1` to stop recording.

## Limitations

No dirty-state transport into materialized worktrees, no Windows, no
cross-filesystem move, submodules opt-in. The tool never fetches proactively.
Besides its registry under `.repo/repo-worktrees`, it writes only Git's own
worktree registrations in the source (and links inside them for the `.repo`
bridge) and per-tree branches in the source manifest repository.
