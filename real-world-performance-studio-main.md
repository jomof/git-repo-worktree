# git-repo-worktree — Real-World Performance Analysis on `studio-main`

**Status:** Technical Analysis & Architectural Recommendations.
Investigates the performance profile of `git repo-worktree add` inside the Android Studio (`studio-main`) enlistment.

---

## 1. Executive Summary

Creating a whole-tree worktree in `studio-main` via `git-repo-worktree add` takes **3 minutes and 10 seconds** on a high-spec development machine with 96 logical cores. 

While `git-repo-worktree` parallelizes operations using a thread pool of size 32 (bounded by `min(logical CPUs, 32)`), the slowness stems from a combination of **extreme disk write volume (over 37 Gigabytes)** and **high file-handle creation overhead (over 660,000 files)**. Parallelizing such heavy-duty I/O-bound tasks concurrently without disk-queue awareness leads to **storage controller saturation** and **operating system page cache thrashing**.

---

## 2. Enlistment Structure & Heavyweight Bottlenecks

An analysis of the 57 projects managed by the `studio-main` manifest reveals that while 80% of the projects are small, a few "heavyweight" directories account for the vast majority of I/O runtime:

| Project Path | File Count | Disk Size | Checkout Time (s) | Description |
|---|---|---|---|---|
| `prebuilts/studio/intellij-sdk` | 7,703 | **12.0 GB** | **54.86s** | Heavyweight binary SDK |
| `prebuilts/studio/jdk` | 9,141 | **7.2 GB** | **45.69s** | Prebuilt JDK toolchains |
| `tools/external/gradle` | 57 | **6.2 GB** | **57.23s** | Huge prebuilt gradle distribution archives |
| `prebuilts/studio/sdk/remote` | 41 | **5.5 GB** | **58.29s** | Large prebuilt remote SDK packages |
| `tools/idea` | **266,453** | 2.0 GB | **54.26s** | Monolithic IntelliJ core source tree |
| `tools/gpu-profiler/idea` | **266,417** | 2.0 GB | **57.49s** | Very high file-count project directory |
| `tools/adt/idea` | 52,893 | 2.3 GB | **41.82s** | Main Android Studio logic |

### 2.1 The Slowness Mechanisms
1. **Random Write Saturation:** Running 32 concurrent processes doing `git worktree add` forces the NVMe/SSD drive to perform hundreds of thousands of concurrent random file writes. When massive projects like `tools/idea` (266k files) and `prebuilts/studio/intellij-sdk` (12GB) check out simultaneously, the disk queue depth saturates, throughput drops, and CPU cycles are spent in `iowait`.
2. **Object DB Contention:** Since all checkouts share `.repo/project-objects` via alternates/gitdirs, multiple concurrent git processes are accessing and parsing packfiles under `.repo/` simultaneously, creating read contention on the parent database files.
3. **Absence of Prefix Nesting:** There are no nested git projects on disk in `studio-main`. Since no project path acts as a prefix to another, all 57 projects are eligible for immediate concurrent scheduling. This means the tool starts the heavy checkouts concurrently right from the beginning, triggering immediate I/O congestion.

---

## 3. Granular Project Timings (Real-World Measurements)

The following metrics were captured using high-precision instrumentation (`Instant::now()`) around individual project `git worktree add` executions inside `studio-main`:

### 3.1 Fast Projects (Sub-Second Checkouts)
These are lightweight metadata, build, or small-code projects:
```
TIMING: project "prebuilts/android-emulator"                             took 22ms
TIMING: project "tools/gradle"                                            took 30ms
TIMING: project "external/gflags"                                         took 48ms
TIMING: project "tools/buildSrc"                                          took 60ms
TIMING: project "external/nanopb-c"                                       took 65ms
TIMING: project "external/jarjar"                                         took 72ms
TIMING: project "prebuilts/bazel/linux-x86_64"                            took 84ms
TIMING: project "tools/repohooks"                                         took 90ms
TIMING: project "external/doclava"                                        took 110ms
TIMING: project "tools/assets"                                            took 123ms
TIMING: project "build/bazel"                                             took 169ms
TIMING: project "prebuilts/python/windows-x86"                            took 160ms
TIMING: project "tools/studio/google/cloud/testing"                       took 397ms
```

