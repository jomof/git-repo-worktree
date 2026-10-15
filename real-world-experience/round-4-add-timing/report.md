# Round 4: real-world `add` timing on `~/projects/main` (AOSP) and `~/projects/studio-main`

Tool: `git-repo-worktree 2.0.0 (6025da9bd07d)`, built from `main` and installed to
`~/.local/bin`. The machine is one gLinux cloudtop: 96 CPUs, 236 GB RAM, Linux
7.1.6, ext4 on LVM (`/` 7.5 TB, 86% full), git 2.56, repo 2.64/2.65. Source and
trees share one filesystem; reflink is unsupported (ext4), and fuse-overlayfs is
available.

Raw data is in [`data/`](data/) and every measurement script is in
[`scripts/`](scripts/). Times are wall clock unless noted.

## Summary

| | `main` (AOSP, 1,084 projects) | `studio-main` (56 projects) |
| :-- | :-- | :-- |
| Best observed `add` | **60 s** (one run) | **4.8 s** (pool hit) |
| Typical `add` today | **195–243 s** (7 of 8 later runs) | **31–60 s** whenever the pool misses |
| `remove` | 9–74 s (p50 46 s) | 6 s (returns to pool) / 28.5 s (pool full, real delete) |
| Real disk per tree (`du -x`) | 34 GB | 21 GB |
| Apparent size (`du`, crosses overlays) | ~110 GB | 89 GB |
| Overlay mounts per tree | 113 (77.7 GB shared) | 19 |
| fuse-overlayfs RSS | ~2.7 MB per mount (~300 MB per AOSP tree) | ~50 MB per tree |

Findings, most actionable first:

1. **studio-main pool: 132 GB of 171 GB (77%) is spares that `add` never uses.**
   Nine projects that `add` now populates as overlays (clang, jdk,
   intellij-sdk, `prebuilts/tools` …) or as plain checkouts (`external/dexter`)
   still hold two spares each, apparently left by an older pool policy. Nothing
   ever consumes or reaps them.
2. **studio-main pool depth runs out after one or two adds.** With pool size 2
   and refills only on `remove`, the second and third adds of a burst miss and
   take 40 s and 60 s. Across studio-main's full history (72 successful adds),
   pool hits have p50 6.5 s and misses p50 37.5 s; since `5865f3f` only 7 of 17
   real adds hit the pool.
3. **On AOSP the pool barely matters, and `add` time is dominated by
   machine-wide stalls the tool can't see.** After one 60 s run, seven
   consecutive adds took 155–243 s at roughly 1 CPU of utilisation (vs 6.2 CPUs
   on the fast run). Pool, page cache, fsync, and leaked mounts are all ruled
   out (below). Raw `git worktree add` at the same parallelism, outside the
   tool, shows the same intermittent stalls (300 small projects: 0.4 s in one
   run, 5.2 s a minute later).
4. **The `snapshot` phase hides most of its own cost.** `repo manifest -r`
   alone takes 4.6 s on AOSP, yet the phase ranged from 5 s to 93 s. The
   remainder (manifest parse and population-mode preflight) isn't separately
   timed, so the variance can't be attributed.
5. **`remove` on AOSP is slow and noisy** (9–74 s). The `inspect`, `prune`, and
   `worktree-remove` phases swing by 10× between identical runs.

## `~/projects/main` (AOSP `android-latest-release`)

### Client shape

- 1,084 projects (`.repo/project.list`); partial clone (`blob:none`); 56 GB of
  `.repo/project-objects`.
- `add` populates 971 projects as worktrees (32.0 GB checked out) and 113
  `clone-depth` prebuilts as fuse-overlayfs mounts (77.7 GB). The largest
  overlay is `prebuilts/clang/host/linux-x86` at 19.4 GB.
- 14 projects are pool-eligible (cts, frameworks/base, external/cronet,
  external/deqp, external/llvm, external/tensorflow, external/opencl/llvm-project …),
  together ~19 GB.
- 971 "reflink unsupported for filesystem pair" notes per add: expected on ext4,
  but noisy (see Recommendations).
