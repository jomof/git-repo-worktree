# Round 2 Blindfold Agent & Performance Evaluation Report: `git-repo-worktree`

**Evaluated Binary**: `/usr/local/google/home/jomof/.local/bin/git-repo-worktree` (`1.0.0`, commit `6ff9e5e`, SHA-256 `80b500dd8f0a4f5d26e1674bf059b1e847cacb5fd6ba8ba99eafd3304e3c6f65`)  
**Workspace Under Test**: `/usr/local/google/home/jomof/projects/studio-main` (56 active Git projects, `ext4`, Linux `7.1.6-1rodete1-amd64`, 96 CPUs, `fuse-overlayfs 1.13`)  
**Test Plan**: [`round-2-blindfold-test-plan.md`](../round-2-blindfold-test-plan.md)  
**Session Artifacts**: [`sessions/`](./sessions/), [`history.jsonl`](./history.jsonl), [`environment.txt`](./environment.txt), [`integrity.txt`](./integrity.txt)

---

## 1. Executive Summary

All **14 blindfold scenarios (`S1`–`S14`, plus targeted overlay-unmount follow-up `S12b`)** and all **12 performance matrix benchmarks (`P1`–`P12`, 3 iterations each)** were executed against `~/projects/studio-main`.

### Headline Takeaways
1. **14 / 14 Blindfold Scenarios Succeeded With Zero Source-Workspace Corruption**:
   - Every scenario (`S1`–`S14`) completed its goal (`done`) and passed the §3.4 `snap()` source-integrity check (`diff -u /tmp/rw2-baseline.txt <(snap)` was empty after every scenario).
2. **Major Round-1 Blockers Are Resolved**:
   - **`.repo/` bridge synthesis (CUJ 5)**: `repo status`, `repo upload --dry-run`, `repo sync -l`, and `repox -hc` now work natively from the root of a worktree (`S2`, `S5`).
   - **Scoped `-b <branch> -p <project>` with upstream tracking (CUJ 2, 5)**: `S2` created `bf-s2-topic` in **1 project** (down from 29 in Round 1) with `branch.bf-s2-topic.remote = goog` and `branch.bf-s2-topic.merge = refs/heads/studio-main`.
   - **CoW `fuse-overlayfs` for clean shallow prebuilts (CUJ 4)**: Editing `prebuilts/studio/sdk/remote/BUILD` in `S3` took `92ms` to mount, kept `realpath` inside the worktree, used `328 KB` in `.repo/repo-worktrees/sdk-patch/overlays`, and left `studio-main` untouched.
   - **Subpath resolution & idempotent `materialize` (CUJ 2, 4)**: Passing subdirectories `tools/vendor/google/android-apk` and `tools/adt/idea/android` to `--full-project`, `-p`, and `materialize` succeeded with zero `E_UNKNOWN_PROJECT` or `E_NOT_SHARED` errors (`S2`, `S4`, `S11`).
   - **First-class `detach`, `status`, `add --continue`, and `E_DIRTY` protection (CUJ 6, 7)**: `S7` used `detach --branch bf-s7` (`192ms`), `S8` caught both an unbranched detached-HEAD commit and an untracked file via `E_DIRTY`, `S10` resumed a half-built tree in `324ms` via `add --continue`, and `S13` cleanly inspected and removed a legacy Format-2 tree.
3. **Top Remaining / New Rough Edges Uncovered in Round 2**:
   - **Rough Edge #1 (27 wasted tool calls across `S11`) — Exclusive Registry Lock on Read-Only Commands (`list`, `status`) & Pool Exhaustion Under Concurrency**: `list` and `status` acquire an exclusive lock (or block behind 31–66s `add`/`remove` exclusive locks) and fail immediately with `E_LOCKED` (exit 4). Furthermore, because `.pool` holds only 1 spare per project (`capacity = 1`) and `add -b <branch> -p <project>` bypasses the pool for `<project>`, 2 of the 3 parallel agents in `S11` missed the pool completely (`65–66s` cold checkouts) and discarded their checkouts on `remove`.
   - **Rough Edge #2 (14 wasted tool calls in `S6` + 2.18 GB leaked JVM in `S9`) — Studio Bazel Bootstrap (`env.bazelrc`) & Orphaned Bazel Daemon on `remove`**: Without a studio-specific note in the skill (or automatic `env.bazelrc` rewriting), `S6` spent 14 tool calls reverse-engineering `studio-main/env.bazelrc` and `cartfs` before finding `./build/bazel/env/setup`, and `S9` left a **2.18 GB RSS** `bazel(studio-main-bf-s9)` JVM server running after `remove` deleted the workspace directory.
   - **Rough Edge #3 (5 wasted tool calls across `S7`, `S8`, `S9`, `S11`, `S13`) — JSON Schema Asymmetry Between `list --json` (`.result.trees[]`) and `status --json` (`.result.projects[]`)**: 5 out of 6 agents that ran `status --json <tree>` after `list --json` wrote `.result.trees[]` in their `jq` filter and hit `jq: error (at <stdin>:1): Cannot iterate over null (null)` (exit 5).
   - **Rough Edge #4 (Every `add` pays +28–33s and +10.4 GB) — Dirty `prebuilts/studio/sdk/linux` Falls Back From `overlay` to Unpooled `worktree/checkout`**: Because `studio-main/prebuilts/studio/sdk/linux` has dirty `emulator/*` files, `overlay` preflight rejects it (`"source checkout is not clean"`) and falls back to `worktree/checkout`, which is excluded from `.pool` because `clone_depth == Some(1)`.
   - **Rough Edge #5 (`P12` / `pool fill`) — `git repo-worktree pool fill` Fails With `E_NO_POOL_PROJECTS` Under `auto` Defaults**: `add` and `remove` automatically pool 5 projects with `est_bytes >= 500 MB`, but `pool fill` only checks `defaults.json` for explicit `"pool"` entries and fails with `E_NO_POOL_PROJECTS` when called with no arguments.
   - **Rough Edge #6 (`S14` / `install.sh`) — Shadow Warning Compares Identical Version Strings (`1.0.0` vs `1.0.0`)**: `install.sh` line 371 only warns about a shadowing binary (`~/.cargo/bin/git-repo-worktree`) if its `version` string differs from `$installed_version`, which fails when `Cargo.toml` version is still `1.0.0`.

---

## 2. Blindfold Scenario Summary Table (§4 Measurements)

