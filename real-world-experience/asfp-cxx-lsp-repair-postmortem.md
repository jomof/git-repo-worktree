# Post-mortem: `repair` poisons a tree's `.repo` and blocks `repo upload` (`asfp-cxx-lsp`, 2026-10-15)

## Summary

A routine "sync, rebase, upload" of Gerrit topic `asfp-cxx-lsp` in tree
`~/projects/studio-wt-asfp-cxx-lsp` was blocked by `git repo-worktree`.

The agent ran `git repo-worktree repair --json asfp-cxx-lsp` to remount a dead
FUSE overlay after a server restart. The remount worked, but the same call
rewrote the tree's `.repo` into a degenerate client: a `local_manifests` file
with `<remove-project>` for **all 56 projects**, an empty `project.list`, and an
empty `project-objects/`. Every `repo` command in the tree now fails with:

```
error: in `upload -t --cbr --yes ...`: internal error parsing repo-hooks in .../studio-wt-asfp-cxx-lsp/.repo/manifest.xml
```

This also breaks the repohooks pre-upload hook, which runs `repo forall . -c 'echo ${REPO_PROJECT}'`.

The likely root cause is a chain:

1. `repair` fails every time on a tree-global step (`git worktree repair` over
   `manifests.git` errors on other trees' symlinked `.repo/manifests/.git`).
2. That one global failure is recorded as `failed / repair-failed` on all 56
   projects, including 37 perfectly healthy git worktrees.
3. The repo bridge is then regenerated from that per-project state, so every
   "failed" project is treated as "not owned by this tree" and removed from the
   tree's manifest.

No work was lost: branches, commits, and working files are intact. The cost was
a blocked upload, a workaround that silently ran hooks on the wrong commits, and
agent time.

## Environment

| Item | Value |
|---|---|
| Source client | `~/projects/studio-main` (57 projects, `source_layout: legacy`) |
| Tree | `asfp-cxx-lsp` at `~/projects/studio-wt-asfp-cxx-lsp`, 19 `overlay` + 37 `worktree` |
| Tree created by | `git-repo-worktree` 2.0.0 (`5865f3f65006`), `format_version` 3 |
| Repaired by | `git-repo-worktree` 2.1.0 (`e1643e6e3c3c`) |
| git / repo | `2.56.0.176.g3575e65682-goog` / `v2.68.1` |
| Other bridged trees | `studio-feature`, `studio-feature1`, `studio-feature2`, `studio-wt-textmate-bridge`, `studio-wt-vsidl-proto`, `sdv-apex-env`, `cli-dap-r1` |

## Timeline (UTC)

| Time | Event | Evidence |
|---|---|---|
| 10-13 15:02:19 | `add` completes in 23.5 s, `post-add` hook ok, state `complete`. | `history.jsonl` line 367 |
| 10-14 04:26:32 | `repair --json` (all trees, 2.0.0) → `E_PARTIAL`, exit 3. | `history.jsonl` line 381 |
| 10-14 04:58:05 | Same, `E_PARTIAL`. | line 385 |
| 10-15 05:23:19 | `repair --json` (2.1.0) → `E_PARTIAL`. | line 565 |
| (unknown) | Server restart kills the tree's `fuse-overlayfs` daemons. | Errno 107 below |
| 10-15 16:59 | `list --json`: tree `incomplete`, 56/56 projects `failed: repair-failed`. `status --json asfp-cxx-lsp` returns `ok: true` with `projects: []`. Direct `git status` in each project shows them healthy: `asfp-cxx-lsp` branches in `tools/vendor/google` and `tools/adt/idea`, the rest detached at `goog/studio-main`. | tool output |
| 17:00:19 | `repo sync -c -j8 --rebase` in the tree. Fetch succeeds. Checkout fails on `prebuilts/r8` with `OSError: [Errno 107] Transport endpoint is not connected`. `tools/vendor/google` stops on a real rebase conflict, which is unrelated. **`repo` itself still works in the tree at this point.** | sync log |
| 17:02:35 | Agent follows the `repo-worktree` skill's dead-mount recipe: `fusermount -uz` on the one listed mount, then `repair --json asfp-cxx-lsp`. Result: `E_PARTIAL`, state still `incomplete`, 56/56 still `repair-failed`. Afterwards 19 overlays are mounted again. stderr: see below. | tool output |
| 17:02:35 | Same call rewrites the tree's `.repo` (all mtimes 17:02:35): `manifest.xml`, `project.list`, `local_manifests/zz-git-repo-worktree.xml`, `manifests/.git`, and every project's `.git` symlink. | `stat` |
| 17:15:31 | `repo upload` → `internal error parsing repo-hooks`. `repo forall` and the direct `pre-upload.py` call fail the same way. | tool output |

stderr of the 17:02:35 repair:

```
warning: repair manifest worktree: exit 1: error: .git is not a file: /usr/local/google/home/jomof/projects/studio-feature/.repo/manifests
error: .git is not a file: /usr/local/google/home/jomof/projects/studio-feature1/.repo/manifests
error: .git is not a file: /usr/local/google/home/jomof/projects/studio-wt-textmate-bridge/.repo/manifests
error: .git is not a file: /usr/local/google/home/jomof/projects/studio-wt-vsidl-proto/.repo/manifests
error: .git is not a file: /usr/local/google/home/jomof/projects/studio-feature2/.repo/manifests
```

## What the broken `.repo` looks like

Compared with the healthy sibling tree `studio-feature1`:

| Path in tree `.repo/` | Healthy (`studio-feature1`) | Broken (`asfp-cxx-lsp`, after 17:02:35) |
|---|---|---|
| `manifest.xml` | 11,971 B pinned snapshot | 500 B: `<include name="default.xml" />` |
| `project.list` | 1,316 B | 1 B (`\n`) |
| `project-objects` | symlink → source `.repo/project-objects` | empty real directory |
| `local_manifests/` | absent | `zz-git-repo-worktree.xml`: "projects this tree does not own", 56 × `<remove-project ... optional="true"/>`, including `platform/tools/repohooks` |
| `manifests/.git` | symlink → `../manifests.git` | same (re-created) |

`default.xml` declares `<repo-hooks in-project="platform/tools/repohooks">`.
After the local manifest removes that project, `manifest_xml.py` (lines
1669–1683) finds the hooks project list with a length other than 1 and raises
`internal error parsing repo-hooks`. Every `repo` subcommand parses the manifest
first, so all of them fail.

Tree metadata (`.repo/repo-worktrees/asfp-cxx-lsp/meta.json`) after the repair:
`state: incomplete`, `repo_bridge.state: done`, and every project has
`state: failed, reason: repair-failed`. One example is `build/bazel`, a plain
`worktree`-mode project whose checkout is fine.

## Root-cause analysis

### 1. `repair` always fails on a tree-global step (likely cause)

The stderr shows the "repair manifest worktree" step running
`git worktree repair` against `studio-main/.repo/manifests.git`. That command
walks **every** registered manifests worktree. git expects each linked worktree
to have a `.git` *gitfile*, but the repo bridge creates `.repo/manifests/.git`
as a **symlink** (`-> ../manifests.git -> .../manifests.git/worktrees/manifestsN`).
git reports `.git is not a file` for each bridged tree and exits 1.

This explains why every `repair` since 10-14 04:26 returned `E_PARTIAL`, for
2.0.0 and 2.1.0 alike. It is deterministic, not related to the server restart,
and gets worse as more bridged trees exist. A step meant for this tree fails
because of *other* trees.

Open detail: `asfp-cxx-lsp`'s own `manifests/.git` is also a symlink but is not
in the error list. Perhaps the tree's own entry is rewritten before git runs.

### 2. One global failure is stamped onto all 56 projects

`meta.json` marks all 56 projects `failed / repair-failed`, including 37
`worktree`-mode projects that were never broken, and `history.jsonl` records
only `E_PARTIAL` with `remount_failures: 0`. The per-project state therefore
does not describe the projects. It describes a failure elsewhere, copied onto
each one. The opaque reason (`repair-failed`, no detail) hid this from the
agent and from `report`.

### 3. The bridge is regenerated from that state and removes every "failed" project

`zz-git-repo-worktree.xml` is described as "projects this tree does not own".
With 56/56 projects "failed", the bridge treats none as owned and emits
`<remove-project>` for all of them. This includes the repohooks project, which
the manifest needs exactly once. Nothing checked whether the result was a usable
client before writing it, and there is no backup to roll back to.

### 4. A poisoning loop with no way out

- `repair` is the advertised fix for both dead mounts and a broken bridge.
- But `repair` always fails (cause 1), which marks all projects failed (cause 2),
  which empties the bridge (cause 3).
- So `repair` both causes the breakage and is the only supported way to fix it.
- The skill forbids editing `.repo` bridge files by hand, so the agent had no
  supported way out.

### 5. Contributing: dead FUSE mounts are not detected before they bite

After the restart, the mount table listed only **1** `fuse-overlayfs` mount for
the tree, which was dead. After the repair it listed 19. The dead mount was
found only when `repo sync` hit `ENOTCONN` mid-checkout. Neither `list` nor
`status` reported dead or missing overlays beforehand, so the agent's only
remediation was the full `repair` that caused the damage.

### 6. Contributing: `status --json` is empty for an `incomplete` tree

`status --json asfp-cxx-lsp` returned `ok: true` with `projects: []`, giving no
per-project view exactly when one was needed. The agent had to fall back to raw
`git status` in each project.

### 7. Follow-on: `detach --branch` reports success but detaches nothing

`git repo-worktree detach --json --branch asfp-cxx-lsp` returned
`{"ok": true, "result": {"detached": [], "failed": []}}`. But
`git worktree list` still showed `asfp-cxx-lsp` checked out in the poisoned tree
in both `tools/vendor/google` and `tools/adt/idea`. `detach` apparently skips
projects whose recorded state is `failed`, so the poisoned state also hides
branches from the tool meant to release them. An empty `detached` list should
be checked against `git worktree list` (or be an error) when the branch is
actually checked out somewhere.

## Impact

- `repo upload`, `repo forall`, `repo status`, `repo sync`, and the repohooks
  pre-upload hook all fail inside the tree.
- **Wrong workaround:** `repo upload --br=asfp-cxx-lsp` from the *source*
  client (branches are shared) passed its dry run, but repohooks checked the
  source checkout's current `HEAD` commits (`mother`, "Port the GitHub DAP
  architecture…"), not the topic. A real upload this way would have reported a
  hook pass that never checked the uploaded commits.
- No data loss. The `asfp-cxx-lsp` branches (`tools/vendor/google` 5 commits,
  `tools/adt/idea` 1 commit), the rebase result, and the working files are intact.

## Recommendations

### Fix the bugs

1. **Scope and fix the manifest-worktree repair.** Do not run
   `git worktree repair` across every manifests worktree when repairing one tree.
   Either make the bridge's `.repo/manifests/.git` a gitfile, as git expects, or
   repair only the target tree's entry. A failure caused by another tree should
   be a warning, not `E_PARTIAL` for this tree.
2. **Keep step failures separate from project state.** A failure in a
   tree-global step (bridge, manifests worktree, directives) must not set
   `failed/repair-failed` on projects. Record it as a step failure with the
   exact git stderr.
3. **Build the bridge from probed reality, not cached state.** Decide whether
   the tree owns a project by checking the project (`.git` resolves, `git
   rev-parse HEAD` works, the overlay is mounted), not from `meta.json`.
4. **Add guard rails to bridge writes.** Refuse to write a `local_manifests`
   file that removes the `repo-hooks` project, removes every project, or removes
   more than a threshold. Write `manifest.xml`, `project.list`, and
   `local_manifests/*` atomically, keep a `.prev` copy, and self-test after
   writing (`repo list -n` or `repo forall . -c true` from the tree root).
   Roll back if the test fails.
5. **Make `repair` idempotent and safe to retry.** A `repair` that returns
   `E_PARTIAL` must leave the tree no worse than before.

### Improve visibility

6. `status --json <tree>` should list projects for `incomplete` trees, each with
   both the recorded state and a live probe result.
7. `list` and `status` should detect dead or missing overlays (`stat` →
   `ENOTCONN`, or an expected mount missing from `/proc/self/mounts`) and suggest
   a targeted fix.
8. Record per-project failure *detail* in `meta.json` and `history.jsonl`, not
   just `repair-failed`.
9. Add a narrow remount command (for example `repair --mounts-only <tree>`) that
   only restarts overlays and never touches the bridge or project state. Point
   the dead-mount recipe at it.

### Update the skills until fixed

10. `repo-worktree` skill, "Dead Mounts" section: warn that `repair` can rewrite
    the `.repo` bridge, and tell agents to check `repo list -n` in the tree
    before and after.
11. Both skills: say that `repo upload --br=<branch>` from the source client
    runs repohooks against the source checkout's `HEAD`, not the named branch.
    It is not a valid workaround for a broken tree.

## Open questions

- Why did `repo sync` at 17:00 still work if `repair` had been failing since
  10-14 04:26? Either the earlier repairs failed before the bridge-rewrite step,
  or 2.1.0 at 17:02 took a different path. The history entries do not say.
- Why were 18 of the 19 overlays missing from the mount table rather than dead,
  and what did those project directories show while unmounted?
- Is this tree's `format_version` 3 / 2.0.0 metadata handled differently by the
  2.1.0 `repair`?

## Resolution (17:27–17:31)

- Detached the branches in the poisoned tree with plain `git checkout --detach`,
  because `detach` could not see them (§7). The poisoned tree is otherwise left
  intact for debugging.
- `add --name asfp-cxx-lsp-2` → `complete` in about 24 s (20 overlay + 36
  worktree), `post-add` ok, and `repo list -n` lists 56 projects including
  repohooks.
- `repo sync -c`, check out `asfp-cxx-lsp` in both projects, then rebase onto
  `goog/studio-main`, which had advanced one commit in each project. Both
  rebases were clean.
- The ktfmt pre-upload failure **reproduced in the healthy tree**, so it was
  *not* caused by the poisoning. `CMakedClasspath.kt`, new in "Add per-server
  clangd sessions and the ClangdHost seam", was not ktfmt-formatted. A fixup was
  autosquashed into that commit and the Change-Ids were preserved.
- `repo upload` succeeded from the new tree.

## Current state and recovery options

The tree's `.repo` is left as the repair wrote it, preserved for debugging.
Options:

- **Rebuild the bridge by hand** from a healthy sibling's layout: remove
  `local_manifests/zz-git-repo-worktree.xml`, restore a snapshot `manifest.xml`
  (`.repo/repo-worktrees/asfp-cxx-lsp/snapshot.xml`), regenerate
  `project.list`, and point `project-objects` at the source. This edits tool
  state by hand.
- **Throw the tree away:** the branches are shared with the source, so
  `remove --force` the tree, `add` a fresh one, and check out `asfp-cxx-lsp` in
  `tools/vendor/google` and `tools/adt/idea`.
- **Upload without `repo`:** run `pre-upload.py --project <name> <commits...>`
  in each project (this works), then
  `git push goog HEAD:refs/for/studio-main%topic=asfp-cxx-lsp`.
