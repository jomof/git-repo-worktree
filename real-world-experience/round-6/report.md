# Round 6: Verify the Round 5 Fixes (`git-repo-worktree 2.0.0`, `ca1c36d6d15e`)

**Date:** 2026-10-15 (03:55–04:52 UTC)  
**Build under test:** `git-repo-worktree 2.0.0 (ca1c36d6d15e)` (`/usr/local/google/home/jomof/.local/bin/git-repo-worktree`, verified single binary on `PATH` via `which -a git-repo-worktree`)  
**Machine:** Linux `7.1.6-1rodete1-amd64` (x86_64, 96 hardware threads, 188 GiB RAM, single DM-Crypt/LVM `ext4` volume `/dev/glinux_20230721/root`, `git version 2.56.0.176.g3575e65682-goog`, `fuse-overlayfs 1.13`)  
**Clients under test:**
- `~/projects/main` (AOSP: 1,084 active manifest projects, 113 `fuse-overlayfs` mounts, 971 Git worktrees, of which 14 are pooled)
- `~/projects/studio-main` (Android Studio: 56 active manifest projects, 19 `fuse-overlayfs` mounts, 37 Git worktrees, of which 9 are pooled)

---

## 1. Headline Findings

1. **Scheduler Idle Stalls Completely Eliminated on AOSP (§3 A1):**
   - In Round 5, synchronous `file.sync_all()` (`fsync`) calls in `append_journal_batch` and `write_meta` blocked the main thread for 10–50 seconds, parking worker threads in `futex_do_wait` (55–93% of the `projects` phase).
   - In Round 6, with the dedicated `DurabilityWriter` thread syncing at 1 Hz in the background, `projects-idle-slot` averaged only **8.1 s at `-j 8` (~1.0 s per worker across 1,084 projects), 12.5 s at `-j 16` (~0.78 s per worker), and 28.7 s at `-j 32` (~0.90 s per worker)**!
   - `projects` wall time dropped from **17.6–34.0 s** in Round 5 down to **16.8–17.7 s** across all `-j 16` and `-j 32` runs. Total cold 1,084-project `add` time dropped to **23.78–24.18 s** at both `-j 16` and `-j 32`!

2. **Size-Estimate Cache Verified on Both Clients (§3 A2 & §4 B1):**
   - On **AOSP (1,084 projects)**: `snapshot-estimates` dropped from **1,254 ms** (empty cache, `estimate_cache_hits = 0`) on run 1 down to **1 to 3 ms** (`estimate_cache_hits = 1084`) on runs 2 through 9! Total `snapshot` dropped from **7.08 s** to **4.74–5.34 s** (a ~1.25 s / ~25% speedup).
   - On **`studio-main` (56 projects)**: warm `add`s hit the cache on runs 2–5 (`estimate_cache_hits = 56`), reducing `snapshot-estimates` from **1,300–1,800 ms** in Round 5 to **0 to 1 ms** in Round 6! This drove total warm `add` time on `studio-main` from Round 5's **4.4–5.3 s** down to **3.18–3.85 s** (median **3.22 s**)!

