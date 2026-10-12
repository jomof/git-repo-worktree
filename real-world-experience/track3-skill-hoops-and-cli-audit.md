# Track 3 Audit: Skill Hoops, CLI Discrepancies & Full External `SKILL.md` Reference

This document contains:
1. A side-by-side discrepancy audit between the external operational skill (`~/.gemini/config/skills/repo-worktree/SKILL.md`), the embedded [`skill/SKILL.md`](../skill/SKILL.md), and the actual Rust CLI ([`src/cli.rs`](../src/cli.rs) / [`src/error.rs`](../src/error.rs)).
2. The complete text of the external `~/.gemini/config/skills/repo-worktree/SKILL.md` that agents have been using in production (so a reviewing agent inside this repository can see every defensive workaround agents had to learn).

---

## 1. CLI, Flag, Error Code & JSON Schema Discrepancy Matrix

| Area | External `~/.gemini/config/skills/repo-worktree/SKILL.md` | Embedded [`skill/SKILL.md`](../skill/SKILL.md) | Actual Implementation ([`src/cli.rs`](../src/cli.rs), [`src/error.rs`](../src/error.rs), [`src/tree.rs`](../src/tree.rs)) | Action Needed |
| :--- | :--- | :--- | :--- | :--- |
| **`git repo-worktree --help`** | Warns agents: *"Always use `-h`, not `--help` (`No manual entry for git-repo-worktree`)"* | Uses `git repo-worktree -h` | [`build.rs`](../build.rs#L52-L55) generates `git-repo-worktree.1` in Cargo `OUT_DIR`, but [`install.sh`](../install.sh) never installs it to `~/.local/share/man/man1/`. | Install `git-repo-worktree.1` in `install.sh` so `git repo-worktree --help` works. |
| **Binary Installation Path** | Assumes `git-repo-worktree` on `PATH` matches repo | Installs to `~/.local/bin` | On user's machine, `~/.cargo/bin/git-repo-worktree` (older build) shadows `~/.local/bin/git-repo-worktree`. | Have `install.sh` detect/update/warn when another `git-repo-worktree` binary precedes `~/.local/bin` on `PATH`. |
| **Full Checkout Flag on `add`** | Documents `add --full-all` (lines 118, 224) | Documents `add --full` | [`AddArgs`](../src/cli.rs#L42) only accepts `--full` (`--full-all` fails in `clap`). | Add `#[arg(long, visible_alias = "full-all")]` on `AddArgs::full` and update external skill. |
| **`defaults` Subcommands** | Documents:<br>`defaults set <project> <worktree\|symlink>`<br>`defaults unset <project>`<br>`defaults list` | Documents:<br>`defaults list`<br>`defaults symlink <proj...>`<br>`defaults worktree <proj...>`<br>`defaults auto <proj...>` | [`DefaultsCommand`](../src/cli.rs#L173-L202) implements `list`, `symlink`, `worktree`, `auto`, `clear` (`set`/`unset` fail in `clap`). | Support `set`/`unset` aliases or align external `SKILL.md` with `src/cli.rs`. |
| **`materialize` JSON Output** | Shows `{"materialized": "prebuilts/tools"}` (singular string) | N/A | [`run_materialize_command`](../src/tree.rs#L2264-L2270) outputs `{"tree": "<name>", "materialized": ["..."]}` (array) and fails with `E_NOT_SHARED` if already a worktree. | Make `materialize` idempotent (`already_materialized`) and sync JSON examples in skill. |
| **Error Codes** | Lists `E_NOT_REPO`, `E_NOT_FOUND`, `E_AMBIGUOUS`, `E_EXISTS`, `E_DEST_NOT_EMPTY`, `E_LOCKED`, `E_MANIFEST`, `E_GIT` | Lists `E_DIRTY`, `E_LOCK_TIMEOUT`, `E_DEST_OCCUPIED`, `E_TREE_EXISTS`, `E_TREE_AMBIGUOUS`, `E_NOT_SHARED` | [`src/error.rs`](../src/error.rs) & [`src/tree.rs`](../src/tree.rs) emit `E_NO_REPO_CLIENT`, `E_TREE_NOT_FOUND`, `E_TREE_AMBIGUOUS`, `E_TREE_EXISTS`, `E_DEST_OCCUPIED`, `E_LOCK_TIMEOUT`, `E_REPO_MANIFEST_FAILED`, `E_GIT_WORKTREE_FAILED`. | Sync error code table in external `SKILL.md` and `spec.md`. |
| **`remove` on Dirty Tree (`E_DIRTY`)** | Tell agents to run `status --json` first because `remove` fails | Mentions `E_DIRTY` | [`run_remove_command`](../src/tree.rs#L1877-L1888) sets `result: null` and formats up to 10 dirty project names into `error.message` string; also ignores detached-HEAD commits! | Include structured `dirty_projects` array in error details and check for unbranched commits on detached `HEAD`. |

---

## 2. Full Snapshot of External `~/.gemini/config/skills/repo-worktree/SKILL.md`

Below is the exact text of `/usr/local/google/home/jomof/.gemini/config/skills/repo-worktree/SKILL.md` showing the operational hoops agents had to follow:

```markdown
---
name: repo-worktree
description: Create, manage, and clean up isolated parallel checkouts (worktrees) of Google `repo` workspaces such as `studio-main` using `git repo-worktree`. Use whenever you need an isolated `studio-main` or `repo` checkout for feature implementation, Gerrit CL review, benchmarking, or parallel subagent/swarm work without disturbing the active workspace.
---

# `git repo-worktree` Skill

`git repo-worktree` (also invocable as `git-repo-worktree`) manages multi-repository linked worktrees for Google `repo` clients (especially `~/projects/studio-main`). It snapshots the source workspace's manifest, creates linked Git worktrees for active source repositories, symlinks read-only/shallow prebuilts (`symlink-shared`), and replays manifest `<copyfile>` and `<linkfile>` rules.

> [!CAUTION]
> **Subagent `Workspace: "share"` Incompatibility**:
> Built-in subagent `Workspace: "share"` only understands single-repo Git/Fig checkouts and **breaks** on multi-repo `repo` workspaces like `studio-main`. Whenever spawning parallel workers or isolating work in `studio-main`, keep `Workspace: "inherit"` and create an isolated checkout with `git repo-worktree add` instead.

---

## Critical Operational Rules (Read First!)

1. **Always pass `--json`**: Every command supports `--json`, returning a deterministic envelope (`{ "ok": true, "command": "...", "result": { ... } }`). Parse it with `jq`.
2. **Always use `-h`, not `--help`**: Running `git repo-worktree --help` invokes `man git-repo-worktree`, which fails with `No manual entry for git-repo-worktree`. Always use `git repo-worktree -h` or `git repo-worktree <subcommand> -h` (or `git-repo-worktree --help` with the hyphenated binary name).
3. **Run commands from the source `repo` client (`~/projects/studio-main`)**:
   - Worktrees created by `git repo-worktree` intentionally **do not contain a `.repo/` directory**.
   - Therefore, **raw `repo` CLI commands (`repo status`, `repo sync`, `repo start`, `repo upload`) do NOT work inside a worktree**. Use `git` directly inside the sub-repositories (`<worktree>/tools/adt/idea`, `<worktree>/tools/base`, etc.) or `repox` where supported, and run `git repo-worktree <cmd>` from `~/projects/studio-main` (or from inside the worktree via its `.repo-worktree` breadcrumb).
4. **Copy `env.bazelrc` before running `bazel` in a `studio-main` worktree**:
   - `git repo-worktree add` replays manifest `<linkfile>` and `<copyfile>` rules (such as `.bazelrc`), but **`env.bazelrc` is an untracked local file** in `~/projects/studio-main` that configures remote caching (`--config=rcache`).
   - `.bazelrc` uses `try-import %workspace%/env.bazelrc`, so if `env.bazelrc` is missing in the worktree, `bazel` silently runs **without** remote cache and compiles 38,000+ targets locally from scratch!
   - Immediately after `git repo-worktree add` on `studio-main`, copy `env.bazelrc`:
     ```bash
     cp -f ~/projects/studio-main/env.bazelrc "<worktree_path>/env.bazelrc"
     ```
5. **Respect `symlink-shared` directories (Never edit or run `git` inside them without `materialize`)**:
   - By default, shallow (`clone-depth="1"`) projects (`prebuilts/*`, `tools/external/gradle`, etc.) and configured heavy projects (`tools/idea`, `tools/gpu-profiler/idea`) are symlinked directly to `~/projects/studio-main/<project>` (`"mode": "symlink-shared"`).
   - **Editing files or running `git checkout`/`git commit`/`git reset` inside a `symlink-shared` project silently mutates the primary `~/projects/studio-main` checkout!**
   - If your task needs to modify or switch branches in a shared project (e.g., `tools/idea` or `prebuilts/tools`), either:
     - Pass `--full-project <project>` during `git repo-worktree add`, OR
     - Promote it in an existing worktree with `git repo-worktree materialize <tree-name> <project>`.
6. **Know what revision your worktree starts at (`HEAD` pinning vs. `goog/studio-main` vs. `mother`)**:
   - `git repo-worktree add` runs `repo manifest -r` in `~/projects/studio-main`, which **pins every project to whatever commit is currently checked out (`HEAD`) in `~/projects/studio-main`**.
   - If `~/projects/studio-main` currently has local WIP commits or an unmerged branch checked out in `tools/adt/idea`, the new worktree's detached `HEAD` will start at that local commit, **not** `goog/studio-main`!
   - Always choose the right starting revision in the sub-repositories you touch:
     - **Clean upstream task (new bug fix, greenfield feature, or Gerrit CL review)**: Reset the relevant sub-repo(s) in the worktree to `goog/studio-main`:
       ```bash
       git -C "$WT_PATH/tools/adt/idea" fetch goog studio-main
       git -C "$WT_PATH/tools/adt/idea" checkout --detach goog/studio-main
       ```
     - **Continuing or stacking on the `mother` multi-repo branch**: If the user wants to work on top of `mother` (or another integration branch), verify or check out `mother` (or detach at `mother` if `mother` is already checked out in `studio-main`):
       ```bash
       git -C "$WT_PATH/tools/adt/idea" checkout --detach mother
       ```
7. **Watch out for the Git Worktree Branch-Lock Trap**:
   - Because linked worktrees share the underlying `.repo/projects/<project>.git` repository with `~/projects/studio-main`, **Git forbids checking out the same branch name in two worktrees at once** (`fatal: '<branch>' is already used by worktree at ...`).
   - Do **not** pass `-b <branch>` to `git repo-worktree add` (that creates `<branch>` in all 57+ materialized projects without upstream tracking, which breaks `repox -hb <branch>`).
   - Instead, create a branch **only** in the specific sub-repository you are modifying (`git -C "$WT_PATH/tools/adt/idea" checkout -b <branch>`), and detach `HEAD` (`git checkout --detach HEAD`) before switching `~/projects/studio-main` to that branch.
```

---

## Addendum: `env.bazelrc` Origin (`./build/bazel/env/setup`) & Generic `<md5(workspace_root)>` Rewriting

See [`addendum-env-bazelrc-and-cartfs.md`](addendum-env-bazelrc-and-cartfs.md) for the full analysis of `build/bazel/env/setup_dev_env.py`:
- `env.bazelrc` is generated by running `./build/bazel/env/setup` (not `repo sync`), which walks upward looking for `.repo/` and sets `startup --output_base=/google/cartfs/mount/<md5(str(workspace_root))>`.
- Instead of stripping `startup --output_base` when copying `env.bazelrc` into a new worktree, replacing `<md5(source_root)>` with `<md5(worktree_root)>` (and `<source_root>` with `<worktree_root>`) keeps `git-repo-worktree` 100% generic while giving each worktree an isolated CartFS-backed `output_base`.
