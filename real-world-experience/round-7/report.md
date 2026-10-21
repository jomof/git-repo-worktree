# Round 7: Real-World Feedback & Live Telemetry Evaluation (`v2.2.1`)

| Field | Value |
| :--- | :--- |
| Tool version | `git-repo-worktree 2.2.1` (`HEAD b0a0d532cedd`) |
| Clients surveyed | `~/projects/studio-main` (56 projects, 38 live trees, 892 telemetry events) and `~/projects/main` (AOSP, 1,084 projects, 4 live trees, 122 telemetry events) |
| Host & filesystem | `jomof-specialist`, Linux `7.1.6-1rodete1-amd64`, 96 CPUs, `git 2.56.0`, `ext4` at 96% used (`348 GiB` free of `7.5 TiB`) |
| Originating workload | `evalbox` (`EVAL-ISOLATION-DESIGN.md` §3, §7.7, §14.1) plus continuous multi-agent production usage on `jomof-specialist` (`2026-10-12` to `2026-10-21`) |
| Sanitized machine data | [`./data/`](./data/) (`studio-main/`, `main/`, and [`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt)) |

## Executive Context

`evalbox` needs $N$ byte-identical, writable, isolated `studio-main` trees for paired agent evaluations. Its design currently uses `git repo-worktree add` only to build a single baseline and then works around the tool in four ways:

1. It freezes the baseline with `chmod -R a-w` and a BLAKE3 digest.
2. It forces `core.checkStat=minimal` everywhere.
3. It rewrites `env.bazelrc` in each worker.
4. It mounts its own kernel overlays on top.

This report evaluates which of those workarounds point to real gaps in `git-repo-worktree`, verifies every claim against `v2.2.1` (`b0a0d53`) source code and 1,014 real-world `history.jsonl` events across `studio-main` and `main`, and adds three live discoveries uncovered while auditing this machine:

- **F1 (Confirmed & live self-perpetuating cycle reproduced):** Killing the wedged `fsmonitor--daemon` (`PID 233316`) and removing the probe tree returned its worktree admin dir (`-filling-1792593082103359931-141343-1`) to `.pool`. Nine minutes later, another agent's `add --name prefab-arm2` consumed that spare and immediately spawned **a new leaked daemon (`PID 1927772`) on the exact same admin dir**, growing the leaked `tools/idea` daemon count to **9**.
- **F2 (Expanded into F2a + F2b):** Beyond inherited caller descriptors across all **300 `fuse-overlayfs` daemons** and detached `pool maintain` (**23–31 open FDs each**, not just `fd 3`), `src/registry.rs` contains a `Box::leak(Box::new(RwLock::new(file.try_clone()?)))` bug (`acquire_within`) that leaks a file descriptor on **every lock acquisition and every 100 ms contention retry**.
- **F6 (New operational finding from live telemetry):** A single untracked runtime directory (`?? environments/`) inside `~/projects/studio-main/prebuilts/studio/sdk/linux` silently demotes that **10.39 GB / 213,697-file** shallow prebuilt from `overlay` (`~200 ms`, `0 B`) into `.pool` (**11.27 GB per spare — 63.3% of total pool disk usage**), causing background `pool maintain` runs at `ionice -c 3` to spend **17–23 minutes** refilling `sdk/linux` first while all 7 actual source repos sit at `0 spares`.

Items below are in priority order. Each includes verified evidence, exact `b0a0d53` code locations, a proposed change, and how to verify it.

---

## F1. Pool spares spawn `fsmonitor--daemon`s that later wedge `add` (bug, critical)

### What happened
`git repo-worktree add` of probe tree `feedback-probe` (`2026-10-21T15:32:57Z`) took **372.03 s** (`372,034 ms`, `projects` phase `368,873 ms` in [`probe-and-followup-events.json`](./data/studio-main/probe-and-followup-events.json)) instead of the normal 5–8 s pool hit. The pool had depth 1 at the time.

- Every project finished in `<5 s` except `tools/idea`. Its `git checkout -q --detach da85120…` sat in `S` state with **0 s of CPU for over 5 min** at kernel `wchan = unix_stream_read_generic`.
- Its `fd 8` was connected to `fsmonitor--daemon.ipc`, owned by `PID 233316`.
- `PID 233316` had been started ~1 hour earlier by **pool maintenance / return**:
  - `GIT_TRACE2_PARENT_NAME=_run_dashed_/worktree/reset/fsmonitor--daemon`
  - `GIT_DIR=…/tools/idea.git/worktrees/-filling-1792593082103359931-141343-1`
  - `GIT_WORK_TREE=.` with `cwd` `$HOME`
  - `/proc/233316/environ` also retained caller environment metadata from an unrelated earlier `add` (`studio-wt-aragorn`).
- Running `kill 233316` let `git checkout` finish within seconds (`tools/idea` recorded `368,690 ms`), and `add` completed normally.

### Live proof that `remove` + `add` immediately re-triggers the leak
At `15:33:30Z`, `git repo-worktree remove feedback-probe` returned clean `tools/idea` back into `.pool` via `pool::give_back` ([`../../src/pool.rs#L420-L469`](../../src/pool.rs#L420-L469)), keeping the exact admin dir name `-filling-1792593082103359931-141343-1`. Nine minutes later (`15:42:59Z`), another agent ran `git repo-worktree add --name prefab-arm2`, which claimed that slot via `pool::take` ([`../../src/pool.rs#L368-L417`](../../src/pool.rs#L368-L417)). Its `git checkout` immediately spawned **a brand-new `fsmonitor--daemon` (`PID 1927772`) bound to the exact same `GIT_DIR = …/worktrees/-filling-1792593082103359931-141343-1`** (captured live in [`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt) lines 94–104).

### Why it happens
- `~/projects/studio-main/tools/idea/.git/config` sets `core.fsmonitor=true` locally (`tools/idea` is the only project in `studio-main` with this setting; see [`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt) lines 238–258).
- Linked worktrees inherit `core.fsmonitor=true` from the common `.git/config`. Every Git command that reads/updates the index (`git worktree add`, `git checkout`, `git clean`, `git status`) auto-starts a detached `fsmonitor--daemon` for that worktree's admin dir.
- Even worse, `pool::fill_one` ([`../../src/pool.rs#L1188-L1211`](../../src/pool.rs#L1188-L1211)) runs `git worktree add --detach .pool/tools%2Fidea/.filling-<name> HEAD` (spawning a daemon watching `.filling-<name>`) and then moves the directory to `.pool/tools%2Fidea/<name>` — so **every pooled spare's daemon is already watching a non-existent path before any `add` ever claims it**.
- When `pool::take` later moves `<name>` to `<tree>/tools/idea` and runs `git checkout` ([`../../src/pool.rs#L391-L411`](../../src/pool.rs#L391-L411)), the IPC socket lives in the preserved `.git/worktrees/-filling-…` admin dir, so `git checkout` connects to the stale daemon and can block indefinitely on `unix_stream_read_generic`.

### Scale on this machine right now
**9 tool-spawned `tools/idea` daemons** (out of 10 total `fsmonitor--daemon` processes on the host), started from tool-driven `checkout` or `worktree/reset`, some running for **6.7 days** (full sanitized dump in [`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt)):

| PID | Age | `GIT_DIR` (under `tools/idea.git/worktrees/`) | `GIT_TRACE2_PARENT_NAME` |
| :--- | :--- | :--- | :--- |
| 3499576 | `6-16:03` | `idea8` | `_run_dashed_/worktree/reset/fsmonitor--daemon` |
| 3638579 | `6-15:59` | `idea9` | `_run_dashed_/worktree/reset/fsmonitor--daemon` |
| 138033 | `01:22` | `-filling-1792564571836697264-2005375-9` | `_run_dashed_/checkout/fsmonitor--daemon` |
| 179353 | `01:20` | `-filling-1792570573468372694-255041-1` | `_run_dashed_/checkout/fsmonitor--daemon` |
| 190569 | `01:20` | `idea4` | `_run_dashed_/worktree/reset/fsmonitor--daemon` |
| 208660 | `01:19` | `idea5` | `_run_dashed_/worktree/reset/fsmonitor--daemon` |
| 233316 | *(killed at 01:01)* | `-filling-1792593082103359931-141343-1` (**wedged `feedback-probe` `add` for 369 s**) | `_run_dashed_/worktree/reset/fsmonitor--daemon` |
| 348733 | `01:12` | `-filling-1792593417769484726-141343-9` | `_run_dashed_/worktree/reset/fsmonitor--daemon` |
| 1927772 | `00:06` | `-filling-1792593082103359931-141343-1` (**re-spawned 9 min later by `prefab-arm2` `add`!**) | `_run_dashed_/checkout/fsmonitor--daemon` |

### Proposed change
1. **Pass `-c core.fsmonitor=false` on every `git` invocation spawned by `git-repo-worktree`** (and `-c core.untrackedCache=false` on pool staging/reset operations). Centralize this in a single `git_command()` helper in `src/util.rs` so no call site in `pool.rs`, `tree.rs`, `overlay.rs`, `status.rs`, or `repair.rs` can accidentally trigger daemon auto-spawn. Notably, `overlay::populate` already disables fsmonitor on the copied index via `update-index --no-fsmonitor --no-untracked-cache` ([`../../src/overlay.rs#L261-L265`](../../src/overlay.rs#L261-L265)), confirming this is already a known hazard on overlay indices—it just was never applied to pool staging (`fill_one`, `pool.rs#L1190-L1211`), pool checkout (`take`, `pool.rs#L395-L411`), pool return (`give_back`, `pool.rs#L453-L467`), or direct checkouts.
2. **Stop any user-started daemon before moving or removing a worktree**: before `git_worktree_move` (`pool::give_back`, `pool::fill_one`) or `git worktree remove` (`remove` / `trash`), run `git -c core.fsmonitor=false -C <wt> fsmonitor--daemon stop` (ignoring non-zero exit when none is running). That handles daemons spawned interactively while a user or agent worked inside `<tree>/<project>`.
3. **Clean up orphaned daemons in `repair` and `pool maintain`**: detect running `fsmonitor--daemon` processes whose `GIT_DIR` points to a `.pool` slot, a non-existent worktree, or a `-filling-*` admin dir whose worktree moved, stop them, and surface the count in `--json`.
4. **Per-project command timeout / stall diagnostic**: if a child `git checkout` / `git worktree add` makes zero progress for >60 s, log which project and child PID is stalled (and optionally time out cleanly rather than hanging silently for 6+ minutes).

### Verification
- Integration regression test: set `git -C <source>/<proj> config core.fsmonitor true`, run `pool fill`, `add`, run `git -C <tree>/<proj> status` (which spawns a user-session daemon), `remove`, and `add` again. Assert zero `fsmonitor--daemon` processes survive on pooled slots and both `add` runs complete cleanly without stalling.
- On `studio-main`: `pgrep -af 'fsmonitor--daemon'` does not grow across `pool fill` -> `add` -> `remove` -> `add`.

---

## F2. File descriptor leaks: into long-lived child daemons (F2a) and inside `registry.rs` lock acquisition (F2b) (bug, medium-high)

Auditing `/proc/<pid>/fd` on `jomof-specialist` confirmed the reported child FD leak—and revealed both that the external leak is much wider than `fd 3` (**F2a**) and that `git-repo-worktree` itself leaks file descriptors on every lock operation in `src/registry.rs` (**F2b**).

### F2a. Long-lived `fuse-overlayfs` daemons and `pool maintain` inherit 20+ caller sockets, pipes, and log files

#### Evidence
Inspecting `/proc/<pid>/fd` across all **300 active `fuse-overlayfs` daemons** and the background `pool maintain` process ([`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt) lines 142–236):
- Every `fuse-overlayfs` daemon (`PID 136587`, `PID 4035493`, etc.) holds **23 to 29 open descriptors**, not just `fd 3`:
  - `fd 3` (`socket:[…]` from the `/usr/bin/git` wrapper)
  - `fd 4 -> /proc/3310579` and `fd 7 -> /proc/3310579/smaps` (from the calling `jetski-hub-server` parent!)
  - `11–16` high-numbered sockets (`fd 80`, `204`, `612`, `933`, `1180`, `1254`, `1412`, `1688`, …) and pipes (`fd 1423`) inherited from `jetski-hub-server`
  - `fd 1242 -> /usr/local/google/tmp/jetski-hub-server…BINARY_INFO…`
- Detached `git-repo-worktree -C … -q pool maintain` (`PID 1645076`, spawned via [`../../src/pool.rs#L1131-L1139`](../../src/pool.rs#L1131-L1139)) inherits the exact same ~20 caller sockets, `/proc/3310579/smaps`, and `BINARY_INFO` log file—and passes them down further into any child `git` processes it spawns.
- Similarly, the blocked `git checkout` during F1 held **~1,500 open FDs** inherited from its parent agent host.

#### Impact
- **300 permanent `fuse-overlayfs` daemons** keep caller RPC sockets/pipes open for days after the originating agent session closes, preventing EOF delivery and pinning deleted/rotated log files on an already 96%-full disk.
- Under harnesses like `evalbox`, inherited FDs leak parent harness descriptors across privilege/subject boundaries.

#### Proposed change (F2a)
- Add a small helper (e.g. `close_extra_fds(&mut Command)` using `libc::syscall(SYS_close_range, 3, !0u32, CLOSE_RANGE_CLOEXEC)` in `CommandExt::pre_exec` on Linux, falling back to iterating `/proc/self/fd` on macOS/older kernels) and apply it to:
  1. `fuse-overlayfs` mount spawn ([`../../src/overlay.rs#L172-L182`](../../src/overlay.rs#L172-L182))
  2. Detached `pool maintain` and background trash deletion spawns ([`../../src/pool.rs#L1131-L1139`](../../src/pool.rs#L1131-L1139), `src/trash.rs`)
  3. Child `git` commands (or run `CLOSE_RANGE_CLOEXEC` once near the top of `main()` so **every** child process spawned anywhere in `git-repo-worktree` starts clean automatically!).

### F2b. `registry::acquire_within` leaks 1 open FD + heap allocation on every lock call and every 100 ms contention retry

#### Evidence
In [`../../src/registry.rs#L67-L101`](../../src/registry.rs#L67-L101):
```rust
    loop {
        let attempt = file
            .try_clone()
            .map_err(|error| AppError::io(format!("open lock {}", path.display()), error))?;
        let lock: &'static mut RwLock<File> = Box::leak(Box::new(RwLock::new(attempt)));
        match lock.try_write() {
            Ok(mut guard) => {
                ...
                return Ok(Held { _guard: guard });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
```
- `Held` holds only `RwLockWriteGuard<'static, File>`. Dropping `Held` unlocks `flock`, **never drops `RwLock<File>`**, and therefore **never closes `attempt`**!
- Confirmed live in `/proc/1645076/fd` ([`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt) lines 213–220): `pool maintain` still holds open descriptors `fds 8..15` to all 8 `.pool/*.lock` files long after releasing them.
- Worse: inside the contention `loop` (`100 ms` sleep at line 117), **every single `WouldBlock` retry calls `file.try_clone()?` and `Box::leak(...)` again**, leaking **10 open file descriptors per second**! Under a conservative `RLIMIT_NOFILE = 256` (macOS default) or `1024` (Linux default), a command waiting for `--lock-timeout 300` exhausts its entire FD table in **25.6 s** (macOS) or **102.4 s** (Linux) and crashes with `E_IO: Too many open files` instead of waiting cleanly or returning `E_LOCKED`.

#### Proposed change (F2b)
Replace `fd-lock` + `Box::leak` in `src/registry.rs` with direct `libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)` (or `File::try_lock()` stabilized in Rust 1.89!) directly on `file`, returning `Held { _file: file }`. Dropping `File` automatically releases the kernel `flock` and closes the descriptor—zero `try_clone`, zero `Box::leak`, and constant 1 FD regardless of retry count.

### Verification (F2a + F2b)
- Run `add` from a parent process holding 100 open dummy FDs; assert `/proc/<fuse-overlayfs-pid>/fd` has `<= 7` entries (stdin/out/err, `lowerdir`, `upperdir`, `workdir`, `/dev/fuse`).
- Unit test for `registry::acquire_within`: hold a lock in thread A, let thread B contend for 2 seconds (`~20` retries) and then acquire after release; assert `/proc/self/fd` count returns to baseline after `Held` drops.

---

## F3. Overlay and symlink-shared projects are live views; there is no frozen-tree option (feature, medium)

### Evidence
- [`../../docs/population.md`](../../docs/population.md):
  - *"Because the view is live, a source checkout that later changes branch shows up as modifications in the overlay."*
  - Symlink-shared projects *"are live views of the source and should be treated as read-only."*
- In a clean probe tree on `studio-main`, **19 of 55 projects** are `fuse-overlayfs` mounts with `lowerdir=~/projects/studio-main/<project>` (`2,714` total overlay mounts recorded across `add` events in [`studio-main/report.txt`](./data/studio-main/report.txt); `300` live mounts currently active on the host).
- `studio-main` is the human developer's active client: local branches (`mother`, `part/*`) and `repo sync` runs routinely mutate checkouts in `studio-main`.

### Why `evalbox` worked around it
A paired evaluation requires that worker $k$ at minute 20 sees the exact same bytes as worker $1$ at minute 0. With live `lowerdir` overlays, a `repo sync` or branch switch in `studio-main` silently mutates every live tree's overlaid projects—and Linux overlayfs/fuse-overlayfs exhibits undefined behavior (stale dentries, spurious diffs, `ESTALE`) when a mounted lower layer mutates underneath an active mount. Consequently, `evalbox` currently materializes one full copy, runs `chmod -R a-w`, hashes it with BLAKE3, and overlays its own copy (~40 GiB of extra work).

### Proposed change (in increasing scope)
- **a. Source drift detection in `status` (cheap, high value):** at `add`, record each shared/overlaid source project's `HEAD` SHA and `.git/index` mtime/hash in tree metadata (`meta.json`). Have `status` / `status --json` report `"source_drift": [<project>, ...]` and warn clearly whenever a live tree's lower layer has moved or become dirty since `add` (and note unmounted overlays explicitly—19 older trees on this host currently have `overlays_unmounted > 0` after earlier restarts; see [`studio-main/list.json`](./data/studio-main/list.json)).
- **b. Discoverable `--isolated` alias / documentation for `--full`:** `--full` already forces every project onto `worktree`/`.pool` without `overlay` or `symlink-shared`, decoupling trees from source-checkout mutations, though the flag name `--full` sounds like "all manifest groups/projects" rather than "no shared/live lower layers." Adding `--isolated` (or `--no-share`) as an explicit alias or prominently documenting `--full` in `add --help` and `docs/population.md` immediately answers *"how do I guarantee source client edits cannot affect this tree?"*
- **c. Content-pinned immutable lower snapshots (larger, eliminates 40 GiB workaround):** for shallow/fixed-depth projects, mount `fuse-overlayfs` over a tool-managed read-only lower snapshot `.repo/repo-worktrees/.lower/<project>@<sha>` rather than the live user checkout `<source>/<project>`. Multiple trees at the same `<sha>` share the same immutable lower directory; refcount/GC removes unreferenced `@<sha>` dirs on `remove` / `pool maintain`.

### Verification
- For (a)/(c): switch branches or dirty a file inside `<source>/prebuilts/tools` after `add`. Under (a), `status --json` reports `source_drift: ["prebuilts/tools"]`; under (c), `<tree>/prebuilts/tools` file bytes and `git status` remain completely unchanged.

---

## F4. No way to start a tree from a pinned manifest or upstream revision (feature, medium)

### Evidence
- `add --help` states by design: *"every project starts on a detached HEAD at the commit that project currently has checked out in the source client … `add` never fetches and never resets to the remote branch."*
- Downstream skills (`repo-worktree` §3) and harnesses currently script manual post-`add` checkout loops (`for p in ...; git checkout --detach goog/studio-main`), where forgetting any co-evolving repository (`tools/base`, `tools/adt/idea`, `tools/vendor/google`, `prebuilts/tools`) silently breaks the build.
- Live `add` runs on `studio-main` also warn when local extra projects differ from manifest state (`included 4 synced checkout(s) absent from repo manifest output; pinned from source HEAD`).
- `evalbox`'s `repo-baseline` provider specifically needs *"a tree at a pinned manifest snapshot or upstream ref"* without disturbing the human's dirty/branched `studio-main` working copy.

### Proposed change
- **`add --manifest <file>`**: accept a `repo manifest -r` XML file, parse each project's pinned revision SHA locally (no network fetch; fail cleanly with a structured `--json` list of missing SHAs if `repo sync` hasn't fetched them yet), and populate every project directly at that exact SHA in a single pass—instead of checking out the source `HEAD` first and running a second checkout loop afterward.
- **`add --from-manifest` / `add --at-upstream`** (optional convenience): resolve revisions from `.repo/manifest.xml` (`revision` / `upstream` remote-tracking refs already fetched in `.repo/projects/*.git`) rather than whatever local feature branches happen to be checked out in the human's working tree.
- **Registry `tree_digest`**: record the resolved `(project_path, mode, pinned_sha)` vector hash in `meta.json` and expose `tree_digest` in `list --json` / `status --json` so harnesses can verify two trees started from identical commits in $O(1)$.
- **Clean interaction with F3**: when a project's requested SHA differs from `<source>/<project>`'s current `HEAD`, existing preflight rules already skip `overlay`/`symlink-shared` and populate from `.pool` / `worktree` at the target SHA automatically.

---

## F5. Outdated root performance report misleads downstream designs (docs, low effort)

### Evidence
[`../../real-world-performance-studio-main.md`](../../real-world-performance-studio-main.md) at the repository root describes the **v1 pre-pool, pre-overlay** architecture: `3m 10s` wall time, `~37 GiB` written per tree, and "Proposed Improvements A–D" as unimplemented future work (whereas `symlink-shared`, `fuse-overlayfs` overlays, `.pool` spares, `--heavy-jobs`, and parallel `checkout.workers` are all shipped in `v2.2.1`). Because that file sits prominently at the repo root with no version banner, `EVAL-ISOLATION-DESIGN.md` §3 read it as current fact to conclude *"Full worktree copies (~37 GiB writes per worker) exhaust disk."*

Current empirical numbers on `jomof-specialist` (`v2.2.1`, [`studio-main/report.txt`](./data/studio-main/report.txt) & [`main/report.txt`](./data/main/report.txt)):

| Metric | Old root report (v1) | Measured on `v2.2.1` (`b0a0d53`) |
| :--- | :--- | :--- |
| `studio-main` `add` wall time | `3m 10s` (`190 s`) | **5–8 s** warm pool hit; **13.3 s p50 / 41.8 s p90** across 147 production `add`s (`372 s` max when hit by **F1**) |
| `main` (AOSP, 1,084 projects) `add` wall time | — | **20.5 s** warm pool / **23.8–31.4 s p50** post-Round 6 |
| Real disk per `studio-main` tree (`du -sh -x`) | `~37 GiB` written | **~18 GiB** real ext4 when `sdk/linux` is pooled (**~7 GiB** when clean/overlaid; **~2 GiB** net new writes when spares move in from `.pool`) |
| Overlay mounts per tree (`studio-main`) | — | **19–20 `fuse-overlayfs` mounts** (~68–79 GiB apparent, ~0 B written) |
| `post-add` / `pre-remove` hooks (`studio-main` p50) | — | **100 ms** (`n=122`) / **50 ms** (`n=96`) |
| First / second `git status` (`tools/adt/idea`, pooled) | — | **1.72 s** first, **0.68 s** second (`tools/base`: **0.75 s** / **0.45 s**) |

### Proposed change
Add a clear banner at line 1 of [`../../real-world-performance-studio-main.md`](../../real-world-performance-studio-main.md) marking it as a historical pre-v2 baseline (or move it under `real-world-experience/` / `docs/history/`) and link to current `v2.2.x` numbers.

---

## F6. One untracked runtime directory (`?? environments/`) in `prebuilts/studio/sdk/linux` silently demotes a 10.4 GB shallow prebuilt from `overlay` to `.pool`, starving source-repo refills for 17+ minutes (perf/UX, medium-high)

While inspecting live pool telemetry on `studio-main` ([`studio-main/pool-list.json`](./data/studio-main/pool-list.json), [`studio-main/report.txt`](./data/studio-main/report.txt), and [`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt)), we uncovered why `.pool` on `studio-main` was completely empty (`depth: 0` across all 8 projects) despite `pool maintain` running continuously for **16m 51s**.

### Evidence from live machine state
1. **The single stray directory**: `~/projects/studio-main/prebuilts/studio/sdk/linux` (`clone-depth="1"`, **213,697 files, 10.39 GB**) has zero modified tracked files—only a single untracked directory `?? environments/` created by SDK/emulator tooling ([`process-and-fd-survey.txt`](./data/process-and-fd-survey.txt) lines 259–261).
2. **Silent demotion from `0 B` overlay to `11.27 GB` pooled worktree**: `pool::source_is_clean` ([`../../src/pool.rs#L181-L188`](../../src/pool.rs#L181-L188)) runs `git status --porcelain=v1 -z --untracked-files=all`. Because `stdout` is non-empty, `prebuilts/studio/sdk/linux` fails overlay preflight (`104` `"overlay preflight changed"` events in [`studio-main/report.txt`](./data/studio-main/report.txt)) and falls into `.pool`:
   - `prebuilts/studio/sdk/linux` alone consumes **`11,266,306,481` bytes (`11.27 GB`) per spare** (`22.5 GB` in `.pool` + `11.3 GB` in every tree created while it isn't overlaid!).
   - All **7 actual source projects combined** (`tools/idea`, `tools/adt/idea`, `tools/base`, `external/jetbrains/kotlin`, `external/intellij-plugins`, `tools/vendor/google`, `tools/vendor/intellij/cidr`) total **`6,522,850,245` bytes (`6.52 GB`) per spare**. One untracked folder inside one shallow SDK repo **nearly triples** per-spare disk consumption (`6.52 GB` -> `17.79 GB`, **63.3% of the pool**) on a disk with only `348 GiB` free across 38 live trees!
3. **Head-of-line blocking in `pool maintain` (`12.5–22.6 min` per fill pass)**:
   - `plan.kept` sorts **largest `estimated_checkout_ms` first** ([`../../src/pool.rs#L336-L338`](../../src/pool.rs#L336-L338)), putting `prebuilts/studio/sdk/linux` (`213,697` files) **ahead of every source repository**.
   - Because `pool maintain` runs under `ionice -c 3` (idle disk priority) while active Bazel builds compete for NVMe I/O, refilling `prebuilts/studio/sdk/linux` takes **747 s to 1,355 s (12.5–22.6 minutes)** (`pool maintain` `p90_ms = 352,944`, `max_ms = 3,164,450` in [`studio-main/report.txt`](./data/studio-main/report.txt); observed live in `PID 1645076` spending `16m 51s` on `.filling-…-0` of `sdk/linux` while all 8 pooled repos remained at `spares: 0`!).
   - Similarly, background trash cleanup inside `pool maintain` held `.maintain.lock` for **795 s** and **3,164 s (52.7 min)** under heavy I/O load before any pool spares were refilled.

### Proposed change
1. **Surface overlay-demotion warnings prominently on large shallow projects**: when a shallow/prebuilt project (`>1 GB` or `>50,000` files) falls out of `overlay` solely due to source dirty/untracked files, emit a concise warning naming the project and reason (e.g. `note: prebuilts/studio/sdk/linux fell back from overlay to worktree (1 untracked entry: environments/; +11.3 GB)`), rather than silently recording `"overlay preflight changed"` only in telemetry.
2. **Consider ignoring untracked files on shallow/fixed-revision overlays (or whiteout masking)**: for `clone-depth="1"` prebuilts where all *tracked* files match `HEAD` (`git status --untracked-files=no` is empty), either allow `overlay` with overlayfs opaque/whiteout entries hiding any top-level untracked paths present at `add` time, or provide a config option (`overlay.ignoreUntrackedInShallow = true`).
3. **Avoid starving fast source spares behind multi-hundred-thousand-file checkouts & trash sweeps**:
   - In `pool maintain`, perform quick phase-1 refills of source repos (or refill smallest/fastest projects within `level 0 -> 1` first so 7 source repos finish in `<20 s` instead of waiting `17+ min` behind one `213k`-file SDK directory), and run `.trash` `rm -rf` **after** refilling `.pool` to depth 1 (not before!).

---

## Telemetry & Sanitized Data Attached in `./data/`

All files under [`./data/`](./data/) were captured on `jomof-specialist` on `2026-10-21` and checked to strip agent-harness session credentials/metadata while preserving exact Git/process/timing diagnostics:

- [`./data/process-and-fd-survey.txt`](./data/process-and-fd-survey.txt) — Live `df -h`, all 10 `fsmonitor--daemon` processes with sanitized `/proc/<pid>/environ` (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_TRACE2_PARENT_NAME`, `cwd`), `/proc/<pid>/fd` readlink dumps of oldest/newest `fuse-overlayfs` daemons (`300` running) and active `pool maintain` (`PID 1645076`, showing both inherited caller FDs and leaked `.pool/*.lock` FDs `8..15`), `tools/idea` local `.git/config`, and `prebuilts/studio/sdk/linux` status.
- [`./data/studio-main/report.txt`](./data/studio-main/report.txt) & [`./data/studio-main/report.json`](./data/studio-main/report.json) — Aggregate report over **892 events** (`2026-10-12` to `2026-10-21`) on `~/projects/studio-main` (38 live trees).
- [`./data/studio-main/probe-and-followup-events.json`](./data/studio-main/probe-and-followup-events.json) — Full `history.jsonl` records for `feedback-probe` (`372,034 ms` `add` wedged on `PID 233316`), `prefab-arm1`, and `prefab-arm2` (which reclaimed `-filling-1792593082103359931-141343-1` and spawned `PID 1927772`).
- [`./data/studio-main/pool-list.json`](./data/studio-main/pool-list.json), [`./data/studio-main/list.json`](./data/studio-main/list.json), [`./data/studio-main/archive-list.json`](./data/studio-main/archive-list.json), and [`./data/studio-main/history.jsonl`](./data/studio-main/history.jsonl) — Complete `studio-main` pool state, 38-tree inventory, archives, and 892-event history log.
- [`./data/main/report.txt`](./data/main/report.txt), [`./data/main/report.json`](./data/main/report.json), [`./data/main/pool-list.json`](./data/main/pool-list.json), [`./data/main/list.json`](./data/main/list.json), and [`./data/main/history.jsonl`](./data/main/history.jsonl) — Complete 122-event AOSP (`~/projects/main`, 1,084 projects) telemetry history.

---

## Considered and *not* tool problems

These workarounds in `EVAL-ISOLATION-DESIGN.md` stem from `evalbox`'s own threat model and blinding requirements. They are listed explicitly so `git-repo-worktree` does not spend engineering effort on them:

| `evalbox` workaround | Why it is *not* a `git-repo-worktree` problem |
| :--- | :--- |
| **`core.checkStat=minimal` everywhere** (`EVAL-ISOLATION-DESIGN.md` §14.1: `45.2 s` first `git status` -> `0.99 s`) | That `45 s` index rehash comes from `evalbox` mounting its own *kernel* `overlayfs` on top of an already-built tree, changing `st_dev` and inode numbers. `git-repo-worktree`'s own overlay path copies the source index and lets `fuse-overlayfs` expose matching stat data ([`../../src/overlay.rs#L329-L333`](../../src/overlay.rs#L329-L333)), yielding **0.45–1.72 s** first `git status` in our measurements. At most, a one-sentence note in `docs/population.md` for callers re-exporting trees over overlayfs/NFS suffices. |
| **Per-worker `env.bazelrc` / CartFS `output_base` rewrite** | `post-add` already gives each tree an isolated `output_base` and `pre-remove` runs `bazel shutdown` ([`../../docs/hooks.md`](../../docs/hooks.md)). `evalbox` only rewrites `env.bazelrc` because it clones one baseline tree $N$ times outside `git-repo-worktree`. Once **F3c** + **F4** make creating $N$ real pinned trees cheap and immutable, `evalbox` can call `git repo-worktree add` $N$ times directly. |
| **Kernel `overlayfs` inside an unprivileged user namespace** (`7–10 ms`) | Kernel overlays mounted inside an unprivileged `userns` are invisible outside that namespace. `git-repo-worktree` trees must be visible across host processes (IntelliJ IDEA, Bazel server daemons, shells, sibling agents), so `fuse-overlayfs` is the right architecture for the tool. |
| **Mounting every evaluation worker at the same canonical path** | Required solely so compiler DWARF paths and build artifacts do not leak A/B arm names to blind judges—purely an evaluation-harness concern. |

---

## Suggested Order of Work

1. **F1** (`-c core.fsmonitor=false` across all spawned `git` commands + `fsmonitor--daemon stop` before move/remove + orphan daemon reaper in `pool maintain`/`repair` + regression test) — highest urgency; eliminates silent 6-minute hangs and multi-day leaked daemons.
2. **F2b + F2a** (replace `Box::leak` + `try_clone` in `src/registry.rs` with direct `flock` on `File`, and apply `CLOSE_RANGE_CLOEXEC` before spawning `fuse-overlayfs`, `pool maintain`, and child processes) — small, self-contained, fixes FD exhaustion under contention and stops 300+ long-lived daemons from pinning caller sockets/logs.
3. **F6** (warn on high-cost overlay demotions, refill fast source repos before 10 GB prebuilts / before `.trash` sweeps in `pool maintain`, and optionally tolerate/whiteout untracked dirs in shallow prebuilts) — immediately restores warm-pool hit rates under disk pressure.
4. **F5** (mark root `real-world-performance-studio-main.md` historical and link current `v2.2` benchmarks) — 5-minute doc fix.
5. **F4** (`add --manifest <file>` + `tree_digest`), followed by **F3a/F3b** (`status` `source_drift` detection + `--isolated` alias/docs) and optionally **F3c** (content-pinned immutable `.lower/<project>@<sha>` snapshots).