- An existing user tree, `main-feature` (created 00:17 with `5865f3f`, cold
  pool), took **119.3 s**: snapshot 31.7 s, projects 76.1 s.

### Runs (tree `main-wt-report`, sibling of the client)

| Time (UTC) | Op | Total | snapshot | projects | Pool | CPU | Notes |
| :-- | :-- | --: | --: | --: | --: | --: | :-- |
| 00:24 | add | **60.3 s** | 5.2 | 50.5 | 0 (empty) | 618% | first run |
| 00:26 | remove | 9.2 s | | | 14 returned | 1596% | |
| 00:30 | add | 221.3 s | 73.7 | 140.6 | 14 hits | 108% | |
| 00:35 | add | 239.0 s | 82.4 | 151.9 | 14 hits | 96% | |
| 00:40 | add | 231.5 s | 89.4 | 140.3 | 14 hits | 96% | |
| 00:44 | add | 161.5 s | 16.8 | 137.1 | 14 hits | — | child-process sampler attached |
| 00:49 | add | 233.3 s | 89.7 | 141.3 | 14 hits | 96% | disk ~7% util before start |
| 00:54 | add | 243.3 s | 93.3 | 143.5 | 14 hits | 89% | |
| 00:59 | add | 195.1 s | 29.1 | 164.4 | 0 (pool cleared) | 168% | A/B: no pool |

`remove` across the same cycles took 9.2, 33.4, 73.9, 46.2, 56.9, 65.8, 16.2,
and 20.3 s. The swing comes from `inspect` (2–32 s), `prune` (0.2–32 s), and
`worktree-remove` (5–34 s).

The fast run's resources: 141 s user and 231 s system CPU, 604 MB max RSS,
69.7 M filesystem output blocks (~35.7 GB written). The disk-used delta for one
tree was 36.8 GB, matching `du -sx` (34 GB) plus metadata.

### Per-project comparison, first run vs. latest pool-hit run

| Strategy | n | Σ ms first → latest | p50 ms first → latest |
| :-- | --: | :-- | :-- |
| checkout | 957 | 1,161 s → 3,537 s | 349 → 3,210 |
| fuse-overlayfs | 113 | 164 s → 724 s | 127 → 4,426 |
| pool | 14 | 72 s (checkout) → 33 s | 4,605 → 1,572 |

The slowdown is uniform. Tiny projects regressed the most: 25 MB checkouts went
from ~0.3 s to ~16.5 s, and 25–125 MB overlay mounts from ~0.15 s to ~17–20 s.
The pool saved 39 s of summed project time on the 14 large projects, which is
negligible next to the 2,400 s regression on everything else.

### Tree usability (verified)

- All 1,084 projects pinned identically to the source (e.g. `frameworks/base`
  `94b4c163…`). `git repo-worktree status`: 0 dirty, in 1.6 s.
- `repo status -j32` works in the tree: 25.0 s (source: 13.9 s).
- Manifest linkfiles/copyfiles (`build/envsetup.sh`, `Android.bp`,
  `lk_inc.mk`, trusty bazel files) are present.
- `du -sh -x` takes 11 s; the tree has 1.35 M files on ext4.
- No hooks are installed for `main`.

### Ruling things out

| Hypothesis | Test | Result |
| :-- | :-- | :-- |
| Pool spares slow the add | `pool clear`, then add | Still 195 s, so no |
| Page cache evicted | `fincore` on the largest packs (frameworks/base, cts, cronet) | 85–100% resident, so no |
| Tool overhead per project | One `git worktree add` + checkout of `tools/apksig` by hand | 0.05 s (tool recorded 16.5 s), so not git itself in isolation |
| fsync / ext4 journal | Raw parallel run with `-c core.fsync=none` | No improvement (p50 684 ms vs 1,267 ms; tails worse), so no |
| Leaked fuse-overlayfs daemons/mounts | `pgrep`/`findmnt` | 377 daemons = 113 + 113 (`main`) + 151 (studio trees); no leak |
| Tool-specific serialisation | Raw `git worktree add` of 300 smallest projects, `xargs -P 96`, no tool | 375 ms wall in one run, **5,184 ms** a minute later; same stall pattern without the tool |
| CPU saturation | `top -d 10` attribution | Only ~17% of 96 CPUs busy (git 9 cores, crosvm 5), so not plain saturation |

