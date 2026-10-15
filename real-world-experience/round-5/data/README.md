# Round 5 Raw Data (`a9a71ee28036`)

Everything in this directory was captured on one Linux workstation (see [`environment.txt`](environment.txt)) using `git-repo-worktree 2.0.0 (a9a71ee28036)`. The analysis and tables are in [`../report.md`](../report.md), and the scripts that produced these files are in [`../scripts/`](../scripts/).

## Where to Start

- **Round 5 filtered history (`R5-*` sessions + `pool maintain` events):**
  - [`studio-main/history-r5.jsonl`](studio-main/history-r5.jsonl) (59 events across P1–P4)
  - [`main/history-r5.jsonl`](main/history-r5.jsonl) (30 events across S2, S4, S5)
  - Each `add` record includes the new `snapshot-*` sub-phases (`snapshot-repo-manifest`, `snapshot-git-dirs`, `snapshot-estimates`, `snapshot-preflight`), `projects-journal-sync`, `projects-meta-write`, `system_start`, `system_end`, top-level `reflink`, and per-project `queue_ms`.
  - Each `remove` record includes `phases`, `returned_to_pool`, and `pruned_git_dirs`.
  - Each `pool maintain` record includes `phases` (`plan`, `fill`, `reap`), `filled[]`, `trimmed[]`, `reaped[]`, `deferred[]`, `skipped`, `system_start`, and `system_end`.
- **Unfiltered full client history:**
  - [`studio-main/history-full.jsonl`](studio-main/history-full.jsonl) (all 516 events since 2026-10-12T23:35Z)
  - [`main/history-full.jsonl`](main/history-full.jsonl) (all 69 events since 2026-10-15T00:14Z)
- **Aggregates (`git repo-worktree report`):**
  - [`studio-main/report.txt`](studio-main/report.txt)
  - [`main/report.txt`](main/report.txt)

---

## `studio-main/` (`~/projects/studio-main`, §2 & §3 P1–P4)

| File / Pattern | Content |
| :--- | :--- |
| `setup-list.json`, `final-list.json` | `git repo-worktree list --json` before and after Round 5 (21 pre-existing user trees). |
| `snap-before.txt`, `snap-after.txt`, `snap-diff.txt` | Source checkout integrity snapshot (`HEAD`, `git status --porcelain=v1`, `.git/worktrees` per project) before and after Round 5 (`snap-diff.txt` is 0 bytes). |
| `P1-pool-list-before.txt`, `.json` | `pool list` before cleanup: depth `2 of 2` for 9 pooled projects + 18 orphaned spares (`about 142.4 GB`) across 9 projects. |
| `P1-clear-orphaned.json`, `.stderr`, `.time` | Foreground `pool clear --orphaned --json` envelope, `-v` stderr, and `/usr/bin/time -v` (19.77 s). |
| `P1-df-before.txt`, `P1-df-after.txt`, `P1-du-pool-before.txt`, `P1-du-pool-after.txt` | `df -B1 /` and `du -sx .pool` before (`179,874,336 KB`) and after (`41,051,472 KB`) orphan cleanup, reclaiming `138,822,864 KB` (`132.39 GiB` / `142.15 GB`). |
| `P1-pool-list-after.txt`, `.json` | `pool list` immediately after `pool clear --orphaned` (`depth 2 of 2`, 0 orphans). |
| `P2-add-A.{json,stderr,time}`, `P2-after-A-maintain-event.json`, `P2-pool-list-after-A.{txt,json}` | P2.1 single add (`studio-wt-r5-A`, 4.92 s, 9/9 hits) and its background `pool maintain` event (172.09 s, 9 spares filled). |
| `P2-df-before-A.txt`, `P2-df-immediately-after-A.txt`, `P2-df-after-A-maintain.txt` | `df -B1 /` before A, immediately after A (`+1.53 GB`), and after A's background refill (`+23.06 GB`). |
| `P2-burst-add-{B,C,D}.{json,stderr,time}`, `P2-after-burst-maintain-event.json`, `P2-pool-list-after-burst.txt` | P2.2 back-to-back burst adds: B (5.34 s, 9/9 hits), C (6.54 s, 9/9 hits), D (49.80 s, 0/9 hits), and B's background `pool maintain` (296.25 s, 18 spares filled). |
| `P2-df-before-burst.txt`, `P2-df-after-burst-{B,C,D}.txt` | `df -B1 /` before burst and immediately after B (`+1.52 GB`), C (`+1.70 GB`), and D (`+30.86 GB`). |
| `P2-rm-{B,C,D}.{json,stderr,time}`, `P2-after-rm-BCD-maintain-event.json`, `P2-pool-list-before-spaced.{txt,json}` | Removal of B, C, D and pool state (`depth 2 of 2`) before P2.3 spaced adds. |
| `P2-spaced-add-S{1,2,3}.{json,stderr,time}`, `P2-after-spaced-maintain-event.json` | P2.3 spaced adds 2 minutes apart (S1: 7.59 s, S2: 8.55 s, S3: 7.39 s — all 9/9 pool hits; refills took 114.98 s, 113.74 s, 112.80 s). |
| `P2-rm-{A,S1,S2,S3}.{json,stderr,time}`, `P2-after-rm-all-maintain-event.json` | P2.4 cleanup of A, S1, S2, S3. |
| `P3-maint-{1,2}-add.{json,stderr,time}`, `P3-maint-{1,2}-bazel.{stdout,stderr,time}`, `P3-maint-{1,2}.{iostat,vmstat}`, `P3-maint-{1,2}-rm.{json,stderr}` | P3 Bazel builds (`//tools/base/common:tools.common`) while `pool maintain` ran in the background (29.46 s, 21.06 s). |
| `P3-nomaint-{1,2}-add.{json,stderr,time}`, `P3-nomaint-{1,2}-bazel.{stdout,stderr,time}`, `P3-nomaint-{1,2}.{iostat,vmstat}`, `P3-nomaint-{1,2}-rm.{json,stderr}` | P3 Bazel builds with `GIT_REPO_WORKTREE_NO_POOL_MAINTENANCE=1` (29.39 s, 30.28 s). |
| `P4-du-pool-after-P2.txt`, `P4-du-pool-per-project-after-P2.txt`, `P4-df-after-P2.txt`, `P4-pool-list-after-P2.{txt,json}` | P4 `.pool` disk usage (`41,051,040 KB` total and per-project breakdown) after P2. |
| `final-pool-list.txt`, `final-pool-list.json` | Final `pool list` on `studio-main` (`depth 2 of 2`, 0 orphans). |

