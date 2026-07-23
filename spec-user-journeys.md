# git-repo-worktree — User Journeys (Spec Addendum)

**Status:** Addendum to `git-repo-worktree-spec.md` v1.0-draft. Normative where
it shows commands and exit codes: every journey below must work exactly as
written against the v1 implementation, and each maps to the numbered behavior
in the main spec (references in brackets). Journeys marked **[agent]** are
expected to be driven programmatically; those transcripts show `--json` usage.

Conventions: the source client lives at `~/aosp`; commands are run from inside
it unless noted. `$` is a human shell; agent journeys show the commands an
agent issues and the fields it reads.

---

## J1 — Hotfix without disturbing a mid-flight change (the classic)

*As a developer with half-finished work across several projects, I need a clean
tree at current state to build and fix an urgent bug, without stashing
anything.*

```
$ cd ~/aosp
$ git repo-worktree add ../aosp-hotfix
created aosp-hotfix at /home/me/aosp-hotfix: 412 projects, 3 skipped, 0 failed
$ cd ../aosp-hotfix
# build, reproduce, fix — every project is a real git checkout, detached at
# the same SHAs the source tree had at `add` time [spec §6.3, §7.1]
$ cd frameworks/base
$ git switch -c fix/crash-on-boot
$ git commit -am "Fix crash on boot" && git push origin HEAD:refs/for/main
$ cd ~/aosp
$ git repo-worktree remove ../aosp-hotfix
```

Notes: dirty state in `~/aosp` neither blocks `add` nor is copied [P3]. The
fix branch was created manually with plain git — the tool never needed to know
about it, and `remove` succeeds because the branch was pushed and the checkout
left clean; had it been dirty, `remove` would exit 1 `E_DIRTY` [§7.3].

## J2 — Parallel agent sessions on isolated trees **[agent]**

*As an orchestrator, I fan three coding agents out onto three isolated images
of the client so their edits can't collide.*

```
$ for i in 1 2 3; do
    git repo-worktree add --json --name agent-$i ../work/agent-$i
  done
```

Each invocation's stdout is one JSON document [§10]; the orchestrator asserts
`.ok == true` and hands `.result.path` to the agent as its cwd. Each agent
works with plain git inside its tree. Teardown, tolerating dirt the agents
left behind:

```
$ git repo-worktree remove --force ../work/agent-1
$ git repo-worktree remove --force ../work/agent-2
$ git repo-worktree remove --force ../work/agent-3
```

## J3 — Reviewing a colleague's change while keeping your own work open

*As a reviewer, I want a throwaway tree to fetch and build a Gerrit patchset.*

```
$ git repo-worktree add ../review-31415
$ cd ../review-31415/platform/build
$ git fetch origin refs/changes/15/31415/2 && git checkout FETCH_HEAD
# inspect, build, comment...
$ cd ~/aosp && git repo-worktree remove --force ../review-31415
```

`--force` is used deliberately: checking out FETCH_HEAD and building typically
leaves the tree "dirty" from the tool's perspective; the reviewer knows nothing
is worth keeping [§7.3.1].

## J4 — Topic branch across every project from birth

*As a developer starting a cross-cutting feature, I want the new tree to open
on a topic branch in every project, like `repo start --all` would.*

```
$ git repo-worktree add -b topic/big-refactor ../aosp-refactor
```

If `topic/big-refactor` already exists in some project, that project is
recorded `failed` with reason `branch-exists`, the run exits 3, and
`git repo-worktree list -v` names the offenders [§7.1.5]. After deleting or
renaming the stale branches in those projects:

```
$ git repo-worktree add --continue aosp-refactor
```

## J5 — Interrupted creation: laptop lid, Ctrl-C, or ENOSPC **[agent]**

*As an agent (or an unlucky human), my `add` died at project 600 of 1000. I
resume instead of starting over.*

```
$ git repo-worktree add --json --name nightly ../nightly
# ... process killed ...
$ git repo-worktree list --porcelain
worktree /home/me/nightly
name nightly
state creating
...
$ git repo-worktree add --continue --json nightly
```

The resume skips every project already `done` (HEAD-at-SHA check [§7.1.5
idempotency]), retries the `failed`, executes the `pending`, and the agent
asserts `.result.state == "complete"`. No re-pin occurs — the tree is built
against the original snapshot even if the source has since synced [§9.1].

## J6 — Partial-clone source, offline at the wrong moment **[agent]**

*As an agent on a partial-clone client, my `add` hit network failures on lazy
blob fetches.*

```
$ git repo-worktree add --json --name exp ../exp
# exit code 3
```

The agent reads `.error.code == "E_PARTIAL"`, then filters
`.result.projects[] | select(.state=="failed")` and sees reasons carrying the
hint `(source uses partial clone; network/auth may be required — retry with
'add --continue')` [§9.5]. Once connectivity/auth is restored:

```
$ git repo-worktree add --continue --json exp     # → .result.state == "complete"
```

## J7 — Long-lived reference tree, protected from cleanup

*As a team, we keep one pristine tree of the last release for quick diffing,
and it must survive prunes and absent-minded removes.*

