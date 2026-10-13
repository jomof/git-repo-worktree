# Round 3 Blindfold Evaluation Report: `git-repo-worktree` v2.0.0 (`f5bd402c18ae`)

- **Date:** 2026-10-13
- **Binary Under Test:** `git-repo-worktree 2.0.0 (f5bd402c18ae)` (`~/.local/bin/git-repo-worktree`)
- **Bundled Skill:** `git-repo-worktree` (`x-installed-version: "2.0.0"`, synced via `git repo-worktree skill sync`)
- **Source Client:** `/usr/local/google/home/jomof/projects/studio-main` (56 active projects: 37 full-clone `worktree`, 19 `clone-depth="1"` copy-on-write `fuse-overlayfs` overlays; `prebuilts/studio/sdk/linux` falls back from dirty overlay to `worktree/pool`)
- **Host Environment:** Linux `7.1.6-1rodete1-amd64` x86_64, 96 CPUs, `ext4`, `git version 2.56.0.385.gd3acb90ef8-goog`, `repo version v2.68.1` (`2.65` launcher)

---

## 1. Executive Summary

Round 3 tested the six v2.0.0 changes (site hooks `post-add`/`pre-remove`, blocking registry/tree locks up to `--lock-timeout 300`, per-project pool locks, file-count pooling threshold `POOL_MIN_FILES = 30_000` / `POOL_MIN_BYTES = 2 GiB`, dirty-overlay fallback pooling for `prebuilts/studio/sdk/linux`, and `repair` re-running `post-add`) across **7 blindfold scenarios** (`9` blindfold subagent runs) and **6 orchestrator benchmarks** (`Q1`–`Q6`, 3 runs each).

| Metric | Round 2 (`c95501f9a0e4`) | Round 3 (`f5bd402c18ae`, v2.0.0) | Improvement |
|---|---|---|---|
| **R3-S6 (Bazel build + `remove`) wall clock** | `352s` (`24` tool calls, `14` on `env.bazelrc`) | **`108s`** (`12` tool calls, **`0` wasted on `env.bazelrc`**) | **3.3× faster, -50% tool calls** |
| **R3-S9 (Leaked Bazel server after `remove`)** | Leaked `2.18 GB` server (`PID 1984735`) | **`0` leaked servers** (`pre-remove` runs `bazel shutdown`) | **Eliminated** |
| **R3-S11 (3 parallel agents) `E_LOCKED` errors** | `28` `E_LOCKED` errors (`27` wasted retry calls) | **`0` `E_LOCKED` errors** (`0` retries) | **Eliminated (`28 -> 0`)** |
| **R3-S11 (3 parallel agents) total tool calls** | `81` tool calls (`185s`–`249s` wall clock) | **`34` tool calls** (`100s`–`160s` wall clock) | **-58% tool calls, 1.6× faster** |
| **Warm `add` latency (`Q2`, 3-run median)** | `31,422 ms` (`31.4 s`, `4` pool hits) | **`6,421 ms` (`6.42 s`, `6` pool hits incl. `sdk/linux`)** | **4.9× faster** (beats `< 12s` target) |
| **Cold `add` latency (`Q1`, 3-run median)** | `35,843 ms` (`35.8 s`) | **`31,866 ms` (`31.9 s`)** | **1.12× faster** |
| **`remove` default vs `--no-pool` (`Q3` median)** | `22,824 ms` vs `34,889 ms` | **`14,544 ms` (`6` pooled) vs `32,571 ms` (`--no-pool`)** | **1.57× faster default `remove`** |
| **Hook overhead (`Q5` median, live Bazel server)** | n/a | **`post-add`: `105 ms`; `pre-remove`: `502 ms`** | Well under `< 1s` / `< 5s` targets |
| **Source client integrity (`diff -u baseline final`)** | `0` lines changed | **`0` lines changed** (`154/154` lines identical) | **100% clean** |

---

## 2. Site Hooks Installed (`real-world-experience/round-3/hooks/`)