---

## `main/` (`~/projects/main` — AOSP, §2 & §4 S1–S5)

| File / Pattern | Content |
| :--- | :--- |
| `setup-list.json`, `final-list.json` | `git repo-worktree list --json` before and after Round 5 (2 pre-existing user trees: `main-feature` and `studio-main.temp`). |
| `setup-pool-list.{txt,json}`, `setup-df.txt`, `setup-du-pool.txt` | Initial state before S2 (`19,351,208 KB` in `.pool`, 1 spare each of 14 pooled projects). |
| `snap-before.txt`, `snap-after.txt`, `snap-diff.txt` | Source checkout integrity snapshot across all 1,084 projects before and after Round 5 (`snap-diff.txt` is 0 bytes). |
| `S2-init-pool-clear.{json,stderr}` | Foreground `pool clear` before S2 so all S2 runs are cold checkouts. |
| `S2-r{1..9}-j{8,16,32}.{precond,samples,stderr,time,vmstat}` | S2 rotating `-j 8/16/32` matrix (9 runs): precondition wait (`Dirty` and `loadavg`), S1 1 Hz thread/descendant wait-channel sampler (`sample2.sh`), `-v` stderr, `/usr/bin/time -v`, and `vmstat 1`. |
| `S2-r{1..9}-j{8,16,32}-rm-nopool.{json,stderr,time}` | `remove --no-pool` after each S2 run (`prune` 25–47 ms, `pruned_git_dirs: 113`). |
| `S4-add1.{precond,samples,stderr,time,vmstat}` | S4 first cold `add` (`main-wt-r5-S4a`, 61.35 s) with background maintenance enabled. |
| `S4-maintain-active.txt`, `S4-maintain-events.json`, `S4-pool-list-after-maintain.{txt,json}` | Verification of running `pool maintain` process, its `history.jsonl` event (232.61 s, 28 spares filled, 0 deferred), and `pool list` afterwards (`depth 2 of 2`). |
| `S4-add2.{samples,stderr,time,vmstat}` | S4 second `add` (`main-wt-r5-S4b`, 97.71 s) started 2 s into `S4-add1`'s background `pool maintain`. |
| `S4-add3-warm.{precond,samples,stderr,time,vmstat}` | S4 warm-pool `add` (`main-wt-r5-S4c`, **20.50 s**, 14/14 pool hits) after `pool maintain` reached `depth 2 of 2`. |
| `S5-rm-default-{1,2}.{json,stderr,time}` | S5 default `remove` of `main-wt-r5-S4a` (25.54 s, `ret=14`, `prune=32 ms`) and `main-wt-r5-S4b` (14.41 s, `ret=14`, `prune=27 ms`). |
| `S5-rm-nopool-1.{json,stderr,time}` | S5 `remove --no-pool` of `main-wt-r5-S4c` (24.31 s, `ret=0`, `prune=33 ms`). |
| `S5-final-pool-clear.{json,stderr}`, `final-pool-list.{txt,json}` | Final `pool clear` and `pool list` leaving `main` at `depth 0 of 2` (0 spares). |
