# Round 6 Raw Data (`ca1c36d6d15e`)

All data here was collected on one Linux workstation (see [`environment.txt`](environment.txt)) using `git-repo-worktree 2.0.0 (ca1c36d6d15e)`. The analysis and comparison tables with Round 5 are in [`../report.md`](../report.md), and the scripts used to run the tests are in [`../scripts/`](../scripts/).

## Where to Start

- **Round 6 filtered history (`R6-*` sessions + `pool maintain` events):**
  - [`main/history-r6.jsonl`](main/history-r6.jsonl) (31 events across A1, A3, A4)
  - [`studio-main/history-r6.jsonl`](studio-main/history-r6.jsonl) (37 events across B1, B2, B3, B4)
  - Each `add` record includes `projects-idle-slot`, `estimate_cache_hits`, `snapshot-estimates`, `projects-journal-sync`, and `projects-meta-write`.
  - Each `remove` record includes `pool-return`, `pruned_git_dirs`, and `pool_maintenance`.
  - Each `pool maintain` record includes `"paused"` in `phases`, `filled[]`, `system_start`, and `system_end`.
- **Refill Process `SIGSTOP` Pausing Log:**
  - [`main/A3-S4b-refill-git.samples`](main/A3-S4b-refill-git.samples): per-second `ps` capture of the background refill `git` process `1493072` showing state **`TN` (`wchan: do_signal_stop`)** continuously while `S4b` ran, resuming to `SN` immediately after `S4b` completed.
- **Unfiltered full histories:**
  - [`main/history-full.jsonl`](main/history-full.jsonl) (all events on `main`)
  - [`studio-main/history-full.jsonl`](studio-main/history-full.jsonl) (all events on `studio-main`)
- **Aggregates (`git repo-worktree report`):**
  - [`main/report.txt`](main/report.txt)
  - [`studio-main/report.txt`](studio-main/report.txt)

---

## `main/` (`~/projects/main` — AOSP, §2 & §3 A1–A4)

| File / Pattern | Content |
| :--- | :--- |
| `setup-list.json`, `final-list.json` | `git repo-worktree list --json` before and after Round 6 (2 pre-existing user trees: `main-feature` and `studio-main.temp`). |
| `setup-pool-list.{txt,json}`, `setup-df.txt`, `setup-du-pool.txt` | Initial state before A1. |
| `snap-before.txt`, `snap-after.txt`, `snap-diff.txt` | Source checkout integrity snapshot across all 1,084 projects before and after Round 6 (`snap-diff.txt` is 0 bytes). |
| `A1-init-pool-clear.{json,stderr}` | Initial `pool clear` before A1. |
| `A1-r{1..9}-j{8,16,32}.{precond,samples,stderr,time,vmstat}` | A1 rotating `-j 8/16/32` matrix (9 runs): wait-channel sampler (`sample2.sh`), `/usr/bin/time -v`, and `vmstat 1`. |
| `A1-r{1..9}-j{8,16,32}-rm-nopool.{json,stderr,time}` | `remove --no-pool` after each A1 run. |
| `A3-init-pool-clear.{json,stderr}` | Pool clear before A3. |
| `A3-S4a.{precond,samples,stderr,time,vmstat}` | Cold `add` S4a with background maintenance enabled. |
| `A3-S4-maintain-active.txt`, `A3-S4-maintain-events.json` | Active `pool maintain` process ID and completion events (`"paused": 30540` ms). |
| `A3-S4b.{samples,stderr,time,vmstat}` | Cold `add` S4b started 2 s into `pool maintain`. |
| `A3-S4b-refill-git.samples` | 1 Hz sampling of refill `git` process showing `TN` state while S4b ran. |
| `A3-S4-pool-list-after-maintain.{txt,json}` | Pool list after refill completed (`depth 2 of 2`, 28 spares). |
| `A3-S4c-warm.{precond,samples,stderr,time,vmstat}` | Warm `add` S4c (14/14 pool hits). |
| `A4-rm-S4a-default.{json,stderr,time}`, `A4-rm-S4b-default.{json,stderr,time}` | Default `remove` on full pool (`pool-return`: 2–3 ms, `"pool_maintenance": null`). |
| `A4-rm-S4c-nopool.{json,stderr,time}` | `remove --no-pool` for S4c. |
| `A4-final-pool-clear.{json,stderr}`, `final-pool-list.{txt,json}` | Final pool clear leaving `main` at `depth 0 of 2`. |

---

## `studio-main/` (`~/projects/studio-main`, §2 & §4 B1–B4)

| File / Pattern | Content |
| :--- | :--- |
| `setup-list.json`, `final-list.json` | `git repo-worktree list --json` before and after Round 6 (21 pre-existing user trees). |
| `snap-before.txt`, `snap-after.txt`, `snap-diff.txt` | Source checkout integrity snapshot across all 56 projects before and after Round 6 (`snap-diff.txt` is 0 bytes). |
| `setup-pool-list.{txt,json}`, `setup-df.txt`, `setup-du-pool.txt` | Initial state (`depth 2 of 2`). |
| `B1-r{1..5}.{precond,stdout,stderr,time}` | B1 warm `add`s (5 runs): run 1 misses cache (est=20.9 s), runs 2–5 hit cache (est=0 ms, total=3.19–5.21 s). |
| `B1-r{1..5}-rm.{json,stderr,time}` | Default `remove` returning 9 spares on each run. |
| `B1-pool-list-after.txt` | Pool list after B1 (`depth 2 of 2`). |
| `B2-burst-{B,C,D,E}.{stdout,stderr,time}` | B2 burst adds: B (3.38 s, 9/9 hits), C (3.32 s, 9/9 hits), D (37.95 s, 0/9 hits), E (38.53 s, 0/9 hits). |
| `B2-maintain-events.json` | B2 `pool maintain` events: B's refill recorded `"paused": 77459` ms. |
| `B2-pool-list-after.txt` | Pool list after B2 cleanup (`depth 2 of 2`). |
| `B3-r{1..3}-add.{stdout,stderr,time}` | B3 adds with maintenance OFF. |
| `B3-r{1..3}-rm.{json,stderr,time}` | B3 removes with maintenance ON: returned 9 spares, `"pool_maintenance": null` (0 spurious runs). |
| `B4-maint-add.{stdout,stderr,time}`, `B4-maint-bazel.{stdout,stderr,time}` | B4 Bazel build with maintenance ON (22.75 s). |
| `B4-nomaint-add.{stdout,stderr,time}`, `B4-nomaint-bazel.{stdout,stderr,time}` | B4 Bazel build with maintenance OFF (16.18 s). |
| `final-pool-list.txt`, `final-pool-list.json` | Final `pool list` on `studio-main` (`depth 2 of 2`, 0 orphans). |