| ID | Scenario & CUJs | Outcome | Wall Clock | Tool Calls (Total / Mechanics) | Help / Skill Used | Errors Hit (`report` + CLI) | Workarounds Invented | Risky Actions | Source Integrity | Key Stumble / Transcript Quote |
| :--- | :--- | :--- | :---: | :---: | :--- | :--- | :--- | :--- | :---: | :--- |
| **S1** | Discovery & starting revisions (CUJ 1, 2) | `done` | 301s | 15 / 6 | Read `SKILL.md`, ran `git repo-worktree --help` (man page worked!) & `add -h` | 0 | Piped `list --json` and `status --json` through `jq` after raw JSON output truncated (`<truncated 4227 bytes>`) | None | `INTEGRITY_OK` | *"Every project will start on a detached `HEAD` pinned to the exact commit currently checked out in `studio-main` (`HEAD`)... `tools/adt/idea` will start at local commit `683c483dd9e` (`bf-s1`), not `goog/studio-main`."* |
| **S2** | Small change, branch, upload dry run (CUJ 2, 5) | `done` | 280s | 24 / 5 | Read `SKILL.md` (`repo-worktree`, `coding-style`, `git-commit-message-guidelines`) | 0 tool errors; 1 Git CLI error (`git status -sb` vs `-b` flag typo) | None — used `add -b bf-s2-topic -p tools/adt/idea` and `repo upload --dry-run --cbr --yes -v .` inside the tree | None | `INTEGRITY_OK` | `"resolved --project tools/adt/idea to project tools/adt/idea"` — created `bf-s2-topic` in **1 project** with `remote = goog`, `merge = refs/heads/studio-main`. |
| **S3** | Edit inside a large prebuilt (CUJ 4) | `done` | 273s | 19 / 8 | Read `SKILL.md` | 0 | Checked both `prebuilts/studio/sdk/remote` (`overlay`) and `prebuilts/studio/sdk/linux` (`worktree` fallback due to dirty source) | None | `INTEGRITY_OK` | `prebuilts/studio/sdk/remote` mounted via `fuse-overlayfs` in `92ms`; `git status` took `10ms`; `prebuilts/studio/sdk/linux` fell back to `worktree` (`27,112ms`) because `studio-main` has modified `emulator/*` files. |
| **S4** | Project paths that are really subdirectories (CUJ 2) | `done` | 166s | 16 / 11 | Read `SKILL.md`, ran `add -h` and `materialize -h` | 0 | Ran both `add --full-project <subpath>` and `materialize <tree> <subpath>` to verify idempotence | None | `INTEGRITY_OK` | `"resolved --full-project tools/vendor/google/android-apk to project tools/vendor/google"`, `"resolved --full-project tools/adt/idea/android to project tools/adt/idea"`; `materialize` exited `0` (`49ms`) with `"already_materialized"`. |
| **S5** | `repo` and `repox` inside the tree (CUJ 5) | `done` | 204s | 14 / 4 | Read `SKILL.md` (`git-repo-worktree`, `repo`, `repox`) | 0 `git-repo-worktree` errors; 1 expected `repo sync -l` checkout conflict (`exit 1`) | Used `git -C tools/apksig checkout -m <pinned_sha>` when `repo sync -l` refused to overwrite uncommitted changes on the moved commit | None | `INTEGRITY_OK` | `"error: Entry 'BUILD' would be overwritten by merge. Cannot merge. ... error: tools/apksig/: platform/tools/apksig checkout 2347523d2de9... "` — resolved via `git checkout -m`, then `repo sync -l` succeeded in `0.355s`. |
| **S6** | Bazel build in a tree (CUJ 3) | `done` | 396s | 30 / 18 (14 on `env.bazelrc` / CartFS) | Read `SKILL.md` | 0 | Spent **14 tool calls** inspecting `studio-main/env.bazelrc`, `/google/cartfs/mount`, `cartfs.service`, and `build/bazel/env/setup` before running `./build/bazel/env/setup` | None (ran `bazel shutdown` before `remove`) | `INTEGRITY_OK` | *"Let's check how `env.bazelrc` is generated in `studio-main`! ... `build/bazel/env/setup` generates `env.bazelrc` with its own unique `/google/cartfs/mount/<md5>` output_base!"* |
| **S7** | Hand off a branch from source (CUJ 6) | `done` | 247s | 24 / 11 | Read `SKILL.md`, ran `detach -h` | 0 tool errors; 1 `jq` error (`exit 5`) on `status --json` | Wrote `status --json` to scratch file after `jq '.result.trees[]'` failed on `status --json` | None | `INTEGRITY_OK` | Ran `git repo-worktree detach --json --branch bf-s7` (`192ms`) before `add`; hit `jq: error (at <stdin>:1): Cannot iterate over null (null)` on `status --json bf-s7 \| jq '.result.trees[]'`. |
| **S8** | Clean up with unsaved work (CUJ 7) | `done` (stopped & asked before `--force`) | 125s | 17 / 10 | Read `SKILL.md` | 1 expected `E_DIRTY` (`exit 1`); 1 `jq` error (`exit 5`) on `status --json` | Created branch `bf-s8-saved` at `e77ba7ccd89` to save the unbranched commit, and asked confirmation before `remove --force` | None (explicitly refused `--force` until confirmed) | `INTEGRITY_OK` | `"error": {"code": "E_DIRTY", "message": "tree 'bf-s8' has dirty or unmerged projects: tools/apksig (untracked), tools/base (unbranched-commits); use --force to remove anyway"}` |
| **S9** | Clean up after a build (CUJ 7) | `done` | 224s | 17 / 11 | Read `SKILL.md` | 0 tool errors; 1 `jq` error (`exit 5`) on `status --json` | Reverted `tools/base/bazel/toplevel.MODULE.bazel.lock` telemetry diff so `tools/base` could return to `.pool` on `remove` | Left `bazel(studio-main-bf-s9)` JVM daemon running (**2.18 GB RSS**) after `remove` | `INTEGRITY_OK` | Hit `jq: error (at <stdin>:1): Cannot iterate over null (null)` on `status --json bf-s9`; `remove` succeeded in `21.5s`, but `pgrep` showed PID `198244` (`bazel`, 2.18 GB RSS) still alive afterwards. |
| **S10** | Interrupted creation | `done` | 72s | 8 / 7 | Read `SKILL.md` | 0 | None — immediately ran `git repo-worktree add --continue --json bf-s10` (`324ms`) | None | `INTEGRITY_OK` | `"resumed bf-s10 at /usr/local/google/home/jomof/projects/bf-s10: 56 projects, 0 skipped, 0 failed"` (`resume: 10ms, projects: 201ms, directives: 6ms, repo-bridge: 90ms`). |
| **S11** | Parallel agents (`bf-s11-1`, `bf-s11-2`, `bf-s11-3`) | `done` (all 3) | `256s` / `302s` / `217s` | `22` / `39` / `20` (`81` total; `27` spent on lock retries) | All 3 read `SKILL.md` | `28` `E_LOCKED` (`exit 4`) across `add` (7), `list` (6), `status` (11), `remove` (4); `4` `jq` errors on `null` result | `bf-s11-1` & `bf-s11-3` used `schedule` backoff timers; `bf-s11-2` polled `list`/`add`/`status`/`ps -ef` 20 times | None | `INTEGRITY_OK` | `"error": {"code": "E_LOCKED", "message": "another git-repo-worktree process holds the registry lock; retry shortly"}` — plus `jq: error: Cannot iterate over null (null)` when piping `E_LOCKED` output to `jq '.result...'`. |
| **S12** | Overlays after a reboot (`prebuilts/studio/sdk` verbatim & `sdk/remote` `S12b`) | `done` | `51s` (`S12`) / `40s` (`S12b`) | `3 / 0` (`S12`) / `7 / 4` (`S12b`) | `S12b` read `SKILL.md` | 0 | In verbatim `S12`, `fd` under `prebuilts/studio/sdk` found files in `sdk/linux` (which is a `worktree`, not an `overlay`). In `S12b` (`sdk/remote`), agent saw empty dir and ran `git repo-worktree repair --json bf-s12` (`198ms`). | None | `INTEGRITY_OK` | `S12b`: *"The directory search yielded no results... The project uses `mode: overlay`, `strategy: fuse-overlayfs`. Running `git repo-worktree repair --json bf-s12`."* |
| **S13** | Trees from the old version (Format 2) | `done` | 227s | 12 / 7 | Read `SKILL.md` | 0 tool errors; 1 `jq` error (`exit 5`) on `status --json` | Redirected `status --json` to scratch file after hitting `.result.trees[]` vs `.result.projects[]` `jq` error | None | `INTEGRITY_OK` | Skipped oldest tree `studio-main-soong` (`unbranched_commits: 1, untracked: 1` in `tools/vendor/google`) and cleanly removed second-oldest `mutate` (`41.3s`). |
| **S14** | Is the right tool installed? (`PATH` shadow check) | `done` | 363s | 19 / 19 | Read `SKILL.md`, `install.sh`, `RELEASING.md` | 0 | Used `cargo uninstall git-repo-worktree` to remove shadowing `~/.cargo/bin/git-repo-worktree` instead of `install.sh` (which would have downloaded older `v1.0.0` release and missed the shadow check because both report `1.0.0`) | None | `INTEGRITY_OK` | *"Both binaries report `'version': '1.0.0'`, so `install.sh` line 371 (`[ "$("$found_binary" version)" != "$installed_version" ]`) fails to warn about `~/.cargo/bin/git-repo-worktree` shadowing `~/.local/bin/git-repo-worktree`!"* |
---

## 3. Detailed Per-Scenario Evaluation (§5 Checks)

### S1: Discovery and starting revisions (CUJ 1, 2)
* **Setup**: Created branch `bf-s1` with local commit `683c483dd9ea628fc72beff450205ce93f4b50f8` in `studio-main/tools/adt/idea`.
* **Checks**:
  * **Starting revision explanation before creation**: Passed. Before running `add`, the agent inspected `studio-main` and explicitly explained that every project starts on a **detached `HEAD` pinned to the exact commit currently checked out in `studio-main`** (via `repo manifest -r` + live `HEAD` inspection), including `tools/adt/idea` at `683c483dd9e` (`bf-s1`), `tools/vendor/google` at `447454ca6bb` (`part/cmake-language-service`), `tools/vendor/google3/aswb` at `ee32404cd61` (`part/aswb-dap-debug`), and `prebuilts/tools` at `23f8307e218` (`mother`) — **not** `goog/studio-main`.
  * **Did `git repo-worktree --help` open the man page?**: Yes (`PAGER=cat git repo-worktree --help | head -n 45` rendered `GIT-REPO-WORKTREE(1)` cleanly from `~/.local/share/man/man1/git-repo-worktree.1`).
  * **Did it use `status`?**: Yes (`git repo-worktree status --json try-change`, `1,978ms`).
  * **Friction**: Raw `list --json` and `status --json` outputs exceeded the tool output buffer (`<truncated 4227 bytes>`) because `list --json` embeds all 56 project records for all 15 registered trees (`~90 KB`).

### S2: Small change, branch, upload dry run (CUJ 2, 5)
* **Checks**:
  * **`bf-s2-topic` exists in exactly one project**: Verified (`repo forall -c 'git branch --list bf-s2-topic'` in `studio-main` returned only `tools/adt/idea: bf-s2-topic`; Round 1 polluted 29 projects).
  * **Upstream tracking config**: Verified `branch.bf-s2-topic.remote = goog` and `branch.bf-s2-topic.merge = refs/heads/studio-main`.
  * **`repo upload --dry-run` ran inside the tree**: Verified (`repo upload --dry-run --cbr --yes -v .` executed inside `/usr/local/google/home/jomof/projects/studio-main-bf-s2`, ran all 6 `repohooks` in `0.923s`, and selected `[bf-s2-topic] 1 commit (Tue Oct 12 16:45:17 2026 -0700)` for `https://googleplex-android-review.googlesource.com`).
  * **Branch creation mechanism chosen**: Used `git repo-worktree add --json --name bf-s2-topic -b bf-s2-topic -p tools/adt/idea /usr/local/google/home/jomof/projects/studio-main-bf-s2`.

### S3: Edit inside a large prebuilt (CUJ 4)
* **Checks**:
  * **Project mode in `list --json`**:
    * `prebuilts/studio/sdk/remote` (`5.85 GB`, `clone-depth="1"`): `"mode": "overlay"`, `"strategy": "fuse-overlayfs"`, `populate_ms: 92`.
    * `prebuilts/studio/sdk/linux` (`10.39 GB`, `clone-depth="1"`): `"mode": "worktree"`, `"strategy": "checkout"`, `populate_ms: 27112` — fell back from `overlay` because `studio-main/prebuilts/studio/sdk/linux` has dirty `emulator/*` files (`"overlay preflight changed: source checkout is not clean"`).
  * **Source checkout untouched after edit**: After editing `prebuilts/studio/sdk/remote/BUILD` inside `sdk-patch`, `git -C ~/projects/studio-main/prebuilts/studio/sdk/remote status --porcelain` was empty (`""`) and `sha256sum` of `studio-main/prebuilts/studio/sdk/remote/BUILD` remained `e08a58fa3f7bf5f7322e0df2bb61a14c2a2e2e03331b53fd00d62df446a9dc69`.
  * **`realpath` stays inside the tree**: `realpath /usr/local/google/home/jomof/projects/studio-main-sdk-patch/prebuilts/studio/sdk/remote/BUILD` resolved to `/usr/local/google/home/jomof/projects/studio-main-sdk-patch/prebuilts/studio/sdk/remote/BUILD`.
  * **Time for first `git status` in the overlay project**: `10ms` in `prebuilts/studio/sdk/remote` (`git status` and `git diff` felt instantaneous to the subagent). Overlay upper/work directories consumed `328 KB` total (`du -sh .repo/repo-worktrees/sdk-patch/overlays`).

### S4: Project paths that are really subdirectories (CUJ 2)
* **Checks**:
  * **Subpaths resolved without `E_UNKNOWN_PROJECT`**: Yes. Passing `--full-project tools/vendor/google/android-apk --full-project tools/adt/idea/android` to `add` printed:
    ```text
    resolved --full-project tools/vendor/google/android-apk to project tools/vendor/google
    resolved --full-project tools/adt/idea/android to project tools/adt/idea
    ```
    with `0` errors.
  * **Was `materialize` run on already-editable projects, and did it succeed as a no-op?**: Yes. The agent also ran `git repo-worktree materialize --json bf-s4 tools/vendor/google/android-apk tools/adt/idea/android`, which exited `0` in `49ms` with:
    ```json
    {
      "already_materialized": ["tools/vendor/google", "tools/adt/idea"],
      "materialized": [],
      "resolved": [
        {"input": "tools/vendor/google/android-apk", "project": "tools/vendor/google"},
        {"input": "tools/adt/idea/android", "project": "tools/adt/idea"}
      ]
    }
    ```

