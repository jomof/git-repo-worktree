# Agent Rough Spots & Critical User Journey Friction Report: `git-repo-worktree`

**Target Audience**: Autonomous Engineering / Architecture Review Agent auditing [`git-repo-worktree`](../) to revise specifications ([`spec.md`](../spec.md), [`spec-user-journeys.md`](../spec-user-journeys.md), [`spec-fast-add.md`](../spec-fast-add.md)) and implementation ([`src/`](../src)).

**Evidence Base**:
1. **Production Agent Session Transcripts**: Multi-hour sessions on `~/projects/studio-main` (57 Git projects, ~37 GB, ~660,000 files on `ext4`), including conversations `c611aa69-19dd-489e-988d-220c114c7393`, `512d6afd-da2c-4636-8573-d0728f1a81cb`, and Code Red prompt-tuning audit `aec56779-7e3d-4f56-8346-87bd77fbf8c4`.
2. **Accumulated Defensive Workarounds in Skills**: Comparison of the user's operational skill `~/.gemini/config/skills/repo-worktree/SKILL.md` (298 lines of defensive warnings and multi-step bash ceremonies; preserved in [`track3-skill-hoops-and-cli-audit.md`](./track3-skill-hoops-and-cli-audit.md)) against the repository's embedded [`skill/SKILL.md`](../skill/SKILL.md) (115 lines).
3. **Live Inspection of 14 Active `studio-main-*` Worktrees**: Forensic audit of `/usr/local/google/home/jomof/projects/studio-main/.repo/repo-worktrees/*` and `/usr/local/google/home/jomof/projects/studio-main-*`.

---

## Executive Summary: Critical User Journey (CUJ) Scorecard

[`spec-user-journeys.md`](../spec-user-journeys.md) currently covers creation (`add`), inspection (`list`/`status`), health (`doctor`/`repair`), removal (`remove`/`prune`), and `symlink-shared` promotion (`materialize`/`defaults`). However, **real agent workflows span 7 end-to-end Critical User Journeys**, every single one of which currently requires undocumented manual workarounds or fails catastrophically:

| CUJ | Journey Description | Current Agent Experience | Severity |
| :--- | :--- | :--- | :--- |
| **CUJ 1** | **Discover & Invoke CLI (`--help`, `version`, `--json`)** | `git repo-worktree --help` fails (`No manual entry`); `~/.cargo/bin` shadows `~/.local/bin`; JSON envelope field names and flags differ between docs and [`src/cli.rs`](../src/cli.rs). | **High** |
| **CUJ 2** | **Provision Isolated Worktree (`add`)** | By design, `add` pins the detached `HEAD` commits of the initiating repo, but `--help` lacks a manpage and docs/CLI output don't clearly explain this behavior to agents; passing subpaths to `--full-project` fails with `E_UNKNOWN_PROJECT`. | **High** |
| **CUJ 3** | **Build & Test Inside Worktree (`bazel build/test`)** | Missing untracked `env.bazelrc` drops `--config=rcache` (triggering 38,000-action local C++/Rust builds that hang 45+ min). Worse: copying `env.bazelrc` verbatim copies `startup --output_base=...`, hijacking `studio-main`'s Bazel server lock! | **Blocker** |
| **CUJ 4** | **Edit, Inspect & Touch Shared Projects (`symlink-shared`)** | Directory symlinks to `studio-main` silently corrupt the source checkout when edited, built into, or when `git` runs inside them; `realpath` escapes the worktree; `repox`'s `os.walk` skips symlinked repos entirely. | **Blocker** |
| **CUJ 5** | **Multi-Repo Branching, Navigation & Upload (`repo` / `repox`)** | `repo status`, `repo start`, `repo upload`, `repox -hc`, `repox -hb`, `repox stack` all fail inside a worktree because `.repo/` is omitted (P1 non-goal). Agents using `add -b <branch>` pollute all 57 repos without upstream tracking (`branch.<name>.merge`). | **Blocker** |
| **CUJ 6** | **Exchange WIP & Commits with Primary Checkout** | Shared `.repo/projects/<path>.git` locks branches across worktrees (`fatal: '<branch>' is already used by worktree`); no built-in way to detach the primary workspace's branch or transplant dirty WIP. | **High** |
| **CUJ 7** | **Teardown & Cleanup (`remove` / `prune`)** | Build side-effects trigger `E_DIRTY` with `result: null` and truncated error strings; orphaned worktree Bazel JVM daemons stay alive holding tens of GB of RAM; detached-HEAD commits are silently dropped without an unmerged-commit warning. | **High** |

