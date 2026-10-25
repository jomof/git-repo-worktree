# Round 8 Real-World Experience Report (`v2.3.0`, Oct 21–25, 2026)

## 1. Executive Summary & Dataset

This packet captures 4 days of production multi-agent usage on `git-repo-worktree` **`v2.3.0` (`f7b1ab346520`)** between `2026-10-21T17:07:45Z` and `2026-10-25T15:58:41Z`, anchored by the root-cause analysis in **[`./worktree-rca.md`](./worktree-rca.md)**.

### Included Artifacts & Telemetry ([`./data/`](./data/))

* **[`./worktree-rca.md`](./worktree-rca.md)**: Root-cause analysis of the `2026-10-25 14:23–14:28 UTC` incident where worktree `v4-package` inherited 6 local commits from `~/projects/studio-main/tools/vendor/google` (`mother` branch at `f0d61ab633`) because the agent omitted a manual `git checkout goog/studio-main` step after `git repo-worktree add`.
* **[`./data/v2.3.0-highlights.json`](./data/v2.3.0-highlights.json)**: Curated slice of the `v2.3.0` telemetry events backing every finding in this report (`R7-F6` SDK pool reap, `R8-1` source-branch inheritance events, `R8-2` multi-GB self-expiring archives, `R8-3` `remove --idle` and `E_ARCHIVE_INCOMPLETE` failures, `R8-4` `pool maintain` 88-minute trash sweep, and all `v2.3.0` non-zero exit events).
* **[`./data/source-branch-divergence.json`](./data/source-branch-divergence.json)**: Snapshot of the 7 source projects in `~/projects/studio-main` after `tools/vendor/google` was detached back to `goog/studio-main` (`f491491880`) at `2026-10-25 15:33:38 UTC`, while the two idle spares in `.pool/tools%2Fvendor%2Fgoogle/` (`1792938346578056453-1776552-6` and `1792938421396127992-1776552-13`) still sit at `f0d61ab633` (`mother`, 6 commits ahead).
* **[`./data/studio-main/`](./data/studio-main/)**: Full sanitized `studio-main` telemetry (`1,239` all-time events, **`318` events on `v2.3.0`**): [`report.txt`](./data/studio-main/report.txt), [`report.json`](./data/studio-main/report.json), [`history.jsonl`](./data/studio-main/history.jsonl), [`pool-list.json`](./data/studio-main/pool-list.json), [`list.json`](./data/studio-main/list.json), and [`archive-list.json`](./data/studio-main/archive-list.json).
* **[`./data/main/`](./data/main/)**: Full sanitized AOSP `main` telemetry (`140` all-time events, **`14` events on `v2.3.0`**): [`report.txt`](./data/main/report.txt), [`report.json`](./data/main/report.json), [`history.jsonl`](./data/main/history.jsonl), [`pool-list.json`](./data/main/pool-list.json), and [`list.json`](./data/main/list.json).

### Summary of `v2.3.0` Production Events on `studio-main` (318 Events)

| Command | Runs | Succeeded | `p50` | `p90` | `max` | Failures (`error`) |
| :--- | ---: | ---: | ---: | ---: | ---: | :--- |
| `add` | 43 | 42 | `17,891 ms` | `38,257 ms` | `72,882 ms` | `1` (`E_DEST_EXISTS`) |
| `remove` | 59 | 49 | `12,174 ms` | `204,705 ms` | `871,844 ms` | `6` `E_ARCHIVE_INCOMPLETE`, `3` `E_INTERRUPTED`, `1` `E_USAGE` |
| `pool maintain` | 85 | 85 | `18 ms` | `65,244 ms` | `5,286,392 ms` | `0` |
| `list` | 67 | 67 | `11 ms` | `14 ms` | `892 ms` | `0` |
| `status` | 35 | 20 | `3,897 ms` | `5,028 ms` | `11,013 ms` | `15` (`E_USAGE`) |
| `pool list` | 8 | 8 | `714 ms` | `1,095 ms` | `1,252 ms` | `0` |
| `report` | 6 | 6 | `17 ms` | `23 ms` | `30 ms` | `0` |
| `archive list` | 6 | 6 | `1 ms` | `10 ms` | `10 ms` | `0` |
| `prune` | 3 | 3 | `8 ms` | `2,591 ms` | `2,591 ms` | `0` |
| `repair` | 3 | 2 | `6,348 ms` | `46,356 ms` | `46,356 ms` | `1` (`E_USAGE`) |
| `detach` | 2 | 2 | `676 ms` | `704 ms` | `704 ms` | `0` |
| `skill` | 1 | 1 | `1 ms` | `1 ms` | `1 ms` | `0` |