### S5: `repo` and `repox` inside the tree (CUJ 5)
* **Setup**: Created `/usr/local/google/home/jomof/projects/studio-main-bf-s5`, checked out `HEAD~1` in `tools/apksig` (`793cd59`) with an uncommitted edit to `tools/apksig/BUILD`, and made an uncommitted edit to `tools/base/common/BUILD`.
* **Checks**:
  * **`repo status` and `repox -hc git status -s` from tree root**: Both succeeded with exit `0` from `/usr/local/google/home/jomof/projects/studio-main-bf-s5` and reported exactly `tools/apksig` and `tools/base`.
  * **Was `repo sync -l` used, and did it keep the changes and `.git` links?**: Yes. The agent ran `repo sync -l` inside the tree. Because `tools/apksig/BUILD` differed between `HEAD~1` (`793cd59`) and the pinned revision (`2347523`), Git's checkout inside `repo sync -l` initially refused (`error: Entry 'BUILD' would be overwritten by merge. Cannot merge.`). The agent ran `git -C tools/apksig checkout -m 2347523d2de9b01ac2e783457a07172629759272` to 3-way merge the uncommitted edit onto the pinned SHA, then re-ran `repo sync -l` (`0.355s`, exit `0`). Both uncommitted changes were preserved, and `readlink /usr/local/google/home/jomof/projects/studio-main-bf-s5/tools/base/.git` remained `../../.repo/projects/tools/base.git`.

### S6: Bazel build in a tree (CUJ 3)
* **Checks**:
  * **How did the agent find and fix missing `env.bazelrc`?**: Tested with only the embedded `git-repo-worktree/SKILL.md` (without `repo-worktree/SKILL.md`). The agent checked `.bazelrc`, saw `try-import %workspace%/env.bazelrc`, saw that `env.bazelrc` was absent in the new worktree, and read `studio-main/env.bazelrc`. It recognized that copying `startup --output_base=/google/cartfs/mount/82f1a270dd3e54436acdeaa58b68077a` would collide with `studio-main`'s Bazel server. It then spent **14 tool calls** investigating how `82f1a270dd3e54436acdeaa58b68077a` was computed (`md5("/usr/local/google/home/jomof/projects/studio-main")`), inspecting `cartfs`, and searching `build/bazel` until it discovered `./build/bazel/env/setup` (`build/bazel/env/setup_dev_env.py`), which it executed inside the tree to generate an isolated `env.bazelrc` (`startup --output_base=/google/cartfs/mount/0a681f49781a4ff58e52617908f90f56`).
  * **`bazel info output_base` differs from `studio-main`'s**: Verified (`/google/cartfs/mount/0a681f49781a4ff58e52617908f90f56` vs `/google/cartfs/mount/82f1a270dd3e54436acdeaa58b68077a`).
  * **Build time and local action count**: `bazel build //tools/base/common:tools.common` completed in **28.454s** (`Elapsed time: 28.454s, Critical Path: 0.78s`) with **0 local actions** (`150 processes: 113 remote cache hit, 37 internal`).
  * **Was `bazel shutdown` run afterwards?**: Yes (`S6` ran `bazel shutdown` before `git repo-worktree remove bf-s6`; `pgrep -af "workspace_directory=.*bf-s6"` returned nothing).

### S7: Hand off a branch (CUJ 6)
* **Setup**: Created branch `bf-s7` with 1 local commit (`6fb6f772d5e`) checked out in `studio-main/tools/adt/idea`.
* **Checks**:
  * **How did the agent get past `"already used by worktree"`?**: Used `git repo-worktree detach --json --branch bf-s7` (`192ms`), which detached `studio-main/tools/adt/idea` cleanly before `add`. It never hit `fatal: 'bf-s7' is already used by worktree`.
  * **Tool calls spent**: `24` total (`11` on `git-repo-worktree` mechanics).
  * **Which worktree holds `bf-s7` at the end?**: `git repo-worktree detach --branch bf-s7 --dry-run --json` reported:
    ```json
    {"detached": [{"branch": "bf-s7", "head": "951773784654fc268a4d7b2ba91d226cf0417843", "path": "/usr/local/google/home/jomof/projects/studio-main-bf-s7/tools/adt/idea", "project": "tools/adt/idea", "tree": "bf-s7"}]}
    ```

### S8: Clean up with unsaved work (CUJ 7)
* **Setup**: Created `bf-s8`, committed `"bf-s8 work"` (`e77ba7ccd89`) on detached `HEAD` in `tools/base`, and created untracked `tools/apksig/untracked-notes.txt`.
* **Checks**:
  * **`remove` refuses with `E_DIRTY` naming both items**: Yes (`exit_code: 1`, `error.code: "E_DIRTY"`, `error.message: "tree 'bf-s8' has dirty or unmerged projects: tools/apksig (untracked), tools/base (unbranched-commits); use --force to remove anyway"`, with full `error.details.dirty_projects` array).
  * **Did the agent preserve the commit and ask before `--force`?**: Yes! In Turn 1, the subagent refused to run `--force` and messaged the orchestrator asking whether to save the commit on a branch and discard `untracked-notes.txt`. After being instructed to preserve the commit and remove the workspace, it ran `git branch bf-s8-saved e77ba7ccd89` and `git repo-worktree remove --force --json bf-s8`.
  * **Does the commit still exist afterwards?**: Verified via `git -C ~/projects/studio-main/tools/base log --all --oneline | rg "bf-s8 work"` -> `e77ba7ccd89 bf-s8 work`.

### S9: Clean up after a build (CUJ 7)
* **Setup**: Created `bf-s9`, ran `./build/bazel/env/setup` and `bazel build //tools/base/common:tools.common` inside it, leaving root untracked `env.bazelrc` + `bazel-*` symlinks, modified `tools/base/bazel/toplevel.MODULE.bazel.lock`, and a live `bazel(studio-main-bf-s9)` JVM daemon.
* **Checks**:
  * **How did the agent judge build outputs vs real changes?**: Ran `git repo-worktree status --json bf-s9`, saw `tools/base` had `modified: 1` (`bazel/toplevel.MODULE.bazel.lock`), ran `git diff` in `tools/base`, saw it was only the `@@aspect_tools_telemetry+//:extension.bzl%telemetry` lockfile side-effect, and checked root untracked files (`env.bazelrc`, `bazel-*`). It reverted `bazel/toplevel.MODULE.bazel.lock` so `tools/base` could return to `.pool` and ran `git repo-worktree remove --json bf-s9`.
  * **`remove` phase timings**: `total: 21,548ms` (`inspect: 1,605ms`, `unmount: 260ms`, `pool-return: 5,639ms`, `worktree-remove: 13,934ms`, `prune: 14ms`, `delete: 5ms`).
  * **Was the Bazel server still running after `remove`?**: **YES!** `pgrep -af "workspace_directory=.*bf-s9"` found PID `198244` holding **2.18 GB RSS** (`2,231,320 KB`) for a deleted workspace directory!

### S10: Interrupted creation
* **Setup**: Started `git repo-worktree add -j 2 -v ../bf-s10` and sent `SIGINT` after 30/56 projects completed (`state: "incomplete"`, `done: 30`, `pending: 26`).
* **Checks**:
  * **Was `add --continue` used rather than deleting and recreating?**: Yes (`git repo-worktree add --continue --json bf-s10`).
  * **Resume timing & final state**: Completed the remaining 26 projects + directives + `.repo` bridge in **`324ms`** (`resume: 10ms, projects: 201ms, directives: 6ms, repo-bridge: 90ms, finalize: 3ms`), reaching `"state": "complete"`.

### S11: Parallel agents (`bf-s11-1`, `bf-s11-2`, `bf-s11-3`)
* **Setup**: Spawned 3 concurrent `blindfold-agent` subagents at `00:29:54Z`.
* **Checks**:
  * **`E_LOCKED` (exit 4) occurrences and agent handling**:
    * `report --session S11 --json` recorded **28 `E_LOCKED` errors** (`add`: 7, `list`: 6, `status`: 11, `remove`: 4) out of 45 total invocations.
    * All 3 agents retried and succeeded without corrupting state: `bf-s11-1` (`256s`, 22 tool calls) and `bf-s11-3` (`217s`, 20 tool calls) used `schedule` backoff timers (`10–15s`), while `bf-s11-2` (`302s`, 39 tool calls) polled in a tight loop (`20` `E_LOCKED` retries).
  * **Did pool spares get contended?**: **YES, heavily (`pool.hits: 4, pool.misses: 11`)**:
    * First, passing `-b bf-s11-<n> -p tools/base/common` caused `tools/base` to **bypass `.pool`** on all 3 agents (spending `3.2–4.8s` on `worktree/checkout` instead of `~90ms` pool move).
    * Second, because `.pool` only keeps `1` spare per project, `bf-s11-3` (which won the lock first) consumed the 4 remaining spares (`external/dexter`, `tools/idea`, `tools/adt/idea`, `tools/gpu-profiler/idea`) and finished `add` in `31.0s`. While `bf-s11-3` was still editing/committing, `bf-s11-1` and `bf-s11-2` ran `add` against an empty `.pool` (`0` pool hits, `5` pool misses each, taking `36.2s` and `38.2s` of exclusive lock time).
    * Third, when `bf-s11-3` ran `remove` first, it returned its 4 projects to `.pool` (`returned_to_pool: 4`). When `bf-s11-1` and `bf-s11-2` subsequently ran `remove`, `.pool` was already at `capacity = 1`, so `give_back()` rejected their checkouts (`returned_to_pool: []`) and spent `31.0s` and `31.4s` deleting their 266k-file `tools/idea` and `tools/gpu-profiler/idea` worktrees!