The two hooks from §2.3 of the test plan were installed verbatim into `/usr/local/google/home/jomof/projects/studio-main/.repo/repo-worktrees/hooks/` with **zero modifications needed** (`./build/bazel/env/setup` runs non-interactively in `~100 ms`, and `hooks::run` in [`src/hooks.rs`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/hooks.rs#L114-L115) redirects hook `stdout` to `stderr` so `--json` output on `stdout` stays valid JSON):

- **`post-add`** (`114` bytes, mode `0755`):
  ```sh
  #!/bin/sh
  # Give each tree its own env.bazelrc (remote cache, per-tree output_base).
  exec ./build/bazel/env/setup
  ```
- **`pre-remove`** (`245` bytes, mode `0755`):
  ```sh
  #!/bin/sh
  # Stop this tree's Bazel server, if one is running, without starting one.
  ob=$(sed -n 's/^startup --output_base=//p' env.bazelrc 2>/dev/null | tail -n 1)
  [ -n "$ob" ] && [ -f "$ob/server/server.pid.txt" ] || exit 0
  exec bazel shutdown
  ```

---

## 3. Scenario Summary Table

| Scenario | Subagent ID | Wall Clock | Tool Calls (Total / `git-repo-worktree` / Wasted) | Outcome & Key Checks |
|---|---|---|---|---|
| **R3-S6** (Bazel build in a tree) | `a433a18e-80d7-4383-96f7-64be076f08e6` | `108s` (R2: `352s`) | `12` / `5` / **`0`** (R2: `24` / `14` on `env.bazelrc`) | **PASS.** `add` took `6,896 ms` (`hook-post-add: 107 ms`, `6` pool hits). Agent verified `hooks.post-add.status == "ok"` and `env.bazelrc` (`output_base=/google/cartfs/mount/1be41f956f4276b162b23d7ceb0e88de` != `studio-main`'s `82f1a270...`), built `//tools/base/common:tools.common` in `16.337s` (`113 remote cache hit, 37 internal`, `0` local actions), and removed tree (`hook-pre-remove: 751 ms`, `lingering_processes: []`). |
| **R3-S9** (Clean up after a build) | `83f237d4-2bcc-44cb-8671-61b321e73450` | `109s` (R2: `52s`) | `7` / `4` / **`0`** | **PASS.** Setup started Bazel server `PID 3029425` in `bf-r3-s9`. Agent inspected `status --json`, ran `remove --json bf-r3-s9`, which ran `pre-remove` (`status: "ok"`) and stopped the Bazel server (`lingering_processes: []`, `pgrep` empty). *(Wall time included a 60s system `cartfs.service` restart during `bazel shutdown`; normal `pre-remove` is `501–751 ms`.)* |
| **R3-S11** (Parallel agents × 3) | `ef69b9e7` (`bf-r3-1`)<br>`7fcd6af5` (`bf-r3-2`)<br>`37005510` (`bf-r3-3`) | `100s`<br>`160s`<br>`138s`<br>(R2: `185–249s`) | `11` (`4` grw)<br>`10` (`4` grw)<br>`13` (`5` grw)<br>**Total: `34` / `0` retries** (R2: `81` / `27` retries) | **PASS.** **`0` `E_LOCKED` errors** (vs `28` in R2). `bf-r3-2` and `bf-r3-3` overlapped for `42s` (`lock-wait: 605 ms` on `bf-r3-3`). All 3 agents branched `tools/base`, committed, and removed their trees keeping branches `bf-r3-1..3`. |
| **R3-H1** (A hook that fails) | `ef536a29-5b1f-4fb6-a5bb-52c9a4401b80` | `202s` (across 2 turns) | `28` / `6` / `11` *(pre-`add` exploration after reading failing hook script)* | **PASS.** Agent saw `hooks.post-add.status: "failed"` (`exit_code: 1`) and stderr `"env setup failed: credentials expired; run gcert, then repair the tree"`, explicitly refused to copy `studio-main/env.bazelrc`, asked user to run `gcert`, then ran `git repo-worktree repair --json build-tools-common` (`198 ms`, `post-add: "ok"`, `100 ms`), built target (`16.490s`), and removed tree (`hook-pre-remove: 651 ms`). |
| **R3-H2** (Existing tree missed hook) | `94ff6237-aee9-4486-aad1-5e13c807af8b` | `140s` | `18` / `9` / `2` *(re-running `repair --json` to filter `projects` with `jq`)* | **PASS.** Agent saw `bf-r3-h2` lacked `env.bazelrc`, ran `git repo-worktree repair --json bf-r3-h2` (`186 ms`, `hooks.post-add.status: "ok"`, `100 ms`), verified `env.bazelrc`, and built target (`14.826s`, `113 remote cache hit, 37 internal`). |
| **R3-S2** (Branch in pooled project) | `aca4a60e-ec11-4c48-b78c-39ce6da5d220` | `331s` | `20` / `5` / `0` | **PASS.** `add` took `6,039 ms` (`6` pool hits). `tools/adt/idea` used `"strategy": "pool"` (`374 ms`), set `branch.bf-r3-s2.merge = refs/heads/studio-main`, and was the only project with branch `bf-r3-s2`. Agent noticed `studio-main/tools/adt/idea` was 2 commits ahead of `goog/studio-main`, showed the 3-commit `repo upload --dry-run`, and rebased onto `goog/studio-main` to show the 1-commit `repo upload --dry-run`. |
| **R3-S8** (Clean up with unsaved work) | `d823afa2-a8e3-4649-b1cf-f4fe52f96413` | `40s` | `6` / `4` / **`0`** | **PASS.** `remove --json bf-r3-s8` refused with `E_DIRTY` (`1,546 ms`), listing `tools/apksig` (`["untracked"]`) and `tools/base` (`["unbranched-commits"]`). `pre-remove` did **not** run (`hooks: {}`). Agent stopped and asked user whether to branch or `--force`. |

---

## 4. Detailed Scenario Findings & Verbatim Evidence

### R3-S6: Bazel build in a tree (`a433a18e-80d7-4383-96f7-64be076f08e6`)
- **What happened:**
  1. The agent read `SKILL.md`, ran `version --json`, `list --json`, and `hooks list --json`, and viewed `hooks/post-add` and `hooks/pre-remove`.
  2. It ran `git repo-worktree add --json --name common-build-test /usr/local/google/home/jomof/projects/studio-main-common-build-test` (`6,896 ms` total; `snapshot: 2,433 ms`, `projects: 4,115 ms`, `hook-post-add: 107 ms`; `6` pool hits).
  3. From `history.jsonl` (`ts: "2026-10-13T02:07:32Z"`):
     ```json
     {"cmd":"add","hooks":[{"event":"post-add","exit_code":0,"ms":103,"status":"ok"}],"ms":6896,"phases":{"directives":6,"finalize":3,"hook-post-add":107,"projects":4115,"repo-bridge":105,"snapshot":2433,"submodules":0,"total":6887}}
     ```
  4. It inspected `/usr/local/google/home/jomof/projects/studio-main-common-build-test/env.bazelrc`:
     ```text
     import %workspace%/tools/vendor/google/bazel/glinux.bazelrc
     startup --output_base=/google/cartfs/mount/1be41f956f4276b162b23d7ceb0e88de
     build --config=rcache
     build --config=release
     ```
     (confirming an isolated `output_base` distinct from `studio-main`'s `/google/cartfs/mount/82f1a270dd3e54436acdeaa58b68077a`).
  5. It ran `bazel build --sandbox_debug //tools/base/common:tools.common` (`16.337s`, `150 processes: 113 remote cache hit, 37 internal`, `0` local actions).
  6. It ran `git repo-worktree remove --json common-build-test`, which produced verbatim output:
     ```json
     {"format_version":1,"command":"remove","ok":true,"exit_code":0,"error":null,"result":{"hooks":{"post-add":{"at":"2026-10-13T02:07:32Z","exit_code":0,"ms":103,"reason":null,"status":"ok"},"pre-remove":{"at":"2026-10-13T02:08:30Z","exit_code":0,"ms":751,"reason":null,"status":"ok"}},"lingering_processes":[],"name":"common-build-test","path":"/usr/local/google/home/jomof/projects/studio-main-common-build-test","removed":true,"returned_to_pool":["external/intellij-plugins","tools/idea","external/jetbrains/kotlin","tools/adt/idea","tools/gpu-profiler/idea","prebuilts/studio/sdk/linux"]}}
     ```

### R3-S9: Clean up after a build (`83f237d4-2bcc-44cb-8671-61b321e73450`)
- **What happened:**
  1. Orchestrator created `bf-r3-s9` and ran `bazel build //tools/base/annotations:annotations`, leaving Bazel server `PID 3029425` (`bazel(studio-main-bf-r3-s9) ... workspace_directory=/usr/local/google/home/jomof/projects/studio-main-bf-r3-s9`) running.
  2. The agent checked `status --json bf-r3-s9`, verified `dirty_projects: []`, and ran `git repo-worktree remove --json bf-r3-s9`.
  3. Verbatim `remove --json` output from transcript step 9:
     ```json
     {"format_version":1,"command":"remove","ok":true,"exit_code":0,"error":null,"result":{"hooks":{"post-add":{"at":"2026-10-13T02:10:02Z","exit_code":0,"ms":100,"reason":null,"status":"ok"},"pre-remove":{"at":"2026-10-13T02:12:30Z","exit_code":0,"ms":61420,"reason":null,"status":"ok"}},"lingering_processes":[],"name":"bf-r3-s9","path":"/usr/local/google/home/jomof/projects/studio-main-bf-r3-s9","removed":true,"returned_to_pool":["external/intellij-plugins","tools/idea","external/jetbrains/kotlin","tools/adt/idea","tools/gpu-profiler/idea","prebuilts/studio/sdk/linux"]}}
     ```
  4. Post-scenario `pgrep -af "workspace_directory=.*bf-r3-s9"` confirmed **0 Bazel server processes remained**.
  5. *Note on `ms: 61420`:* `journalctl -u cartfs.service` showed that systemd restarted `cartfs.service` at `02:11:23–02:12:57 UTC` (after 1w 11h uptime) right as `bazel shutdown` accessed `/google/cartfs/mount/...`. Across all other live-Bazel-server removals (`R3-hook-verify`, `R3-S6`, `R3-H1`, `R3-H2`, `Q5`), `pre-remove` took **`501–751 ms`**.

### R3-S11: Parallel agents (`ef69b9e7`, `7fcd6af5`, `37005510`)
- **What happened:**
  Three subagents (`bf-r3-1`, `bf-r3-2`, `bf-r3-3`) were launched simultaneously.
- **Telemetry from `history.jsonl` and `sessions/R3-S11.json`:**
  - **`E_LOCKED` count:** **`0`** across all commands (`add`, `list`, `status`, `remove`).
  - **`add` timeline & overlap:**
    1. `bf-r3-1`: finished at `02:15:18Z`, `ms: 6294` (`snapshot: 2059 ms`, `projects: 3876 ms`, `hook-post-add: 105 ms`; **6 pool hits**, 0 misses).
    2. `bf-r3-3`: started at `02:15:22Z`, finished at `02:16:09Z`, `ms: 47133` (`lock-wait: 605 ms`, `snapshot: 2784 ms`, `projects: 43839 ms`, `hook-post-add: 112 ms`; 0 pool hits, 6 misses).
    3. `bf-r3-2`: started at `02:15:27Z`, finished at `02:16:13Z`, `ms: 46438` (`snapshot: 2158 ms`, `projects: 43929 ms`, `hook-post-add: 124 ms`; 0 pool hits, 6 misses).
    - `bf-r3-2` and `bf-r3-3` ran their `add` operations concurrently for **42 seconds** (`02:15:27Z`–`02:16:09Z`), with `bf-r3-3` waiting only `605 ms` in `lock-wait` and neither agent seeing an error.
  - **`remove` timeline & pool returns:**
    1. `bf-r3-1`: `ms: 15235` (`inspect: 4481 ms`, `pool-return: 7872 ms`, `worktree-remove: 2777 ms`, `hook-pre-remove: 50 ms`, `returned_to_pool: 6`).
    2. `bf-r3-3`: `ms: 12494` (`inspect: 1324 ms`, `pool-return: 8171 ms`, `worktree-remove: 2906 ms`, `hook-pre-remove: 50 ms`, `returned_to_pool: 6`).
    3. `bf-r3-2`: `ms: 32948` (`inspect: 4301 ms`, `pool-return: 1 ms`, `worktree-remove: 28529 ms`, `hook-pre-remove: 50 ms`, `returned_to_pool: 0` — because `bf-r3-1` and `bf-r3-3` had already filled `.pool` to its default capacity of `2` spares per project).
  - **Did `tools/base` use the pool?** No: `tools/base` (`est_files: 20400`, `est_bytes: 926380995`) is below both `POOL_MIN_FILES` (`30,000`) and `POOL_MIN_BYTES` (`2 GiB`) in [`TreeMeta::uses_pool`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/model.rs#L191-L202), so `tools/base` used `"strategy": "checkout"` (`2,782 ms` in `bf-r3-1`, `4,462 ms` in `bf-r3-3`, `4,637 ms` in `bf-r3-2`).

### R3-H1: A hook that fails (`ef536a29-5b1f-4fb6-a5bb-52c9a4401b80`)
- **What happened:**
  1. With `post-add` replaced by the failing script (`echo "env setup failed: credentials expired; run gcert, then repair the tree" >&2; exit 1`), the agent ran `add` (`6,399 ms`, `6` pool hits).
  2. Verbatim `hooks` object in the `add` JSON output (transcript step 36) and `history.jsonl`:
     ```json
     "hooks":{"post-add":{"at":"2026-10-13T02:21:42Z","exit_code":1,"ms":50,"reason":"exited with status 1","status":"failed"}}
     ```
  3. Instead of copying `studio-main/env.bazelrc` or running `./build/bazel/env/setup` behind the tool's back, the agent paused and sent the following message (verbatim from transcript step 39):
     > I created the isolated worktree `build-tools-common` at `/usr/local/google/home/jomof/projects/studio-main-build-tools-common`, and `git repo-worktree add` ran the `post-add` hook (`hooks.post-add.status: "failed"`, `exit_code: 1`), which failed with:
     >
     > ```text
     > env setup failed: credentials expired; run gcert, then repair the tree
     > ```
     >
     > Per the `git-repo-worktree` workflow:
     > - I did **not** copy `studio-main/env.bazelrc` into the worktree (since sharing `output_base` collides with `studio-main`'s Bazel server) or bypass the hook manually.
     > - Please run `gcert` (and restore/update `.repo/repo-worktrees/hooks/post-add` if needed), then let me know so I can run `git repo-worktree repair --json build-tools-common` to re-run `post-add` and proceed with `bazel build //tools/base/common:tools.common`.
  4. After the orchestrator restored the real `post-add` hook and replied that `gcert` was complete, the agent ran `git repo-worktree repair --json build-tools-common` (`198 ms`), which re-ran `post-add` (`"status": "ok"`, `"ms": 100`) without recreating the tree, built `//tools/base/common:tools.common` (`16.490s`, `113 remote cache hit, 37 internal`), and removed the tree (`hook-pre-remove: 651 ms`, `lingering_processes: []`).

### R3-H2: An existing tree that missed the hook (`94ff6237-aee9-4486-aad1-5e13c807af8b`)
- **What happened:**
  1. `bf-r3-h2` was created beforehand with `git repo-worktree add --no-hooks ../bf-r3-h2` (so `/usr/local/google/home/jomof/projects/bf-r3-h2/env.bazelrc` did not exist).
  2. The agent inspected `.bazelrc`, `hooks list --json`, and `hooks/post-add` (`exec ./build/bazel/env/setup`), saw that `bf-r3-h2` had no `env.bazelrc` and `"hooks": null` in `list --json`, and ran `git repo-worktree repair --json bf-r3-h2`.
  3. Verbatim `hooks` field from `git repo-worktree repair --json bf-r3-h2 | jq 'del(.result.projects)'` (transcript step 29):
     ```json
     "hooks": {
       "post-add": {
         "at": "2026-10-13T02:28:56Z",
         "exit_code": 0,
         "ms": 100,
         "reason": null,
         "status": "ok"
       }
     }
     ```
  4. The agent verified `/usr/local/google/home/jomof/projects/bf-r3-h2/env.bazelrc` (`startup --output_base=/google/cartfs/mount/b8a71e90d09f72507305b8bfa1e1e869`) and built `//tools/base/common:tools.common` in `14.826s` (`150 processes: 113 remote cache hit, 37 internal`).

### R3-S2: Branch in a pooled project (`aca4a60e-ec11-4c48-b78c-39ce6da5d220`)
- **What happened:**
  1. The agent ran `git repo-worktree add --json --name bf-r3-s2 -b bf-r3-s2 -p tools/adt/idea /usr/local/google/home/jomof/projects/studio-main-bf-r3-s2` (`6,039 ms` total, `6` pool hits).
  2. Verification checks (`list -v --json` and `git config`):
     ```json
     {
       "path": "tools/adt/idea",
       "mode": "worktree",
       "strategy": "pool",
       "upstream": "refs/heads/studio-main"
     }
     ```
     ```text
     branch.bf-r3-s2.remote goog
     branch.bf-r3-s2.merge refs/heads/studio-main
     ```
     ```json
     {
       "name": "bf-r3-s2",
       "branch": "bf-r3-s2",
       "branch_projects": [
         "tools/adt/idea"
       ],
       "checked_out_branches": [
         {
           "branch": "bf-r3-s2",
           "project": "tools/adt/idea"
         }
       ]
     }
     ```
  3. `tools/adt/idea` populated from `.pool` in **`374 ms`** (vs `11,489 ms` cold checkout) while still creating branch `bf-r3-s2` with upstream `refs/heads/studio-main` on `tools/adt/idea` only.

### R3-S8: Clean up with unsaved work (`d823afa2-a8e3-4649-b1cf-f4fe52f96413`)
- **What happened:**
  1. The agent ran `status --json bf-r3-s8` and then `git repo-worktree remove --json bf-r3-s8`.
  2. Verbatim `E_DIRTY` output from transcript step 9:
     ```json
     {"format_version":1,"command":"remove","ok":false,"exit_code":1,"error":{"code":"E_DIRTY","message":"tree has dirty projects: tools/apksig (untracked), tools/base (unbranched-commits); inspect with `status`, or pass --force to discard","details":{"dirty_projects":[{"branch":null,"dirty":true,"head":"2347523d2de9b01ac2e783457a07172629759272","mode":"worktree","modified":0,"ms":10,"path":"tools/apksig","pinned":"2347523d2de9b01ac2e783457a07172629759272","reasons":["untracked"],"unbranched_commits":0,"untracked":1},{"branch":null,"dirty":true,"head":"f8c39e5be2a5dbae489edb7f3de9c49894dda6fe","mode":"worktree","modified":0,"ms":252,"path":"tools/base","pinned":"909b56a082a9eb94ff81d9cf83321e072a0acf27","reasons":["unbranched-commits"],"unbranched_commits":1,"untracked":0}]}},"result":null}
     ```
  3. `sessions/R3-S8.json` confirmed `"hooks": {}` — `pre-remove` is only called at [`src/tree.rs:2300`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/tree.rs#L2298-L2304) *after* dirty-project and lock checks succeed, so refusing removal never shuts down a tree's Bazel server prematurely.

---

## 5. Performance Matrix (`Q1`–`Q6`, 3 Runs Each)

All runs were tagged with `GIT_REPO_WORKTREE_SESSION=R3-perf-Q<n>` and saved in `real-world-experience/round-3/sessions/R3-perf-Q*.json`.

| Row | Operation | Run 1 | Run 2 | Run 3 | **Round 3 Median** | Round 2 Median | Target |
|---|---|---|---|---|---|---|---|
| **Q1** | Cold `add` (`pool clear`, then `add`) | `31,129 ms` (`projects: 29,069 ms`) | `32,265 ms` (`projects: 30,283 ms`) | `31,891 ms` (`projects: 29,251 ms`) | **`31,891 ms` (`31.9 s`)** | `35,843 ms` (`35.8 s`) | `~48 s` |
| **Q2** | **Warm `add` (pool populated, 6 hits incl. `sdk/linux`)** | `5,274 ms` (`projects: 2,911 ms`) | `6,427 ms` (`projects: 2,811 ms`) | `8,138 ms` (`projects: 4,349 ms`) | **`6,427 ms` (`6.4 s`)** | `31,422 ms` (`31.4 s`) | **`< 12 s`** (`~9 s`) |
| **Q3a** | `remove` default (`returned_to_pool: 6`) | `14,544 ms` (`pool-return: 8,412 ms`, `wt-rm: 2,802 ms`) | `13,729 ms` (`pool-return: 7,538 ms`, `wt-rm: 2,928 ms`) | `17,208 ms` (`pool-return: 8,222 ms`, `wt-rm: 3,257 ms`) | **`14,544 ms` (`14.5 s`)** | `22,824 ms` (`22.8 s`) | Record both |
| **Q3b** | `remove --no-pool` (`returned_to_pool: 0`) | `32,571 ms` (`wt-rm: 28,563 ms`) | `31,406 ms` (`wt-rm: 28,910 ms`) | `37,140 ms` (`wt-rm: 34,694 ms`) | **`32,571 ms` (`32.6 s`)** | `34,889 ms` (`34.9 s`) | Record both |
| **Q4** | Parallel `add` × 3 (`9` total `add`s across 3 batches; `E_LOCKED = 0`) | `16,381 ms` / `49,657 ms` / `50,015 ms` (6 spares in pool) | `12,349 ms` / `12,536 ms` / `44,547 ms` (12 spares in pool) | `12,285 ms` / `26,625 ms` / `46,032 ms` (12 spares in pool) | **`26,625 ms` (`26.6 s`)** (`12.3 s` warm / `46.0 s` 3rd tree; **`0` `E_LOCKED`**) | `35–46 s`, `28` `E_LOCKED` | **`0` `E_LOCKED`** |
| **Q5a** | `hook-post-add` duration (`./build/bazel/env/setup`) | `104 ms` (script `100 ms`) | `161 ms` (script `152 ms`) | `105 ms` (script `100 ms`) | **`105 ms` (`0.11 s`)** | n/a | **`< 1 s`** |
| **Q5b** | `hook-pre-remove` duration (live Bazel server `bazel shutdown`) | `502 ms` (`PID 3951633`) | `502 ms` (`PID 3970134`) | `501 ms` (`PID 3986341`) | **`502 ms` (`0.50 s`)** | n/a | **`< 5 s`** |
| **Q6** | `git repo-worktree list --json \| wc -c` (13 live trees) | `8,975 B` | `8,975 B` | `8,975 B` | **`8,975 B` (`8.76 KiB`)** | `~90 KB` (`~425 KB` in v1.0) | **`~8 KB`** |

### Pool Disk Usage (`du -sh .repo/repo-worktrees/.pool`)
- After `Q1` `remove` (1 spare per pooled project = **6 spares**): **`18G`** (`du -sh .repo/repo-worktrees/.pool`).
- At full default pool capacity (`DEFAULT_SPARES = 2` per project = **12 spares**): **`36G`**.

---

## 6. Answers to §6 Questions

### Q1. Do the hooks remove the Bazel friction (`R3-S6` tool calls, `R3-S9` leaked server)? Is `./build/bazel/env/setup` fast and quiet enough as a `post-add` hook?
**Yes on all counts.**
- In **R3-S6**, total tool calls dropped from **`24` to `12`** (`-50%`), wasted tool calls on `env.bazelrc` dropped from **`14` to `0`**, and end-to-end wall clock dropped from **`352s` to `108s`** (`3.3×` faster).
- In **R3-S9**, `remove` automatically executed `pre-remove` (`bazel shutdown`), leaving **0 leaked Bazel servers** (vs a leaked `2.18 GB` server in Round 2). When no Bazel server was started in a tree (`R3-S11`, `Q1`–`Q4`), the `[ -f "$ob/server/server.pid.txt" ] || exit 0` guard in `pre-remove` exited in **`50 ms`** without spawning a JVM.
- `./build/bazel/env/setup` runs in **`100–152 ms`** (`p50: 100 ms` across 25 runs in `history.jsonl`) and emits only 3 informational lines (`INFO: Detected platform...`, `INFO: Found workspace root...`, `INFO: Wrote env.bazelrc`), which `hooks::run` ([`src/hooks.rs:114-115`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/hooks.rs#L114-L115)) routes to `stderr` so `--json` stdout is never polluted.

### Q2. When a hook fails (`R3-H1`) or never ran (`R3-H2`), do agents recover without falling into the `env.bazelrc` copy trap?
**Yes.** Neither `R3-H1` nor `R3-H2` copied `studio-main/env.bazelrc` or ran `./build/bazel/env/setup` manually:
- In **R3-H1**, the agent saw `"hooks": {"post-add": {"status": "failed", "exit_code": 1}}` and the stderr message, explicitly told the user why copying `studio-main/env.bazelrc` is unsafe (shared `output_base`), waited for `gcert`, and then ran `git repo-worktree repair --json build-tools-common` (`198 ms`), which re-executed `post-add` (`status: "ok"`, `100 ms`).
- In **R3-H2**, the agent saw `bf-r3-h2` had no `env.bazelrc`, read `SKILL.md`, and ran `git repo-worktree repair --json bf-r3-h2` (`186 ms`), which ran `post-add` (`status: "ok"`, `100 ms`) and created `bf-r3-h2/env.bazelrc`.

### Q3. Is lock contention gone for parallel agents (`R3-S11`)? Do parallel `add`s overlap (`Q4`)?
**Yes.**
- **`E_LOCKED` errors dropped from `28` in Round 2 to `0` in Round 3** across both `R3-S11` (3 concurrent subagents) and `Q4` (9 concurrent `add` invocations across 3 batches).
- Parallel `add`s genuinely overlap: in `R3-S11`, `bf-r3-2` and `bf-r3-3` overlapped for **42 seconds** (`lock-wait: 605 ms`); in `Q4` Run 2 (with 12 spares in `.pool`), two concurrent `add`s (`r3-q4-1` and `r3-q4-3`) both finished in **`12.3s` and `12.5s`** simultaneously (`lock-wait: 1,841 ms` and `300 ms`), while the 3rd concurrent `add` (`r3-q4-2`, which missed the 2-spare-capacity pool) ran in parallel and completed in `44.5s`.

### Q4. Does warm `add` reach `~9 s` with `sdk/linux` pooled (`Q2`)? Is the extra spare disk acceptable?
**It beats `~9 s`, reaching a median of `6.43 s` (`5.27 s` best run) — a `4.9×` speedup over Round 2's `31.4 s`!**
- Pooling `prebuilts/studio/sdk/linux` (`213,697` files, `10.39 GB`) cut its population time from **`28,894 ms` (`checkout`) to `1,544 ms` (`pool`)** (`18.7×` faster).
- Together with `tools/idea` (`12,948 ms -> 1,547 ms`), `tools/adt/idea` (`11,489 ms -> 258 ms`), `tools/gpu-profiler/idea` (`11,275 ms -> 786 ms`), `external/jetbrains/kotlin` (`4,310 ms -> 356 ms`), and `external/intellij-plugins` (`1,673 ms -> 128 ms`), the entire 56-project `projects` phase now completes in **`2,811–2,911 ms`** on a warm pool.
- **Disk trade-off:** 1 full set of 6 spares in `.pool` uses **`18 GB`** (`36 GB` at the default capacity of 2 spares per project), of which `prebuilts/studio/sdk/linux` accounts for `~10.4 GB` per spare. On a workstation with ratusan GB of free disk, `18–36 GB` for a `6.4 s` worktree creation is a very worthwhile trade-off; furthermore, clean `studio-main` checkouts where `prebuilts/studio/sdk/linux` is not locally modified will mount `sdk/linux` as a zero-disk `fuse-overlayfs` overlay instead.

### Q5. Did the file-count pooling rule pick the right projects?
**Mostly yes — it captured all 6 of the highest-file-count projects, including `external/jetbrains/kotlin` (`106k` files) and `external/intellij-plugins` (`38k` files) that Round 2 missed — but raising `POOL_MIN_BYTES` from `500 MiB` to `2 GiB` in [`src/model.rs:18-20`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/model.rs#L18-L20) dropped `tools/base` (`20,400` files, `926 MB`), which is now the #1 bottleneck on warm `add`.**

Here are the top 11 worktree projects from `Q1` (cold `add`) vs `Q2` (warm `add`):

| Project | `est_files` | `est_bytes` | Pooled by `uses_pool`? (`files >= 30k \|\| bytes >= 2 GiB`) | Cold `populate_ms` (`Q1`) | Warm `populate_ms` (`Q2`) |
|---|---:|---:|---|---:|---:|
| `prebuilts/studio/sdk/linux` | `213,697` | `10,391 MB` | **Yes** (both) | `28,894 ms` | **`1,544 ms`** (`pool`) |
| `tools/idea` | `274,366` | `982 MB` | **Yes** (`files >= 30k`) | `12,948 ms` | **`1,547 ms`** (`pool`) |
| `tools/adt/idea` | `53,258` | `2,278 MB` | **Yes** (both) | `11,489 ms` | **`258 ms`** (`pool`) |
| `tools/gpu-profiler/idea` | `266,404` | `966 MB` | **Yes** (`files >= 30k`) | `11,275 ms` | **`786 ms`** (`pool`) |
| `external/jetbrains/kotlin` | `106,264` | `305 MB` | **Yes** (`files >= 30k`) | `4,310 ms` | **`356 ms`** (`pool`) |
| **`tools/base`** | **`20,400`** | **`926 MB`** | **No** (dropped when `POOL_MIN_BYTES` rose `500M -> 2G`) | `2,482 ms` | **`2,590 ms`** (`checkout`) |
| `external/intellij-plugins` | `38,304` | `312 MB` | **Yes** (`files >= 30k`) | `1,673 ms` | **`128 ms`** (`pool`) |
| **`tools/vendor/google`** | **`14,554`** | **`457 MB`** | **No** | `1,432 ms` | **`1,499 ms`** (`checkout`) |
| **`external/dexter`** | **`183`** | **`666 MB`** | **No** (dropped when `POOL_MIN_BYTES` rose `500M -> 2G`) | `1,408 ms` | **`1,332 ms`** (`checkout`) |
| **`tools/emulator`** | **`521`** | **`289 MB`** | **No** | `1,226 ms` | **`1,080 ms`** (`checkout`) |
| **`tools/vendor/intellij/cidr`** | **`28,360`** | **`198 MB`** | **No** (just `1,640` files below `30,000`) | `1,019 ms` | **`1,209 ms`** (`checkout`) |

### Q6. Did any scenario change `studio-main` unexpectedly?
**No.** `diff -u /tmp/r3-baseline.txt /tmp/r3-final.txt` produced **0 lines of diff** (`154/154` lines identical; see [`integrity.txt`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/real-world-experience/round-3/integrity.txt)).

### Q7. New rough edges, ranked by tool calls wasted, with verbatim quotes

#### Rough Edge 1: `add --json` and `repair --json` always serialize the full 56-project `projects` array (~25 KB), causing output truncation and inconsistent `jq` filtering (`2` wasted tool calls in `R3-H2`)
- **Code location:** [`src/tree.rs:761`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/tree.rs#L761) (`add`) and [`src/tree.rs:3189-3193`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/tree.rs#L3189-L3193) (`repair`):
  ```rust
  let result = if trees.len() == 1 {
      serde_json::to_value(&trees[0]).expect("tree metadata serializes")
  } else {
      json!({"trees": trees})
  };
  ```
  Unlike `list --json` (which omits `.projects`, `.copyfiles`, and `.linkfiles` unless `-v` / `--verbose` is passed, keeping output to `8.9 KB` across 13 trees), `add --json` and `repair --json` dump the entire `TreeMeta` (`~25 KB` for a single tree). Furthermore, `repair --json` changes its top-level `.result` shape depending on whether 1 tree (`.result.projects`) or multiple trees (`.result.trees[].projects`) were repaired.
- **Verbatim evidence (`R3-H2`, `94ff6237-aee9-4486-aad1-5e13c807af8b`, steps 24–29):**
  1. Step 24 (`git repo-worktree repair --json bf-r3-h2`): single-line 25 KB JSON output was truncated in the middle by the agent's tool-output cap (`<truncated 4227 bytes>`), hiding `.result.hooks`.
  2. Step 26 (`git repo-worktree repair --json bf-r3-h2 | jq 'del(.result.trees[].projects)'`):
     ```text
     jq: error (at <stdin>:1): Cannot iterate over null (null)
     ```
  3. Step 28 (`git repo-worktree repair --json bf-r3-h2 | jq 'del(.result.projects)'`): finally succeeded and revealed `"hooks": {"post-add": {"status": "ok", "ms": 100}}`.
- **Recommendation:** Omit `projects`, `copyfiles`, and `linkfiles` from `add --json` and `repair --json` unless `-v` / `--verbose` is passed (replacing them with `project_counts`, `failed_projects`, and `hooks`, just like `list --json`).

#### Rough Edge 2: `list --json` and `list -v --json` omit `"hooks"` when `--no-hooks` was used (or before hooks run), making a missed hook indistinguishable from a pre-v2.0 tree (`1–2` exploratory calls in `R3-H2`)
- **Code location:** [`src/hooks.rs:58-60`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/hooks.rs#L58-L60) returns `None` when `--no-hooks` is passed, leaving `TreeMeta::hooks` empty (`#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]` in [`src/model.rs:143-144`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/model.rs#L143-L144)).
- **Verbatim evidence (`R3-H2`, `94ff6237-aee9-4486-aad1-5e13c807af8b`, step 12):**
  When the agent queried `git repo-worktree list --json | jq '.result.trees[] | {name, path, state, hooks}'`, `bf-r3-h2` returned:
  ```json
  {
    "name": "bf-r3-h2",
    "path": "/usr/local/google/home/jomof/projects/bf-r3-h2",
    "state": "complete",
    "hooks": null
  }
  ```
- **Recommendation:** Record `"status": "skipped"` in `meta.hooks["post-add"]` when `post-add` is installed and executable in `.repo/repo-worktrees/hooks/post-add` but skipped due to `--no-hooks` or `GIT_REPO_WORKTREE_NO_HOOKS=1`, and include `hooks` in `list --json` summary entries when non-empty.

#### Rough Edge 3: `inspect::processes_referencing` flags ancestor `bash -c` subshells as `lingering_processes` when the shell command mentions the tree path (`0` agent calls wasted, noisy warning)
- **Code location:** [`src/inspect.rs:248-299`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/inspect.rs#L248-L299) (`processes_referencing`) skips only `pid == std::process::id()` (`line 265`), not ancestor PIDs (`PPID` chain). Whenever an agent runs a compound shell command like `bash -c "... /path/to/<tree> && git repo-worktree remove <tree>"`, `/proc/<ppid>/cmdline` contains `/path/to/<tree>` and is reported as a lingering process (`reason: "arguments"`).
- **Verbatim evidence (`R3-S11`, `bf-r3-3` / `37005510-7924-409c-a704-6490b33eb70d`, step 22, and `R3-S2` cleanup):**
  ```json
  "lingering_processes":[{"command":"bash -c git -C /usr/local/google/home/jomof/projects/studio-main/tools/base show-ref --verify refs/heads/bf-r3-3 && git ","pid":3575690,"reason":"arguments"}]
  ```
  ```text
  warning: 1 process(es) still use the removed tree; stop them if they are no longer needed:
    pid 3847217 (arguments): bash -c cd /usr/local/google/home/jomof/projects/studio-main && \
  echo "=== R3-S2 CHECKS ===" && \
  git repo-worktree lis
  ```
- **Recommendation:** Walk the ancestor PID chain via `/proc/<pid>/stat` (`ppid`) from `std::process::id()` up to PID 1 and exclude all ancestor processes in `processes_referencing`.

#### Rough Edge 4: `return_projects_to_pool` in `remove` runs sequentially (`7.5–8.4 s` in `pool-return` phase)
- **Code location:** [`src/tree.rs:2493-2520`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/tree.rs#L2493-L2520) (`return_projects_to_pool`) iterates with a sequential `for project in candidates` loop, running `pool::give_back` (`git worktree move` + `git checkout -q --detach` + `git clean -ffdxq` in [`src/pool.rs:200-218`](file:///usr/local/google/home/jomof/projects/git-repo-worktree/src/pool.rs#L200-L218)) one project at a time.
- **Verbatim evidence (`Q3` in `history.jsonl`):**
  ```json
  {"session":"R3-perf-Q3","cmd":null,"ms":14544,"phases":{"delete":5,"hook-pre-remove":50,"inspect":3046,"pool-return":8412,"prune":14,"unmount":43,"worktree-remove":2802},"pool_hits":null,"pool_misses":null,"returned_to_pool":6}
  ```
- **Recommendation:** Since each candidate project has its own `git_common_dir` and per-project pool lock (`lock_project`), running `candidates.par_iter()` inside `pool.install(...)` should cut `pool-return` from `~8.2 s` to `~2.5 s`, bringing default `remove` under `9 s`.