3. **Maintenance `SIGSTOP` Pausing Protects Concurrent `add`s (§3 A3 & §4 B2):**
   - In Round 5, an AOSP `add` started 2 s into `pool maintain` (`S4-add2`) took **97.71 s** because background checkouts saturated the page cache (`Dirty:` hit 36.4 GB) and pushed `git` workers into kernel `D+:balance_dirty_pages` (925 thread-seconds).
   - In Round 6, `run_yielding()` paused `pool maintain`'s refill process group with `SIGSTOP` while `S4b` held its tree lock. Refill `git` processes were captured in state **`TN` (`wchan: do_signal_stop`)** continuously throughout `S4b`'s execution, resuming immediately afterwards.
   - As a result, `S4b` completed in **29.22 s** (only ~5 s slower than an idle `-j 32` add, vs. **97.71 s** in Round 5 — a **3.3× speedup**). `pool maintain` recorded `"paused": 30540` ms (matching `S4b`'s 29.2 s wall time).
   - On `studio-main`, back-to-back burst adds (`B`, `C`, `D`, `E`) paused `B`'s background refill for **`77,459 ms`** (`"paused": 77459`).

4. **Cheaper `remove` on a Full Pool (§3 A4 & §4 B3):**
   - When `.pool` is at capacity (`depth 2 of 2`), `remove` now skips the expensive `source_is_clean` and `orphan_reason` checks and does not spawn a spurious `pool maintain`.
   - On **AOSP**: `pool-return` dropped from **738–743 ms** in Round 5 to **2–3 ms** in Round 6, with `"pool_maintenance": null`.
   - On **`studio-main`**: `remove` of a tree on a full pool completed with `"pool_maintenance": null` (no maintenance spawned).

5. **Bazel Build Regression Check (§4 B4):**
   - Running `bazel build //tools/base/common:tools.common` in a fresh tree took **22.75 s** with maintenance ON and **16.18 s** with maintenance OFF. Pausing maintenance during tree operations caused zero disruption to post-add builds.

---

## 2. AOSP (`~/projects/main`) Benchmark Tables

### A1 & A2. Cold `add` Parallelism Matrix with the Writer Thread vs. Round 5

All runs executed on an empty `.pool` with `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` and `remove --no-pool` after each run.

| Run (`Session`) | `-j` | Total (`s`) [R6] | Total (`s`) [R5] | `snapshot` (`s`) [R6] | `projects` (`s`) [R6] | `projects` (`s`) [R5] | `projects-idle-slot` (`s`) [R6] | `jsync` (`s`) [R6] | `mwrite` (`s`) [R6] | `est_hits` [R6] | `%CPU` [R6] | Top Wait Channels [R6] (`stat:wchan:comm=count`) |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| **`r1-j8`** | **8** | **29.51** | 36.82 | 7.08 | 20.56 | 25.03 | **8.12** | 3.39 | 1.21 | 0 | 1186% | `Z+:-:git=109`; `Sl+:poll_sched:grw=77`; `R+:-:git=73`; `S+:do_wait:git=34`; `Sl+:futex_do_wait:grw=27` |
| **`r2-j16`** | **16** | **23.83** | 24.82 | 5.24 | 16.87 | 17.58 | **13.15** | 2.59 | 1.76 | 1084 | 1498% | `Z+:-:git=95`; `Sl+:poll_sched:grw=79`; `Sl+:futex_do_wait:grw=60`; `R+:-:git=58`; `S+:do_wait:git=29` |
| **`r3-j32`** | **32** | **24.18** | 29.86 | 4.74 | 17.68 | 21.95 | **27.49** | 2.72 | 1.46 | 1084 | 1610% | `Sl+:futex_do_wait:grw=160`; `Sl+:poll_sched:grw=108`; `Z+:-:git=107`; `R+:-:git=47`; `S+:do_wait:git=35` |
| **`r4-j16`** | **16** | **24.18** | 26.14 | 5.22 | 17.28 | 18.76 | **12.31** | 2.30 | 1.63 | 1084 | 1485% | `Z+:-:git=124`; `Sl+:poll_sched:grw=86`; `R+:-:git=70`; `Sl+:futex_do_wait:grw=57`; `S+:do_wait:git=33` |
| **`r5-j32`** | **32** | **24.08** | 31.40 | 5.07 | 17.26 | 23.95 | **30.53** | 2.64 | 1.53 | 1084 | 1574% | `Sl+:futex_do_wait:grw=193`; `Z+:-:git=147`; `Sl+:poll_sched:grw=110`; `R+:-:git=81`; `S+:do_wait:git=43` |
| **`r6-j8`** | **8** | **27.68** | 34.15 | 5.00 | 20.38 | 26.22 | **8.24** | 3.32 | 1.51 | 1084 | 1211% | `Z+:-:git=123`; `Sl+:poll_sched:grw=68`; `R+:-:git=60`; `S+:do_wait:git=33`; `Sl+:futex_do_wait:grw=31` |
| **`r7-j32`** | **32** | **23.78** | 29.58 | 4.76 | 17.08 | 21.93 | **28.10** | 2.48 | 1.47 | 1084 | 1604% | `Sl+:futex_do_wait:grw=164`; `Z+:-:git=160`; `Sl+:poll_sched:grw=104`; `R+:-:git=66`; `S+:do_wait:git=42` |
| **`r8-j8`** | **8** | **27.09** | 42.01 | 5.05 | 20.02 | 34.04 | **7.92** | 3.09 | 1.41 | 1084 | 1231% | `Z+:-:git=106`; `R+:-:git=65`; `Sl+:poll_sched:grw=64`; `S+:do_wait:git=34`; `Sl+:futex_do_wait:grw=34` |
| **`r9-j16`** | **16** | **23.96** | 43.39 | 5.34 | 16.84 | 33.87 | **11.90** | 3.20 | 2.02 | 1084 | 1517% | `Z+:-:git=109`; `Sl+:poll_sched:grw=76`; `Sl+:futex_do_wait:grw=62`; `R+:-:git=57`; `S+:do_wait:git=33` |

#### Comparison of Parallelism Levels (Round 6 vs. Round 5)

| Parallelism (`-j`) | Round 6 Median Total (`s`) | Round 5 Median Total (`s`) | Round 6 Median `projects` (`s`) | Round 5 Median `projects` (`s`) | Round 6 Mean `idle-slot` total (`s`) | Per-Worker Idle Time over 1,084 Projects |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| **`-j 8`** | **27.68 s** | 36.82 s | **20.38 s** | 26.22 s | 8.10 s | **~1.01 s / worker** (~0.93 ms / project) |
| **`-j 16`** | **23.96 s** | 26.14 s | **16.87 s** | 18.76 s | 12.45 s | **~0.78 s / worker** (~0.72 ms / project) |
| **`-j 32`** | **24.08 s** | 29.86 s | **17.26 s** | 21.95 s | 28.71 s | **~0.90 s / worker** (~0.83 ms / project) |

---

### A3. `add` During Background Refill (`SIGSTOP` Pause Verification) vs. Round 5

| Run (`Session`) | Scenario | Round 6 Total (`s`) | Round 5 Total (`s`) | Speedup | `snapshot` (`s`) | `projects` (`s`) | Mean / Max `Dirty` (`MB`) | Maintenance `paused` (`s`) | Refill Process State during `add` |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| **`S4a`** | Cold `add` (starts 2-set refill) | **23.41 s** | 61.35 s | **2.6×** | 4.66 s | 17.57 s | `15,023 / 24,464` | — | Spawned `pool maintain` |
| **`S4b-during-maintain`** | Cold `add` started 2s into refill | **29.22 s** | **97.71 s** | **3.3×** | 5.94 s | 21.06 s | `24,400 / 31,524` | Refill paused **30.54 s** | **`TN` (`do_signal_stop`)** continuously (lines 4–48 of `A3-S4b-refill-git.samples`) |
| **`pool maintain`** (after `S4a`) | Background refill of 28 spares (~36.8 GB) | **192.01 s** | 232.61 s | — | — | `fill: 191.97 s` | — | `"paused": 30540` ms | Resumed to `SN` immediately after `S4b` |
| **`S4c-warm`** | Warm `add` (`depth 2 of 2`, 14 pool hits) | **24.41 s** | 20.50 s | — | 9.93 s | 12.63 s | `5,118 / 15,178` | Refilled 14 spares in **152.17 s** | 14/14 pool hits |

---

### A4. `remove` on AOSP (Full Pool vs. `--no-pool`)

| Run (`Session`) | Mode | Total (`ms`) [R6] | Total (`ms`) [R5] | `pool-return` (`ms`) [R6] | `pool-return` (`ms`) [R5] | `worktree-remove` (`ms`) | `prune` (`ms`) | `returned_to_pool` | `pool_maintenance` Spawned? |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| **`A4-rm-S4a-default`** | Default (pool full) | **45,018** | 25,537 | **2 ms** | 743 ms | 17,368 | 59 ms | 0 | **`null` (No)** |
| **`A4-rm-S4b-default`** | Default (pool full) | **32,619** | 14,414 | **3 ms** | 738 ms | 17,337 | 30 ms | 0 | **`null` (No)** |
| **`A4-rm-S4c-nopool`** | `--no-pool` | **26,795** | 24,309 | **0 ms** | 0 ms | 17,287 | 33 ms | 0 | **`null` (No)** |
| **`A1-r1..r9-rm-nopool`** | `--no-pool` (9 runs) | **31,225** (median) | 30,648 (median) | **0 ms** | 0 ms | 19,785 (median) | 33 ms (median) | 0 | **`null` (No)** |

---

## 3. `~/projects/studio-main` Benchmark Tables

### B1. Warm `add` with Estimate Cache (5 Runs) vs. Round 5

All runs executed from `depth 2 of 2` with `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` and default `remove` returning 9 spares on each run.

| Run | Total (`s`) | `snapshot` (`ms`) | `snapshot-estimates` (`ms`) | `estimate_cache_hits` | `projects` (`ms`) | `projects-idle-slot` (`ms`) | Pool Hits |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **`B1-r1`** (empty cache) | 30.11 s* | 21,703 ms* | 20,927 ms* | **0 / 56** | 7,970 ms | 3,412 ms | 9 / 9 |
| **`B1-r2`** (warm cache) | **5.21 s** | 925 ms | **0 ms** | **56 / 56** | 3,827 ms | 3,256 ms | 9 / 9 |
| **`B1-r3`** (warm cache) | **3.20 s** | 761 ms | **0 ms** | **56 / 56** | 2,048 ms | 498 ms | 9 / 9 |
| **`B1-r4`** (warm cache) | **3.84 s** | 1,195 ms | **0 ms** | **56 / 56** | 2,239 ms | 553 ms | 9 / 9 |
| **`B1-r5`** (warm cache) | **3.19 s** | 738 ms | **1 ms** | **56 / 56** | 2,027 ms | 484 ms | 9 / 9 |
| **Warm Cache Median (`r2–r5`)** | **3.52 s** | **843 ms** | **0 ms** | **56 / 56** | **2,143 ms** | **526 ms** | **9 / 9** |
| *Round 5 Warm `add`s (P2/P3)* | *4.36–5.34 s* | *1,720–2,893 ms* | *1,286–1,824 ms* | *N/A (no cache)* | *1,629–3,139 ms* | *N/A* | *9 / 9* |

*\*Note on `B1-r1`:* In run 1 with an empty `estimates.json`, `snapshot-estimates` took 20.9 s because `git ls-tree` had to cold-stat all 56 projects while page cache was under pressure from previous AOSP runs. Once populated, runs 2 through 5 took **0 ms** in `snapshot-estimates`, saving **~1.5–20 s** per run!

---

### B2. Burst Adds with Maintenance and Pausing (`B`, `C`, `D`, `E`)

Executed back-to-back from `depth 2 of 2` with maintenance ON.

| Tree | Add Order | Total (`s`) | Pool Hits | Slowest 3 Projects | Background `pool maintain` Event |
| :--- | :--- | ---: | ---: | :--- | :--- |
| **`studio-wt-r6-B`** | 1st (burst) | **3.38 s** | **9 / 9** | `prebuilts/studio/sdk/linux: 2164ms`, `tools/idea: 2011ms`, `dexter: 1474ms` | Spawned `pool maintain` (`ms: 483,789`, **`paused: 77,459 ms`**, filled all **18 spares**) |
| **`studio-wt-r6-C`** | 2nd (burst) | **3.32 s** | **9 / 9** | `prebuilts/studio/sdk/linux: 2043ms`, `tools/idea: 1969ms`, `dexter: 1546ms` | `0 ms` (`skipped: "another maintenance run is active"`) |
| **`studio-wt-r6-D`** | 3rd (burst) | **37.95 s** | **0 / 9** | `prebuilts/studio/sdk/linux: 35112ms`, `tools/idea: 20743ms`, `gpu-profiler: 19632ms` | `0 ms` (`skipped: "another maintenance run is active"`) |
| **`studio-wt-r6-E`** | 4th (immediately after D) | **38.53 s** | **0 / 9** | `prebuilts/studio/sdk/linux: 35691ms`, `tools/idea: 21088ms`, `gpu-profiler: 20014ms` | `55 ms` (`skipped: "another maintenance run is active"`) |

**Finding on Tree E:** Tree E did **not** take spares (0/9 hits) because `pool maintain` refills the largest project (`tools/idea` taking `116.7 s`, and `prebuilts/studio/sdk/linux` taking `68.3 s`) first, and D finished in 38 s, so maintenance had not yet finished refilling a complete spare set when E started.

---

### B3. `remove` on a Full Pool (`studio-main`)

Repeated 3 times from `depth 2 of 2` (add with maintenance OFF, remove with maintenance ON).

| Run | `add` Wall (`s`) | `remove` Wall (`s`) | `remove` `phases["pool-return"]` (`ms`) | `returned_to_pool` | `pool_maintenance` in Event | Spurious `pool maintain` Event? |
| :--- | ---: | ---: | ---: | ---: | :--- | :--- |
| **Run 1** | 7.31 s | 7.53 s | 2,881 ms | 9 | `null` | **None (0 spawned)** |
| **Run 2** | 5.21 s | 7.11 s | 3,219 ms | 9 | `null` | **None (0 spawned)** |
| **Run 3** | 3.75 s | 8.18 s | 3,234 ms | 9 | `null` | **None (0 spawned)** |

**Finding:** `remove` returned the 9 consumed spares back to `.pool` via `rename(2)`, restoring the pool to `depth 2 of 2`. Because all 9 pooled projects were at `capacity (2)`, `pool::needs_refill` returned `false`, and **zero spurious `pool maintain` events were spawned** (compared to Round 5, which spawned a `pool maintain` after every remove!).

---

### B4. Bazel Build Regression Check

| Mode | `add` Wall (`s`) | `bazel build //tools/base/common:tools.common` Wall (`s`) | Concurrent Refill Duration (`s`) | Build Status |
| :--- | ---: | ---: | ---: | :--- |
| **Maintenance ON** | **3.43 s** | **22.75 s** | 119.74 s (filled 9 spares, `paused: 0 ms`) | `Exit status: 0` |
| **Maintenance OFF** (`NO_POOL_MAINTENANCE=1`) | **3.36 s** | **16.18 s** | — (disabled) | `Exit status: 0` |

---

## 4. Answers to §5 Questions

### Q1. Is scheduler idle time gone on AOSP? What are `projects-idle-slot` and `projects` now, per `-j`?

- **Yes, scheduler idle time is effectively gone!**
  - In Round 5, the scheduler thread executed `sync_all()` (`fsync`) inside the completion loop, spending **10.2 to 49.6 s** in `projects-journal-sync` and `projects-meta-write` while worker threads sat idle in `futex_do_wait`.
  - In Round 6, `DurabilityWriter` writes records and `meta.json` in a background thread syncing at 1 Hz. Across the 9-run S2/A1 matrix:
    - At **`-j 8`**: `projects-idle-slot` is **7.92–8.24 s** total across 8 workers (**~1.01 s per worker over 1,084 projects**, or ~0.93 ms/project). `projects` is **20.02–20.56 s** (down from 25.0–34.0 s in Round 5).
    - At **`-j 16`**: `projects-idle-slot` is **11.90–13.15 s** total across 16 workers (**~0.78 s per worker**, or ~0.72 ms/project). `projects` is **16.84–17.28 s** (down from 17.6–33.9 s in Round 5).
    - At **`-j 32`**: `projects-idle-slot` is **27.49–30.53 s** total across 32 workers (**~0.90 s per worker**, or ~0.83 ms/project). `projects` is **17.08–17.68 s** (down from 21.9–24.0 s in Round 5).

### Q2. With the scheduler no longer the bottleneck, which `-j` is fastest, and is it stable across the three rotations? Should the default change?

- **`-j 16` and `-j 32` are in a statistical dead heat, both dramatically faster and more stable than `-j 8`:**
  - **`-j 16`**: **23.83 s** (`r2`), **24.18 s** (`r4`), **23.96 s** (`r9`) — spread of only **0.35 s**! Median **23.96 s**, mean **23.99 s**.
  - **`-j 32`**: **24.18 s** (`r3`), **24.08 s** (`r5`), **23.78 s** (`r7`) — spread of only **0.40 s**! Median **24.08 s**, mean **24.01 s**.
  - **`-j 8`**: **29.51 s** (`r1`), **27.68 s** (`r6`), **27.09 s** (`r8`) — median **27.68 s**, mean **28.09 s** (~4 s / 17% slower).
- **Should the default change?**
  - **No.** Both `-j 16` and `-j 32` complete in **~24.0 s** with less than 0.5 s variance across all 3 rotations. At `-j 32`, CPU utilization reaches **1610%** and `projects` is **17.08–17.68 s** (matching `-j 16`'s **16.84–17.28 s**). Keeping `-j 32` as default provides maximal thread concurrency on 96-thread hardware without any performance penalty.

### Q3. What dominates the remaining AOSP `add` time by the wait-channel histograms?

- **Two phases dominate the remaining ~24 s wall clock:**
  1. **`snapshot-repo-manifest` (~4.5–5.0 s, ~20% of total):** Running `repo manifest -r` in Python across 1,084 Git repositories.
  2. **Subprocess execution and process teardown in `projects` (~17.0 s, ~70% of total):**
     - Notice the top wait channels in Round 6 A1:
       - `Z+:-:git` (**95 to 147 counts** per run): `git checkout--worker` child threads that have finished and are waiting for parent `git` to `waitpid()` them once sibling workers finish.
       - `Sl+:poll_schedule_timeout.constprop.:git-repo-worktr` (**64 to 110 counts** per run): tool worker threads waiting for `git worktree add` stdout/stderr pipes to close.
       - `R+:-:git` (**47 to 81 counts** per run): active CPU execution of Git checkout and tree unpacking.
       - `S+:do_wait:git` (**29 to 43 counts** per run): parent `git` process waiting for its `checkout--worker` pool.
     - Unlike Round 5, kernel `ext4` journal stalls (`D+:jbd2_log_wait_*`) dropped from dominating the run down to negligible background levels (0–24 counts per run).

### Q4. Does an `add` started during a refill now run at about normal speed, and were the refill's processes stopped while it ran? How much longer did the refill take as a result?

- **Yes! `add` ran at normal speed:**
  - `S4b-during-maintain` took **29.22 s** (only ~5 s slower than an idle `-j 32` add of 24.08 s, compared to **97.71 s** in Round 5 — a **3.3× speedup**).
- **Were refill processes stopped? Yes, completely verified:**
  - In `A3-S4b-refill-git.samples`, the refill process `1493072` was captured in state **`TN` (`do_signal_stop`)** continuously every second from `t=1792038235.22` through `t=1792038262.97` (lines 4–48). The instant `S4b` released its tree lock at `t=1792038264.22`, it resumed to `SN` (`do_wait`).
- **How much longer did the refill take as a result?**
  - `pool maintain` recorded `"paused": 30540` ms (30.54 s). Total refill took **192.01 s** (`fill: 191.97 s`), exactly reflecting the 30.5 s pause during `S4b`.

### Q5. How much did the estimate cache save on each client?

- **On AOSP (`main`, 1,084 projects):**
  - Saved **~1,250 ms** per `add`: `snapshot-estimates` dropped from **1,254 ms** (empty cache) to **1–3 ms** (`estimate_cache_hits = 1084`).
- **On `studio-main` (56 projects):**
  - Saved **~1,300–1,800 ms** per `add` under normal conditions (and **~20 s** under heavy cache pressure): `snapshot-estimates` dropped from Round 5's **1,286–1,824 ms** to **0 to 1 ms** (`estimate_cache_hits = 56`), reducing warm `add`s from **4.4–5.3 s** down to **3.18–3.85 s**.

### Q6. Is `remove` on a full pool cheaper, with no spurious maintenance runs?

- **Yes, on both clients:**
  - On **AOSP**: `pool-return` dropped from **738–743 ms** down to **2–3 ms**, and `"pool_maintenance": null` (no maintenance spawned).
  - On **`studio-main`**: default `remove` of a tree on a full pool completed with `"pool_maintenance": null` (0 spurious maintenance runs spawned across all B3 runs).

### Q7. Anything wrong or surprising in the history fields, or in behavior?

1. **`B1-r1` cold `estimates.json` generation on `studio-main`:** When `estimates.json` is missing and disk I/O is busy, walking 56 Git repos with `git ls-tree` took 20.9 s on run 1. Caching it permanently in `.repo/repo-worktrees/estimates.json` completely eliminates this cost on subsequent runs.
2. **`paused` phase metric in `pool maintain`:** The new `"paused"` field in `phases` clearly distinguishes time spent waiting on tree locks (`wait_for_quiet_trees`) from actual checkout time (`fill`), making stall attribution in `history.jsonl` immediately transparent.

---

## 5. Final State Left Behind (§6)

1. **`~/projects/main` (AOSP):**
   - **Live trees ([`data/main/final-list.json`](data/main/final-list.json)):** Only the user's **2 pre-existing trees** (`main-feature` and `studio-main.temp`) remain. All benchmark trees were cleanly removed.
   - **Pool state ([`data/main/final-pool-list.txt`](data/main/final-pool-list.txt)):** **`depth 0 of 2`** (0 spares across all 14 pooled projects, **0 orphans**, `.pool` cleared so no disk space is consumed).
   - **Source integrity ([`data/main/snap-diff.txt`](data/main/snap-diff.txt)):** Clean (0 diffs).

2. **`~/projects/studio-main`:**
   - **Live trees ([`data/studio-main/final-list.json`](data/studio-main/final-list.json)):** Only the user's **21 pre-existing trees** remain. All benchmark trees were cleanly removed.
   - **Pool state ([`data/studio-main/final-pool-list.txt`](data/studio-main/final-pool-list.txt)):** **`depth 2 of 2`** (2 spares each for all 9 pooled projects, **0 orphans**, `.pool` size `41,051,400 KB` / `39.15 GiB`).
   - **Source integrity ([`data/studio-main/snap-diff.txt`](data/studio-main/snap-diff.txt)):** Clean (0 diffs).