---

## Detailed Rough Spots & Failure Modes

### Rough Spot 1: The `env.bazelrc` Double-Trap (38,000-Action Local Rebuilds vs. `--output_base` Hijacking)
* **Affected Journey**: CUJ 3 (Build & Test Inside Worktree)
* **Code Locations**: [`reconcile_links_and_copies`](../src/tree.rs#L1390-L1485), [`extract_links_and_copies`](../src/manifest.rs#L327-L365)
* **Empirical Evidence**:
  * In session `c611aa69` (prompt #8), the user had to interrupt the agent: *"You sure that bazel test run isn't hung?"*
  * Root cause 1: `studio-main/.bazelrc` (symlinked from `tools/base/bazel/toplevel.bazelrc`) contains `try-import %workspace%/env.bazelrc`. Because `env.bazelrc` is an untracked machine-local file generated outside the manifest's `<copyfile>`/`<linkfile>` list, `git repo-worktree add` does not copy it. Without `env.bazelrc`, Bazel runs without `--config=rcache` / RBE settings and compiles 38,000+ C++, Rust, and Protobuf targets from scratch.
  * Root cause 2 (**New Forensic Finding across 8 of 9 `studio-main-*` worktrees**): To work around Root Cause 1, the external `repo-worktree` skill instructs agents to run `cp ~/projects/studio-main/env.bazelrc "$WT_PATH/env.bazelrc"`. However, `~/projects/studio-main/env.bazelrc` contains:
    ```text
    build --config=rcache
    startup --max_idle_secs=10800
    startup --output_base=/google/cartfs/mount/82f1a270dd3e54436acdeaa58b68077a
    ```
    When copied verbatim into a worktree, running `bazel` in the worktree **forces it to share `studio-main`'s exact `--output_base`**! This causes the worktree's Bazel invocation to contend for `studio-main`'s server lock, overwrite `execroot/_main` symlinks back and forth between `studio-main` and `studio-main-<wt>`, and invalidate incremental analysis caches in both workspaces.
* **What the Reviewing Agent Should Address**:
  * `git-repo-worktree add` must support workspace environment bootstrapping (e.g., copying/filtering untracked root `.bazelrc` / `env.bazelrc` / `local.properties` files configured via `.repo/repo-worktrees/config.json` or built-in heuristics).
  * Specifically for `env.bazelrc` (or `*.bazelrc`), `git-repo-worktree` should **strip or rewrite `startup --output_base=...`** (or synthesize a worktree-specific `output_base` on the same fast mount if `/google/cartfs/mount/...` is used, or let Bazel compute its own workspace-hash `output_base`).

---

### Rough Spot 2: Silent Source-Workspace Corruption via `symlink-shared`
* **Affected Journey**: CUJ 4 (Edit, Inspect & Touch Shared Projects)
* **Code Locations**: [`decide_project_population_modes`](../src/tree.rs#L436-L464), [`create_shared_project_symlink`](../src/tree.rs#L1185-L1200), [`spec-fast-add.md`](../spec-fast-add.md)
* **Empirical Evidence**:
  * On Linux `ext4` (where `FICLONE` reflink returns `EOPNOTSUPP`), [`decide_project_population_modes`](../src/tree.rs#L436-L464) symlinks all `clone-depth="1"` projects (`prebuilts/*`, `tools/assets`, `tools/external/gradle`) plus anything in `.repo/repo-worktrees/defaults.json` (`tools/idea`, `tools/gpu-profiler/idea` — 274k files each) directly to `/usr/local/google/home/jomof/projects/studio-main/<project>`.
  * **Six distinct ways this bites agents**:
    1. **Silent File Mutation**: If an agent, patch script, or code generator (`bazel run //tools/base/bazel:iml_to_build`, test output, formatter) writes to a file under a `symlink-shared` project (such as `tools/idea`), it directly dirties `~/projects/studio-main`.
    2. **Silent Git State Mutation**: Because `<wt>/tools/idea` is a directory symlink to `<source>/tools/idea`, `<wt>/tools/idea/.git` resolves to `<source>/tools/idea/.git`. Any agent running `git -C <wt>/tools/idea checkout/commit/reset/clean` mutates the primary `studio-main` checkout's `HEAD` and index.
    3. **Path Canonicalization (`realpath`) Escape**: Build tools, IDEs, and scripts that call `realpath()` or `Path::canonicalize()` on `<wt>/tools/idea/...` resolve out of `<wt>` back into `~/projects/studio-main/tools/idea/...`, breaking relative path calculations (`../../tools/adt/idea` resolves to `studio-main/tools/adt/idea` instead of `<wt>/tools/adt/idea`!).
    4. **Invisible to Directory Walkers (`repox`, `fd`, `rg`)**: `os.walk(repo_root)` (used by `repox` in `workspace.py:189`) and default `rg` / `fd` do not follow directory symlinks (`followlinks=False`), making all `symlink-shared` projects invisible to workspace search and multi-repo tooling.
    5. **Blind Spot in `dirty_projects()` and `remove`**: [`dirty_projects`](../src/tree.rs#L1968-L2000) filters for `project.mode == "worktree"`, skipping `symlink-shared` projects entirely.
    6. **5.5 GB Directive Demotion Penalty**: Because `prebuilts/studio/sdk/linux` (5.5 GB) is the target of a manifest `<linkfile>` (`prebuilts/studio/sdk/BUILD`), [`decide_project_population_modes`](../src/tree.rs#L450-L454) demotes it from `symlink-shared` to a slow full checkout.
* **What the Reviewing Agent Should Address**:
  * Replace raw directory symlinks to the live mutable source checkout with safe CoW FUSE overlay isolation (see [`industrial-strength-plan.md`](./industrial-strength-plan.md) and [`track2-fuse-and-fast-isolation-benchmarks.md`](./track2-fuse-and-fast-isolation-benchmarks.md)).

---

### Rough Spot 3: `repo` and `repox` Commands Completely Fail Inside Worktrees
* **Affected Journey**: CUJ 5 (Multi-Repo Branching, Navigation & Upload)
* **Code Locations**: [`spec.md` P1 Non-Goal](../spec.md#L15-L18), [`run_add_command`](../src/tree.rs#L173-L320)
* **Empirical Evidence**:
  * In `c611aa69` step 580, an agent inside `/usr/local/google/home/jomof/projects/studio-main-soong` ran `repo status` and failed with:
    ```text
    error: command 'status' requires repo to be installed first.
            Use "repo init" to install it here.
    ```
  * In `c611aa69` steps 494–496, an agent inside `/usr/local/google/home/jomof/projects/studio-main-cxx-lsp` ran `repox -hc git status -s` and `repox -hb asfp-cxx-lsp git status -s`. Because `repox` locates the workspace root by walking up parents looking for `.repo` (`workspace.py:108-110`), `workspace.repo_root_path` was `None`, causing `repox` from the worktree root to return an empty repo list (`roots = []`) and silently produce zero output!
  * Even `git-repo-worktree version -v` ([`src/main.rs:583-595`](../src/main.rs#L583-L595)) fails to report the `repo` version when run outside a `.repo` client (`repo <repo not installed>`) because `/usr/bin/repo` is a wrapper that looks for `.repo/repo/main.py`.
* **What the Reviewing Agent Should Address**:
  * Retire P1 ("No `.repo` in created trees"). Synthesize a worktree-local `.repo/` bridge directory and per-project `.git -> ../../.repo/projects/<path>.git` symlink structure (Model E1 in [`track1-repo-fidelity-and-symlinks-spike.md`](./track1-repo-fidelity-and-symlinks-spike.md)) so `repo status`, `repo diff`, `repo start`, `repo branches`, `repo abandon`, `repo checkout`, `repo forall`, `repo manifest -r`, `repo upload`, `repo sync -l`, and `repox` all work natively inside any worktree.

---

### Rough Spot 4: `add -b <branch>` Proliferation & Missing Upstream Tracking ("NEVER USE THIS")
* **Affected Journey**: CUJ 2 & CUJ 5 (Provisioning & Multi-Repo Branching)
* **Code Locations**: [`decide_project_population_modes`](../src/tree.rs#L444-L445), [`create_project_worktree`](../src/tree.rs#L1149-L1168)
* **Empirical Evidence**:
  * The external `repo-worktree` skill explicitly warns in bold: **`-b <branch>`: NEVER USE THIS.**
  * In `c611aa69` (steps 503–529), an agent ran `git repo-worktree add --json --name studio-main-cxx-lsp -b asfp-cxx-lsp /usr/local/google/home/jomof/projects/studio-main-cxx-lsp`. Three bad things happened immediately:
    1. **Forced Materialization Penalty**: In [`decide_project_population_modes`](../src/tree.rs#L444-L445), `else if branch { Some("branch creation requires a worktree") }` overrides automatic `clone-depth` sharing, forcing `git worktree add` across all `clone-depth` projects not explicitly in `defaults.json`!
    2. **57-Repo Branch Pollution**: It ran `git worktree add -b asfp-cxx-lsp <wt>/<proj> <rev>` across every materialized project (29+ repos). When the agent later ran `repox -hb asfp-cxx-lsp`, `repox` matched 29 unmodified repos instead of only the 2 repos the agent actually touched. The agent spent **26 consecutive tool calls** manually deleting `asfp-cxx-lsp` from 27 untouched repos!
    3. **Missing `branch.<name>.remote` and `branch.<name>.merge`**: Because [`create_project_worktree`](../src/tree.rs#L1155-L1161) passes `<revision>` (a raw 40-char commit SHA from `snapshot.xml`) as the start-point to `git worktree add -b <branch> <path> <sha>`, Git creates the branch with **no upstream tracking configuration**. Both `repo upload` and `repox stack` require `branch.<name>.remote = goog` and `branch.<name>.merge = refs/heads/studio-main` (which `repo start` writes via `project.py:StartBranch`).
* **What the Reviewing Agent Should Address**:
  * Redesign `-b <branch>` (or replace it with scoped branch creation):
    * Whenever a branch is created by `git-repo-worktree`, automatically configure `branch.<name>.remote` and `branch.<name>.merge` from the manifest's `<project remote="..." upstream="..."/>` / `<default remote="..." revision="..."/>` attributes—matching `repo start`.
    * Scope `-b <branch>` to only the projects specified via `--project` / `--full-project` (or provide `repo start <branch> <proj>` support inside the worktree) rather than blindly creating branches across all 57 repos.

---

### Rough Spot 5: Agent Confusion Over Starting Revisions (Detached `HEAD`s of Initiating Repo by Design)
* **Affected Journey**: CUJ 1 & CUJ 2 (CLI Discovery & Worktree Provisioning)
* **Code Locations**: [`pin_manifest_snapshot`](../src/tree.rs#L221-L241), [`src/cli.rs`](../src/cli.rs), [`skill/SKILL.md`](../skill/SKILL.md)
* **Empirical Evidence**:
  * **By-Design Behavior**: Just like standard `git worktree add --detach`, [`pin_manifest_snapshot`](../src/tree.rs#L221-L241) invokes `repo manifest -r -o snapshot.xml` so that each project in the newly created worktree is set to the detached `HEAD` of the initiating `repo` client.
  * **Why Agents Get Confused**: The embedded [`skill/SKILL.md`](../skill/SKILL.md) and [`src/cli.rs`](../src/cli.rs) `--help` text (`Create a new multi-project worktree`) do not explicitly state that a new worktree inherits the exact detached `HEAD` commits currently checked out in the initiating repo (rather than fetching or resetting to remote `goog/studio-main`). When `studio-main` has a multi-repo branch like `mother` or a feature branch checked out in a subset of repos, agents that did not read the full external skill were surprised by which commits were checked out.
* **What the Reviewing Agent Should Address**:
  * Preserve the by-design behavior: a created worktree is always initialized to the detached `HEAD` commits of the initiating `repo` workspace.
  * Explicitly document this by-design behavior in [`src/cli.rs`](../src/cli.rs) (`about` / `long_about` for `add`, which also feeds the generated manpage), [`skill/SKILL.md`](../skill/SKILL.md), and [`spec-user-journeys.md`](../spec-user-journeys.md) so agents and users know upfront that the worktree mirrors the initiating repo's `HEAD`s (and can run `repo sync -l` or `git checkout --detach goog/studio-main` inside specific projects only when they explicitly want to reset away from the initiating repo's state).

---

### Rough Spot 6: The Shared-Gitdir Branch-Lock Trap (`fatal: '<branch>' is already used by worktree`)
* **Affected Journey**: CUJ 6 (Exchange WIP & Commits with Primary Checkout)
* **Code Locations**: [`create_project_worktree`](../src/tree.rs#L1149-L1183)
* **Empirical Evidence**:
  * Because `<wt>/<proj>` is a linked Git worktree of `~/projects/studio-main/.repo/projects/<proj>.git`, Git enforces its multi-worktree invariant: a local branch (`refs/heads/<branch>`) can only be checked out in one worktree at a time.
  * Two recurring failure patterns in agent transcripts:
    1. **Moving WIP from `studio-main` to a new worktree**: The user is on `my-feature` in `studio-main/tools/adt/idea` and asks an agent to move that work into a `repo-worktree`. When the agent tries `git -C <wt>/tools/adt/idea switch my-feature`, Git fails with `fatal: 'my-feature' is already used by worktree at '/usr/local/google/home/jomof/projects/studio-main/tools/adt/idea'`.
    2. **Leaving a branch locked in a worktree**: An agent checks out `my-feature` inside a worktree, finishes its task, and leaves `my-feature` checked out there. Later, when the user or another agent in `studio-main` runs `git checkout my-feature` (or `repox`), it fails until someone hunts down which `studio-main-*` worktree holds the lock and runs `git checkout --detach HEAD`.
* **What the Reviewing Agent Should Address**:
  * Add a `git repo-worktree detach [<tree>] [--branch <name>]` (or `--steal-branch` / `--take-branch <branch>` on `add` / `checkout`) command that safely detaches `HEAD` in the holding worktree (or source checkout, if clean) so the branch can be checked out without manual hunting.
  * Include `locked_branches` (branches currently checked out across non-detached projects in each worktree) in `git repo-worktree list --json` and `status --json`.

---

### Rough Spot 7: Strict Project-Root Path Validation (`E_UNKNOWN_PROJECT` on Subpaths & Non-Idempotent `materialize`)
* **Affected Journey**: CUJ 2 & CUJ 4 (`add --full-project`, `materialize`, `defaults`)
* **Code Locations**: [`validate_requested_projects`](../src/tree.rs#L343-L359), [`run_materialize_command`](../src/tree.rs#L2192-L2211), [`run_defaults_command`](../src/defaults.rs#L116-L128)
* **Empirical Evidence**:
  * In session `c611aa69` (line 3314), an agent working on C++/DWARF debugging in `tools/vendor/google/android-apk` and `tools/vendor/google/alt-lang` ran:
    ```bash
    git repo-worktree add --json --name dwarf-fix \
      --full-project tools/vendor/google/android-apk \
      --full-project tools/vendor/google/alt-lang \
      --full-project tools/adt/idea \
      /usr/local/google/home/jomof/projects/studio-main-dwarf-fix
    ```
    This failed immediately with `E_UNKNOWN_PROJECT: project not found in manifest: tools/vendor/google/android-apk` because the manifest project root is `tools/vendor/google`, not `tools/vendor/google/android-apk`.
  * Similarly, in `studio-main`, agents frequently pass `tools/adt/idea/android` (project is `tools/adt/idea`) or `prebuilts/tools/common/m2` (project is `prebuilts/tools`).
  * Additionally, in [`run_materialize_command`](../src/tree.rs#L2204-L2209), running `git repo-worktree materialize <tree> <proj>` on a project that is *already* in `worktree` mode fails with `E_NOT_SHARED` (exit code 1) instead of succeeding idempotently as a no-op! Agents that defensively run `materialize` before editing a project get an unexpected error when the project wasn't symlink-shared.
* **What the Reviewing Agent Should Address**:
  * Resolve any subpath passed to `--full-project`, `materialize`, or `defaults` to its longest-prefix enclosing manifest project path (reporting the resolved project in the JSON response), and also accept filesystem paths relative to the current working directory.
  * Make `materialize <tree> <proj...>` **idempotent**: if a project is already materialized (`PopulationMode::Worktree`), record it as `"already_materialized"` in the result and exit `0`.

---

### Rough Spot 8: CLI Discovery (`git repo-worktree --help`), Binary Shadowing & Doc/Schema Drift
* **Affected Journey**: CUJ 1 (Discover & Invoke CLI) & CUJ 7 (Teardown)
* **Code Locations**: [`install.sh`](../install.sh), [`build.rs`](../build.rs), [`src/cli.rs`](../src/cli.rs), [`src/error.rs`](../src/error.rs), [`skill/SKILL.md`](../skill/SKILL.md)
* **Empirical Evidence**:
  1. **`git repo-worktree --help` fails**: In session `512d6afd` (lines 1330–1335), an agent ran `git repo-worktree --help` and received:
     ```text
     No manual entry for git-repo-worktree
     ```
     Because `git <subcmd> --help` is intercepted by the `git` binary itself and translated into `man git-<subcmd>`, and [`install.sh`](../install.sh) never copies the `git-repo-worktree.1` manpage generated by [`build.rs`](../build.rs#L52-L55) into `~/.local/share/man/man1/`, `--help` always fails when invoked via `git repo-worktree --help`!
  2. **Binary `PATH` Shadowing (`~/.cargo/bin` vs. `~/.local/bin`)**: On the user's machine, `which -a git-repo-worktree` returns:
     * `/usr/local/google/home/jomof/.cargo/bin/git-repo-worktree` (built previously via `cargo install`, earlier on `PATH`)
     * `/usr/local/google/home/jomof/.local/bin/git-repo-worktree` (installed by `./install.sh`, later on `PATH`)
     Running `./install.sh` updates `~/.local/bin/git-repo-worktree`, leaving the stale `~/.cargo/bin/git-repo-worktree` shadowing it!
  3. **JSON Envelope Misconception (`.data` vs. `.result`)**: In `c611aa69` (steps 481–484), an agent ran `git repo-worktree list --json | jq -r '.data.trees[]'` and failed (`jq: error: Cannot iterate over null`) because the envelope puts payload under `.result`, not `.data`.
  4. **Severe Drift Between `~/.gemini/config/skills/repo-worktree/SKILL.md` and `src/cli.rs`**:
     | Feature | What `SKILL.md` Tells Agents | Actual [`src/cli.rs`](../src/cli.rs) / [`src/error.rs`](../src/error.rs) |
     | :--- | :--- | :--- |
     | Full checkout flag | `add --full-all` | `add --full` (`--full-all` is rejected by `clap`!) |
     | Configure defaults | `defaults set <proj> worktree\|symlink`<br>`defaults unset <proj>` | `defaults worktree <proj...>`<br>`defaults symlink <proj...>`<br>`defaults auto <proj...>` |
     | `materialize` JSON | `{"materialized": "prebuilts/tools"}` (singular) | `{"tree": "...", "materialized": ["..."]}` (array) |
     | Error codes | `E_NOT_REPO`, `E_NOT_FOUND`, `E_AMBIGUOUS`, `E_EXISTS`, `E_DEST_NOT_EMPTY`, `E_LOCKED`, `E_MANIFEST`, `E_GIT` | `E_NO_REPO_CLIENT`, `E_TREE_NOT_FOUND`, `E_TREE_AMBIGUOUS`, `E_TREE_EXISTS`, `E_DEST_OCCUPIED`, `E_LOCK_TIMEOUT`, `E_REPO_MANIFEST_FAILED`, `E_GIT_WORKTREE_FAILED` |
     | Dirty `remove --json` | Expects structured list of dirty projects | `result: null`, dirty projects concatenated into `error.message` and **truncated at 10** ([`src/tree.rs:1877-1888`](../src/tree.rs#L1877-L1888))! |

---

### Rough Spot 9: Teardown Hazards (`remove` Blocked by Build Dirtiness, Orphaned Bazel JVMs, Silent Loss of Unbranched Commits)
* **Affected Journey**: CUJ 7 (Teardown & Cleanup)
* **Code Locations**: [`run_remove_command`](../src/tree.rs#L1849-L1974), [`project_is_dirty`](../src/tree.rs#L2295-L2317)
* **Empirical Evidence**:
  1. **Unmerged Detached-HEAD Commits Are Silently Deleted**: Because projects in a worktree start on a detached `HEAD` (and agents are told never to use `-b`), agents sometimes make a commit directly on the detached `HEAD` without creating a named branch. [`project_is_dirty`](../src/tree.rs#L2295-L2317) only runs `git status --porcelain`! A committed change on detached `HEAD` has a clean working tree (`git status --porcelain` is empty), so `git repo-worktree remove` (even *without* `--force`) deletes the worktree and **strands the detached commit in the reflog** with zero warning!
  2. **Orphaned Bazel Server JVMs**: If an agent ran `bazel` inside a worktree (with its own `output_base`), calling `git repo-worktree remove` deletes the worktree directory via `fs::remove_dir_all`, leaving the worktree's `bazel` server JVM running in the background holding 8–16 GB of RSS until `max_idle_secs` (3 hours) expires.
  3. **Structured Error Details Missing on `E_DIRTY`**: When `remove` fails with `E_DIRTY`, agents have to re-run `status --json` or parse the human error string to see which projects are dirty and why (untracked build outputs vs. modified tracked files vs. unmerged detached commits).
* **What the Reviewing Agent Should Address**:
  * In `project_is_dirty` / `remove`: check not only `git status --porcelain`, but also whether `HEAD` is ahead of the pinned snapshot revision and not reachable from any `refs/heads/*` branch (`E_UNMERGED_COMMITS` or include in `dirty_projects` unless `--force` is passed).
  * Return structured `error.details` (or populate `result`) on `E_DIRTY` containing the full list of dirty/unmerged projects and their dirty reasons.
  * Before removing a worktree directory, detect if a Bazel server is associated with that workspace path (or check `bazel info server_pid` / `/proc/*/cwd`) and terminate it cleanly.

---

### Rough Spot 10: `<extend-project>` Manifest Blind Spot
* **Affected Journey**: CUJ 2 & CUJ 3 (`<linkfile>` / `<copyfile>` reconciliation)
* **Code Locations**: [`extract_links_and_copies`](../src/manifest.rs#L327-L365)
* **Empirical Evidence**:
  * While all 13 `<linkfile>` and 3 `<copyfile>` entries in `studio-main`'s current `default.xml` are attached to `<project>` tags (and verified as `state: "done"` across all 14 existing `studio-main-*` worktrees), [`extract_links_and_copies`](../src/manifest.rs#L334) only checks `if name == "project"`.
  * In `repo`'s manifest specification (`manifest_xml.py`), `<extend-project name="...">` elements in overlay manifests can also contain `<copyfile>` and `<linkfile>` children that attach to the base `<project>`, and `repo manifest -r` emits them under `<extend-project>` when overlays are active. Currently, `git-repo-worktree` silently drops any `<copyfile>` or `<linkfile>` inside `<extend-project>`.

---

## Summary Checklist for the Project Review Agent

When reviewing and updating `git-repo-worktree` (`spec.md`, `spec-user-journeys.md`, `spec-fast-add.md`, `src/`, `skill/SKILL.md`), verify that every item below is closed:

- [ ] **CUJ 1 (CLI & Docs)**: Install manpages in [`install.sh`](../install.sh) so `git repo-worktree --help` works; detect/warn on `~/.cargo/bin` PATH shadowing; add `--full-all` alias for `--full` and `defaults set/unset` aliases; sync `~/.gemini/config/skills/repo-worktree/SKILL.md` with [`skill/SKILL.md`](../skill/SKILL.md).
- [ ] **CUJ 2 (Provisioning & Path Resolution)**: Clearly document in `--help` ([`src/cli.rs`](../src/cli.rs)), manpages, and [`skill/SKILL.md`](../skill/SKILL.md) the by-design behavior that `add` initializes each project to the detached `HEAD` of the initiating `repo` workspace; resolve project subpaths passed to `--full-project`, `materialize`, and `defaults` to their enclosing manifest project roots.
- [ ] **CUJ 3 (Bazel & Build Isolation)**: Automatically propagate untracked workspace config files (`env.bazelrc`, `local.properties`) while stripping/isolating `startup --output_base=...` so worktrees never hijack `studio-main`'s Bazel server lock.
- [ ] **CUJ 4 (Safe Fast Sharing)**: Replace raw `symlink-shared` directory symlinks to mutable `studio-main` directories with safe isolation (FUSE Copy-on-Write overlay or `--no-checkout` + index-copy / read-only store) and make `materialize` idempotent.
- [ ] **CUJ 5 (`repo` & `repox` Fidelity)**: Synthesize `.repo/` and per-project `.git -> ../../.repo/projects/<path>.git` links inside worktrees (Model E1) so `repo status`, `repo start`, `repo branches`, `repo diff`, `repo forall`, `repo upload`, `repo sync -l`, and `repox` work natively; configure upstream tracking (`branch.<name>.remote` and `branch.<name>.merge`) whenever creating branches.
- [ ] **CUJ 6 (Cross-Worktree Branch Handoff)**: Add `git repo-worktree detach` (and branch-lock reporting in `list`/`status`) to eliminate the `fatal: '<branch>' is already used by worktree` trap.
- [ ] **CUJ 7 (Safe Teardown)**: Guard against deleting unbranched commits on detached `HEAD`; return structured dirty project lists on `E_DIRTY`; shut down worktree Bazel server processes on `remove`.