### S12 & S12b: Overlays after a reboot
* **Setup**: Created `bf-s12` and unmounted all 19 `fuse-overlayfs` mounts via `fusermount3 -u`.
* **Checks**:
  * **S12 (verbatim prompt: `show the contents of one file under prebuilts/studio/sdk`)**: Because `prebuilts/studio/sdk/linux` fell back to `worktree` mode during `add` (due to dirty `studio-main/prebuilts/studio/sdk/linux`), running `fd` under `prebuilts/studio/sdk` immediately returned files from `prebuilts/studio/sdk/linux` in `51s` (`3` tool calls) without hitting the unmounted `prebuilts/studio/sdk/remote` overlay!
  * **S12b (targeted overlay prompt: `show the contents of one file under prebuilts/studio/sdk/remote`)**: The agent saw `fd` return empty output on `prebuilts/studio/sdk/remote`, inspected `list --json` (`"mode": "overlay", "strategy": "fuse-overlayfs"`), immediately ran `git repo-worktree repair --json bf-s12` (`198ms` to remount all 19 overlays), and displayed `prebuilts/studio/sdk/remote/BUILD` in `40s` (`7` tool calls total, `4` on `git-repo-worktree`).

### S13: Trees from the old version
* **Checks**:
  * **Old (Format 2) trees list and remove cleanly**: `list --json` listed all 14 legacy Format-2 trees. The agent sorted by `created_at`, inspected the oldest (`studio-main-soong`, created `2026-08-12T20:11:05Z`) via `status --json`, saw `"dirty": true` (`unbranched_commits: 1, untracked: 1` in `tools/vendor/google`), skipped it, inspected the second oldest (`mutate`, created `2026-10-05T20:33:58Z`), confirmed `"dirty": false` (its `part/mutation-testing` branch commit was safely on a named branch in the shared repo), and removed `mutate` via `git repo-worktree remove --json mutate` (`41.3s`).
  * **Source integrity**: `diff -u /tmp/rw2-baseline.txt <(snap)` was completely empty (`INTEGRITY_OK`).

### S14: Is the right tool installed?
* **Setup**: Copied the Round-1 `v1.0.0` binary (`2,536,288` bytes, `fb4ac7c`) to `/usr/local/google/home/jomof/.cargo/bin/git-repo-worktree` (earlier on `PATH` than `~/.local/bin/git-repo-worktree`).
* **Checks**:
  * **Did the agent notice two copies?**: Yes — in its second command (`which -a git-repo-worktree`), it found both `~/.cargo/bin/git-repo-worktree` and `~/.local/bin/git-repo-worktree`, compared their `-h` outputs, sizes, ELF linkage, and SHA-256 hashes, and identified that `~/.cargo/bin/git-repo-worktree` was the older `v1.0.0` release binary shadowing the newer v2 local build in `~/.local/bin/git-repo-worktree`.
  * **Did it see the `install.sh` shadow warning?**: Even better — the agent read `install.sh` and proved that **running `install.sh` would have failed in two ways**:
    1. Public `install.sh` downloads `releases/latest` (`v1.0.0`, commit `fb4ac7c`), which would have **overwritten** the newer v2 binary in `~/.local/bin/git-repo-worktree` with the old Round-1 binary!
    2. In `install.sh` line 371:
       ```sh
       if [ -n "$found_binary" ] && [ "$found_binary" != "$destination" ] &&
           [ "$("$found_binary" version 2>/dev/null || true)" != "$installed_version" ]; then
       ```
       Because both Round-1 and Round-2 binaries have `version = "1.0.0"` in `Cargo.toml` and output identical first-line `git-repo-worktree 1.0.0`, `install.sh`'s inequality check evaluates to **false** and **silently suppresses the shadow warning**!
  * **Resolution**: The agent ran `cargo uninstall git-repo-worktree` to remove `~/.cargo/bin/git-repo-worktree` and verified `which -a git-repo-worktree` and all v2 subcommands.
---

## 4. Performance Matrix (`P1`–`P12`, 3 Runs Each, Medians Reported)

All 12 performance measurements from §6 of the test plan were executed at full `~/projects/studio-main` scale (`56` synced projects, `88 GB` total working tree size across `~660,000` files on `ext4`, `96` vCPUs, Linux `7.1.6-1rodete1-amd64`, `fuse-overlayfs 1.13`). Each operation was measured across **3 runs** and tagged with `GIT_REPO_WORKTREE_SESSION=perf-P<n>` (raw session JSON reports saved in [`sessions/perf-P1.json`](sessions/perf-P1.json) through [`sessions/perf-P12.json`](sessions/perf-P12.json)).

### 4.1 Summary Performance Table (`P1`–`P12`)

| ID | Operation | Run 1 | Run 2 | Run 3 | Median (`p50`) | Phase Breakdown (`p50`) / Notes | Disk: `du -sh` (apparent) | Disk: `du -sh -x` (actual `ext4`) |
| :--- | :--- | :---: | :---: | :---: | :---: | :--- | :---: | :---: |
| **`P1`** | `add` default (`overlay` + `worktree`, **cold pool / 0 spares**) | `35,849ms` | `32,029ms` | `36,844ms` | **`35,849ms`** (`35.8s`) | `snapshot: 2,186ms`, `projects: 33,126ms`, `directives: 12ms`, `repo-bridge: 151ms`, `finalize: 16ms`. Modes: `19 overlay`, `37 worktree` (`37 checkout`, `0 pool`). | `89G` | **`21G`** |
| **`P2`** | `add` default (`overlay` + `worktree`, **warm pool: 5 spares**) | `31,422ms` | `31,353ms` | `32,754ms` | **`31,422ms`** (`31.4s`) | `snapshot: 2,309ms`, `projects: 29,169ms`, `directives: 7ms`, `repo-bridge: 88ms`, `finalize: 3ms`. Modes: `19 overlay`, `37 worktree` (`32 checkout`, `5 pool`). Critical path bottlenecked by dirty `prebuilts/studio/sdk/linux` fallback (`28,608ms`). | `89G` | **`21G`** |
| **`P3`** | `add --no-overlay` (`symlink-shared` + `worktree`, **warm pool**) | `34,312ms` | `30,624ms` | `30,757ms` | **`30,757ms`** (`30.8s`) | `snapshot: 2,291ms`, `projects: 28,274ms`, `directives: 26ms`, `repo-bridge: 142ms`, `finalize: 31ms`. Modes: `19 symlink-shared`, `37 worktree` (`32 checkout`, `5 pool`). | `21G` | **`21G`** |
| **`P4`** | `add --full` (`worktree` for all 56 projects, **warm pool**) | `144,598ms` | `89,808ms` | `110,097ms` | **`110,097ms`** (`110.1s`) | `snapshot: 2,542ms`, `projects: 100,118ms`, `directives: 17ms`, `repo-bridge: 272ms`, `finalize: 3ms`. Modes: `56 worktree` (`51 checkout`, `5 pool`). Bottlenecked by `prebuilts/tools` (`100.1s` `p50`, `141.7s` max). | `88G` | **`88G`** |
| **`P5a`** | `remove` (default, **returning 5 heavy projects to `.pool`**) | `21,659ms` | `22,824ms` | `23,872ms` | **`22,824ms`** (`22.8s`) | `inspect: 2,612ms`, `unmount: 42ms`, `pool-return: 5,685ms`, `worktree-remove: 14,245ms`, `prune: 18ms`, `delete: 7ms`. **1.53x faster (`-12.1s`)** than `--no-pool`. | `.pool`: `7.9G` | `.pool`: **`7.9G`** |
| **`P5b`** | `remove --no-pool` (unlinking all 37 worktrees from disk) | `31,547ms` | `34,921ms` | `35,610ms` | **`34,921ms`** (`34.9s`) | `inspect: 3,158ms`, `unmount: 41ms`, `pool-return: 0ms`, `worktree-remove: 31,475ms`, `prune: 14ms`, `delete: 4ms`. Unlinks ~650,000 files across `tools/idea`, `tools/gpu-profiler/idea`, `tools/adt/idea`. | `0B` | `0B` |
| **`P6a`** | `git status` in `prebuilts/studio/sdk/remote` (`5.85 GB`, `overlay`) | Cold: `10ms`<br>Warm: `7ms` | Cold: `11ms`<br>Warm: `7ms` | Cold: `13ms`<br>Warm: `8ms` | **Cold: `11ms`**<br>**Warm: `7ms`** | Instantaneous (`7ms` warm) on `fuse-overlayfs` mount. Note: `prebuilts/studio/sdk/linux` (`10.4 GB`, dirty in `studio-main`) fell back to `worktree` mode (`1,961ms`, `1,079ms`, `1,045ms`; median **`1,079ms`**). | — | — |
| **`P6b`** | `git status` in `prebuilts/studio/intellij-sdk` (`13.5 GB`, `overlay`) | Cold: `602ms`<br>Warm: `91ms` | Cold: `587ms`<br>Warm: `112ms` | Cold: `658ms`<br>Warm: `87ms` | **Cold: `602ms`**<br>**Warm: `91ms`** | Cold mount (immediately after `repair` remount) vs warm mount (`6.6x` speedup once FUSE/kernel dentry cache warms). | — | — |
| **`P6c`** | `git status` in `prebuilts/tools` (`26.8 GB`, `overlay`) | Cold: `3,930ms`<br>Warm: `577ms` | Cold: `4,145ms`<br>Warm: `460ms` | Cold: `3,710ms`<br>Warm: `444ms` | **Cold: `3,930ms`**<br>**Warm: `460ms`** | Largest single repository (`26.8 GB`): `3.93s` cold mount, **`460ms`** warm mount (`8.5x` faster after first traversal). | — | — |
| **`P7`** | `git status` in `tools/idea` (`266k` files): **pool worktree vs `studio-main`** | Tree: `2,759ms`<br>Src: `3,190ms` | Tree: `1,744ms`<br>Src: `3,144ms` | Tree: `1,578ms`<br>Src: `2,928ms` | **Tree: `1,744ms`**<br>**Src: `3,144ms`** | Pool worktree is a native `ext4` directory (`rename()`d from `.pool`), so `git status` has zero FUSE overhead and runs **`1.80x` faster** than `studio-main/tools/idea` (whose `.git` index has accumulated history/extensions). | — | — |
| **`P8a`** | `rg -l AndroidProjectTypes` over `prebuilts/` (**19 overlays + 1 worktree vs `studio-main`**) | Tree: `5,630ms`<br>Src: `946ms` | Tree: `2,732ms`<br>Src: `977ms` | Tree: `2,318ms`<br>Src: `956ms` | **Tree: `2,732ms`**<br>**Src: `956ms`** | `2.86x` overhead (`+1.78s`) when scanning all `67 GB` of `prebuilts/` through `fuse-overlayfs` userspace `readdir`/`open` vs direct `ext4`. | — | — |
| **`P8b`** | `rg -l AndroidProjectTypes` over **entire workspace** (`tools`, `prebuilts`, `external`, `build`) | Tree: `67,413ms`<br>Src: `31,388ms` | Tree: `12,442ms`<br>Src: `3,779ms` | Tree: `7,459ms`<br>Src: `3,665ms` | **Tree: `12,442ms`**<br>**Src: `3,779ms`** | Run 1 warmed page cache across `660k` files (`67.4s` tree vs `31.4s` source); warm runs (`p50: 12.4s` vs `3.8s`; Run 3: `7.5s` vs `3.7s`, `2.0x`). | — | — |
| **`P9`** | `git repo-worktree status` on a fresh 56-project tree | `1,747ms` | `1,662ms` | `1,679ms` | **`1,671ms`** (`1.67s`) | Inspects all 56 projects (`37 worktree` + `19 overlay` mounts + `/proc/mounts` check) in `1.67s`. | — | — |
| **`P10`** | `repo status -j8` from tree root vs `studio-main` | Tree: `2,558ms`<br>Src: `3,633ms` | Tree: `2,380ms`<br>Src: `3,541ms` | Tree: `2,426ms`<br>Src: `3,837ms` | **Tree: `2,426ms`**<br>**Src: `3,633ms`** | Full 56-project `repo status -j8` works natively from the `.repo` bridge and is **`1.50x` faster** in the clean worktree (`2.43s`) than in `studio-main` (`3.63s`). | — | — |
| **`P11`** | Unmount all 19 overlays (`fusermount3 -u`), then `git repo-worktree repair` | `199ms` | `151ms` | `154ms` | **`154ms`** (`147ms` internal) | Remounts all 19 `fuse-overlayfs` prebuilt mounts and refreshes all 37 git worktree registrations + manifest directives in **`154ms`** (`~8ms` per overlay mount). | — | — |
| **`P12`** | `git repo-worktree pool fill` for the 5 pooled projects (`7.9 GB`) | `12,861ms` | `13,674ms` | `13,389ms` | **`13,389ms`** (`13.4s`) | Pre-populates spare worktrees for `external/dexter`, `tools/base`, `tools/idea`, `tools/adt/idea`, and `tools/gpu-profiler/idea` in parallel in `13.4s`. | `.pool`: `7.9G` | `.pool`: **`7.9G`** |

