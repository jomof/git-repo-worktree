# Round 2: Blindfold Test Plan for `git-repo-worktree` v2

**Audience:** the orchestrating agent on the Linux workstation with
`~/projects/studio-main`.
**Build under test:** `main` at or after commit `60fc84f` (implements
[`../spec-v2.md`](../spec-v2.md)).
**Deliverable:** a report committed under `real-world-experience/round-2/` (§8).

## 1. Goals

1. **Did the rough edges from round 1 get smoother?** Run realistic tasks with
   subagents that know only what the tool itself teaches them (the bundled skill,
   `--help`, error messages). Record where they still stumble.
2. **Real performance data at studio-main scale** for the new default modes
   (pool for heavy source projects, overlay for fixed-depth projects) against
   the old ones.
3. **Safety:** confirm that no scenario changes the source client in any way the
   task did not ask for.

## 2. What "blindfold" means

Subagents receive only the task prompt from §5. They must not see:

* this plan, the round-1 reports, `spec*.md`, or the repository source;
* the old external skill `~/.gemini/config/skills/repo-worktree/SKILL.md`
  (it documents workarounds that no longer apply; see §3.2);
* any hint about the tool's features, flags, or past problems.

They may use anything installed on the machine: `git repo-worktree --help`,
`-h`, the bundled skill (installed in §3.2), error messages, and `report`.

The orchestrator does not help during a scenario. If a subagent is stuck for
more than ~15 tool calls on tool mechanics, stop it, record that as the
finding, and move on.

## 3. Setup (orchestrator, not blindfolded)

### 3.1 Install the build under test

No release has been cut yet. Build and install it so the copy Git runs is
unambiguous:

```bash
git clone https://github.com/jomof/git-repo-worktree ~/src/git-repo-worktree   # or git pull
cd ~/src/git-repo-worktree && git log -1 --oneline
cargo build --release --locked
mkdir -p ~/rw2 && cp "$(command -v git-repo-worktree)" ~/rw2/git-repo-worktree.round1   # for S14
install -m 755 target/release/git-repo-worktree ~/.local/bin/git-repo-worktree
mkdir -p ~/.local/share/man/man1
cp "$(dirname "$(find target/release/build -path '*/out/man/git-repo-worktree.1' | head -1)")"/*.1 ~/.local/share/man/man1/
which -a git-repo-worktree           # record; remove or rename stale copies, e.g. ~/.cargo/bin
git repo-worktree version -v         # must report this build
git repo-worktree --help | head -3   # must open the manual page
```

Record the `which -a` output before you fixed it: a stale `~/.cargo/bin` copy
was a round-1 finding.

`fuse-overlayfs` and `fusermount3` must be on `PATH`. Confirm with
`git repo-worktree report --json` after the first `add` (environment
`overlay_unavailable` must be `null`).

### 3.2 Skills

```bash
mv ~/.gemini/config/skills/repo-worktree ~/repo-worktree-skill.round1.bak   # restore after the run
git repo-worktree skill list
git repo-worktree skill add --scope user
git repo-worktree skill list         # record where it was installed
```

If the subagent framework loads skills from somewhere else, install there with
`skill add --path <dir>`. Studio-specific guidance (for example running
`./build/bazel/env/setup` inside a new tree) is deliberately **not** in the
bundled skill. Do not add it for this run; scenario S6 measures whether its
absence matters.

### 3.3 Existing trees

Round 1 left ~14 `studio-main-*` trees created by the old binary (metadata
format 2, no `.repo`, `symlink-shared` prebuilts). Leave them in place. They
are used by S13 and must keep working.

### 3.4 Source-integrity baseline

Take a snapshot before the run and after **every** scenario:

```bash
cd ~/projects/studio-main
snap() {
  repo forall -c 'printf "%s %s %s\n" "$REPO_PATH" "$(git rev-parse HEAD)" "$(git symbolic-ref -q --short HEAD || echo -)"' | sort
  repo forall -c 'git status --porcelain=v1 -uno | sed "s|^|$REPO_PATH: |"' | sort
  for f in env.bazelrc .bazelrc; do printf '%s %s\n' "$f" "$(sha256sum "$f" 2>/dev/null | cut -c1-16)"; done
  git -C .repo/manifests rev-parse HEAD
  find . -maxdepth 4 -name .git -type l -printf '%p -> %l\n' | sort | sha256sum
}
snap > /tmp/rw2-baseline.txt
```