---

## 2. Field Verification of Round 7 (`v2.3.0`) Fixes

All three core fixes shipped in `v2.3.0` (`f7b1ab346520`) performed as intended in production:

1. **Round 7 F6 (Overlay whiteouts for untracked source files — [`src/overlay.rs:35-66`](../../src/overlay.rs#L35-L66))**:
   * On the very first `pool maintain` after upgrading to `v2.3.0` (`2026-10-21T19:35:26Z`), `git-repo-worktree` reaped both orphaned `prebuilts/studio/sdk/linux` spares (`22,532,612,962` bytes = **22.53 GB / 20.99 GiB reclaimed**, `"reason": "shared or overlaid from its clean source checkout"`) and refilled all 7 actual source repos (`tools/idea`, `external/jetbrains/kotlin`, `tools/adt/idea`, `tools/base`, `external/intellij-plugins`, `tools/vendor/intellij/cidr`, `tools/vendor/google`) to `depth 2 of 2`.
   * Every subsequent `add` overlaid `prebuilts/studio/sdk/linux` via `fuse-overlayfs` in `~2.8 s` (`0` pool spares consumed), cutting each spare set from `~18.9 GB` to `~7.6 GB`.
2. **Round 7 F1 (`fsmonitor--daemon` prevention & teardown — [`src/pool.rs:527-576`](../../src/pool.rs#L527-L576))**:
   * Across the 4-day window, `v2.3.0` stopped **16** lingering `fsmonitor--daemon` processes (`12` during `remove`, `4` during `pool maintain`).
   * Across all 43 `v2.3.0` `add` runs, **zero** checkouts hit the 372-second `fsmonitor` hang seen in `v2.2.1`; `add` `max` dropped from `372,499 ms` (`v2.2.1`) to `72,882 ms` (`v2.3.0`), with warm-pool `studio-main` adds completing in **`5.2–6.7 s`** (`2026-10-25T14:23:11Z` `android-cli-1p` in `5,244 ms`).
3. **Round 7 F2 (File-descriptor hygiene & `registry::acquire_within`)**:
   * No file-descriptor leaks or inherited agent sockets occurred across any `v2.3.0` `fuse-overlayfs` or `pool maintain` processes.

---

## 3. Finding R8-1: Source Branch Inheritance Footgun & Single-Client Baseline Gap ([`./worktree-rca.md`](./worktree-rca.md))

### What Happened in Production (`2026-10-25 14:23–14:28 UTC`)

As detailed in **[`./worktree-rca.md`](./worktree-rca.md)**:

1. In the user's primary checkout `~/projects/studio-main`, `tools/vendor/google` was checked out on a local branch `mother` at `f0d61ab633` (**6 commits ahead** of `goog/studio-main` at `f491491880`).
2. Because `git repo-worktree add` snapshots the initiating client's current on-disk `HEAD` commits (and `pool maintain` pre-fills spares at those commits), both idle spares in `.repo/repo-worktrees/.pool/tools%2Fvendor%2Fgoogle/` and every newly created worktree started `tools/vendor/google` at `f0d61ab633` (with the 6 local `mother` commits baked into history).
3. At `14:23:11 UTC`, the agent working in `studio-wt-android-cli-1p` remembered to run `git checkout goog/studio-main` inside `tools/vendor/google`.
4. At `14:24:28 UTC`, the agent working in `v4-package` did **not** run `git checkout goog/studio-main` and committed `Add 1P package for android-cli` directly on top of the 6 unrelated `mother` commits, contaminating its branch/CL stack.
5. Even after `~/projects/studio-main/tools/vendor/google` was manually detached back to `goog/studio-main` (`f491491880`) at `15:33:38 UTC`, the two pre-filled spares in `.pool/tools%2Fvendor%2Fgoogle/` (`1792938346578056453-1776552-6` and `1792938421396127992-1776552-13`) remain at `f0d61ab633` on disk until consumed by `pool::take` (which fortunately runs `git checkout --detach -f <sha>` when `current_head != sha` at [`src/pool.rs:408-417`](../../src/pool.rs#L408-L417)).

### Why Round 7's `add --manifest <file>` Did Not Prevent This

In Round 7 ([`../round-7/response.md`](../round-7/response.md)), `add --manifest <file>` was added for reproducible baselines, while a built-in `--from-manifest` / `--at-upstream` flag was declined with the suggestion:
> *"Run `repo manifest -r -o baseline.xml` in a reference client that has been synced to the desired state, then pass `--manifest baseline.xml` to each `add`."*

On a developer workstation, however, **there is no second reference client**—there is only `~/projects/studio-main` (`~37 GB`), where the developer or another agent has `tools/vendor/google` checked out on a feature branch (`mother`). Running `repo manifest -r` inside `~/projects/studio-main` records `revision="f0d61ab633..."` (`mother`), not `goog/studio-main`!

As a result, every subagent spawned to do clean upstream work must remember to manually find which projects diverged and run `git checkout --detach goog/studio-main` after `git repo-worktree add`. Whenever an agent forgets (as `v4-package` did), it silently builds on top of whatever local branch happens to be checked out in the main client.

### Actionable Proposals for `git-repo-worktree`

While keeping `add`'s default behavior (snapshotting the source client's current `HEAD`s) unchanged:

1. **Surface diverged/branched source projects at `add` (and `status`) time**:
   * [`src/manifest.rs`](../../src/manifest.rs) already parses each project's manifest `remote` and `upstream` (`refs/heads/studio-main` -> `refs/remotes/goog/studio-main`).
   * During `add` snapshot preflight, check whether each project's source `HEAD` differs from `refs/remotes/<remote>/<upstream_branch>` (or is on a local branch). When any project diverges from its manifest upstream, print a concise stderr note (when not `-q`):
     ```text
     note: 1 project in source client differs from manifest upstream (goog/studio-main):
       tools/vendor/google: at branch 'mother' (f0d61ab633, +6/-0 vs goog/studio-main)
     ```
     and include a `diverged_from_upstream` array in `add --json` and `status --json`. That single line gives both humans and agents immediate visibility that the new tree inherited local commits.
2. **Add an opt-in upstream baseline flag (`git repo-worktree add --upstream` or `git repo-worktree detach --upstream`)**:
   * Resolve each project's target SHA from its local remote-tracking ref `refs/remotes/<remote>/<upstream>` (resolved from `.repo/manifest.xml` `remote` + `upstream`/`revision`, with **zero network I/O**) instead of the source checkout's working `HEAD`.
   * Because [`pool::take`](../../src/pool.rs#L408-L417) already checks out any target `sha` on a pooled spare when `current_head != sha`, and because only the 1–2 diverged source projects differ from `refs/remotes/<remote>/<upstream>`, `add --upstream` stays just as fast as a normal warm-pool `add` while eliminating the multi-step `git checkout --detach goog/studio-main` ritual from agent prompts.

---

## 4. Finding R8-2: Multi-GB Archives (`76.34 GB` Across 42 Removes) Immediately Self-Expire via `apply_retention` While `remove` Still Claims They Are Restorable

### Evidence from `v2.3.0` Telemetry ([`./data/studio-main/report.txt`](./data/studio-main/report.txt), [`./data/v2.3.0-highlights.json`](./data/v2.3.0-highlights.json))

In [`./data/studio-main/report.txt`](./data/studio-main/report.txt):

```text
Archives (current: 0, 0 B)
  created: 42 (71.10 GiB), restored: 0, pruned: 0, expired: 42 (71.10 GiB)
```

Every single one of the 42 archives created under `v2.3.0` (`76,343,554,239` bytes = **76.34 GB / 71.10 GiB**) was **expired**, leaving `0` archives on disk—including massive single-tree removes that spent up to **7 minutes** in the `archive` phase:

| Timestamp (`UTC`) | Tree | Total `remove` (`ms`) | `phases.archive` (`ms`) | `archive_bytes` | `archive_id` | `archives_expired` |
| :--- | :--- | ---: | ---: | ---: | :--- | :--- |
| `2026-10-21T20:47:38Z` | `studio-wt-pippin` | `58,526 ms` | `20,031 ms` | `17,484,565,532 B` (**16.28 GiB**) | `studio-wt-pippin-20261021T204716Z` | **`["studio-wt-pippin-20261021T204716Z"]`** |
| `2026-10-22T01:49:49Z` | `studio-wt-finn-w4` | `460,315 ms` (**7.7 min**) | `419,997 ms` (**7.0 min**) | `6,223,131,829 B` (**5.80 GiB**) | `studio-wt-finn-w4-20261022T014242Z` | **`["studio-wt-finn-w4-20261022T014242Z", ...]`** |
| `2026-10-22T15:54:57Z` | `restack` | `78,617 ms` | `66,307 ms` | `6,962,251,719 B` (**6.48 GiB**) | `restack-20261022T155338Z` | **`["restack-20261022T155338Z", ...]`** |
| `2026-10-23T19:01:28Z` | `vsidl-proto` | `871,844 ms` (**14.5 min**) | `306,353 ms` (**5.1 min**) | `1,001,259,012 B` (**0.93 GiB**) | `vsidl-proto-20261023T184715Z` | `[]` (later wiped by next `>2 GiB` remove) |
| `2026-10-25T12:04:30Z` | `studio-wt-rhea-2` | `72,068 ms` | `69,899 ms` | `14,720,407,382 B` (**13.71 GiB**) | `studio-wt-rhea-2-20261025T120319Z` | **`["studio-wt-rhea-2-20261025T120319Z"]`** |
| `2026-10-25T12:18:17Z` | `studio-wt-purewins` | `30,058 ms` | `13,864 ms` | `6,593,517,671 B` (**6.14 GiB**) | `studio-wt-purewins-20261025T121749Z` | **`["studio-wt-purewins-20261025T121749Z", ...]`** |
| `2026-10-25T12:29:33Z` | `prefab-arm1` | `29,886 ms` | `10,163 ms` | `21,280,558,645 B` (**19.82 GiB**) | `prefab-arm1-20261025T122905Z` | **`["prefab-arm1-20261025T122905Z", ...]`** |

Look closely at the `archives_expired` field on `prefab-arm1` (`2026-10-25T12:29:33Z`):
* `remove` created `prefab-arm1-20261025T122905Z` (`21.28 GB`) and **in the very same `remove` call** expired `["prefab-arm1-20261025T122905Z", "prefab-verify-20261025T122829Z", "prefab-arm2-20261025T122806Z", "asfp-symbols-shepherd-20261025T122719Z", "asfp-symbols-legolas-20261025T122659Z"]`!

### Root Cause Chain in [`src/archive.rs`](../../src/archive.rs) and [`src/tree.rs`](../../src/tree.rs)

1. **`unique_bytes` omits `--not <archived.pinned>` ([`src/archive.rs:372-381`](../../src/archive.rs#L372-L381))**:
   ```rust
   fn unique_bytes(git_dir: &str, tip: &str) -> u64 {
       let Ok(output) = git_dir_command(git_dir)
           .args([
               "rev-list",
               "--objects",
               "--disk-usage",
               tip,
               "--not",
               "--branches",
               "--remotes",
               "--tags",
           ])
           .output()
   ```
   Compare this with `count_unbranched` in [`src/inspect.rs:363`](../../src/inspect.rs#L363), which passes `"--not", &project.sha`. Because `unique_bytes` does **not** pass `"--not", &archived.pinned`, whenever a project's `pinned` commit (or an ancestor) is not reachable from current `--branches --remotes --tags` (for instance, after an upstream force-push, a detached/shallow prebuilt fetch, or an amended local branch), `git rev-list --objects --disk-usage` counts **the entire repository object history (`6–21 GiB`)** as part of the archive's `bytes`!
2. **`snapshot_changes` hashes arbitrary untracked build artifacts into `.repo/projects/*.git/objects` ([`src/archive.rs:457-490`](../../src/archive.rs#L457-L490))**:
   * `snapshot_changes` runs `git add -A -- .` with a temporary `GIT_INDEX_FILE`. When an agent leaves untracked build artifacts, `.deb` packages, or extracted SDK/NDK directories inside a project directory (as in `studio-wt-finn-w4` and `vsidl-proto`), `git add -A` spends **5–7 minutes** compressing gigabytes of binaries into the shared `.repo/projects/<path>.git/objects` store.
3. **`expired()` immediately deletes the just-created archive (`index 0`) AND wipes all older archives ([`src/archive.rs:908-921`](../../src/archive.rs#L908-L921))**:
   ```rust
   pub fn expired(records: &[Record], policy: &Policy, now: u64) -> Vec<Record> {
       let mut total = 0u64;
       let mut expired = Vec::new();
       for (index, record) in records.iter().enumerate() {
           if record.keep {
               continue;
           }
           total = total.saturating_add(record.bytes);
           let too_old = index > 0 && now.saturating_sub(record.archived_unix) > policy.keep_secs;
           let over_budget = total > policy.budget_bytes;
           if too_old || over_budget {
               expired.push(record.clone());
           }
       }
       expired
   }
   ```
   Notice that `too_old` guards `index > 0`, **but `over_budget` does NOT guard `index > 0`!** And because `total = total.saturating_add(record.bytes)` is never decremented when `index 0` is expired, a single archive whose computed `record.bytes > 2 GiB` (`DEFAULT_BUDGET_BYTES` at [`src/archive.rs:33`](../../src/archive.rs#L33)) not only **deletes itself (`index 0`) immediately** inside `apply_retention` ([`src/tree.rs:3022`](../../src/tree.rs#L3022)), it also causes `total > policy.budget_bytes` to remain `true` for every subsequent record (`index 1..N`), **wiping out every other non-kept archive in the repository**!
4. **`remove` still tells the user that the deleted archive can be restored ([`src/tree.rs:3042-3056`](../../src/tree.rs#L3042-L3056))**:
   * Twenty lines after `apply_retention` deletes `archive.id`, `remove` prints:
     ```text
     archived as prefab-arm1-20261025T122905Z (3 project(s) with changes, 19.8 GiB); `git repo-worktree restore prefab-arm1` brings it back
     ```
     even though `prefab-arm1-20261025T122905Z` was just deleted on line 3022!

### Recommended Fixes

1. **Pass `"--not", &archived.pinned` to `unique_bytes` ([`src/archive.rs:372`](../../src/archive.rs#L372))**:
   * An archive only uniquely retains objects reachable from `tip` that are not reachable from `archived.pinned` (or `--branches --remotes --tags`). Adding `"--not", &archived.pinned` prevents counting 6–21 GiB of base repo history when `pinned` is detached.
2. **Never self-expire `index == 0` (or stop accumulating `total` for expired records) in `expired()` ([`src/archive.rs:908-921`](../../src/archive.rs#L908-L921))**:
   * If `remove` just spent time creating `records[0]`, `apply_retention` should never delete `records[0]` in the same breath and then print `` `git repo-worktree restore <tree>` brings it back ``.
   * Either exempt `index == 0` from `over_budget` (Just as `too_old` already checks `index > 0`, and only count `record.bytes` toward `total` when a record is kept/retained, or cap `total` appropriately so one large archive doesn't cascade-delete smaller recent archives), **or** check untracked file sizes before `git add -A` and fail with `E_ARCHIVE_INCOMPLETE` (or warn) when untracked files exceed a reasonable threshold unless `--force` / `--discard` is passed.

---

## 5. Finding R8-3: `remove --idle` Latency (`5–12.5 min`), Unmounted Overlay `E_ARCHIVE_INCOMPLETE` Dead-End, and `--idle --force` Rejection

### Evidence from `2026-10-25 11:48–12:30 UTC` ([`./data/v2.3.0-highlights.json`](./data/v2.3.0-highlights.json))

When cleaning up ~45 accumulated worktrees on `2026-10-25`:

1. `git repo-worktree remove --idle --dry-run` (`11:48:23Z`) failed immediately with `E_USAGE` (`--idle` requires a `<DURATION>` argument).
2. `git repo-worktree remove --idle 12h --dry-run` (`11:53:19Z`) ran for **`290,397 ms` (4m 50s)** before being interrupted (`E_INTERRUPTED`).
3. `git repo-worktree remove --idle 24h` (`12:00:54Z`) ran for **`436,105 ms` (7m 16s)** before being interrupted (`E_INTERRUPTED`).
4. `git repo-worktree remove --idle 24h` (`12:17:19Z`) ran for **`753,023 ms` (12m 33s)** before being interrupted (`E_INTERRUPTED`).
5. The user then tried `git repo-worktree remove --idle 24h --force` (`12:17:29Z`), which failed with `E_USAGE` because [`src/cli.rs:310`](../../src/cli.rs#L310) marks `--idle` as `conflicts_with = "force"`.
6. Falling back to per-tree `git repo-worktree remove <tree>`, 6 trees failed with `E_ARCHIVE_INCOMPLETE`:
   * `studio-wt-rhea-2`, `studio-wt-purewins`, `restack-upper`: `prebuilts/tools (unmounted-overlay: overlay is not mounted; run git repo-worktree repair --mounts-only)`
   * `prefab-arm1`, `prefab-arm2`, `prefab-verify`: untracked nested git directory inside a project (`nested-git`).

### Root Causes & Recommended Fixes

1. **`last_activity` runs `git status` on every project of every tree—even clean `overlay` projects ([`src/inspect.rs:308-339`](../../src/inspect.rs#L308-L339))**:
   * In `inspect_project` ([`src/inspect.rs:195`](../../src/inspect.rs#L195)), `git status` is skipped for `MODE_OVERLAY` whenever `overlay::upper_has_changes(&upper) == Ok(false)`.
   * In `last_activity` ([`src/inspect.rs:308-339`](../../src/inspect.rs#L308-L339)), however, there is **no `upper_has_changes` fast-path** and **no early short-circuit**: it spawns `git status --porcelain=v1 -z --untracked-files=normal` on all 57 projects of all 45 trees (`~2,565` `git status` invocations, `~1,800` of them over `fuse-overlayfs`), even when `meta.json` or `.git/HEAD` already shows the tree is active, or when an overlay's `upper` directory has zero modifications!
   * **Fix**:
     1. In `last_activity`, check `overlay::upper_has_changes(&upper)` first for `MODE_OVERLAY` projects—if `Ok(false)`, skip both `git status` and walking the overlay mount point (`admin/HEAD` and `admin/logs/HEAD` suffice).
     2. Pass the idle cutoff timestamp (`cutoff: Option<u64>`) into `last_activity`: as soon as `latest >= cutoff` (from `meta.json` or the first project's `.git/index` / `logs/HEAD`), short-circuit and return `latest` immediately without running `git status` on the remaining 56 projects!
2. **`processes_referencing` scans all of `/proc` once per tree ([`src/inspect.rs:406-450`](../../src/inspect.rs#L406-L450), [`src/tree.rs:3146`](../../src/tree.rs#L3146))**:
   * `remove_idle` calls `processes_referencing(&root)` inside the per-tree loop (and `remove()` calls it again), walking `/proc/<pid>/cwd` and `/proc/<pid>/fd/*` across thousands of system processes `2 * N_trees` times.
   * **Fix**: Snapshot `/proc` open paths once before the `remove_idle` loop (or only call `processes_referencing` after a tree passes the `idle_secs < threshold` check).
3. **Unmounted overlays with zero upper changes fail `remove` with `E_ARCHIVE_INCOMPLETE` ([`src/archive.rs:437-450`](../../src/archive.rs#L437-L450))**:
   * When an older tree has an unmounted overlay (e.g., `prebuilts/tools` after a reboot or after an earlier `git worktree prune` removed its worktree admin dir so `remount_overlays` fails `protect_registration` with `"overlay registration not found"`), `capture_project` ([`src/archive.rs:437-450`](../../src/archive.rs#L437-L450)) unconditionally emits `Item { kind: "unmounted-overlay", detail: "overlay is not mounted; run git repo-worktree repair --mounts-only" }`.
   * Even worse, the suggested command (`git repo-worktree repair --mounts-only`) calls the exact same `remount_overlays` function that `remove` just called at [`src/tree.rs:3007`](../../src/tree.rs#L3007), so running `repair --mounts-only` does not fix it!
   * **Fix**: In `capture_project` ([`src/archive.rs:437-450`](../../src/archive.rs#L437-L450)), if `project.mode == MODE_OVERLAY && !crate::overlay::is_live(&checkout)`, check `crate::overlay::upper_has_changes(&upper)`:
     * If `upper_has_changes(&upper) == Ok(false)` and `admin_head(&upper, &checkout).as_deref().unwrap_or(&project.sha) == project.sha`, the overlay has **no user changes at all**—archive it cleanly at `project.sha` without emitting `unmounted-overlay`!
4. **Allow `--force` and `--discard` with `remove --idle`, and give `--idle` a sensible default duration ([`src/cli.rs:305-315`](../../src/cli.rs#L305-L315))**:
   * Allow `remove --idle [DURATION] --force` and `remove --idle [DURATION] --discard` (`num_args = 0..=1, default_missing_value = "24h"`), so users and agents can batch-clean stale trees without getting stuck in a flag conflict.

---

## 6. Finding R8-4: `prune` (`capture_orphaned`) Loses Overlay Project Commits on Linux (`tests/tree_cli.rs::dirty_remove_archives_and_prune_records_heads`)

While verifying the test suite on this host (`Linux 6.12`, `fuse-overlayfs` installed, `git 2.56.0`), `cargo test --test tree_cli dirty_remove_archives_and_prune_records_heads` fails deterministically at [`tests/tree_cli.rs:334`](../../tests/tree_cli.rs#L334):

```text
thread 'dirty_remove_archives_and_prune_records_heads' panicked at tests/tree_cli.rs:334:5:
assertion `left == right` failed
  left: "0aec36e2a20107995d3b777f5f8827edf7a0e9ae"
 right: "c597aa8bccc60523fbe839371f42f40ac7444e29"
```

### Root Cause in [`src/archive.rs:781-815`](../../src/archive.rs#L781-L815) and [`src/overlay.rs:191-206`](../../src/overlay.rs#L191-L206)

1. In `dirty_remove_archives_and_prune_records_heads` ([`tests/tree_cli.rs:307-335`](../../tests/tree_cli.rs#L307-L335)), project `p` is created in `MODE_OVERLAY` (on Linux when `fuse-overlayfs` is available), commits `stranded work` (`c597aa8b`), and then the tree directory `second` is deleted with `fs::remove_dir_all(&second)` to test `git repo-worktree prune` + `git repo-worktree restore`.
2. During `prune`, `capture_orphaned` calls `orphaned_head(meta, project)` ([`src/archive.rs:781-786`](../../src/archive.rs#L781-L786)):
   ```rust
   fn orphaned_head(meta: &TreeMeta, project: &ProjectMeta) -> Option<(String, Option<String>)> {
       let checkout = meta.root().join(&project.path);
       if project.mode == MODE_OVERLAY {
           let upper = crate::overlay::upper_dir(&meta.registry_entry(), &project.path);
           return crate::overlay::admin_head(&upper, &checkout).map(|head| (head, None));
       }
   ```
3. `overlay::admin_head(&upper, &checkout)` calls `overlay::admin_dir(&upper, &checkout)` ([`src/overlay.rs:191-206`](../../src/overlay.rs#L191-L206)):
   * Because `bridge::synthesize` rewrote `upper/.git` into a relative symlink `../.repo/projects/p.git/worktrees/p1` (relative to `mount_point`, i.e., `<deleted-tree>/p`), `mount_point.join(target)` normalizes to `<deleted-tree>/.repo/projects/p.git/worktrees/p1`.
   * Because `<deleted-tree>` was deleted by `rm -rf` (the exact precondition for `prune`!), `<deleted-tree>/.repo/projects` **no longer exists**, so `fs::canonicalize(absolute)` fails with `ENOENT` and `orphaned_head` returns `None`!
4. Meanwhile, lines 787–814 of `orphaned_head` scan `Path::new(&project.git_common_dir).join("worktrees")` for the registration whose `gitdir` matches `checkout.join(".git")`—which lives in the **source** client's `.repo/projects/p.git/worktrees/p1`, works identically for both `MODE_WORKTREE` and `MODE_OVERLAY`, and also preserves the branch name `(commit, branch)`!
5. **Secondary leak in `prune` ([`src/tree.rs:4000-4022`](../../src/tree.rs#L4000-L4022))**:
   * Because `overlay::populate` writes `.repo/projects/<path>.git/worktrees/<id>/locked` (`repo-worktree-overlay`), running `git --git-dir=<gitdir> worktree prune` in `prune` ([`src/tree.rs:4014`](../../src/tree.rs#L4014)) skips locked overlay registrations! Before `fs::remove_dir_all(&entry)` at [`src/tree.rs:3990`](../../src/tree.rs#L3990), `prune` should unlock any worktree registration belonging to `tree` (matching `gitdir == checkout.join(".git")`) so `git worktree prune` actually cleans them up.

---

## 7. Finding R8-5: `pool maintain` Phase Accounting (`phases.fill` Nesting `phases.trash`) & Agent CLI Ergonomics

### 7.1 `pool maintain` Phase Timer Nesting ([`src/pool.rs:1066-1102`](../../src/pool.rs#L1066-L1102))

At `2026-10-25T13:18:46Z`, a `pool maintain` run that swept 52 trashed trees recorded:

```json
{
  "ts": "2026-10-25T13:18:46Z",
  "ms": 5286392,
  "phases": {
    "fill": 5280619,
    "paused": 1624901,
    "plan": 115,
    "reap": 5655,
    "trash": 5280503
  },
  "filled": [],
  "trees_deleted": 52
}
```

Even though `filled: []` (0 spares were filled), `phases.fill` reported `5,280,619 ms` (**88.0 minutes**) because in [`src/pool.rs:1066-1102`](../../src/pool.rs#L1066-L1102):

```rust
let step = Instant::now();
...
if level == 2 {
    deleted_trees = empty_trash(client_root, registry, jobs, &mut trash_ms);
}
crate::metrics::phase("fill", step.elapsed().as_millis() as u64);
```

`empty_trash` at `level == 2` is executed inside the `step` timer for `"fill"`, so `phases.fill` double-counts the entire `phases.trash` duration (`5,280,503 ms`). Moving the `if level == 2 { deleted_trees = empty_trash(...); }` block outside the `step` timer for `"fill"` will keep `phases.fill` and `phases.trash` orthogonal in `report`.

### 7.2 Agent CLI Ergonomics (`15` `status` `E_USAGE` Errors from the Source Client)

Out of 27 non-zero exit events across all 318 `v2.3.0` commands, **15 (`55.6%`)** were agents running `git repo-worktree status` or `git repo-worktree status --json` while their `cwd` was the **source client** (`~/projects/studio-main`), hitting:

```text
error[E_USAGE]: status requires a tree name or a current directory inside a worktree
```

* **Recommendation**: Either make bare `git repo-worktree status` in the source client fall back to listing active trees (or add a hint to the `E_USAGE` message: `"status requires a <tree> name when run from the source client; use 'git repo-worktree list' to inspect all worktrees"`).
