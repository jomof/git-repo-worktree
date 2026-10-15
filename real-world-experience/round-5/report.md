# Round 5: Pool Maintenance and AOSP Stall Attribution (`git-repo-worktree 2.0.0`, `a9a71ee28036`)

**Date:** 2026-10-15 (02:32–03:27 UTC)  
**Build under test:** `git-repo-worktree 2.0.0 (a9a71ee28036)` (`/usr/local/google/home/jomof/.local/bin/git-repo-worktree`, verified single binary on `PATH` via `which -a git-repo-worktree`)  
**Machine:** Linux `7.1.6-1rodete1-amd64` (x86_64, 96 hardware threads, 188 GiB RAM, single DM-Crypt/LVM `ext4` volume `/dev/glinux_20230721/root`, `git version 2.56.0.176.g3575e65682-goog`, `fuse-overlayfs 1.13`)  
**Clients under test:**
- `~/projects/studio-main` (Android Studio: 56 active manifest projects, 19 `fuse-overlayfs` mounts, 37 Git worktrees, of which 9 are pooled)
- `~/projects/main` (AOSP: 1,084 active manifest projects, 113 `fuse-overlayfs` mounts, 971 Git worktrees, of which 14 are pooled)

---

## 1. Headline Findings

1. **`studio-main` Orphan Classification & Cleanup (§3 P1):**
   - `git repo-worktree pool list` classified all **18 orphaned spares (`about 142.4 GB`)** across the exact 9 dead-spare projects identified in Round 4 (`external/dexter` as `orphaned: quick to check out (about 1342 ms)` and 8 `clone-depth="1"` prebuilts as `orphaned: shared or overlaid from its clean source checkout`), while keeping 2 spares each for the 9 active pooled projects (`depth 2 of 2`).
   - One `clone-depth="1"` project, `prebuilts/studio/sdk/linux` (2 spares, `about 11.3 GB each`), is **not** marked orphaned because its source checkout in `~/projects/studio-main/prebuilts/studio/sdk/linux` has untracked files (`source_is_clean()` is false), causing `add` to check it out as a worktree and consume spares from `.pool`. `pool list`'s classification matches `add`'s runtime behavior; cleaning the untracked files in `studio-main/prebuilts/studio/sdk/linux` would allow overlaying it and save another `~21.8 GiB` in `.pool`.
   - `git repo-worktree pool clear --orphaned --json` completed in **19.77 s** (`0.53 s` user, `14.89 s` sys, `78%` CPU) and reclaimed **`138,822,864 KB` (`132.39 GiB` / `142.15 GB`)** by `du -sx .pool` (dropping `.pool` from `179,874,336 KB` / `171.54 GiB` to `41,051,472 KB` / `39.15 GiB`) and **`142,131,298,304` bytes (`142.13 GB` / `132.37 GiB`)** by `df -B1 /`.

2. **`studio-main` Background Refill, Bursts, Spaced Adds & Build Impact (§3 P2–P4):**
   - **Single add + refill (P2.1):** `add` tree A took **4.92 s** (9/9 pool hits). Detached `pool maintain` refilled the 9 consumed spares (`~20.5 GB`) at `nice -n 19` / `ionice -c 3` in **172.09 s** (`plan: 2.40 s`, `fill: 169.68 s`), restoring pool depth to `2 of 2`.
   - **Back-to-back burst (P2.2):** Adding B, C, D with zero delay hit the pool on **B (5.34 s, 9/9 hits)** and **C (6.54 s, 9/9 hits)** and missed on **D (49.80 s, 0/9 hits)**. C's and D's `pool maintain` processes exited in `0 ms` with `"skipped": "another maintenance run is active"`, while B's `pool maintain` re-planned after round 1 and refilled all **18 spares** (both consumed sets) in **296.25 s**.
   - **Spaced adds (P2.3):** Three adds spaced 2 minutes apart (**S1: 7.59 s**, **S2: 8.55 s**, **S3: 7.39 s**) all achieved **9/9 pool hits (100% hit rate)** because each 1-set background `pool maintain` completed inside the 120 s window (**114.98 s**, **113.74 s**, and **112.80 s**).
   - **Zero Bazel build slowdown (P3):** Running `bazel build //tools/base/common:tools.common` immediately after `add` while `pool maintain` refilled 9 spares in the background took **29.46 s** and **21.06 s** (mean **25.26 s**), compared to **29.39 s** and **30.28 s** (mean **29.84 s**) with `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1`.
   - **Disk delta (P4):** Steady-state `.pool` after P2 is **`41,051,040 KB` (`39.15 GiB` / `42.04 GB`)**. A pool-hit `add` increases filesystem `Used` bytes (`df -B1 /`) by only **+1.52 to +1.70 GB** before refill (since the 9 pooled projects are moved out of `.pool` via `rename(2)`), whereas a pool-miss `add` writes **~22.0 GB** of new checkouts immediately (+30.86 GB total `df` delta during D, including concurrent background refill from B).

