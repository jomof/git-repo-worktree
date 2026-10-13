# Round 3: Blindfold Test Plan with Site Hooks

**Audience:** the orchestrating agent on the Linux workstation with
`~/projects/studio-main`.
**Build under test:** `main` at or after the commit that adds site hooks
(`git repo-worktree version` prints the commit).
**Method:** as in [`round-2-blindfold-test-plan.md`](./round-2-blindfold-test-plan.md):
same blindfold rules (§2), safety rails (§3.6), source-integrity `snap()` (§3.4),
per-scenario measurements (§4), and session tagging (§3.5). This document only
lists what is new or different.
**Deliverable:** `real-world-experience/round-3/` (§7).

## 1. What changed since round 2

Fixes for the round-2 findings ([`round-2/report.md`](./round-2/report.md)):

* **Locks:** read-only commands (`list`, `status`, `report`) never lock;
  commands that change a tree wait (default 300 s, `--lock-timeout`) and lock
  only that tree, so parallel `add`/`remove` of different trees overlap.
* **Pool:** projects are pooled by tracked file count (≥ 30,000) or size
  (≥ 2 GiB). Fixed-depth projects that cannot be overlaid because the source is
  dirty (`prebuilts/studio/sdk/linux`) now fall back to a *pooled* worktree.
  Branched projects use spares. Each project keeps up to 2 spares
  (`pool size N`). `pool fill` with no arguments fills every pooled project;
  `pool status` is an alias of `pool list`.
* **`list --json`** summarizes trees; per-project detail is in `list -v --json`
  or `status --json <tree>`.
* **`remove`** lists processes still using the tree (`lingering_processes`).
* **Installer** warns about any differing shadowing binary; `version` shows
  the commit; the crate version is 2.0.0.
* **Site hooks (new):** executables in
  `~/projects/studio-main/.repo/repo-worktrees/hooks/` named `post-add` and
  `pre-remove` run automatically. This round tests whether they remove the
  Bazel setup and shutdown friction from S6 and S9 without any studio-specific
  agent instructions.

## 2. Setup

### 2.1 Clean up round 2

```bash
cd ~/projects/studio-main
git repo-worktree list --json | jq -r '.result.trees[] | select(.name | startswith("bf-")) | .name'
# remove each listed bf-* tree (inspect with status first; --force only if it holds nothing worth keeping)
git repo-worktree pool clear
repo forall -c 'git branch --list "bf-*"'   # delete leftover bf-* branches
```

Keep the remaining pre-v2 trees untouched.

### 2.2 Install the build

Follow round 2 §3.1 (build, install binary and man pages, check `which -a`),
using the new `main`. Confirm `git repo-worktree version` prints `2.0.0 (<commit>)`.
Re-run `git repo-worktree skill sync` so the installed skill matches the binary.
Keep `~/.gemini/config/skills/repo-worktree` moved aside, as in round 2.

### 2.3 Install the site hooks

```bash
H=~/projects/studio-main/.repo/repo-worktrees/hooks
mkdir -p "$H"
cat > "$H/post-add" <<'EOF'
#!/bin/sh
# Give each tree its own env.bazelrc (remote cache, per-tree output_base).
exec ./build/bazel/env/setup
EOF
cat > "$H/pre-remove" <<'EOF'
#!/bin/sh
# Stop this tree's Bazel server, if one is running, without starting one.
ob=$(sed -n 's/^startup --output_base=//p' env.bazelrc 2>/dev/null | tail -n 1)
[ -n "$ob" ] && [ -f "$ob/server/server.pid.txt" ] || exit 0
exec bazel shutdown
EOF
chmod +x "$H/post-add" "$H/pre-remove"
git repo-worktree hooks list
```

Before the scenarios, verify by hand once: create a tree, check
`.result.hooks["post-add"]` is `ok`, that `<tree>/env.bazelrc` exists with an
`output_base` different from studio-main's, and record the hook's `ms`. Run a
small `bazel build` there, then `remove` the tree and confirm the
`pre-remove` hook stopped the server (`pgrep -af "workspace_directory=<tree>"`
is empty, `lingering_processes` is empty). If `./build/bazel/env/setup` prompts,
needs credentials, or prints a lot, adjust the hook (for example
`exec ./build/bazel/env/setup >/dev/null`) and record what you changed.

Do **not** add anything about Bazel or hooks to any skill the subagents load.
The bundled skill mentions hooks generically; that is part of what is being
tested.

### 2.4 Sessions

Tag scenarios as `GIT_REPO_WORKTREE_SESSION=R3-<id>`. Do not reuse round-2
labels; the history file keeps both rounds.

## 3. Reporting accuracy

Round 2's report paraphrased several tool outputs inside quotation marks (for
example the `E_DIRTY` message and the `detach --dry-run` JSON did not match the
binary's real output). This round:

* Quote tool output only by copying it from the transcript, `report --json`, or
  `history.jsonl`. Mark anything reconstructed as a paraphrase.
* When attributing a problem to a code location, cite the file and function
  you actually read, or say that it is a hypothesis.

## 4. Blindfold scenarios

Prompts are given verbatim to fresh subagents in `~/projects/studio-main`, as in
round 2. *Italics* are orchestrator notes.

### R3-S6. Bazel build in a tree (repeat of S6)

> In an isolated workspace, build //tools/base/common:tools.common with bazel,
> using remote caching like studio-main does. Report how long the build took
> and how many actions ran locally.