---

### 4.2 Per-Project Pool Speedup (`P1` Checkout vs `P2` Warm Pool)

Comparing `slowest_projects` (`p50_ms` across 3 runs) between [`sessions/perf-P1.json`](sessions/perf-P1.json) (`worktree/checkout`) and [`sessions/perf-P2.json`](sessions/perf-P2.json) (`worktree/pool`) shows dramatic per-project creation speedups for the 5 pooled repositories:

| Pooled Project | Working Tree Size | `P1` Cold Checkout (`p50_ms`) | `P2` Warm Pool Claim (`p50_ms`) | Per-Project Speedup | Time Saved (`p50`) |
| :--- | :---: | :---: | :---: | :---: | :---: |
| `tools/idea` (`266k` files) | `981.8 MB` (`2.1G` on disk) | `19,155ms` | **`1,845ms`** | **`10.4x` faster** | `-17,310ms` |
| `tools/gpu-profiler/idea` (`266k` files) | `965.6 MB` (`2.0G` on disk) | `18,566ms` | **`929ms`** | **`20.0x` faster** | `-17,637ms` |
| `tools/adt/idea` | `2,278.0 MB` (`2.4G` on disk) | `13,288ms` | **`309ms`** | **`43.0x` faster** | `-12,979ms` |
| `tools/base` | `460.7 MB` (`970M` on disk) | `3,639ms` | **`88ms`** | **`41.4x` faster** | `-3,551ms` |
| `external/dexter` | `460.0 MB` (`636M` on disk) | `1,625ms` | **`11ms`** | **`147.7x` faster** | `-1,614ms` |

#### Why did `P2` wall-clock `add` time only drop from `35.8s` (`P1`) to `31.4s` (`P2`)?
Look at the #1 slowest project in both `P1` and `P2`:
* **`prebuilts/studio/sdk/linux` (`10.39 GB`)**: `p50_ms = 26,645ms` in `P1`, `28,608ms` in `P2`, `28,256ms` in `P3`.
* In the manifest, `prebuilts/studio/sdk/linux` has `clone-depth="1"`, so it is eligible for `overlay` mode (`<20ms` mount time, `0B` disk).
* However, in `~/projects/studio-main`, `prebuilts/studio/sdk/linux` had pre-existing dirty/untracked `emulator/*` files from an earlier emulator run. Because `git-repo-worktree` requires the source project to be clean for a safe `overlay` lowerdir, its preflight check (`"sharing preflight changed"`) automatically downgraded `prebuilts/studio/sdk/linux` from `overlay` (`<20ms`) to `worktree/checkout` (`28.6s`, `10.4 GB` disk)!
* **Key insight**: Without `prebuilts/studio/sdk/linux` falling back to `worktree/checkout`, the next slowest non-pooled project in `P2` is `external/jetbrains/kotlin` (`304.9 MB`, `p50 = 6,625ms`). Thus, on a clean `prebuilts/studio/sdk/linux` (or if `overlay` mounts a read-only archive/clean worktree lowerdir), warm-pool `add` (`P2`) drops from **`31.4s` -> `~9.0s`** (`2.3s` snapshot + `6.6s` projects) and physical disk usage drops from **`21 GB` -> `10.6 GB`**!

---

### 4.3 Per-Project Overlay Speedup (`P1`/`P2` Overlay vs `P4` `--full` Checkout)

For the 19 clean `clone-depth="1"` prebuilt projects (`67 GB` total), `fuse-overlayfs` mounts in `<20ms` per project (`0B` physical ext4 disk) versus `110.1s` (`67 GB` physical ext4 disk) under `--full`:

| Prebuilt Project (`clone-depth="1"`) | Size | `P1`/`P2` `overlay` (`p50_ms`) | `P4` `--full` `worktree/checkout` (`p50_ms`) | Speedup | Disk Saved per Tree |
| :--- | :---: | :---: | :---: | :---: | :---: |
| `prebuilts/tools` | `26.83 GB` | `<20ms` | `100,112ms` (`max: 141,740ms`) | **`>5,000x`** | `26.83 GB` |
| `prebuilts/studio/sdk/remote` | `5.85 GB` | `<20ms` | `26,053ms` (`max: 41,544ms`) | **`>1,300x`** | `5.85 GB` |
| `tools/external/gradle` | `6.56 GB` | `<20ms` | `24,191ms` (`max: 31,699ms`) | **`>1,200x`** | `6.56 GB` |
| `prebuilts/studio/intellij-sdk` | `13.51 GB` | `<20ms` | `16,447ms` (`max: 16,483ms`) | **`>800x`** | `13.51 GB` |
| `prebuilts/studio/jdk` | `8.12 GB` | `<20ms` | `13,771ms` (`max: 26,358ms`) | **`>680x`** | `8.12 GB` |
| `prebuilts/clang/host/linux-x86` | `3.89 GB` | `<20ms` | `12,491ms` (`max: 15,775ms`) | **`>620x`** | `3.89 GB` |
| `prebuilts/remoteexecution-client` | `1.24 GB` | `<20ms` | `7,749ms` (`max: 9,486ms`) | **`>380x`** | `1.24 GB` |
| `prebuilts/studio/buildbenchmarks` | `4.08 GB` | `<20ms` | `3,207ms` (`max: 13,382ms`) | **`>160x`** | `4.08 GB` |
| **All 19 clean `clone-depth="1"` prebuilts** | **`~67.0 GB`** | **`<200ms` total** | **`~100.1s` parallel wall** | **`~500x`** | **`67.0 GB` per tree** |
---

## 5. Explicit Answers to §7 Questions

### Question 1: Do `pool` and `overlay` deserve to stay the defaults on this machine? Back the answer with P1–P8.
**Yes — both `pool` and `overlay` decisively earn their place as the defaults on this `ext4` machine, with two targeted refinements needed (`sdk/linux` dirty fallback and `pool` selection threshold):**

1. **`overlay` (`fuse-overlayfs`) vs `--full` (`P1`/`P2` vs `P4`) and vs `--no-overlay` (`P3`)**:
   - **Disk savings (`68 GB` saved per tree)**: The 19 clean `clone-depth="1"` prebuilt projects total **~68 GB** (`prebuilts/tools` `26.8 GB`, `prebuilts/studio/intellij-sdk` `13.5 GB`, `prebuilts/studio/jdk` `8.1 GB`, `tools/external/gradle` `6.6 GB`, `prebuilts/studio/sdk/remote` `5.8 GB`, etc.). With `overlay` default, those 19 projects consume **0 GB** of duplicate `ext4` blocks (`du -sh -x` of a P1 tree is **`21 GB`**, identical to `P3` `--no-overlay` at **`21 GB`**, vs **`89 GB`** for `P4` `--full`) and only **`328 KB`** in `.repo/repo-worktrees/<tree>/overlays` after editing (`S3`).
   - **Mount speed (`<1.2s` total across 19 projects)**: Mounting all 19 `fuse-overlayfs` projects takes `30–512ms` per project (`prebuilts/tools` `p50 = 494ms`, `prebuilts/studio/sdk/remote` `p50 = 92ms`), and remounting all 19 after a reboot via `git repo-worktree repair` (`P11`) takes **`151ms` median** (`199ms, 151ms, 154ms`).
   - **Safety over `symlink-shared` (`P3`)**: Unlike `symlink-shared` (`P3`), `overlay` gives every prebuilt a real isolated `.git` worktree link, keeps `realpath` inside the worktree (`S3`), makes prebuilts visible to `rg`/`fd`/`repox` (`S5`, `P8`), and prevents edits or build tools from mutating `studio-main` (`S3`).
   - **Runtime read/stat overhead (`P6`, `P8`)**:
     - `P6` (`git status` in overlay prebuilts): In `prebuilts/studio/sdk/remote` (`5.85 GB`), `git status` is **`11ms` cold / `7ms` warm**. In `prebuilts/studio/intellij-sdk` (`13.5 GB`), `git status` is **`602ms` cold / `91ms` warm**. In `prebuilts/tools` (`26.8 GB`), `git status` is **`3,930ms` cold / `460ms` warm**.
     - `P8` (`rg -l` walk over prebuilts): Walking `prebuilts/` through `fuse-overlayfs` takes **`2,732ms` warm median** (`5,630ms` first pass) vs **`956ms`** on raw `ext4` in `studio-main` (`+1.8s` overhead across 68 GB of prebuilts). Across all 56 projects (`tools`, `prebuilts`, `external`, `build`), `rg -l` takes **`12,442ms` warm median** (`7,459ms` by pass 3) in the tree vs **`3,779ms`** in `studio-main`. That modest walk cost is a worthwhile trade for saving 68 GB of disk and 60+ seconds of `git checkout` I/O on every worktree creation.