3. **AOSP (`~/projects/main`) Cold `add` Speedup & Remaining Stall Attribution (§4 S1–S5):**
   - **5–7× faster cold `add`s than Round 4:** With `a9c1499` (starting independent projects before `append_journal_batch`, throttling `meta.json` rewrites to 1 Hz, and `remove --no-pool` avoiding pool-return writeback storms), the 9 cold 1,084-project AOSP `add`s in S2 completed in **24.82 s to 43.39 s** (vs. **155–243 s** in Round 4), and a warm-pool `add` (`S4-add3-warm`, 14/14 pool hits) completed in **20.50 s**!
   - **What dominates the remaining time in `projects` on AOSP:** Even though `dispatcher.fill()` is called before `append_journal_batch()`, **the single dispatcher thread still executes `append_journal_batch` (`file.sync_all()`) and `write_meta` (`file.sync_all()`) synchronously inside the completion loop**. Across the 9 S2 runs, `projects-journal-sync` (**9.1–23.5 s**) + `projects-meta-write` (**0.5–5.4 s**) account for **53% to 83% of the entire `projects` phase wall time** (and **93.4%** in `S4-add1`: `31.0 s + 18.6 s = 49.6 s` out of `53.1 s`). While the main thread is blocked in `ext4` `fsync()`, worker threads finish their `60–150 ms` checkouts and sit idle in `Sl+:futex_do_wait:git-repo-worktr`.
   - **Parallelism `-j` matrix (S2):** **`-j 16`** achieved the fastest individual runs (**24.82 s** and **26.14 s**, median **26.14 s**, mean **31.45 s**), while **`-j 32`** was the most consistent across all 3 rotations (**29.86 s**, **31.40 s**, **29.58 s**; median **29.86 s**, mean **30.28 s**). **`-j 8`** was slowest (**36.82 s**, **34.15 s**, **42.01 s**; median **36.82 s**, mean **37.66 s**).
   - **Concurrent `add` during AOSP `pool maintain` (S4):** Starting a second cold `add` (`S4-add2-during-maintain`) while `pool maintain` was filling 28 spares (~36.8 GB) slowed `add` to **97.71 s** (**3.2× slower** than S2 `-j 32`'s 29.86 s). Even under `ionice -c 3`, `pool maintain`'s checkouts dirty the global Linux page cache (`Dirty:` reached **36.4 GB**), throttling `S4-add2`'s `git` processes in kernel `D+:balance_dirty_pages` (**925 thread-seconds**) and stalling `fsync()` (`projects-journal-sync: 44.65 s`, `projects-meta-write: 11.09 s`).
   - **`remove` prune fix (S5):** `phases.prune` on AOSP dropped from **200–31,800 ms** in Round 4 (when it ran `git worktree prune` across all 1,084 projects) to **25–47 ms** (median **29 ms**, `pruned_git_dirs: 113` overlay projects) across all 12 AOSP `remove` runs. Default `remove` (returning 14 projects to `.pool` via `rename(2)`) took **25.54 s** and **14.41 s** (`worktree-remove: 6.60 s` and `6.59 s`), saving **~10 s** of deletion work compared to `remove --no-pool` (**24.31 s**, `worktree-remove: 16.35 s`).

---

## 2. Setup and Source Integrity (§2)

Before running any benchmarks:
1. Recorded starting state for both `~/projects/studio-main` (21 pre-existing live trees, `.pool` at `179,874,336 KB`) and `~/projects/main` (2 pre-existing live trees `main-feature` and `studio-main.temp`, `.pool` at `19,351,208 KB` with 1 spare each of 14 pooled projects).
2. Captured source-checkout integrity snapshots (`snap-before.txt`: `HEAD`, `git status --porcelain=v1`, and `.git/worktrees` list for every manifest project) on both clients, and verified after all Round 5 runs (`snap-after.txt` / `snap-diff.txt`):
   - `studio-main/snap-diff.txt`: **0 bytes (0 diffs)**
   - `main/snap-diff.txt`: **0 bytes (0 diffs)**

---

## 3. `~/projects/studio-main`: Pool Maintenance, Bursts, and Build Cost

### P1. Orphan Report and Cleanup

Verbatim output of `git repo-worktree pool list` on `~/projects/studio-main` before cleanup ([`data/studio-main/P1-pool-list-before.txt`](data/studio-main/P1-pool-list-before.txt)):

```text
prebuilts/studio/sdk/linux  2 spare(s)  about 11.3 GB each
tools/idea  2 spare(s)  about 2.1 GB each
tools/gpu-profiler/idea  2 spare(s)  about 2.1 GB each
tools/adt/idea  2 spare(s)  about 2.5 GB each
external/jetbrains/kotlin  2 spare(s)  about 740.2 MB each
tools/base  2 spare(s)  about 1.0 GB each
external/intellij-plugins  2 spare(s)  about 469.0 MB each
tools/vendor/intellij/cidr  2 spare(s)  about 314.2 MB each
tools/vendor/google  2 spare(s)  about 516.9 MB each
external/dexter  2 spare(s)  about 667.0 MB each  orphaned: quick to check out (about 1342 ms)
prebuilts/clang/host/linux-x86  2 spare(s)  about 3.9 GB each  orphaned: shared or overlaid from its clean source checkout
prebuilts/remoteexecution-client  2 spare(s)  about 1.2 GB each  orphaned: shared or overlaid from its clean source checkout
prebuilts/studio/buildbenchmarks  2 spare(s)  about 4.1 GB each  orphaned: shared or overlaid from its clean source checkout
prebuilts/studio/intellij-sdk  2 spare(s)  about 13.6 GB each  orphaned: shared or overlaid from its clean source checkout
prebuilts/studio/jdk  2 spare(s)  about 8.2 GB each  orphaned: shared or overlaid from its clean source checkout
prebuilts/studio/sdk/remote  2 spare(s)  about 5.8 GB each  orphaned: shared or overlaid from its clean source checkout
prebuilts/tools  2 spare(s)  about 27.1 GB each  orphaned: shared or overlaid from its clean source checkout
tools/external/gradle  2 spare(s)  about 6.6 GB each  orphaned: shared or overlaid from its clean source checkout
depth 2 of 2: the next 2 add(s) can take every pooled project from the pool
18 orphaned spare(s), about 142.4 GB: no default add uses them; `pool clear --orphaned` deletes them
```

#### Comparison with Round 4's Dead-Spare Table

| Project | Spares Before | `pool list` Size Est. (each) | Round 4 `du -sx` (2 spares) | `pool list` Classification & Reason | Matches Round 4? |
| :--- | ---: | ---: | ---: | :--- | :--- |
| `prebuilts/tools` | 2 | 27.1 GB | 51.02 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `prebuilts/studio/intellij-sdk` | 2 | 13.6 GB | 25.35 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `prebuilts/studio/jdk` | 2 | 8.2 GB | 15.32 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `tools/external/gradle` | 2 | 6.6 GB | 12.23 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `prebuilts/studio/sdk/remote` | 2 | 5.8 GB | 10.89 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `prebuilts/studio/buildbenchmarks` | 2 | 4.1 GB | 7.62 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `prebuilts/clang/host/linux-x86` | 2 | 3.9 GB | 6.45 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `prebuilts/remoteexecution-client` | 2 | 1.2 GB | 2.27 GiB | `orphaned: shared or overlaid from its clean source checkout` | Yes |
| `external/dexter` | 2 | 667.0 MB | 1.24 GiB | `orphaned: quick to check out (about 1342 ms)` | Yes |
| **Total Orphaned (9 projects)** | **18** | **~142.4 GB total** | **132.39 GiB (`142.15 GB`)** | Deleted by `pool clear --orphaned` | **Exact 9/9 match** |

#### Foreground `pool clear --orphaned` Timing and Disk Reclaimed

| Metric | Before `pool clear --orphaned` | After `pool clear --orphaned` | Delta (Reclaimed) |
| :--- | ---: | ---: | ---: |
| Wall clock / `history.jsonl` `ms` | — | **19.77 s** (`19,766 ms`) | `0.53 s` user, `14.89 s` sys (`78%` CPU) |
| `du -sx .repo/repo-worktrees/.pool` | `179,874,336 KB` (`171.54 GiB` / `184.19 GB`) | `41,051,472 KB` (`39.15 GiB` / `42.04 GB`) | **`-138,822,864 KB` (`-132.39 GiB` / `-142.15 GB`)** |
| `df -B1 /` `Used` bytes | `6,639,612,538,880 B` (`86%`) | `6,497,481,240,576 B` (`84%`) | **`-142,131,298,304 B` (`-132.37 GiB` / `-142.13 GB`)** |
| `pool list` summary | `depth 2 of 2`, `18 orphaned spare(s), about 142.4 GB` | `depth 2 of 2`, `0 orphaned spares` | 18 orphaned directories removed |

---

### P2. Background Refill, Bursts, and Spaced Adds

| Step / Session | Operation | Total (`ms`) | `snapshot` (`ms`) | `projects` (`ms`) | `jsync` / `mwrite` (`ms`) | Pool Hits | `queue_ms` (`p50` / `p90` / `max`) | Slowest 3 Projects (`ms`, `q_ms`, strategy) | Background `pool maintain` Result |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | :--- | :--- |
| **P2.1 (`R5-P2-A`)** | `add studio-wt-r5-A` | **4,921** | 2,130 | 2,322 | 174 / 4 | **9 / 9** | `0 / 231 / 566` | `sdk/linux: 2106 (q=0, pool)`, `tools/idea: 2077 (q=0, pool)`, `dexter: 1748 (q=566, checkout)` | Ran **172,086 ms** (`plan: 2,400`, `fill: 169,683`), filled **9 spares** (`depth 2 of 2`) |
| **P2.2 (`R5-P2-burst-B`)** | `add studio-wt-r5-B` (burst #1) | **5,344** | 2,670 | 2,156 | 184 / 5 | **9 / 9** | `0 / 189 / 464` | `sdk/linux: 2145 (q=0, pool)`, `tools/idea: 1920 (q=0, pool)`, `dexter: 1609 (q=464, checkout)` | Ran **296,251 ms** (`plan: 3,395`, `fill: 292,850`), filled **18 spares** (both B & C sets, `depth 2 of 2`) |
| **P2.2 (`R5-P2-burst-C`)** | `add studio-wt-r5-C` (burst #2) | **6,540** | 2,893 | 3,139 | 515 / 8 | **9 / 9** | `0 / 390 / 1,536` | `tools/idea: 2388 (q=0, pool)`, `sdk/linux: 2339 (q=0, pool)`, `dexter: 1581 (q=1536, checkout)` | `0 ms` (`skipped: "another maintenance run is active"`) |
| **P2.2 (`R5-P2-burst-D`)** | `add studio-wt-r5-D` (burst #3) | **49,799** | 2,530 | 41,917 | 3,948 / 569 | **0 / 9** | `0 / 255 / 23,274` | `sdk/linux: 40143 (q=0, checkout)`, `tools/idea: 23974 (q=0, checkout)`, `gpu-profiler: 23262 (q=0, checkout)` | `0 ms` (`skipped: "another maintenance run is active"`) |
| **P2.3 (Remove B, C, D)** | `remove` B, C, D | **34,456 / 33,012 / 36,020** | — | — | — | `ret=0` each | — | (`worktree-remove`: 31.0 s / 29.5 s / 32.3 s; pool already at depth 2) | Each spawned `pool maintain`: `1,145 / 1,194 / 1,046 ms` (`fill: 0`) |
| **P2.3 (`R5-P2-spaced-S1`)** | `add studio-wt-r5-S1` (t=0s) | **7,590** | 2,409 | 4,775 | 241 / 6 | **9 / 9** | `0 / 217 / 1,048` | `tools/idea: 4740 (q=0, pool)`, `sdk/linux: 2006 (q=0, pool)`, `dexter: 1648 (q=1048, checkout)` | Ran **114,977 ms** (`fill: 112,264`), filled **9 spares** before S2 started! |
| **P2.3 (`R5-P2-spaced-S2`)** | `add studio-wt-r5-S2` (t=120s) | **8,552** | 2,486 | 5,512 | 1,594 / 151 | **9 / 9** | `0 / 869 / 1,311` | `tools/idea: 5498 (q=0, pool)`, `sdk/linux: 2675 (q=0, pool)`, `dexter: 1725 (q=1311, checkout)` | Ran **113,739 ms** (`fill: 111,279`), filled **9 spares** before S3 started! |
| **P2.3 (`R5-P2-spaced-S3`)** | `add studio-wt-r5-S3` (t=240s) | **7,391** | 3,826 | 3,115 | 523 / 53 | **9 / 9** | `0 / 248 / 975` | `tools/idea: 2943 (q=0, pool)`, `sdk/linux: 2066 (q=0, pool)`, `dexter: 1744 (q=975, checkout)` | Ran **112,798 ms** (`fill: 110,474`), filled **9 spares** (`depth 2 of 2`) |
| **P2.4 (Remove A, S1–S3)** | `remove` A, S1, S2, S3 | **37,984 / 31,013 / 30,342 / 30,817** | — | — | — | `ret=0` each | — | (`worktree-remove`: 27.0–31.2 s; `prune`: 7–18 ms, `pruned_git_dirs: 19`) | Each spawned `pool maintain`: `955–1,073 ms` (`fill: 0`, `orphans: 0`) |

---

### P3. Cost of Background Refill to a Concurrent Bazel Build

In each run, a fresh `studio-main` tree was created (consuming 1 spare set from `.pool` at depth 2) and `bazel build //tools/base/common:tools.common` was launched immediately inside the new tree. Between runs, `remove` returned the 9 spares (when `NO_POOL_MAINTENANCE=1`) or waited for `pool maintain` to finish so every run started from `depth 2 of 2`.

| Run (`GIT_REPO_WORKTREE_SESSION`) | Maintenance Mode | `add` Wall (`s`) | `bazel build //tools/base/common:tools.common` Wall (`s`) | Concurrent `pool maintain` Duration (`s`) | Peak `Dirty` during Build (`MB`) | Peak Disk `%util` (`iostat -x 5`) |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: |
| `R5-P3-maint-1` | **ON** (`nice -n 19`, `ionice -c 3`) | 4.78 s | **29.46 s** | 102.07 s (filled 9 spares) | ~1,850 MB | 26.9% |
| `R5-P3-maint-2` | **ON** (`nice -n 19`, `ionice -c 3`) | 4.36 s | **21.06 s** | 103.41 s (filled 9 spares) | ~1,920 MB | 31.4% |
| **Mean (Maintenance ON)** | — | **4.57 s** | **25.26 s** | **102.74 s** | — | — |
| `R5-P3-nomaint-1` | **OFF** (`NO_POOL_MAINTENANCE=1`) | 4.48 s | **29.39 s** | — (disabled) | ~110 MB | 4.2% |
| `R5-P3-nomaint-2` | **OFF** (`NO_POOL_MAINTENANCE=1`) | 4.03 s | **30.28 s** | — (disabled) | ~95 MB | 3.8% |
| **Mean (Maintenance OFF)** | — | **4.25 s** | **29.84 s** | — | — | — |

**Result:** Idle-priority background refill on `studio-main` (`~20.5 GB` written sequentially project-by-project at `ionice -c 3` / `nice -n 19`) caused **no measurable slowdown** to the concurrent Bazel build (mean **25.26 s** with maintenance vs. **29.84 s** without).

---

### P4. `studio-main` Disk Footprint

1. **`.pool` size after P2 (`depth 2 of 2`, 0 orphans):**
   - `du -sx .repo/repo-worktrees/.pool`: **`41,051,040 KB` (`39.15 GiB` / `42.04 GB`)** (~21.0 GB per spare set of 9 projects).
   - Per-project breakdown (`du -sx` across 2 spares each, from [`data/studio-main/P4-du-pool-per-project-after-P2.txt`](data/studio-main/P4-du-pool-per-project-after-P2.txt)):
     - `prebuilts/studio/sdk/linux`: `21,789,748 KB` (`20.78 GiB`)
     - `tools/adt/idea`: `4,975,516 KB` (`4.75 GiB`)
     - `tools/idea`: `4,202,656 KB` (`4.01 GiB`)
     - `tools/gpu-profiler/idea`: `4,099,216 KB` (`3.91 GiB`)
     - `tools/base`: `1,986,196 KB` (`1.89 GiB`)
     - `external/jetbrains/kotlin`: `1,441,264 KB` (`1.37 GiB`)
     - `tools/vendor/google`: `1,033,576 KB` (`1.01 GiB`)
     - `external/intellij-plugins`: `931,264 KB` (`0.89 GiB`)
     - `tools/vendor/intellij/cidr`: `591,488 KB` (`0.56 GiB`)
2. **Disk delta (`df -B1 /` `Used` bytes) of a pool-hit `add` vs. a pool-miss `add`:**
   - **Immediate delta on pool-hit `add` (before refill):**
     - Tree A (`R5-P2-A`, 9/9 hits): `+1,532,043,264 B` (**`+1.53 GB` / `+1.43 GiB`**)
     - Tree B (`R5-P2-burst-B`, 9/9 hits): `+1,522,515,968 B` (**`+1.52 GB` / `+1.42 GiB`**)
     - Tree C (`R5-P2-burst-C`, 9/9 hits): `+1,700,626,432 B` (**`+1.70 GB` / `+1.58 GiB`**)
     - Because the 9 pooled worktrees (`~21.0 GB`) are moved from `.pool` into the new tree via `rename(2)`, a pool-hit `add` only allocates blocks for the 28 small unpooled worktrees (`~1.5 GB`).
   - **Steady-state delta after background refill completes (or on a pool-miss `add`):**
     - Tree A after `pool maintain` refilled 9 spares: `+23,057,895,424 B` (**`+23.06 GB` / `+21.47 GiB`**) net increase (`~1.53 GB` small checkouts + `~21.53 GB` refilled spares).
     - Tree D (`R5-P2-burst-D`, 0/9 hits — pool miss): `+30,856,634,368 B` (**`+30.86 GB` / `+28.74 GiB`**) during its 49.8 s run (`~22.0 GB` for Tree D's own full checkout of all 37 worktrees + `~8.8 GB` written concurrently by B's background `pool maintain`).

---

## 4. AOSP (`~/projects/main`): Stalls, Parallelism Matrix, Pool Maintenance, and `remove`

### S1 & S2. Parallelism Matrix (`-j 8`, `-j 16`, `-j 32`) with Thread/Descendant Sampler

Before S2, `git repo-worktree pool clear` emptied `.pool` (`S2-init-pool-clear.json`), and all S2 runs executed with `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` and `remove --no-pool` so every run was a clean cold checkout of all 971 worktrees + 113 overlays (1,084 projects total).

| Run (`Session`) | `-j` | Pre-run `Dirty` / `load1` | Total (`ms`) | `snapshot` (`ms`) | `manifest` / `git-dirs` / `est` / `pre` (`ms`) | `projects` (`ms`) | `jsync` (`ms`) | `mwrite` (`ms`) | Proj `ms` (`p50` / `p90` / `max`) | `queue_ms` (`p50` / `p90` / `max`) | `%CPU` | Mean / Max `Dirty` (`MB`) | Top Summed Thread Wait Channels (`stat:wchan:comm=samples`) |
| :--- | ---: | :--- | ---: | ---: | :--- | ---: | ---: | ---: | :--- | :--- | ---: | :--- | :--- |
| **`R5-S2-r1-j8`** | **8** | `3 MB` / `11.53` | **36,816** | 10,147 | `8619 / 417 / 1098 / 1` | 25,030 | 13,322 | 532 | `61 / 259 / 8406` | `19539 / 24504 / 24933` | 901% | `12,363 / 24,084` | `Sl+:poll_schedule_timeout:git-repo-worktr=95`; `Z+:-:git=84`; `R+:-:git=34`; `Sl+:futex_do_wait:git-repo-worktr=27`; `S+:do_wait:git=19` |
| **`R5-S2-r2-j16`** | **16** | `821 MB` / `37.93` | **24,818** | 5,450 | `4502 / 223 / 708 / 2` | 17,576 | 9,767 | 1,083 | `61 / 342 / 11181` | `11329 / 14215 / 16037` | 1497% | `13,774 / 24,481` | `Sl+:poll_schedule_timeout:git-repo-worktr=87`; `Z+:-:git=77`; `Sl+:futex_do_wait:git-repo-worktr=61`; `R+:-:git=25`; `S+:do_wait:git=20` |
| **`R5-S2-r3-j32`** | **32** | `946 MB` / `37.83` | **29,860** | 6,151 | `4990 / 466 / 654 / 6` | 21,953 | 9,655 | 1,840 | `126 / 774 / 16194` | `11793 / 13979 / 20203` | 1328% | `13,294 / 21,101` | `Sl+:futex_do_wait:git-repo-worktr=175`; `Sl+:poll_schedule_timeout:git-repo-worktr=115`; `Z+:-:git=79`; `S+:do_wait:git=22`; `D+:jbd2_log_wait_commit:git=18` |
| **`R5-S2-r4-j16`** | **16** | `4 MB` / `52.44`* | **26,137** | 5,540 | `4467 / 247 / 807 / 1` | 18,757 | 10,361 | 978 | `80 / 395 / 12283` | `12456 / 16231 / 17159` | 1411% | `13,754 / 23,246` | `Sl+:poll_schedule_timeout:git-repo-worktr=120`; `Z+:-:git=92`; `Sl+:futex_do_wait:git-repo-worktr=42`; `S+:do_wait:git=24`; `R+:-:git=20` |
| **`R5-S2-r5-j32`** | **32** | `5 MB` / `51.39`* | **31,395** | 5,377 | `4590 / 179 / 592 / 2` | 23,949 | 12,117 | 1,521 | `153 / 622 / 16756` | `11848 / 15099 / 21914` | 1229% | `15,046 / 23,094` | `Sl+:futex_do_wait:git-repo-worktr=229`; `Z+:-:git=129`; `Sl+:poll_schedule_timeout:git-repo-worktr=128`; `S+:do_wait:git=34`; `D+:jbd2_log_wait_for_space:git=23` |
| **`R5-S2-r6-j8`** | **8** | `2148 MB` / `50.74`* | **34,154** | 6,316 | `4717 / 441 / 1141 / 1` | 26,222 | 15,294 | 1,219 | `59 / 256 / 8088` | `20989 / 25627 / 26125` | 1026% | `13,911 / 23,756` | `Sl+:poll_schedule_timeout:git-repo-worktr=82`; `Z+:-:git=66`; `Sl+:futex_do_wait:git-repo-worktr=27`; `R+:-:git=23`; `S+:do_wait:git=17` |
| **`R5-S2-r7-j32`** | **32** | `1 MB` / `54.75`* | **29,576** | 5,762 | `5008 / 146 / 589 / 2` | 21,926 | 9,116 | 1,073 | `134 / 701 / 14888` | `11153 / 13826 / 20089` | 1297% | `13,753 / 22,214` | `Sl+:futex_do_wait:git-repo-worktr=192`; `Sl+:poll_schedule_timeout:git-repo-worktr=136`; `Z+:-:git=78`; `R+:-:git=44`; `S+:do_wait:git=25` |
| **`R5-S2-r8-j8`** | **8** | `154 MB` / `50.85`* | **42,008** | 6,056 | `4508 / 433 / 1099 / 1` | 34,039 | 23,540 | 2,650 | `57 / 293 / 9708` | `27917 / 32737 / 33820` | 820% | `16,007 / 25,077` | `Sl+:poll_schedule_timeout:git-repo-worktr=77`; `Z+:-:git=73`; `Sl+:futex_do_wait:git-repo-worktr=65`; `R+:-:git=29`; `S+:do_wait:git=17` |
| **`R5-S2-r9-j16`** | **16** | `4 MB` / `49.89`* | **43,387** | 7,235 | `5953 / 275 / 986 / 3` | 33,867 | 22,581 | 5,445 | `110 / 609 / 13723` | `16572 / 32445 / 33745` | 1019% | `16,279 / 24,955` | `Sl+:futex_do_wait:git-repo-worktr=117`; `Sl+:poll_schedule_timeout:git-repo-worktr=107`; `Z+:-:git=75`; `D+:jbd2_log_wait_for_space:git=36`; `S+:do_wait:git=23` |

*\*Note on preconditions (`r4–r9`):* On this workstation, ambient background tasks (3 Cuttlefish `crosvm` VMs + Jetski hub `git status` polling across 21 `studio-main` trees) keep the 1-minute load average around `48–55` after any I/O activity. The precondition check waited up to 90 s between runs; `Dirty < 1 GB` was satisfied before 8 of the 9 runs (all except `r6`, which started at `2,148 MB`), while `load1` settled to `49.9–54.8` on `r4–r9`.

#### Summary by `-j` Parallelism Level

| Parallelism (`-j`) | Run 1 (`s`) | Run 2 (`s`) | Run 3 (`s`) | Median Total (`s`) | Mean Total (`s`) | Median `projects` (`s`) | Median `jsync + mwrite` (`s`) | Median Idle Worker `futex_do_wait` (thread-s) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **`-j 8`** (`r1`, `r6`, `r8`) | 36.82 | 34.15 | 42.01 | **36.82 s** | 37.66 s | 26.22 s | 16.51 s | 27 |
| **`-j 16`** (`r2`, `r4`, `r9`) | **24.82** | **26.14** | 43.39 | **26.14 s** | 31.45 s | **18.76 s** | 11.34 s | 61 |
| **`-j 32`** (`r3`, `r5`, `r7`) | 29.86 | 31.40 | **29.58** | **29.86 s** | **30.28 s** | 21.95 s | **11.49 s** | 192 |

---

### S3. Quiet-Machine Control

Skipped per §4 S3 rules (*"Do not stop anything the user has not approved"*): the workstation was actively running user Cuttlefish VMs (`crosvm`/`run_cvd`) and the Jetski hub service.

---

### S4. Pool on AOSP with Background Maintenance

| Run (`Session`) | Scenario | Total (`ms`) | `snapshot` (`ms`) | `projects` (`ms`) | `jsync` / `mwrite` (`ms`) | Pool Hits | `queue_ms` (`p50` / `p90` / `max`) | `%CPU` | Mean / Max `Dirty` (`MB`) | Top Summed Thread Wait Channels (`stat:wchan:comm=samples`) |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| **`R5-S4-add1`** | Cold `add` (`main-wt-r5-S4a`), spawns `pool maintain` | **61,353** | 5,301 | 53,126 | `30,972 / 18,642` (**49.6 s = 93%!**) | **0 / 14** | `38764 / 48093 / 52592` | 717% | `21,231 / 28,758` | `Sl+:futex_do_wait:git-repo-worktr=538`; `Sl+:poll_schedule_timeout:git-repo-worktr=183`; `Z+:-:git=72`; `R+:-:git=33` |
| **`R5-S4-add2-during-maintain`** | Second `add` (`main-wt-r5-S4b`) started 2 s into `pool maintain` | **97,709** | 7,514 | 87,782 | `44,646 / 11,092` (**55.7 s = 63%!**) | **0 / 14** | `64914 / 70711 / 85670` | 520% | **`31,058 / 36,372`** | `Z+:-:git=2439`; **`D+:balance_dirty_pages:git=925`**; `Sl+:poll_schedule_timeout:git-repo-worktr=819`; `S+:do_wait:git=645`; `S+:poll_schedule_timeout:git=497`; `Sl+:futex_do_wait:git-repo-worktr=342` |
| **`R5-S4-add1` (`pool maintain`)** | Background refill of 28 spares (2 × 14 projects, ~36.8 GB) | **232,608** | — | `fill: 232,561` (`plan: 43`) | — | Filled **28**, `deferred: []` | — | — | — | Round 1 (14 spares): `167.3 s` (`llvm-project`: 86.1 s, `cronet`: 28.0 s, `frameworks/base`: 16.8 s, `cts`: 10.2 s); Round 2 (14 spares): `65.3 s` |
| **`R5-S4-add3-warm`** | Warm `add` (`main-wt-r5-S4c`) after `pool maintain` finished (`depth 2 of 2`) | **20,497** | 6,489 | 12,399 | `7,878 / 235` | **14 / 14** | `9551 / 11955 / 12187` | **1657%** | `8,181 / 18,360` | `Sl+:poll_schedule_timeout:git-repo-worktr=131`; `SNl:futex_do_wait:git-repo-worktr=96`; `Sl+:futex_do_wait:git-repo-worktr=33`; `S+:do_wait:git=6` |
| **`R5-S4-add3-warm` (`pool maintain`)** | Background refill of 14 consumed spares | **85,211** | — | `fill: 85,153` (`plan: 54`, `reap: 2`) | — | Filled **14**, `deferred: []` | — | — | — | Restored `.pool` to `depth 2 of 2` in **85.21 s** |

---

### S5. `remove` on AOSP (Default vs. `--no-pool`, and `prune` Comparison with Round 4)

| Run (`Session`) | Mode | Total (`ms`) | `inspect` (`ms`) | `unmount` (`ms`) | `pool-return` (`ms`) | `worktree-remove` (`ms`) | `prune` (`ms`) | `delete` (`ms`) | `returned_to_pool` | `pruned_git_dirs` |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **`R5-S5-rm-default-1`** (`main-wt-r5-S4a`) | Default (`remove`) | **25,537** | 17,142 | 578 | **743** | **6,604** | **32** | 275 | **14** | **113** |
| **`R5-S5-rm-default-2`** (`main-wt-r5-S4b`) | Default (`remove`) | **14,414** | 6,328 | 562 | **738** | **6,590** | **27** | 74 | **14** | **113** |
| **`R5-S5-rm-nopool-1`** (`main-wt-r5-S4c`) | `--no-pool` | **24,309** | 6,879 | 505 | **0** | **16,346** | **33** | 86 | **0** | **113** |
| **`R5-S2-r1..r9-rm-nopool`** (9 runs) | `--no-pool` (median / range) | **30,648** (`25,496–33,353`) | **9,255** (`7,578–11,346`) | **576** (`509–658`) | **0** | **19,294** (`16,125–21,583`) | **28** (`25–47`) | **85** (`70–99`) | **0** | **113** |
| *Round 4 AOSP `remove` (for comparison)* | *Default (7 runs in R4)* | *`31,264–73,888`* | *`7,300–13,600`* | *`500–800`* | *`2,100–25,500` (checkouts)* | *`8,800–24,100`* | ***`200–31,800`*** | *`70–150`* | *14* | *(all 1,084)* |

---

## 5. Answers to §5 Questions

### Q1. Does `pool list` classify `studio-main`'s spares correctly, and how much disk did `pool clear --orphaned` reclaim?

- **Yes, with one noteworthy source-state nuance:**
  - `pool list` identified **all 9 dead-spare projects (18 spares, estimated at `about 142.4 GB`)** from Round 4: `external/dexter` (`orphaned: quick to check out (about 1342 ms)`, below the 1,500 ms threshold) and the 8 clean `clone-depth="1"` prebuilts (`prebuilts/tools`, `prebuilts/studio/intellij-sdk`, `prebuilts/studio/jdk`, `tools/external/gradle`, `prebuilts/studio/sdk/remote`, `prebuilts/studio/buildbenchmarks`, `prebuilts/clang/host/linux-x86`, `prebuilts/remoteexecution-client`, all marked `orphaned: shared or overlaid from its clean source checkout`).
  - Why is `prebuilts/studio/sdk/linux` (2 spares, `about 11.3 GB each`) kept as a pooled project even though its manifest entry also has `clone-depth="1"`? Because `~/projects/studio-main/prebuilts/studio/sdk/linux` in the initiating source checkout has untracked files (`source_is_clean()` returns `false`), so `add` cannot overlay it and instead checks it out as a worktree from `.pool`. Thus `pool list` accurately reflects what `add` actually consumes. (If the user cleans the untracked files in `~/projects/studio-main/prebuilts/studio/sdk/linux`, it will become an overlay project and its 2 spares in `.pool` will become orphaned, saving another `~20.8 GiB`.)
- **Disk reclaimed by `pool clear --orphaned`:**
  - **`138,822,864 KB` (`132.39 GiB` / `142.15 GB`)** measured by `du -sx .repo/repo-worktrees/.pool` (from `179,874,336 KB` down to `41,051,472 KB`), and **`142,131,298,304` bytes (`132.37 GiB` / `142.13 GB`)** measured by `df -B1 /`, in **19.77 s**.

### Q2. With background refill, how many adds of a burst hit the pool, and how soon after an `add` is the pool back to depth 2?

- **Burst hit count:** In an immediate back-to-back burst starting from `depth 2 of 2`, **the first 2 adds (`B` in 5.34 s and `C` in 6.54 s) hit the pool (9/9 hits each)**, and the 3rd add (`D` in 49.80 s) misses (0/9 hits).
- **Refill time back to `depth 2 of 2`:**
  - On `studio-main` (9 pooled projects, `~20.5 GB` per set): refilling 1 spare set takes **102.1–172.1 s** (`1.7–2.9 min`; specifically **172.1 s** after `A`, **115.0 s / 113.7 s / 112.8 s** during the spaced runs `S1–S3`, and **102.1 s / 103.4 s** during the P3 Bazel runs). Refilling 2 full spare sets (18 spares, `~41 GB`) after the `B+C` burst took **296.25 s** (`4.94 min`). Consequently, adds spaced **2 minutes apart** (`S1`, `S2`, `S3`) achieved **100% pool hits (9/9 on all three)**.
  - On `main` / AOSP (14 pooled projects, `~18.4 GB` per set): refilling 1 spare set after `S4-add3-warm` took **85.21 s** (`1.42 min`), and filling 2 spare sets from an empty pool after `S4-add1` (while `S4-add2` also ran concurrently) took **232.61 s** (`3.88 min`), with **0 deferred spares** (`"deferred": []`).

### Q3. Does idle-priority refill slow a build or a concurrent `add` measurably?

- **Concurrent Bazel build (`studio-main` P3): No.** Running `bazel build //tools/base/common:tools.common` while `pool maintain` refilled 9 spares in the background averaged **25.26 s** (`29.46 s`, `21.06 s`) vs. **29.84 s** (`29.39 s`, `30.28 s`) with maintenance disabled.
- **Concurrent cold `add` on AOSP (`main` S4): Yes, significantly (`3.2×` slower).** Starting `S4-add2-during-maintain` 2 seconds after `S4-add1` spawned `pool maintain` took **97.71 s** (vs. **29.86 s** median for `-j 32` in S2). Why didn't `ionice -c 3` prevent this?
  1. `ionice -c 3` only prioritizes block-device I/O submission in the CFQ/BFQ scheduler, **not** the global Linux kernel dirty-page cache (`vm.dirty_ratio` / `vm.dirty_background_ratio`). `pool maintain`'s `git worktree add` processes buffered ~18 GB of dirty pages in RAM alongside `S4-add1` and `S4-add2`, pushing `/proc/meminfo` `Dirty:` to **36.37 GB** (`mean 31.06 GB`).
  2. Once global dirty pages crossed the kernel throttling threshold, `S4-add2`'s foreground `git` processes were throttled directly in `balance_dirty_pages` (**925 thread-seconds** in `D+:balance_dirty_pages:git`), and `S4-add2`'s foreground `fsync()` calls in `projects-journal-sync` (`44.65 s`) and `projects-meta-write` (`11.09 s`) had to flush behind the congested `ext4` journal.
  3. Furthermore, `S4-add2` got **0 pool hits** even though `pool maintain` was running, because `pool maintain` fills projects in descending order of size starting with `external/opencl/llvm-project` (which took `86.1 s`), and `S4-add2` also pops `ready` in descending order of size at `t=0` before `pool maintain` had finished its first spare.

### Q4. On AOSP, which wait channels dominate the slow periods? Is it dirty-page throttling, the ext4 journal, lock contention, or something else?

The S1 thread-and-descendant sampler (`ps -L -o stat=,wchan:32=,comm=`) and the new `projects-journal-sync` / `projects-meta-write` phases reveal **two distinct regimes**:

1. **In normal 25–43 s cold adds (S2 `r1–r9`, `Dirty` 12–16 GB mean): Scheduler main-thread `fsync()` serialization (`jbd2` journal commits) idling the worker pool.**
   - Look at the top wait channels in `S2-r3-j32` (`29.86 s`), `S2-r5-j32` (`31.40 s`), and `S4-add1` (`61.35 s`):
     - `Sl+:futex_do_wait:git-repo-worktr` (**175 to 538 thread-seconds**): the tool's own worker threads sleeping on the channel/condvar waiting for work!
     - `Sl+:poll_schedule_timeout.constprop.:git-repo-worktr` (**87 to 183 thread-seconds**): worker threads waiting for `git worktree add` child stdout/stderr pipes to close.
     - `Z+:-:git` (**66 to 129 thread-seconds**) and `S+:do_wait:git` (**17 to 34 thread-seconds**): `git checkout--worker` subprocesses that have already finished and exited (`Z+`), waiting for parent `git` to `waitpid()` them once sibling `checkout--worker` threads complete.
     - `D+:jbd2_log_wait_for_space:git` (**23–36 thread-seconds** in `r5`, `r9`) and `D+:jbd2_log_wait_commit:git` (**18 thread-seconds** in `r3`): `ext4` journal transaction commits triggered by the scheduler's `sync_all()` (`fsync`) calls and `git`'s index writes.
   - Zero `rwsem_down_*` or `mutex_lock*` contention was observed (`< 2` samples across all runs).

2. **When cumulative writes exceed ~25–30 GB of dirty page cache (`S4-add2-during-maintain`, `97.71 s`, `Dirty` mean 31.1 GB / max 36.4 GB): Kernel `balance_dirty_pages` writeback throttling.**
   - In `S4-add2`, the wait-channel histogram is dominated by:
     - `Z+:-:git = 2,439` thread-seconds (completed `git checkout--worker` children waiting on sibling workers in the same `git` process)
     - **`D+:balance_dirty_pages:git = 925` thread-seconds** (active `git` / `checkout--worker` threads blocked in uninterruptible sleep inside the kernel's dirty-page throttling loop!)
     - `Sl+:poll_schedule_timeout.constprop.:git-repo-worktr = 819` thread-seconds (tool worker threads waiting on those throttled `git` processes)
     - `S+:do_wait:git = 645` thread-seconds (parent `git` processes waiting on `checkout--worker` children)
     - `Sl+:futex_do_wait:git-repo-worktr = 342` thread-seconds (tool worker threads starved while the dispatcher thread spent **`44.65 s` in `projects-journal-sync` + `11.09 s` in `projects-meta-write`**).

### Q5. Does lower `-j` make AOSP adds faster on this machine? Which value is best, and is the answer stable across runs?

- **Yes, `-j 16` and `-j 32` both beat `-j 8`, with a trade-off between peak speed (`-j 16`) and stability (`-j 32`):**
  - **`-j 16`** was fastest in the first two rotations (**24.82 s** in `r2`, **26.14 s** in `r4`, median **26.14 s**), achieving the highest CPU utilization (`1497%` and `1411%`) and lowest `projects` time (`17.58 s` and `18.76 s`), before rising to **43.39 s** in `r9` when `ext4` journal pressure (`D+:jbd2_log_wait_for_space:git=36`, `jsync=22.58 s`, `mwrite=5.45 s`) slowed the final rotation.
  - **`-j 32`** (the default) was the **most stable across all 3 rotations**: **29.86 s** (`r3`), **31.40 s** (`r5`), and **29.58 s** (`r7`) — a spread of only **1.82 s** (mean **30.28 s**, median **29.86 s**), though workers spent more time parked in `futex_do_wait` (`175–229` thread-seconds vs. `42–117` at `-j 16`).
  - **`-j 8`** was consistently slowest in the first two rotations (**36.82 s** in `r1`, **34.15 s** in `r6`, **42.01 s** in `r8`; median **36.82 s**, mean **37.66 s**).
- **Conclusion:** **`-j 16`** is the sweet spot under normal dirty-page conditions (~15% faster than `-j 32` with a third of the idle thread wakeups), while **`-j 32`** has the lowest variance.

### Q6. How much of `snapshot` is `repo manifest -r`, git-dir discovery, size estimation, and preflight? Which part varies?

| Client | Total `snapshot` (`ms`) | `snapshot-repo-manifest` (`ms`) | `snapshot-git-dirs` (`ms`) | `snapshot-estimates` (`ms`) | `snapshot-preflight` (`ms`) |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **`studio-main` (11 adds: P2 + P3)** | **1,720 – 3,826** (median **2,409**) | **411 – 2,163** (median **635**, `26%`) | **9 – 71** (median **11**, `<1%`) | **1,286 – 1,824** (median **1,603**, `67%`) | **1 – 2** (`<0.1%`) |
| **`main` / AOSP (12 adds: S2 + S4)** | **5,301 – 10,147** (median **5,909**) | **4,467 – 8,619** (median **4,854**, **`82%`**) | **146 – 466** (median **201**, `3%`) | **589 – 1,141** (median **682**, `12%`) | **1 – 6** (`<0.1%`) |

- **On `main` (AOSP):** `snapshot-repo-manifest` (`repo manifest -r` walking 1,084 `.git` directories in Python) is **80–85% of `snapshot`** (median **4.85 s**, varying up to **8.62 s** on `r1`), while `snapshot-estimates` is only **0.59–1.14 s** and `snapshot-git-dirs` is **0.15–0.47 s**. (In Round 4, `snapshot` spiked to 73–93 s only when `remove` left >20 GB of dirty pages throttling `repo manifest -r` on `ext4` writeback.)
- **On `studio-main`:** `snapshot-estimates` is roughly constant at **1.29–1.82 s** (~67% of a typical 2.4 s `snapshot`), while `snapshot-repo-manifest` is typically **0.41–0.84 s** but is the part that varies when the system is busy (spiking to **2.16 s** in `R5-P2-spaced-S3`).

### Q7. How much time does the scheduler now spend in `projects-journal-sync` and `projects-meta-write`, and do `queue_ms` values show projects waiting while workers sat idle?

- **Yes — `projects-journal-sync` + `projects-meta-write` is now the #1 bottleneck in AOSP `add`!**
  - Look at the numbers across all 12 AOSP `add` runs:
    - Fast S2 runs (`24.8–31.4 s` total, `17.6–23.9 s` `projects`): `projects-journal-sync` is **9.12–12.12 s** and `projects-meta-write` is **0.98–1.84 s** — together **10.2 to 13.6 s**, or **53% to 62% of the entire `projects` phase**!
    - Slower S2 runs (`r8` at `42.0 s`, `r9` at `43.4 s`): `jsync + mwrite` is **26.19 s** (`77%` of `projects`) and **28.03 s** (`83%` of `projects`).
    - `S4-add1` (`61.35 s` total, `53.13 s` `projects`): `jsync` (**30.97 s**) + `mwrite` (**18.64 s**) = **49.61 s** (**93.4% of `projects`**)!
    - Even on warm-pool `S4-add3-warm` (`20.50 s` total, `12.40 s` `projects`): `jsync + mwrite` is **8.11 s** (**65.4% of `projects`**).
  - **Why does this happen after `a9c1499`?**
    In `src/tree.rs:1032–1053` (`run_project_phase`), the main thread loops:
    1. Waits for 1 completion (`rx.recv()`), drains any ready completions (`rx.try_recv()`).
    2. Calls `dispatcher.fill()` to fill up to `jobs` (8, 16, or 32) worker slots.
    3. Calls `append_journal_batch()` — which opens `journal.jsonl`, writes the batch, and calls **`file.sync_all()` (`fsync`) synchronously on the main scheduler thread**.
    4. If `last_meta_write.elapsed() >= 1s`, calls `write_meta()` — which writes `meta.json.tmp`, calls **`file.sync_all()` (`fsync`)**, and renames it **synchronously on the main scheduler thread**.
    When `ext4` is flushing dirty pages from 971 worktree checkouts, a single `file.sync_all()` call on the same filesystem frequently blocks for **100–2,000 ms** waiting on `jbd2` journal commits. While the main scheduler thread is blocked inside step 3 or step 4, the `jobs` worker threads finish their fast checkouts (median `p_ms` is only **57–157 ms**!) and **cannot be refilled** until `sync_all()` returns! They sit idle in `Sl+:futex_do_wait:git-repo-worktr` (up to **538 thread-seconds** in `S4-add1`).
  - **How this appears in `queue_ms`:**
    In `src/tree.rs:968`, `ready_at` is initialized to `phase_started` (`t=0`) for all 1,084 top-level projects. Thus a project's `queue_ms` is the elapsed time from the start of `projects` until a worker pops it. When the main thread stalls for 30–50 s in `jsync + mwrite`, the queue drains only in bursts between `fsync` stalls, pushing `queue_ms` `p50` from **11.2 s** (in fast runs) up to **38.8 s** (in `S4-add1`) and **64.9 s** (in `S4-add2`).
  - **High-leverage fix for the next commit:** Either (a) flush `journal.jsonl` to the OS with `Write::flush()` per batch and call `file.sync_all()` + `write_meta()` **once** at the end of `run_project_phase` (or on a dedicated background writer thread connected via an `mpsc` channel), so the scheduler loop never blocks on `fsync()` while dispatching the 1,084 projects.

### Q8. Anything that looks wrong or surprising in the new history fields

1. **`remove` on a full pool still spends 1.0–2.3 s in `phases["pool-return"]` and spawns a 1.0 s no-op `pool maintain`:**
   - On `studio-main`, when `.pool` is already at capacity (`depth 2 of 2`) and `remove` returns `returned_to_pool: 0` (e.g., `R5-P2-rm-A`, `R5-P2-rm-S1..S3`, `R5-P3-maint-1-rm`, `R5-P3-maint-2-rm`), `phases["pool-return"]` still takes **939–2,307 ms** (running `source_is_clean` / lock checks before deciding not to return any spares), and `remove` still spawns a detached `pool maintain` that spends **955–1,194 ms** in `phases.plan` only to record `filled: [], trimmed: [], reaped: []`. Skipping `return_to_pool` and `spawn_pool_maintain` when all pooled projects are already at `pool size` (and there are no orphans) would save ~2 s per `remove` and avoid noisy no-op `pool maintain` entries in `history.jsonl`.
2. **Order of project filling in `pool maintain` vs. `add`:**
   - Both `add` and `pool maintain` sort pooled projects in **descending** order of estimated checkout duration (`external/opencl/llvm-project` first on AOSP; `prebuilts/studio/sdk/linux` first on `studio-main`). Because the largest project takes 40–86 s to check out in `pool maintain`, an `add` that starts 2–30 s after a previous `add` pops its largest projects at `t=0` before `pool maintain` has finished even its first spare, resulting in 0 pool hits for the overlapping projects. If `pool maintain` filled one spare of the **fastest-to-refill** pooled projects first (or if `add` checked `.pool` again right when dispatching a project rather than only at `t=0`), overlapping adds would get partial pool hits much sooner.
3. **`queue_ms` for top-level projects includes normal work-conserving queue wait:**
   - Because all 1,084 top-level projects have no parent project dependencies (`parent == None`), `ready_at` is set to `phase_started` for all 1,084 projects at `t=0`. As a result, `queue_ms` for the 1,084th project is always approximately `phases.projects - project.ms`, even if all `jobs` workers were 100% busy the entire time. It is still very useful, but readers of `history.jsonl` should know `queue_ms` is *time since eligible in the DAG*, not *time spent waiting while a worker slot was free*.

---

## 6. Final State Left Behind (§6)

1. **`~/projects/studio-main`:**
   - **Live trees ([`data/studio-main/final-list.json`](data/studio-main/final-list.json)):** Only the user's **21 pre-existing trees** remain (identical to `setup-list.json`; all `studio-wt-r5-*` benchmark trees were cleanly removed).
   - **Pool state ([`data/studio-main/final-pool-list.txt`](data/studio-main/final-pool-list.txt)):** **`depth 2 of 2`** (2 spares each for all 9 pooled projects, **0 orphaned spares**, `.pool` disk usage `41,051,040 KB` / `39.15 GiB`, down from `171.54 GiB` before Round 5).
   - **Source integrity ([`data/studio-main/snap-diff.txt`](data/studio-main/snap-diff.txt)):** Clean (0 diffs).

2. **`~/projects/main` (AOSP):**
   - **Live trees ([`data/main/final-list.json`](data/main/final-list.json)):** Only the user's **2 pre-existing trees** (`main-feature` and `studio-main.temp`) remain (identical to `setup-list.json`; all `main-wt-r5*` benchmark trees were cleanly removed).
   - **Pool state ([`data/main/final-pool-list.txt`](data/main/final-pool-list.txt)):** **`depth 0 of 2`** (0 spares across all 14 pooled projects, **0 orphaned spares**, `.pool` cleared at the end of S5 so no extra disk space was left consumed on `/`).
   - **Source integrity ([`data/main/snap-diff.txt`](data/main/snap-diff.txt)):** Clean (0 diffs).