*Check:* tool calls spent on `env.bazelrc` (round 2: 14). Did the agent notice
`hooks.post-add` in the `add` output or look for `env.bazelrc` anyway?
`bazel info output_base` differs from studio-main's; remote cache hits; build
time.

### R3-S9. Clean up after a build (repeat of S9)

*Reuse the R3-S6 tree, or build in a fresh one.*

> Remove the workspace from the build task. Keep anything worth keeping.

*Check:* `hooks.pre-remove` status and `ms` in the `remove` result; no Bazel
server left (`pgrep`), `lingering_processes` empty (round 2: 2.18 GB server
leaked). `remove` phase timings, including `hook-pre-remove`.

### R3-S11. Parallel agents (repeat of S11)

Three subagents at once, each with:

> Create your own isolated workspace named bf-r3-<n>, change one comment in
> tools/base/common, commit it on branch bf-r3-<n>, then remove the workspace
> keeping the branch.

*Check:* `E_LOCKED` count (round 2: 28) and any "waiting for tree/registry
lock" messages; whether the three `add` calls overlapped in time (compare `ts`
and `ms` in the history); `lock-wait` phases; pool hits and misses, including
`tools/base` for the branched project; `returned_to_pool` on each `remove`;
per-agent wall clock and tool calls (round 2: 81 total, 27 on retries).

### R3-H1. A hook that fails

*Before:* replace `post-add` temporarily with:

```sh
#!/bin/sh
echo "env setup failed: credentials expired; run gcert, then repair the tree" >&2
exit 1
```

> In an isolated workspace, build //tools/base/common:tools.common with bazel,
> using remote caching like studio-main does.

*Check:* did the agent notice the failed hook (stderr warning,
`hooks.post-add.status: "failed"`)? Did it fix the cause, run
`git repo-worktree repair`, or set up `env.bazelrc` by hand? Restore the real
hook, then have the agent (or yourself) run `repair <tree>` and confirm the hook
reaches `ok` without recreating the tree.

### R3-H2. An existing tree that missed the hook

*Before:* create a tree with `git repo-worktree add --no-hooks ../bf-r3-h2`.

> Use the workspace at ../bf-r3-h2 to build //tools/base/common:tools.common
> with bazel, using remote caching like studio-main does.

*Check:* how the agent obtained `env.bazelrc` (`repair`, running the setup
script itself, copying studio-main's file — the round-1 trap) and tool calls
spent.

### R3-S2. Branch in a pooled project (regression)

> In an isolated workspace (not studio-main), make a one-line comment change in
> any Kotlin file under tools/adt/idea, commit it on a new branch called
> bf-r3-s2, and show me what `repo upload` would send. Do not actually upload.

*Check:* `tools/adt/idea` strategy is `pool` (when a spare exists) and the
branch has `branch.bf-r3-s2.merge = refs/heads/studio-main`; still exactly one
project with the branch.

### R3-S8. Clean up with unsaved work (regression)

Same as round 2 S8, with `bf-r3-s8`. *Check:* `E_DIRTY` details, and that
`pre-remove` did **not** run while removal was refused.

## 5. Performance (orchestrator, not blindfolded)

3 runs each, medians, tagged `R3-perf-<row>`; same methods as round 2 §6.

| Row | Operation | Compare with round 2 |
|---|---|---|
| Q1 | `pool clear`, then `add` (cold pool) | P1 35.8 s |
| Q2 | `remove` the Q1 tree, then `add` again (warm pool, includes `sdk/linux` spare) | P2 31.4 s; expected ~9 s |
| Q3 | `remove` default vs `remove --no-pool` | P5 22.8 s vs 34.9 s |
| Q4 | Three `add` commands started together, different names | new: total wall clock vs 3 × Q2 |
| Q5 | `post-add` and `pre-remove` hook durations | new |
| Q6 | `git repo-worktree list --json` size in bytes with all trees present | round 2: ~90 KB |

Also record `du -sh .repo/repo-worktrees/.pool` after Q2 (spares now include
`prebuilts/studio/sdk/linux`) and which projects the file-count rule pooled
(`add` records' per-project `pooled` field and `report` pool hits and misses).

## 6. Questions to answer

1. Do the hooks remove the Bazel friction (R3-S6 tool calls, R3-S9 leaked
   server)? Is `./build/bazel/env/setup` fast and quiet enough as a `post-add`
   hook?
2. When a hook fails (R3-H1) or never ran (R3-H2), do agents recover without
   falling into the `env.bazelrc` copy trap?
3. Is lock contention gone for parallel agents (R3-S11)? Do parallel `add`s
   overlap (Q4)?
4. Does warm `add` reach ~9 s with `sdk/linux` pooled (Q2)? Is the extra spare
   disk acceptable?
5. Did the file-count pooling rule pick the right projects?
6. Did any scenario change studio-main unexpectedly? (Must be "no".)
7. New rough edges, ranked by tool calls wasted, with verbatim quotes.

## 7. Deliverable

Commit to `real-world-experience/round-3/` the same artifact set as round 2
(`report.md`, `sessions/R3-*.json`, `history.jsonl`, `environment.txt`,
`integrity.txt`), plus `hooks/` with the exact hook scripts used and any
changes made to them. Afterwards remove `bf-r3-*` trees and branches; leave the
hooks installed unless they caused problems.