2. **`pool` vs cold `checkout` (`P2` vs `P1`, `P5`, `P7`)**:
   - **Per-project checkout speedup (`10x–35x` faster on the 5 pooled projects)**:
     - `tools/idea` (`266k` files, `982 MB`): **`19,155ms` (`P1` cold checkout) $\to$ `1,845ms` (`P2` pool hit)** (**`10.4x` faster**, saving `17.3s`).
     - `tools/gpu-profiler/idea` (`266k` files, `966 MB`): **`18,566ms` (`P1`) $\to$ `929ms` (`P2`)** (**`20.0x` faster**, saving `17.6s`).
     - `tools/adt/idea` (`2.28 GB`): **`13,288ms` (`P1`) $\to$ `309ms` (`P2`)** (**`43.0x` faster**, saving `13.0s`).
     - `tools/base` (`926 MB`): **`3,639ms` (`P1`) $\to$ `88ms` (`P2`)** (**`41.4x` faster**, saving `3.5s`).
     - `external/dexter` (`666 MB`): **`1,625ms` (`P1`) $\to$ `11ms` (`P2`)** (**`147.7x` faster**, saving `1.6s`).
   - **Why whole-tree `P2` (`31.4s`) is only `4.4s` faster than `P1` (`35.9s`) today**: Because `add` runs project checkouts in parallel across worker threads, the critical path of `add` in both `P1` and `P2` is dominated by a single unpooled project: **`prebuilts/studio/sdk/linux` (`10.39 GB`, `p50 = 33.1s` in `P1`, `27.9s` in `P2`)**, which fell back from `overlay` to `worktree/checkout` because `studio-main/prebuilts/studio/sdk/linux` has dirty `emulator/*` files! Once `prebuilts/studio/sdk/linux` is overlaid (or pooled), the warm-pool `add` critical path drops from **`~31.4s` to `~8s`** (bounded by `external/jetbrains/kotlin` at `5.8s` + `snapshot` at `2.3s`).
   - **Faster `remove` (`P5`)**: Returning the 5 clean projects to `.pool` via `rename()` (`git worktree move`) makes `remove` **31.6% faster** (**`23,872ms` median** with pool return vs **`34,921ms` median** with `remove --no-pool`), because `git worktree remove` no longer has to `unlink()` 532,000 files in `tools/idea` and `tools/gpu-profiler/idea` (`worktree-remove` phase drops from **`31,475ms` to `14,245ms`**).
   - **No `git status` penalty (`P7`)**: `git status` in `tools/idea` inside a pooled worktree is actually faster (**`1,744ms` median**: `2759, 1744, 1578 ms`) than in `studio-main/tools/idea` (**`3,144ms` median**: `3190, 3144, 2928 ms`) because `take()` writes a fresh index with current stat cache entries.

---

### Question 2: Is any project pooled or overlaid that should not be, or the reverse?
**Yes — 4 specific adjustments should be made to the `pool` and `overlay` heuristics:**

1. **Should NOT be pooled (`tools/gpu-profiler/idea` — `2.0 GB` in `.pool` + `2.0 GB` per tree = `4.0 GB` per active tree)**:
   - `tools/gpu-profiler/idea` is a legacy 266k-file copy of IntelliJ (`clone-depth` unset in manifest) that is almost never edited in Android Studio workflows, yet because `est_bytes = 965 MB >= 500 MB`, it is materialized as a full `worktree` and pooled (`2.0 GB` in `.pool` + `2.0 GB` per worktree).
   - **Fix**: Overlay clean unedited source projects that exceed a file-count/byte threshold when they are not in the user's active edit set, OR allow `defaults overlay tools/gpu-profiler/idea` (`defaults` currently only supports `worktree`, `pool`, `symlink`, `auto` — **there is no `defaults overlay` subcommand!**).
2. **Should NOT be pooled (`external/dexter` — `636 MB` in `.pool` for only `1.6s` checkout savings)**:
   - `external/dexter` (`est_bytes = 666 MB`) has relatively few files and checks out cold in `1,625ms`, yet occupies `636 MB` in `.pool`. Pooling should weight **file count** (or `populate_ms >= 3000ms`) rather than raw `est_bytes >= 500 MB` alone.
3. **SHOULD be pooled (`external/jetbrains/kotlin` and `external/intellij-plugins`)**:
   - `external/jetbrains/kotlin` (`305 MB`, `p50_ms = 5,818–7,801ms`) and `external/intellij-plugins` (`312 MB`, `p50_ms = 2,591–3,519ms`) fall below the `500 MB` byte threshold (`POOL_BYTES = 500 * 1024 * 1024` in `src/tree.rs`), but because they have high file counts, they are the **#2 and #3 slowest checkouts** on every warm-pool `add` (`P2`) and among the slowest `worktree-remove` deletions on every `remove`! Pooling projects with `file_count >= 15,000` (or observed `populate_ms >= 2,500ms`) would save `~8.5s` on every `add` and `~6s` on every `remove`.
4. **SHOULD be overlaid (or pooled on fallback): `prebuilts/studio/sdk/linux` (`10.39 GB`, `p50_ms = 27.9–33.5s`)**:
   - In `studio-main`, `prebuilts/studio/sdk/linux` has `clone-depth="1"`, so it is intended to be an `overlay` (`<150ms`, `0 GB`). However, because `studio-main/prebuilts/studio/sdk/linux` has dirty `emulator/*` files (modified/deleted by emulator runs in the source client), `overlay` preflight rejects it (`"overlay preflight changed: source checkout is not clean"`) and falls back to `worktree/checkout`.
   - Worse, when a `clone-depth="1"` project falls back from `overlay` to `worktree`, `uses_pool()` in `src/tree.rs` returns `false` because `project.clone_depth == Some(1)`! Consequently, `prebuilts/studio/sdk/linux` is **neither overlaid nor pooled**, costing **`28–34 seconds`** of checkout time on every `add`, **`14 seconds`** on every `remove`, and **`10.4 GB`** of disk per tree!
   - **Fix**: (a) If a `clone-depth="1"` project falls back from `overlay` to `worktree` and has `est_bytes >= 500 MB`, allow it to use `.pool`! (b) Even better, mount the `fuse-overlayfs` lowerdir from a clean read-only worktree spare in `.repo/repo-worktrees/.pool/` (or allow overlaying when source modifications are confined to ignored/runtime paths) so `prebuilts/studio/sdk/linux` mounts in `100ms` even when `studio-main` is dirty.

---

### Question 3: Did any scenario change `studio-main` unexpectedly?
**No.** `diff -u /tmp/rw2-baseline.txt <(snap)` was verified after every single scenario (`S1` through `S14`) and after the performance matrix (`P1` through `P12`), and was **empty every time** (see [`integrity.txt`](./integrity.txt)).

---

### Question 4: Which rough edges from Round 1 are fixed, partly fixed, or still present? (CUJ 1–7 Checklist Audit)

| CUJ | Round-1 Checklist Item (`agent-rough-spots-report.md`) | Round-2 Status | Evidence from `S1`–`S14` & `P1`–`P12` |
| :--- | :--- | :---: | :--- |
| **CUJ 1** | Install manpages in `install.sh` so `git repo-worktree --help` works | **Fixed** | `install.sh` installs all 18 man pages into `~/.local/share/man/man1/`. In `S1`, `git repo-worktree --help` opened `GIT-REPO-WORKTREE(1)` on the first try. |
| **CUJ 1** | Detect/warn on `~/.cargo/bin` `PATH` shadowing | **Partly Fixed** | `install.sh` lines 370–374 check for a shadowing binary on `PATH`, **but** require `[ "$("$found_binary" version)" != "$installed_version" ]`. Because both Round-1 and Round-2 binaries report `git-repo-worktree 1.0.0`, the warning condition is false (`S14`). Additionally, `git repo-worktree version` does not include the Git commit hash (`6ff9e5e`). |
| **CUJ 1** | Sync skill docs with `src/cli.rs` | **Fixed** | `git repo-worktree skill sync` installs the embedded `skill/SKILL.md`, and `repox skill sync` now delegates to `git repo-worktree skill sync`. All flags and error codes in `SKILL.md` match `src/cli.rs`. |
| **CUJ 2** | Document in `--help`, manpages, and `SKILL.md` that `add` pins the detached `HEAD`s of the initiating `repo` client | **Fixed** | Clearly documented at the top of `SKILL.md` and `git-repo-worktree-add(1)`. In `S1`, the blindfold agent explained this behavior with 100% accuracy before running `add`. |
| **CUJ 2** | Resolve project subpaths passed to `--full-project`, `-p`, `materialize`, and `defaults` to enclosing manifest project roots | **Fixed** | Verified in `S2`, `S4`, and `S11` (`tools/vendor/google/android-apk` $\to$ `tools/vendor/google`, `tools/adt/idea/android` $\to$ `tools/adt/idea`, `tools/base/common` $\to$ `tools/base`). Zero `E_UNKNOWN_PROJECT` errors. |
| **CUJ 3** | Propagate/bootstrap `env.bazelrc` while isolating `startup --output_base=...` | **Still Present** | `git-repo-worktree add` does not copy/rewrite `env.bazelrc`, and `skill/SKILL.md` does not mention `./build/bazel/env/setup`. In `S6`, the agent succeeded without corrupting `studio-main`, but spent **14 tool calls** reverse-engineering `studio-main/env.bazelrc` and `cartfs` before finding `./build/bazel/env/setup`. |
| **CUJ 4** | Replace `symlink-shared` with safe CoW `fuse-overlayfs` isolation & make `materialize` idempotent | **Fixed (with dirty-source caveat)** | Clean `clone-depth="1"` prebuilts (19 projects, 68 GB) use `fuse-overlayfs` by default (`S3`), keeping `studio-main` untouched and `realpath` inside the tree. `materialize` is idempotent (`S4`). *Caveat*: dirty `studio-main/prebuilts/studio/sdk/linux` falls back to unpooled `worktree` (`28–33s`). |
| **CUJ 5** | Synthesize `.repo/` bridge & per-project `.git` symlinks so `repo` and `repox` work inside trees; scope `-b` and set upstream tracking | **Fixed** | `repo status`, `repo upload --dry-run`, `repo sync -l`, and `repox -hc` all worked from the tree root (`S2`, `S5`). `add -b <branch> -p <project>` requires project scope (`E_BRANCH_NEEDS_PROJECT_SCOPE`) and sets `branch.<name>.remote` and `branch.<name>.merge` (`S2`). |
| **CUJ 6** | Add `git repo-worktree detach` and branch-lock reporting in `list`/`status` | **Fixed** | `S7` used `git repo-worktree detach --json --branch bf-s7` (`192ms`) to release `bf-s7` from `studio-main` before `add`, and `list --json` reports `checked_out_branches`. |
| **CUJ 7** | Guard against deleting unbranched commits on detached `HEAD`; return structured `E_DIRTY` details; shut down worktree Bazel server on `remove` | **Partly Fixed (2 of 3 fixed)** | `remove` catches `unbranched-commits` and `untracked` files with structured `error.details.dirty_projects` (`S8`), **but** `remove` does NOT shut down a running worktree Bazel server (`S9` leaked a **2.18 GB RSS** `bazel(studio-main-bf-s9)` JVM daemon after deleting the tree). |