```
$ git repo-worktree add --lock --reason "release-2026.2 reference tree" ../ref-2026.2
$ git repo-worktree remove ../ref-2026.2
error: tree is locked (release-2026.2 reference tree); pass --force --force to remove
$ git repo-worktree list
/home/me/ref-2026.2  complete  409/409 projects  locked
```

Locked trees are also skipped by `prune` even if someone deletes the directory
[§7.4]. Retiring it later is explicit: `unlock` then `remove`, or
`remove --force --force`.

## J8 — Relocating a tree to a bigger disk partition (same filesystem)

```
$ git repo-worktree move ../aosp-refactor ~/big/aosp-refactor
```

Every project's gitdir back-pointer is repaired and the breadcrumb rewritten
[§7.5]. A cross-filesystem destination fails fast with `E_CROSS_DEVICE` and
the documented advice (create new + remove old). If the process crashes
mid-move, the next `repair` finishes it [§7.5, §7.7.1].

## J9 — "I moved it with `mv` / my backup restored it somewhere weird"

*As a human who forgot the tool existed and used `mv` directly.*

```
$ mv ../aosp-hotfix ../hotfix2          # oops — git worktree links now broken
$ cd ../hotfix2 && git status
fatal: ...
$ git repo-worktree repair              # run from inside the tree: breadcrumb
                                        # locates the source client [§4.1]
```

Wait — the registry still records the old path. `repair` reports the old root
`prunable`; the supported recovery is `mv` it back and then use `move`, or
recreate. This journey exists to pin the *documented* behavior: `repair` fixes
gitdir links and directives for trees at their registered paths; it does not
adopt renamed trees [§7.7]. The README troubleshooting section must include
exactly this scenario.

## J10 — Reaping after `rm -rf`

```
$ rm -rf ../review-31415                # user deleted the tree by hand
$ git repo-worktree prune -n
would prune review-31415 (/home/me/review-31415)
$ git repo-worktree prune
```

Besides dropping the registry entry, this runs `git worktree prune` against
every recorded project gitdir so the source client's per-project worktree
registrations don't accumulate stale entries that would confuse plain
`git worktree list` in project checkouts [§7.6].

## J11 — Vendored submodules, populated on request

```
$ git repo-worktree add ../sm-tree
warning: 2 projects contain .gitmodules and were not populated
         (third_party/foo, third_party/bar); use --submodules to populate
$ git repo-worktree remove ../sm-tree
$ git repo-worktree add --submodules ../sm-tree
```

Submodule fetch failures journal per project and are retried by
`add --continue` like everything else [§7.1.7].

## J12 — Agent discovers state before acting **[agent]**

*As an agent dropped into an unfamiliar machine, I inventory existing trees
before creating more.*

```
$ git repo-worktree list --json
```

The agent reads `.result.trees[]`, using `name`, `path`, `state`, `locked`,
and per-project `failed` reasons to decide: resume an `incomplete` tree
(`add --continue <name>`), reuse a `complete` one, or create a new one with a
non-colliding `--name` (collision would exit 1 `E_NAME_EXISTS` [§6.5]).
Porcelain (`list --porcelain -z`) is the fallback for agents pattern-matching
on `git worktree`-style output [P11].

## J13 — Creation while a sync is running

```
$ git repo-worktree add ../quick        # meanwhile: repo sync in another shell
error: repo sync appears to be active (.repo/manifests.git/index.lock); use --force to proceed
$ git repo-worktree add --force ../quick
```

Exit 4 on detection; `--force` proceeds safely because revisions were going to
be pinned in one pass anyway and worktree registration protects the pinned
commits from prune [§6.2, §6.3]. This journey documents that `--force` here is
low-risk by design, not a footgun.

## J14 — Two humans, one client, no corruption

*As two developers sharing a build machine, we both run the tool at once.*

```
me$  git repo-worktree add ../me-tree
you$ git repo-worktree add ../you-tree
error: another git-repo-worktree process holds the registry lock; retry shortly
```

Second invocation exits 4 `E_LOCKED` immediately (non-blocking flock, §6.1);
callers — human or agent — are expected to retry. Agents should treat exit 4
as retryable-with-backoff and every other nonzero code as requiring diagnosis.

---

## Coverage matrix

| Journey | Exercises |
|---|---|
| J1 | add defaults, P3 dirty semantics, remove clean-check |
| J2 | --json envelope, --name, forced remove, parallel isolation |
| J3 | throwaway trees, FETCH_HEAD workflows, remove --force |
| J4 | -b, branch-collision → exit 3, continue after fix |
| J5 | journal/resume, porcelain state, idempotent re-add |
| J6 | partial clone, E_PARTIAL, §9.5 hint, offline→online resume |
| J7 | --lock/--reason, double --force, prune protection |
| J8 | move, EXDEV refusal, crash-recovery via repair |
| J9 | breadcrumb discovery, repair scope limits, documented non-adoption |
| J10 | prune -n/prune, per-gitdir git worktree prune hygiene |
| J11 | submodule warning, --submodules, retryability |
| J12 | list --json/--porcelain as agent discovery surface |
| J13 | quiescence detection, exit 4, --force rationale |
| J14 | flock contention, retry semantics for agents |

— end of addendum —