Use `-uno` to keep it fast; S3 adds a targeted untracked-file check. Any
difference after a scenario that the task did not call for is a **critical**
finding: stop, record, and restore before continuing.

### 3.5 History and sessions

Every invocation is logged to `~/projects/studio-main/.repo/repo-worktrees/history.jsonl`.
Before each scenario:

```bash
export GIT_REPO_WORKTREE_SESSION=S<n>
```

The subagent's shell must inherit this variable. If your framework cannot pass
it through, record each scenario's start and end times instead and use
`report --last`.

After each scenario, save `git repo-worktree report --session S<n> --json`.

### 3.6 Safety rails for every scenario

* No real `repo upload` or `git push`. Prompts say "dry run"; if a subagent
  tries to really upload, stop it.
* No networked `repo sync` in the source client during the run.
* Use only throwaway branch names starting with `bf-`.
* Remove every tree a scenario creates unless the next scenario reuses it.

## 4. Measurements to collect

For every scenario, record:

| Field | How |
|---|---|
| Outcome | done / done with help / failed |
| Wall clock | start to end of the subagent |
| Tool calls | total, and how many were about `git-repo-worktree` or `repo` mechanics rather than the task |
| Help used | did it run `--help`, `-h`, `skill show`, read the man page? Did that help? |
| Errors hit | error codes from `report --session`, plus anything else it saw |
| Workarounds invented | anything the tool should have done (e.g. manual `cp`, `rm -rf` of a tree, raw `git worktree` commands inside `.repo`, editing `.repo` files) |
| Risky actions | `--force` without asking, deleting work, touching the source |
| Source integrity | `diff /tmp/rw2-baseline.txt <(snap)` is empty |
| Quotes | short transcript excerpts at every stumble |

## 5. Blindfold scenarios

Each prompt is given verbatim to a fresh subagent working from
`~/projects/studio-main`. Text in *italics* is orchestrator setup or checks, not
part of the prompt.

### S1. Discovery and starting revisions (CUJ 1, 2)

> I want to try a change without disturbing my studio-main checkout. Using the
> repo-worktree tool, set up an isolated copy of the whole workspace. Before you
> create it, tell me exactly which commit each project will start at and why.
> Then create it and show me its state.

*Before:* check out a `bf-s1` branch with one local commit in
`tools/adt/idea` of the source (restore afterwards).
*Check:* the answer says each project starts at the source's current commit,
detached, including the local `bf-s1` commit, and not at `goog/studio-main`.
Did `git repo-worktree --help` open the man page? Did it use `status`?

### S2. Small change, branch, upload dry run (CUJ 2, 5)

> In an isolated workspace (not studio-main), make a one-line comment change in
> any Kotlin file under tools/adt/idea, commit it on a new branch called
> bf-s2-topic, and show me what `repo upload` would send. Do not actually upload.

*Check:*
* `bf-s2-topic` exists in exactly one project: `repo forall -c 'git branch --list bf-s2-topic'`
  in the source. Round 1 polluted 29 projects.
* `branch.bf-s2-topic.merge` is `refs/heads/studio-main` and `.remote` is `goog`.
* `repo upload --dry-run` ran **inside the tree** and selected the branch.
* Did the agent use `-b ... -p`, `repo start`, or `git switch -c`? All are fine; record which.

### S3. Edit inside a large prebuilt (CUJ 4)

> In an isolated workspace, I need to patch a file under prebuilts/studio/sdk
> (any small text file) and run `git status` and `git diff` there to show the
> change. My studio-main checkout must not change.

*Check:*
* The project's mode in `list --json` is `overlay`; if it is `symlink-shared`, record why.
* After the edit: `git -C ~/projects/studio-main/prebuilts/studio/sdk status --porcelain` is empty, and the file's hash is unchanged.
* `realpath` of the edited file stays inside the tree.
* Time for the first `git status` in that project; the subagent's experience of it.

### S4. Project paths that are really subdirectories (CUJ 2)

> Create an isolated workspace where I can edit files in
> tools/vendor/google/android-apk and tools/adt/idea/android. Make sure those
> parts are fully editable, then confirm.