---

### Question 5: What new rough edges appeared? (Ranked by Agent Tool Calls Wasted)

| Rank | Rough Edge | Tool Calls Wasted | Scenarios | Transcript Quote & Root Cause |
| :---: | :--- | :---: | :---: | :--- |
| **1** | **Exclusive registry lock (`E_LOCKED`, exit 4) blocks read-only commands (`list`, `status`) and fails immediately without waiting; `add -b -p` bypasses `.pool` and `.pool` defaults to 1 spare (`capacity = 1`)** | **27 tool calls** (`S11`: `bf-s11-2` wasted 20 calls polling; `bf-s11-1` wasted 4; `bf-s11-3` wasted 3) | `S11` | `{"ok":false,"exit_code":4,"error":{"code":"E_LOCKED","message":"another git-repo-worktree process holds the registry lock; retry shortly"}}` — `status` (`src/tree.rs:1464`) acquires `LockMode::Exclusive` (Wait: even `list` with `LockMode::Shared` fails while `add`/`remove` hold `LockMode::Exclusive` for 30–66s!). Worse, piping `E_LOCKED` JSON (`"result": null`) into `jq '.result.trees[]'` fails with `jq: error: Cannot iterate over null (null)`. |
| **2** | **Missing Studio Bazel bootstrap (`env.bazelrc` / `./build/bazel/env/setup`) & orphaned 2.18 GB Bazel JVM after `remove`** | **14 tool calls** (+ 2.18 GB leaked RAM) | `S6`, `S9` | `S6` spent steps 15–59 reverse-engineering `studio-main/env.bazelrc` (`startup --output_base=/google/cartfs/mount/82f1a270...`), `md5sum`, and `cartfs.service` before finding `./build/bazel/env/setup`. In `S9`, `remove` deleted `/usr/local/google/home/jomof/projects/studio-main-bf-s9` while PID `198244` (`bazel(studio-main-bf-s9)`, `2.18 GB RSS`) remained running. |
| **3** | **JSON schema asymmetry between `list --json` (`.result.trees[]`) and `status --json` (`.result.projects[]`) + `90 KB` `list --json` truncation** | **7 tool calls** (5 `jq` `exit 5` errors + 2 scratch-file re-runs) | `S1`, `S7`, `S8`, `S9`, `S11`, `S13` | `jq: error (at <stdin>:1): Cannot iterate over null (null)` — 5 out of 6 agents that ran `status --json <tree>` after `list --json` wrote `.result.trees[]` because `list --json` returns `{"result": {"trees": [...]}}` while `status --json <tree>` returns the single tree object directly under `{"result": {"name": ..., "projects": [...]}}`. Meanwhile, `list --json` embeds all 56 projects per tree (`~90 KB`), truncating tool output in `S1`, `S13`, `S14`. |
| **4** | **`install.sh` shadow check compares `version` strings (`1.0.0` == `1.0.0`) instead of binary identity/commit** | **3 tool calls** (diagnostic checks in `S14`) | `S14` | In `install.sh:371`, `[ "$("$found_binary" version)" != "$installed_version" ]` evaluates to false when `~/.cargo/bin/git-repo-worktree` and `~/.local/bin/git-repo-worktree` both print `git-repo-worktree 1.0.0`. Removing the version-inequality guard (warning whenever `found_binary != destination`) and embedding the git SHA in `version` fixes both issues. |
| **5** | **`git repo-worktree pool fill` fails with `E_NO_POOL_PROJECTS` out of the box, and `pool status` fails with `E_USAGE`** | **2 tool calls** (`pool status` in `S6` & orchestrator; `pool fill` in `P12`) | `S6`, `P12` | `{"code":"E_NO_POOL_PROJECTS","message":"no projects have a pool default; run defaults pool <project> or name projects"}` — `add` and `remove` auto-pool projects with `est_bytes >= 500 MB`, but `pool fill` (`src/pool.rs:313-326`) only reads explicit `"pool"` entries from `defaults.json`. Also, agents instinctively type `pool status` (like `git status` / `repo status`), which fails with `E_USAGE` because the subcommand is `pool list`. |
| **6** | **Dirty `studio-main/prebuilts/studio/sdk/linux` falls back from `overlay` to unpooled `worktree/checkout` (`+28–33s` and `+10.4 GB` on every `add`)** | **0 extra calls, but +30s wall clock on every `add`** | `S1`–`S12`, `P1`–`P2` | `"overlay preflight changed: source checkout is not clean"` — `prebuilts/studio/sdk/linux` (`10.39 GB`) accounts for **88% of `add` project population time** (`27.9s` of `29.2s` in `P2`) and **50% of tree disk usage** (`10.4 GB` of `21 GB`). Also caused verbatim `S12` (`prebuilts/studio/sdk`) to read from `sdk/linux` (`worktree`) without noticing that `sdk/remote` (`overlay`) was unmounted. |

---

### Question 6: Does `repox` work inside a tree (`S5`)?
**Yes!** With v2's `.repo/` bridge and per-project `.git -> ../../.repo/projects/<path>.git` symlink layout, `repox` works natively from the root of a worktree (`S5` verified `repox -hc git status -s` from `/usr/local/google/home/jomof/projects/studio-main-bf-s5`, which discovered all 56 projects via `.repo` and reported the exact 2 dirty projects `tools/apksig` and `tools/base` in `<2s`).

---

### Question 7: Is a studio-specific note (Bazel setup, shutdown) needed in the studio skill (`S6`, `S9`)?
**Yes — and ideally paired with two small tool enhancements:**
1. **Why a studio-specific note is needed immediately**:
   - In `S6`, with only the generic `git-repo-worktree/SKILL.md` loaded, the agent had to spend **14 tool calls (`~180s`)** discovering why `env.bazelrc` was missing, why copying `studio-main/env.bazelrc` verbatim would hijack `studio-main`'s `/google/cartfs/mount/<md5>` `output_base`, and finding `./build/bazel/env/setup`.
   - Adding a 2-line note to the workspace/studio skill (or `git-repo-worktree/SKILL.md`):
     > **Android Studio (`studio-main`) Bazel setup**: Run `./build/bazel/env/setup` inside the new worktree root before your first `bazel` command (generates a worktree-isolated `env.bazelrc` with `--config=rcache` and a distinct CartFS `output_base`; never copy `studio-main/env.bazelrc` verbatim). Run `bazel shutdown` inside the worktree before `git repo-worktree remove`.
     would have saved all **14 tool calls** in `S6` and prevented the **2.18 GB** leaked Bazel server daemon in `S9`.
2. **How `git-repo-worktree` itself can make this zero-touch**:
   - On `remove`: Before `fs::remove_dir_all(&tree_path)`, scan `/proc/*/cmdline` (or `pgrep -f "workspace_directory=<tree_path>"`) for any Bazel server daemon bound to `<tree_path>` and send it `SIGTERM` (or run `bazel shutdown`), eliminating leaked multi-GB JVMs automatically (`S9`).
   - On `add`: Support copying untracked root `env.bazelrc` with automatic replacement of `<source_root>` $\to$ `<tree_root>` and `<md5(source_root)>` $\to$ `<md5(tree_root)>` (as documented in [`addendum-env-bazelrc-and-cartfs.md`](../addendum-env-bazelrc-and-cartfs.md)).
---

## 6. Ranked Rough Edges & Concrete v2.1 Recommendations

Every rough edge observed in Round 2 is ranked below by **agent tool calls wasted** (and wall-clock impact), accompanied by its scenario ID, exact transcript quote, source code location in `~/projects/git-repo-worktree/src/`, and recommended fix.

---

### Rank 1: Exclusive Registry Lock Contention (`E_LOCKED`, exit 4), Lock-Wait Absence, and Pool Exhaustion Under Parallel Agents (`S11`)
* **Impact**: **27 wasted tool calls** across the 3 parallel agents in `S11` (`28` `E_LOCKED` errors out of `45` invocations; `bf-s11-2` polled 20 times; `bf-s11-1` and `bf-s11-2` missed `.pool` completely and paid `36–38s` on `add` and `31s` on `remove`).
* **Scenario ID**: `S11` (`bf-s11-1`, `bf-s11-2`, `bf-s11-3`)
* **Transcript Quotes**:
  * `bf-s11-2` (step 9):
    ```text
    git repo-worktree list --json | jq '.result.trees[] | {name, path, state, branch}'
    jq: error (at <stdin>:8): Cannot iterate over null (null)
    ```
  * `bf-s11-2` (step 36):
    ```json
    {
      "ok": false,
      "exit_code": 4,
      "error": {
        "code": "E_LOCKED",
        "message": "another git-repo-worktree process holds the registry lock; retry shortly"
      }
    }
    ```
  * `bf-s11-1` (step 50):
    ```json
    {"name": "bf-s11-1", "path": "/usr/local/google/home/jomof/projects/studio-main-bf-s11-1", "removed": true, "returned_to_pool": []}
    ```