What was running on the machine throughout (load average 30–60): Jetski hub
polling `git --no-optional-locks status --untracked-files=all` in studio-main
projects (14–22 concurrent processes in 88 of 90 one-second samples;
[`probe.txt`](data/main/probe.txt)), three Cuttlefish `crosvm` VMs, and, for
part of the window, a Bazel build in another studio-main tree. A single small
`git worktree add` stayed at ~90 ms throughout, so the stalls only show up
under wide parallelism. Root cause is **not identified**; candidates are
inode/dentry lock contention against the background `git status` scans of the
same filesystem, or CPU scheduler latency with ~100–160 runnable tasks. Off-CPU
profiling (`perf sched`, `offcputime`) would settle it but needs root.

## `~/projects/studio-main` (Android Studio)

### Controlled runs (01:06, three back-to-back adds, then removes)

Pool before: 27 spares across 18 projects.

| Tree | Total | snapshot | projects | Pool hits | CPU | Disk delta (cumulative) |
| :-- | --: | --: | --: | --: | --: | --: |
| A | **4.8 s** | 2.3 | 2.0 | 9 | 656% | +1.6 GB |
| B | 39.9 s | 1.9 | 37.7 | 0 | 851% | +24.2 GB |
| C | 60.4 s | 2.1 | 57.6 | 0 | 922% | +46.9 GB |

- The slowest projects on a miss are `prebuilts/studio/sdk/linux` (37.7 s,
  57.2 s), `tools/idea` (26.4 s, 50.3 s), and `tools/gpu-profiler/idea`.
- The `post-add` hook (`build/bazel/env/setup`) ran in 104–276 ms, and
  `env.bazelrc` was present in each tree.
- Real disk for one tree is 21 GB (`du -sx`, 7.5 s); apparent size is 89 GB
  through 19 overlays. A pool hit costs only +1.6 GB net, a miss +22–23 GB.
- `git repo-worktree status`: 1.6 s. `repo status -j32` in the source: 3.4 s.
- `remove`: C 6.5 s and B 6.0 s (9 returned each; `inspect` 2.6–3.1 s,
  `pool-return` 2.8 s). A took **28.5 s** with 0 returned: the pool was back at
  size 2, so A's projects were deleted for real (`worktree-remove` 26.6 s).
- Pool after: 36 spares; net disk +21.8 GB vs. before (nine more spares).

### Why B and C missed with 27 spares in the pool

`pool list` grouped by project:

- The 9 projects `add` actually pools (`tools/idea`, `tools/base`,
  `tools/adt/idea`, `tools/vendor/google`, `tools/vendor/intellij/cidr`,
  `tools/gpu-profiler/idea`, `external/intellij-plugins`,
  `external/jetbrains/kotlin`, `prebuilts/studio/sdk/linux`) had **1 spare
  each**. A consumed them all.
- The other 18 spares belonged to 9 projects that `add` populates as overlays
  (`prebuilts/clang/host/linux-x86`, `prebuilts/studio/jdk`,
  `prebuilts/studio/intellij-sdk`, `prebuilts/tools`,
  `prebuilts/studio/sdk/remote`, `prebuilts/studio/buildbenchmarks`,
  `prebuilts/remoteexecution-client`, `tools/external/gradle`) or as a plain
  checkout (`external/dexter`). They are never consumed.
- All spares are at the source's current HEAD, so staleness is not the cause.

Dead-spare disk (`du -sx` per `.pool/<project>`):

| Project | Size |
| :-- | --: |
| prebuilts/tools | 50.5 GB |
| prebuilts/studio/intellij-sdk | 25.2 GB |
| prebuilts/studio/jdk | 15.2 GB |
| tools/external/gradle | 12.2 GB |
| prebuilts/studio/sdk/remote | 10.9 GB |
| prebuilts/studio/buildbenchmarks | 7.6 GB |
| prebuilts/clang/host/linux-x86 | 7.2 GB |
| prebuilts/remoteexecution-client | 2.3 GB |
| external/dexter | 1.2 GB |
| **Total dead** | **132.4 GB** of 171.5 GB pool |