*Check:* whatever flags were used (`--full-project`, `materialize`, or none),
did subpaths resolve without `E_UNKNOWN_PROJECT`? Was `materialize` run on
already-editable projects, and did it succeed as a no-op?

### S5. repo and repox inside the tree (CUJ 5)

*Before:* create a tree and make an uncommitted change in two projects.

> In the workspace at <tree path>, list every project with local changes, using
> repo and repox the way you would in studio-main. Then return all projects to
> the workspace's original revisions without losing the uncommitted changes.

*Check:* `repo status` and `repox -hc git status -s` work from the tree root
and both report the two projects. **repox inside a tree is untested** and the
highest-value check here. Was `repo sync -l` used, and did it keep the changes
and the `.git` links (`readlink <tree>/tools/base/.git`)?

### S6. Bazel build in a tree (CUJ 3)

> In an isolated workspace, build //tools/base/common:tools.common (or a
> similarly small target) with bazel, using remote caching like studio-main
> does. Report how long the build took and how many actions ran locally.

*Check:*
* Did the agent find that `env.bazelrc` is missing, and how did it fix it?
  Running `./build/bazel/env/setup` inside the tree is the expected fix; copying
  studio-main's file is the round-1 trap.
* `bazel info output_base` in the tree differs from studio-main's.
* Total time and local action count; whether remote cache was used.
* Was `bazel shutdown` run in the tree afterwards? Check for a leftover server
  with `pgrep -af "workspace_directory=<tree>"`.

This is the one scenario where studio-specific knowledge may be needed. Report
whether a one-line note in the studio skill would have avoided the problem.

### S7. Hand off a branch (CUJ 6)

*Before:* in the source, create `bf-s7` with one commit in `tools/adt/idea`
and leave it checked out there.

> Continue the work on branch bf-s7 in an isolated workspace instead of in
> studio-main. Add one more commit to it there.

*Check:* how the agent got past "already used by worktree": `detach`, a manual
`git checkout --detach` in the source, or something else? Count the tool calls
spent. At the end, which worktree holds `bf-s7`
(`git repo-worktree detach --branch bf-s7 --dry-run --json`)? Restore the
source's branch state afterwards.

### S8. Clean up with unsaved work (CUJ 7)

*Before:* create tree `bf-s8`; in `tools/base`, commit on the detached HEAD
("bf-s8 work") and leave an untracked file in another project.

> I'm done with the workspace bf-s8. Clean it up.

*Check:* `remove` refuses with `E_DIRTY`, and the details name the
unbranched commit and the untracked file. Did the agent preserve the commit
(branch it) or ask before `--force`? Using `--force` without asking is a
**risky action**. Afterwards, does the commit still exist
(`git -C ~/projects/studio-main/tools/base log --all --oneline | grep "bf-s8 work"`)?

### S9. Clean up after a build (CUJ 7)

*Reuse the S6 tree, or build something in a fresh one.*

> Remove the workspace from the build task. Keep anything worth keeping.

*Check:* how the agent judged build outputs versus real changes using
`status` or the `E_DIRTY` details. Record the `remove` phase timings from the
report, and whether the Bazel server was still running.

### S10. Interrupted creation

*Before:* start `git repo-worktree add ../bf-s10` yourself and kill it with
SIGINT once roughly half the projects are done (`add -v` shows progress).

> A colleague started creating a workspace at ../bf-s10 but it was interrupted.
> Get it into a usable state.

*Check:* `add --continue` was used rather than deleting and recreating. Final
state is `complete`.

### S11. Parallel agents

Run three subagents at once with the same prompt and different names:

> Create your own isolated workspace named bf-s11-<n>, change one comment in
> tools/base/common, commit it on branch bf-s11-<n>, then remove the workspace
> keeping the branch.

*Check:* any `E_LOCKED` or `E_SYNC_ACTIVE` (exit 4), and how agents handled it
(retry vs give up). Did pool spares get contended? Per-agent wall clock.

### S12. Overlays after a reboot

*Before:* create a tree, then unmount its overlays to simulate a reboot:
`grep "$TREE" /proc/self/mountinfo | awk '{print $5}' | xargs -n1 fusermount3 -u`.

> Use the workspace at <tree path> to show the contents of one file under
> prebuilts/studio/sdk.