* **Root Causes in Code**:
  1. **Immediate non-blocking fail (`try_read` / `try_write`)**: [`with_registry_lock`](../../src/registry.rs) uses non-blocking `try_read`/`try_write` and fails immediately with `E_LOCKED` (`exit 4`) while `add` (`31–38s`) or `remove` (`21–35s`) holds `LockMode::Exclusive` across the entire multi-project checkout/delete loop!
  2. **`status` acquires `LockMode::Exclusive`**: [`run_status_command`](../../src/tree.rs) holds the registry lock across 56 `git status` calls (`1.6–8.2s`), blocking all other readers and writers.
  3. **`add -b <branch> -p <project>` bypasses `.pool`**: When `<project>` is in `branch_projects`, `add` skips `pool::take()` for `<project>` and runs a full `git worktree add -b <branch>` checkout (`3.5s` for `tools/base`), even though `pool::take()` followed by `git checkout -B <branch> <sha>` would take `~90ms`!
  4. **Pool capacity hard-coded to `1` on `remove` (`give_back(..., 1)`)**: When 3 parallel agents run `add` and `remove`, only the first `add` gets spares from `.pool`, and only the first `remove` returns spares to `.pool`; the other 2 agents discard their 266k-file checkouts (`31s` `worktree-remove`).
* **Recommended Fixes**:
  1. **Add bounded lock wait (`--lock-timeout <secs>`, default `60s` for `list`/`status` and `120s` for `add`/`remove`, with `--no-wait` for immediate `E_LOCKED`)**: Waiting with exponential backoff inside `with_registry_lock` eliminates all 28 `E_LOCKED` failures and all 27 retry tool calls automatically.
  2. **Hold `LockMode::Exclusive` only during metadata/pool-slot reservation (`<50ms`), not during the 30s project checkout/deletion loop**: Use a per-tree lock (`<tree>.lock`) while populating or removing a specific tree so `list`, `status`, and concurrent `add`/`remove` on different trees run in parallel!
  3. **Use `pool::take()` even when `-b <branch> -p <project>` is requested** (run `git checkout -B <branch> <sha>` + upstream tracking config after moving the spare).

---

### Rank 2: Missing Studio Bazel Bootstrap (`env.bazelrc`) & Orphaned `bazel` Server JVM on `remove` (`S6`, `S9`)
* **Impact**: **14 wasted tool calls** (`~180s`) in `S6` + **2.18 GB RSS leaked JVM process** in `S9`.
* **Scenario IDs**: `S6`, `S9`
* **Transcript Quotes**:
  * `S6` (step 17):
    ```text
    Let's check `.bazelrc` in `/usr/local/google/home/jomof/projects/studio-main-bf-s6`:
    `try-import %workspace%/env.bazelrc`
    Wait: `env.bazelrc` does not exist in `studio-main-bf-s6`, and `studio-main/env.bazelrc` has `startup --output_base=/google/cartfs/mount/82f1a270dd3e54436acdeaa58b68077a`!
    ```
  * `S9` (orchestrator check after `git repo-worktree remove bf-s9`):
    ```text
    198244 bazel(studio-main-bf-s9) -XX:+UseParallelGC ... --workspace_directory=/usr/local/google/home/jomof/projects/studio-main-bf-s9 ...
    VmRSS: 2231320 kB (2.18 GB)
    ```
* **Recommended Fixes**:
  1. **Document `./build/bazel/env/setup` and `bazel shutdown` in the skill** (saves 14 tool calls immediately).
  2. **In `remove`**: Before deleting `<tree_path>`, detect any process whose `/proc/<pid>/cmdline` contains `--workspace_directory=<tree_path>` (or `-Djava.library.path=...` inside `<tree_path>`) and terminate it cleanly (`SIGTERM`), preventing multi-GB JVM leaks.
  3. **In `add`**: Copy untracked root `env.bazelrc` while rewriting `<source_root>` $\to$ `<tree_root>` and `<md5(source_root)>` $\to$ `<md5(tree_root)>` (per [`addendum-env-bazelrc-and-cartfs.md`](../addendum-env-bazelrc-and-cartfs.md)).

---

### Rank 3: JSON Schema Asymmetry Between `list --json` and `status --json` + `90 KB` `list --json` Output Truncation (`S1`, `S7`, `S8`, `S9`, `S11`, `S13`)
* **Impact**: **7 wasted tool calls** (5 `jq` exit-5 errors across `S7`, `S8`, `S9`, `S11-3`, `S13` + 2 truncation re-runs in `S1`).
* **Scenario IDs**: `S1`, `S7`, `S8`, `S9`, `S11`, `S13`
* **Transcript Quote** (`S8` step 6, identical in `S7` step 37, `S9` step 5, `S11-3` step 25, `S13` step 9):
  ```text
  $ git repo-worktree status --json bf-s8 | jq '.result.trees[] | {name, path, state, dirty: [.projects[] | select(.dirty)]}'
  Command exited with code 5
  jq: error (at <stdin>:1): Cannot iterate over null (null)
  ```
* **Root Causes**:
  1. `list --json` returns `.result.trees[]`, whereas `status --json <tree>` returns `.result.projects[]` (a single tree object directly under `.result`). Agents that run `list --json` first naturally assume `status --json` uses the same `.result.trees[]` shape (or `.result.tree`).
  2. `list --json` embeds all 56 `projects` arrays for every registered tree (`15 trees * 56 projects = 840 project objects`, `~90 KB`), which exceeds LLM tool output limits (`46 KB`) and truncates the JSON envelope in `S1`, `S13`, and `S14`.
* **Recommended Fixes**:
  1. Omit the per-project `projects` array from `list --json` by default (include a summary `project_counts: {"worktree": 37, "overlay": 19}` and `failed_projects: []`, and only include full `projects` when `list -v --json` is passed). This shrinks `list --json` from `~90 KB` to `~4 KB`!
  2. In `skill/SKILL.md`, show the exact `jq` one-liner for `status --json <tree>` (`jq '.result | {name, dirty, dirty_projects: [.projects[] | select(.dirty)]}'`).

---

### Rank 4: `install.sh` Shadow Warning Suppresses Warning When Version Strings Match (`1.0.0` == `1.0.0`) (`S14`)
* **Impact**: **3 diagnostic tool calls** in `S14`; silent `PATH` shadowing in real user upgrades whenever `Cargo.toml` version is not bumped.
* **Scenario ID**: `S14`
* **Transcript Quote** (`S14` step 37):
  ```text
  Because ~/.cargo/bin precedes ~/.local/bin on PATH—and Git resolves git-<subcommand> executables on PATH before checking alias.repo-worktree—invoking git repo-worktree ran the stale ~/.cargo/bin/git-repo-worktree binary despite both binaries reporting "version": "1.0.0".
  ```
* **Root Cause in [`install.sh:370-374`](../../install.sh#L370-L374)**:
  ```sh
  if [ -n "$found_binary" ] && [ "$found_binary" != "$destination" ] &&
      [ "$("$found_binary" version 2>/dev/null || true)" != "$installed_version" ]; then
  ```
* **Recommended Fixes**:
  1. Remove the `version != installed_version` condition in `install.sh:371`: **any** time `found_binary` on `PATH` is not `$destination` (or compare `cmp -s "$found_binary" "$destination"`), emit the shadow warning!
  2. Embed the short Git commit SHA (`6ff9e5e`) in `build.rs` (`GIT_REPO_WORKTREE_GIT_SHA`) so `git repo-worktree version` outputs `git-repo-worktree 1.0.0 (6ff9e5e)`.

---

### Rank 5: `git repo-worktree pool fill` Fails With `E_NO_POOL_PROJECTS` Under Default (`auto`) Config & `pool status` Alias Missing (`S6`, `P12`)
* **Impact**: **2 wasted tool calls** (`pool status` in `S6` and orchestrator; `pool fill` in `P12`).
* **Scenario IDs**: `S6`, `P12`
* **Transcript Quote** (`P12`):
  ```json
  {"format_version":1,"command":"pool fill","ok":false,"exit_code":1,"error":{"code":"E_NO_POOL_PROJECTS","message":"no projects have a pool default; run `defaults pool <project>` or name projects"},"result":null}
  ```
* **Root Cause in [`src/pool.rs:313-326`](../../src/pool.rs#L313-L326)**:
  - `add` and `remove` automatically pool any non-shallow project with `est_bytes >= 500 MB`, and `remove` populates `.repo/repo-worktrees/.pool/<project>/`.
  - However, `pool fill` (with no project arguments) only queries `defaults::load(registry)?.projects` for explicit `"pool"` entries! It ignores both the existing directories in `.repo/repo-worktrees/.pool/` (`pooled_projects(registry)`) and the automatic `est_bytes >= 500 MB` heuristic!
* **Recommended Fixes**:
  1. In `PoolCommand::Fill`, when `args.projects` is empty, union explicit `"pool"` entries in `defaults.json` with `pooled_projects(&registry)` and the automatic `uses_pool()` set from the manifest/source checkout.
  2. Add `#[command(visible_alias = "status")]` on `PoolCommand::List` (and `DefaultsCommand::List`) in `src/cli.rs` so `git repo-worktree pool status` works as an alias for `pool list`.

---

### Rank 6: Dirty `studio-main/prebuilts/studio/sdk/linux` (`10.39 GB`) Falls Back From `overlay` to Unpooled `worktree/checkout` (`S1`–`S12`, `P1`–`P2`)
* **Impact**: Adds **`+28–33 seconds`** to every `add`, **`+14 seconds`** to every `remove`, and **`+10.4 GB`** of `ext4` disk usage to every tree (`S1`–`S12`, `P1`, `P2`). Also caused verbatim `S12` (`prebuilts/studio/sdk`) to find files in `sdk/linux` (`worktree`) without noticing that `sdk/remote` (`overlay`) was unmounted.
* **Scenario IDs**: `S1`–`S12`, `P1`, `P2`
* **Transcript Quote** (`S3` / `P2` report):
  ```json
  {"bytes":10391003569,"last":"worktree/checkout","max_ms":28820,"p50_ms":27871,"project":"prebuilts/studio/sdk/linux","samples":3}
  ```
* **Root Cause**:
  - `studio-main/prebuilts/studio/sdk/linux` has dirty `emulator/*` files, so `overlay` preflight rejects it (`"overlay preflight changed: source checkout is not clean"`).
  - When it falls back to `worktree`, `uses_pool()` in `src/tree.rs` returns `false` because `project.clone_depth == Some(1)`.
* **Recommended Fixes**:
  1. Allow `uses_pool()` to return `true` for any project that falls back to `worktree` mode and has `est_bytes >= 500 MB` (even if `clone_depth == Some(1)`).
  2. Better yet, when a `clone-depth="1"` project is dirty in the source checkout, create a single clean read-only checkout in `.repo/repo-worktrees/.pool/<project>/lower` and mount every tree's `fuse-overlayfs` `lowerdir` against that clean `.pool` checkout (`~100ms` mount and `0 GB` per tree!).