### Long-run history (studio-main `history.jsonl`, 439 events since 2026-10-12)

| Class (successful adds) | n | p50 | p90 | min | max |
| :-- | --: | --: | --: | --: | --: |
| Pool hit (≥ 6 pooled projects) | 24 | 6.5 s | 12.5 s | 4.8 s | 16.4 s |
| Pool miss (0 pooled) | 24 | 37.5 s | 47.1 s | 31.1 s | 60.0 s |
| Partial (1–5 pooled) | 24 | 30.9 s | 46.0 s | 0.3 s | 144.6 s |

- All adds: 83 runs, 72 ok, p50 30.6 s, p90 46.0 s. Errors: `E_LOCKED`×7,
  `E_DEST_NESTED`×2, `E_DEST_NOT_EMPTY`, `E_PARTIAL`.
- Since `5865f3f` (pooling 9 projects): 7 of 17 real adds hit the pool.
- `remove`: 73 runs, p50 21.5 s, p90 35.8 s.
- Snapshot is steady on studio-main (p50 2.3 s) but spiked to 8–25 s on a
  few busy days, the same unexplained component as on AOSP.

## Recommendations

1. **Reap spares for projects that aren't pool-mode.** On `add`, `remove`, and
   `pool fill`, delete `.pool/<project>` for any project whose current mode
   isn't `pool`, and have `pool list` flag them as orphaned with their size.
   That would reclaim 132 GB on this machine.
2. **Refill the pool asynchronously after a hit,** at low I/O priority (or in a
   detached `pool fill`), so a burst of 2–3 agent adds doesn't fall off the
   37 s cliff. Alternatively, have `pool list` report effective depth (the
   minimum spares across pool-mode projects) rather than a raw spare count; 27
   spares looked like three warm adds but was one.
3. **Split the `snapshot` phase** into `repo-manifest`, `parse`, and
   `preflight` in metrics. Today 1–88 s of unexplained time lands in it.
4. **Record queue-wait vs. execution time per project**, plus `/proc/loadavg`
   and `/proc/pressure/{cpu,io}` at start and end, in `history.jsonl`. With
   these the AOSP stalls would have been attributable from history alone.
5. **Consider adaptive parallelism** for large clients: 96 jobs plus 8 checkout
   workers each is well past where this machine's stalls begin. A run at `-j 32`
   would be a useful next data point.
6. **Collapse the 971 identical "reflink unsupported" notes** into one per
   filesystem pair in `history.jsonl` and the JSON result (currently ~100 KB
   per AOSP add).
7. **`remove` on large clients:** time `inspect` per project, and consider
   skipping `prune` when nothing is stale. Both swung by over 10× here.

## Reproduce

```sh
# AOSP add/remove cycles with per-sample load (writes to $S):
CYCLES="3 4" bash scripts/cycle.sh
# Pool A/B, raw-git parallel repro, latency-vs-background probe:
bash scripts/ab_pool.sh
N=300 P=96 EXTRA="" bash scripts/rawpar.sh
SECS=90 bash scripts/probe.sh
# studio-main hit/hit/miss burst with disk deltas:
bash scripts/measure.sh
```

The scripts hard-code this machine's paths (`/usr/local/google/home/jomof/...`)
and a scratch directory; edit `S`/`SRC`/`T` before reusing them.

## State left behind

- `main`: only the pre-existing trees remain (`main-feature`,
  `studio-main.temp`); the pool was cleared back to empty, as it was found.
- `studio-main`: all `studio-wt-report-*` trees removed. The pool grew from 27
  to 36 spares (+21.8 GB), because B and C returned their projects.
- A `cargo install` copy of `git-repo-worktree 2.0.0` in `~/.cargo/bin`
  (crates.io build, no commit hash, installed 23:48) came before `~/.local/bin`
  on this shell's `PATH` and shadowed the fresh build, so it was uninstalled.
  `version` now reports `2.0.0 (6025da9bd07d)`.