*Check:* did the agent notice the empty directory, find `status`
(`mounted: false`) or `repair`, and recover? Tool calls spent.

### S13. Trees from the old version

> List all the isolated workspaces of studio-main, pick the oldest one that has
> no unsaved work, and remove it.

*Check:* old (format 2) trees list correctly and remove cleanly. Source
integrity, especially for their `symlink-shared` prebuilts.

### S14. Is the right tool installed?

*Before:* put the round-1 binary back earlier on `PATH` temporarily:
`cp ~/rw2/git-repo-worktree.round1 ~/.cargo/bin/git-repo-worktree`. Remove it
afterwards.

> Make sure the repo-worktree tool on this machine is the latest version and
> working correctly.

*Check:* did the agent notice two copies? If it reinstalled with `install.sh`,
did it see the shadow warning?

## 6. Performance matrix (orchestrator, not blindfolded)

Run each row 3 times and report the median. For cold runs, drop caches if you
can (`sync; echo 3 | sudo tee /proc/sys/vm/drop_caches`); otherwise mark the
results warm-only. Use `GIT_REPO_WORKTREE_SESSION=perf-<row>` and save
`report --session perf-<row> --json`.

| Row | Operation | Notes |
|---|---|---|
| P1 | `add` default, no spares (`pool clear` first) | baseline for the new defaults; per-project `populate_ms` |
| P2 | `add` default, warm pool (after a P1 tree is removed) | pool hit time for `tools/idea`, `tools/gpu-profiler/idea` |
| P3 | `add --no-overlay` | old-style sharing of prebuilts |
| P4 | `add --full` | everything materialized; disk usage too |
| P5 | `remove` of P1 tree (pool return) vs `remove --no-pool` | phases from the report |
| P6 | `git status` in an overlay prebuilt, cold and warm | e.g. `prebuilts/studio/sdk/linux` |
| P7 | `git status` in `tools/idea` (pool worktree) vs source | |
| P8 | `rg -l someRareString` over the tree vs studio-main | overlay walk cost |
| P9 | `git repo-worktree status` on a fresh tree | slow inspections from the report |
| P10 | `repo status` from the tree root vs studio-main | bridge overhead |
| P11 | unmount all overlays, then `repair` | remount time |
| P12 | `pool fill` for the pooled projects | spare creation time |

Also record disk use: `du -sh` of a P1 tree, of
`.repo/repo-worktrees/.pool`, and of `.repo/repo-worktrees/*/overlays` after S3.

Note which projects the defaults pooled (each `add` record's per-project
`pooled` field, the report's pool hits and misses, and `slowest_projects`). A pooled project that rarely changes but costs a lot of
disk is a finding.

## 7. Questions to answer explicitly

1. Do pool and overlay deserve to stay the defaults on this machine? Back the
   answer with P1–P8.
2. Is any project pooled or overlaid that should not be, or the reverse?
3. Did any scenario change studio-main unexpectedly? (Must be "no".)
4. Which rough edges from round 1 are fixed, partly fixed, or still present?
   Go through the CUJ 1–7 checklist at the end of
   [`agent-rough-spots-report.md`](./agent-rough-spots-report.md).
5. What new rough edges appeared? Rank by agent tool calls wasted.
6. Does repox work inside a tree (S5)?
7. Is a studio-specific note (Bazel setup, shutdown) needed in the studio
   skill (S6, S9)?

## 8. Deliverable

Commit to `real-world-experience/round-2/`:

* `report.md`: a summary table with one row per scenario (fields from §4), the
  performance matrix with medians, answers to §7, and ranked rough edges, each
  with a quote and the scenario ID.
* `sessions/S<n>.json` and `sessions/perf-<row>.json`: the `report --json`
  outputs.
* `history.jsonl`: a copy of the full history file for the run.
* `environment.txt`: `git repo-worktree version -v`, `which -a git-repo-worktree`
  before and after the fix, `uname -a`, `fuse-overlayfs --version`, `df -T` of
  the home and tree filesystems.
* `integrity.txt`: the baseline and final `snap` outputs and any differences seen
  along the way.

Restore afterwards: move the round-1 skill back if wanted, remove `bf-*`
branches and trees, and return any source branch state the setup changed.
