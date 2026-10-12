# Track 2 Technical Spike: Eliminating `symlink-shared` Pitfalls & High-Speed Worktree Population (FUSE & CoW Benchmarks)

## Executive Summary

We investigated the environment capabilities of this machine (`jomof-specialist`, Linux `7.1.6-1rodete1-amd64`), audited [`src/tree.rs`](../src/tree.rs) and [`spec-fast-add.md`](../spec-fast-add.md), and executed concrete benchmark spikes in `/tmp/spike-fast-worktree` against real `/usr/local/google/home/jomof/projects/studio-main` projects (`tools/idea` with 274,366 files / ~2 GB, `prebuilts/studio/jdk` with 7.2 GB, and all 56 projects concurrently).

### Key Empirical Findings
1. **Why `symlink-shared` was created — and why `reflink` doesn't help here:**
   - `/usr/local/google/home/jomof` is `ext4` and `/tmp` is `tmpfs`. Both return `EOPNOTSUPP` (`Operation not supported`) for `FICLONE` (`cp --reflink=always`). Consequently, `try_reflink_project()` in [`src/tree.rs:1175`](../src/tree.rs#L1175) **always fails on the first file** on this machine and falls back to full Git checkout.
   - Materializing `tools/idea` (274,366 files) via standard `git worktree add` takes **114.3s** to create and **31.3s** to remove (**145.6s total**).
   - Currently in `studio-main`, **21 of 56 active projects** use `symlink-shared`: 19 `clone-depth="1"` prebuilts plus `tools/idea` and `tools/gpu-profiler/idea` (configured in `.repo/repo-worktrees/defaults.json`).
2. **Hardlink farms (`cp -al`) are strictly worse than `symlink-shared`:**
   - Creating a hardlink farm of `tools/idea` on the same `ext4` filesystem takes **89.4s** and deleting it (`rm -rf`) takes **61.5s** (**150.9s total** — slower than a real `git worktree add`!).
   - Fails across filesystems (`EXDEV` when creating worktrees in `/tmp`).
   - Still shares the exact `ext4` inode (`st_ino`), so in-place writes (`O_WRONLY | O_TRUNC`) from build tools or editors immediately corrupt `~/projects/studio-main`, and `link(2)` updates `st_ctime` on all 274,366 source inodes, invalidating `studio-main`'s Git index stat cache!
3. **FUSE Copy-on-Write (`fuse-overlayfs` + `git worktree add --no-checkout` + Index Copy) gives sub-second creation with 100% Git and workspace isolation:**
   - `/usr/bin/fuse-overlayfs` (v1.14) and `/usr/bin/fusermount3` are **already installed** on this machine, and `/dev/fuse` is world-accessible (`crw-rw-rw-`).
   - Mounting `fuse-overlayfs` over `tools/idea` (274k files) takes **19 ms**; over `prebuilts/studio/jdk` (7.2 GB) takes **230 ms**.
   - Running `git worktree add --no-checkout` across **all 56 projects in parallel** takes **0.858s**, and copying `.git/index` across all 56 projects takes **0.207s** (**1.064s total**).
   - Placing the new worktree's `.git` inside `upperdir/.git` shadows the source checkout's `.git` in `lowerdir`. The mounted `<wt>/<proj>` is a **real isolated Git worktree**:
     - Edits, untracked files, and build outputs land exclusively in `upperdir`; `~/projects/studio-main` is 100% untouched (verified by MD5 before/after).
     - `git status`, `git diff`, `git checkout`, and `git commit` inside `<wt>/<proj>` operate on the worktree's own `HEAD` and index, never touching `studio-main`.
     - `realpath` resolves inside `<wt>/<proj>` (not `studio-main`), eliminating symlink escape bugs in Bazel, IDEs, and compilers.
4. **Root Cause & Fix for FUSE `git status` Performance on 274k-File Repos:**
   - Out of the box, copying `<source>/.git/index` into a `fuse-overlayfs` worktree caused `git status` to take **>3.5 minutes** because the copied index recorded the `ext4` device ID (`st_dev = 64769`), whereas the FUSE mount has an anonymous device ID (`st_dev = 0,334`). Git's default `checkStat` saw a device mismatch on all 274,366 files and read + SHA-1 hashed all 2 GB of file contents over FUSE.
   - Setting `core.checkStat = minimal` and `core.trustctime = false` in the worktree config (or patching `sd_dev` in the copied index) **eliminated 100% of file hashing** (`diff-files` dropped to **0.012s**), bringing warm `git status` down to **12.82s** (and **`2.96s` with `git status -uno`**).
   - Furthermore, for `git repo-worktree remove` / `dirty` checks on CoW overlay projects, inspecting whether `upperdir` has any entries besides `.git` takes **< 0.05 ms (50 µs)** — completely avoiding `git status` on untouched 274k-file projects!
5. **Custom Built-in Rust FUSE Daemon (`fuser` + Linux 6.9+ `FUSE_PASSTHROUGH`) vs `fuse-overlayfs`:**
   - Because this machine runs Linux `7.1.6`, it supports **`FUSE_PASSTHROUGH`** (introduced in Linux 6.9), allowing a custom Rust FUSE daemon to attach backing `ext4` file descriptors so `read`/`write`/`mmap` run at native `ext4` kernel speed with zero userspace data copying.
   - Unlike `fuse-overlayfs` (which is single-threaded, ignores `attr_timeout`/`entry_timeout` flags, calls `getxattr` on every cold lookup, and exposes live `lowerdir` mutations if the source workspace later switches branches), a custom Rust FUSE daemon in `git-repo-worktree` can use multi-threaded lookup, long kernel dcache timeouts (`attr_timeout=86400`), zero xattr overhead, and **Git-object fallback** (serving blobs from `.repo/projects/<proj>.git` if the source checkout drifts from the pinned SHA).

---

## 1. Environment & Filesystem Capability Matrix

| Capability | Measured Result | Architectural Implication |
| :--- | :--- | :--- |
| **OS / Kernel** | `Linux 7.1.6-1rodete1-amd64` | Supports `FUSE_PASSTHROUGH` (>= 6.9) and unprivileged userns `overlay` mounts (>= 5.11). |
| **Home Filesystem (`/usr/local/google/home/jomof`)** | `ext4` (`rw,relatime,errors=remount-ro`) | Does **not** support `FICLONE` reflink (`cp --reflink=always` -> `EOPNOTSUPP`). |
| **Temp Filesystem (`/tmp`)** | `tmpfs` (`119G`, `inode64`) | Cross-device from `/home` (`EXDEV` on hardlinks/rename); also `EOPNOTSUPP` for `FICLONE`. |
| **FUSE Binaries & Device** | `/usr/bin/fuse-overlayfs` (`1.14-1+build8`), `/usr/bin/fusermount3`, `/dev/fuse` (`crw-rw-rw-`) | Ready to use immediately in ambient user namespace without root or apt install. |
| **Available Apt Packages** | `libfuse3-dev`, `bindfs`, `squashfuse`, `unionfs-fuse` | Available for Rust `fuser`/`fuse3` builds or auxiliary mounts. |
| **Unprivileged User Namespaces** | `kernel.unprivileged_userns_clone = 1` | `unshare -U -r -m` and in-kernel `mount -t overlay` work without root. |
| **Kernel Overlay Visibility** | Scoped to userns (`move_mount` to host ns denied by kernel) | Kernel `overlayfs` requires running commands inside `nsenter`/`unshare`; ambient visibility (for IDEs, host shells, Bazel server) requires FUSE (`fuse-overlayfs` or custom FUSE). |

---

## 2. Benchmark Table (`tools/idea` — 274,366 files)

| Strategy | Populate / Mount Time | Teardown / Remove Time | Cold `git status` | Warm `git status` (`-uno`) | Source Mutation Protection | `realpath` Stays in Worktree? | Isolated Git `HEAD`/Index? |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Full `git worktree add`** (current `worktree` mode) | `114.283s` | `31.336s` | `15.666s` | `6.368s` | Yes (full copy) | Yes | Yes |
| **`symlink-shared`** (current fast mode) | `0.001s` | `0.001s` | `15.666s` (on source!) | `6.368s` (on source!) | **NO (live corruption!)** | **NO (escapes to source!)** | **NO (hijacks source `.git`!)** |
| **Hardlink Farm (`cp -al`)** | `89.353s` | `61.546s` | `16.1s` | `6.5s` | **NO (in-place write corrupts source!)** | Yes | Yes |
| **`fuse-overlayfs` + `--no-checkout` + `cp index`** *(naive default config)* | **`0.315s`** *(66ms wt + 230ms idx + 19ms mount)* | **`0.080s`** | `>30m` / `153.6s` *(hashes 2GB due to `st_dev` mismatch)* | `212.4s` | **Yes (100% CoW)** | **Yes** | **Yes** |
| **`fuse-overlayfs` + `--no-checkout` + `cp index` + `checkStat=minimal` + `trustctime=false`** | **`0.315s`** | **`0.080s`** | `106.97s` *(81.8s cold FUSE `lstat` + 24.8s `readdir`)* | **`12.82s`** (**`2.96s`** with `-uno`) | **Yes (100% CoW)** | **Yes** | **Yes** |
| **Kernel `overlayfs` in Userns (`unshare -Urm`) + `--no-checkout` + `cp index`** | **`0.310s`** | **`0.050s`** | **`11.369s`** | **`11.925s`** | **Yes (100% CoW)** | **Yes** | **Yes** |
| **All 56 Projects: Parallel `--no-checkout` + `cp index` (16 jobs)** | **`1.064s` total** *(0.858s wt + 0.207s idx)* | **`~0.4s`** | N/A | N/A | Base for CoW / sparse modes | Yes | Yes |

---

## 3. Deep-Dive on FUSE `st_dev` Index Matching & Two-Phase Evolution

### 3.1 Why Naive `fuse-overlayfs` Was Slow on `git status` — and How We Fixed It
Using `stat` and `GIT_TRACE_PERFORMANCE=1` on `tools/idea`:
- On `studio-main/tools/idea/AGENTS.md` (`ext4`): `Device: 253,1` (`st_dev = 64769`), `Inode: 71978480`.
- On `merged/AGENTS.md` (`fuse-overlayfs`): `Device: 0,334` (anonymous FUSE superblock), `Inode: 71978480` (stable!), `Modify` & `Change` timestamps identical to nanosecond precision.
- **Root Cause:** Because Git's default `core.checkStat = default` compares `st_dev` from `lstat()` against `sd_dev` in `.git/index`, every single one of the 274,366 entries failed `match_stat_data()`! Git assumed all 274,366 files were potentially modified and read + SHA-1 hashed all 2 GB of files through FUSE.
- **Fix:** Setting `git config core.checkStat minimal` and `git config core.trustctime false` (or rewriting the 32-bit `sd_dev` field in the 44 MB `.git/index` while copying it) reduced `diff-files` from **>150s** to **0.012s** (zero file reads/hashes!).

### 3.2 Instant `dirty` / `remove` Detection via `upperdir` Inspection (< 0.05 ms)
In a CoW overlay (`fuse-overlayfs` or custom FUSE), `lowerdir` is never mutated; **every file modification, addition, chmod, or deletion (whiteout) inside `<wt>/<proj>` is written into `<registry>/<tree>/overlays/<proj>/upper`**.
- When `<wt>/<proj>` is freshly mounted, `upper/` contains **only** the `.git` entry.
- Therefore, `dirty_projects()` ([`src/tree.rs:1968`](../src/tree.rs#L1968)) does **not** need to run `git status` on overlay projects whose `upper/` directory contains only `.git`!
- Checking `std::fs::read_dir(&upper)` takes **< 50 microseconds** per project, and only runs `git status` if `upper/` actually contains modified/untracked files.

### 3.3 Comparison: Current (`symlink-shared`) vs. Phase 1 (`fuse-overlayfs`) vs. Phase 2 (Built-in Rust FUSE + `FUSE_PASSTHROUGH`)

| Metric / Property | Current (`symlink-shared`) | Phase 1 (`fuse-overlayfs` + `--no-checkout` + Index Patch) | Phase 2 (Built-in Rust FUSE + `FUSE_PASSTHROUGH` + In-Memory Stat Arena) |
| :--- | :--- | :--- | :--- |
| **`add` Time (`studio-main` 56 projects)** | `~6–8s` | **`~6–8s`** (even faster since `prebuilts/studio/sdk/linux` isn't demoted!) | **`~2–5s`** |
| **Edits Mutate `studio-main`?** | **YES (Silent Bug)** | **NO (100% Isolated in `upperdir`)** | **NO (100% Isolated in `upperdir`)** |
| **`git checkout/reset` Mutates `studio-main`?** | **YES (Silent Bug)** | **NO (Isolated `.git` worktree)** | **NO (Isolated `.git` worktree)** |
| **`realpath` Escapes Worktree?** | **YES (Silent Bug)** | **NO** | **NO** |
| **`remove` Detects Dirty Shared Projects?** | **NO (Silent Data Loss)** | **YES (in 50 µs via `upperdir` check)** | **YES (in 50 µs via `upperdir` check)** |
| **Cold `git status` on `tools/idea` (274k files)** | `15.7s` (mutates source index) | `107s` cold / **`12.8s` warm** (`2.9s` `-uno`) | **`~1.5–3s` cold & warm** (served from RAM stat arena) |
| **Survives `studio-main` Switching Branches?** | **NO (Drifts live)** | Drifts only if lowerdir files change | **YES (Falls back to `.git` packfile blob)** |
| **Implementation Complexity** | Already exists | **Low (~250 LoC in `src/tree.rs`, uses installed `/usr/bin/fuse-overlayfs`)** | **Medium (~800 LoC Rust `fuser` + `FUSE_PASSTHROUGH`)** |