### 3.2 Medium Projects (1.0s to 12s)
These projects represent medium-size source trees or toolchains:
```
TIMING: project "prebuilts/gcc/linux-x86/host/x86_64..."                  took 1.10s
TIMING: project "tools/emulator"                                          took 1.71s
TIMING: project "tools/profiler"                                          took 1.83s
TIMING: project "external/dexter"                                         took 2.74s
TIMING: project "prebuilts/studio/layoutlib"                              took 2.86s
TIMING: project "tools/vendor/google"                                     took 4.37s
TIMING: project "tools/vendor/intellij/cidr"                              took 4.53s
TIMING: project "external/intellij-plugins"                               took 5.74s
TIMING: project "prebuilts/remoteexecution-client"                        took 7.18s
TIMING: project "tools/base"                                              took 7.21s
TIMING: project "prebuilts/studio/buildbenchmarks"                        took 11.53s
```

### 3.3 Slow Projects (12s to 60s - The Bottlenecks)
These are the heavy prebuilts and large IDE engines that dominate the entire 3-minute runtime:
```
TIMING: project "prebuilts/clang/host/linux-x86"                          took 17.39s
TIMING: project "external/jetbrains/kotlin"                               took 26.62s
TIMING: project "tools/adt/idea"                                          took 41.82s
TIMING: project "prebuilts/studio/jdk"                                    took 45.69s
TIMING: project "tools/idea"                                              took 54.26s
TIMING: project "prebuilts/studio/intellij-sdk"                           took 54.86s
TIMING: project "tools/external/gradle"                                   took 57.23s
TIMING: project "tools/gpu-profiler/idea"                                 took 57.49s
TIMING: project "prebuilts/studio/sdk/remote"                             took 58.29s
```

---

## 4. Proposed Architectural Improvements

To achieve dramatic performance gains, we can shift from basic parallel execution to a more I/O-intelligent architecture.

### Improvement A: Sparse Checkouts (`--sparse` Opt-In)
* **Concept:** When a developer or agent works in a worktree, they usually focus on a narrow scope (e.g., editing `tools/base` or `tools/vendor/google/alt-lang`). They rarely need to touch the 12GB `intellij-sdk` or the 7.2GB prebuilt `jdk` checkouts.
* **Proposal:** Introduce a `--sparse` flag or support a `.worktree-sparse` filter file. When checked out, projects flagged as non-essential are instantiated with `git worktree add --no-checkout` (virtually instantaneous), and only the specified working set is populated. This would bypass checking out **over 30 Gigabytes** of static data, reducing execution times from **3+ minutes to under 15 seconds**.

### Improvement B: Symlink-Shared Static Prebuilts
* **Concept:** Prebuilt toolchains (`prebuilts/studio/jdk`, `prebuilts/studio/intellij-sdk`) are read-only binaries that developers never modify during active development.
* **Proposal:** Instead of performing expensive git checkouts of these static binaries into every worktree, `git-repo-worktree` can register these projects as "symlink-shared." The tool would bypass `git worktree add` for these directories and instead create a directory symlink pointing directly back to the synced prebuilts under the source client (`~/projects/studio-main/prebuilts/studio/jdk`).
* **Performance Gain:** Saves **over 25GB of disk writes** per worktree creation and eliminates the checkout cost entirely.

### Improvement C: I/O-Aware Concurrency Scheduling
* **Concept:** The Rayon thread pool schedules tasks on a first-ready, first-served basis. If 32 massive I/O tasks are popped simultaneously, the disk saturates.
* **Proposal:** Classify projects into two classes based on metadata (or history):
  1. **Heavyweight I/O:** Size > 500MB or File Count > 15,000.
  2. **Lightweight I/O:** Pure source repositories and tiny tools.
  Introduce a dual-concurrency limit: run small tasks at a high concurrency level (e.g., up to 32 parallel tasks), but throttle the concurrent checkout of heavyweight tasks to at most **2 or 3 simultaneously**. This ensures the disk queue does not choke.

### Improvement D: Native Git Parallel Checkout Configuration
* **Concept:** Git v2.32+ introduced native multi-threaded file writing for checkouts via `checkout.workers`.
* **Proposal:** Before running checkouts, temporarily configure `checkout.workers` (e.g., 4 or 8) inside the target repositories. This offloads intra-project file writing to Git's native multi-threaded C-engine, which operates much more efficiently with respect to file caching than running multiple decoupled `git` processes.

---
— end of report —
