# Round 4 raw data

Everything here was captured on one machine (see [`environment.txt`](environment.txt))
with `git-repo-worktree 2.0.0 (6025da9bd07d)`. The analysis is in
[`../report.md`](../report.md), and the scripts that produced these files are in
[`../scripts/`](../scripts/).

## Where to start

- **Per-project timings for every run**: `*/history-runs.jsonl`. These are
  `history.jsonl` records filtered to the trees this round created (plus the
  user's `main-feature` add on `main`). Each `add` record has `phases`,
  `environment`, and `projects[]` with `path`, `mode`, `strategy`
  (`checkout` / `fuse-overlayfs` / `pool`), `bytes`, `ms`, and `pooled`. Each
  `remove` record has `phases` and `returned_to_pool`.
- **Aggregates**: `*/report.txt` is `git repo-worktree report` output for each
  client after the runs.
- **Complete usage history**: `*/history-full.jsonl` is each client's entire
  `.repo/repo-worktrees/history.jsonl`, unfiltered, with every command
  (`add`, `remove`, `list`, `status`, `pool *`, `repair`, `materialize`,
  `hooks`, `report`) and their per-project arrays and errors.
  - studio-main: 457 events from 2026-10-12T23:35Z to 2026-10-15T01:49Z,
    across tool builds `f5bd402c18ae` (136 events), `5865f3f65006` (85), and
    `6025da9bd07d` (22); 214 events have no `commit` field. It covers the
    user's and agents' real day-to-day trees, not just this round's runs.
  - main: everything from the first `add` attempt at 2026-10-15T00:14Z.

## `main/` (AOSP, tree `main-wt-report`)

The numbering follows the report's run table, oldest first.

| File | Content |
| :-- | :-- |
| `add1.*` | First add (60 s, empty pool). |
| `add2.*` | First warm add (221 s, 14 pool hits). |
| `add3.*`, `add4.*` | `cycle.sh` warm adds (239 s, 231 s). |
| `add5.stderr`, `add5.samples` | Add with a child-process sampler (`sample.sh`): per-second `ps` state, wchan, and kernel stack of the tool's children. |
| `add6.*`, `add7.*` | `cycle.sh` warm adds under lower disk load (233 s, 243 s). |
| `add8.*` | A/B add after `pool clear` (195 s). |
| `*.stderr` | `-v` output: phase start/end lines and `project N/1084: added <path> (<strategy>)` in completion order. |
| `*.time` | `/usr/bin/time -v` (wall, user/sys CPU, %CPU, max RSS, fs in/out blocks). `remove2–6.time` are wall time and %CPU only. |
| `remove*.json`, `remove*.stderr`, `remove7.err` | Remove envelopes. Some `remove*.json` files have stderr mixed in, so read past the first line. |
| `poolclear.json` | `pool clear --json` before `add8`. |
| `rawpar.list`, `rawpar.ms` | Raw `git worktree add` repro without the tool: the 300 smallest checkout projects, and per-project ms from the **last** `rawpar.sh` invocation (P=8, `core.fsync=none`). Earlier invocations' wall/p50/p90 figures are only in the report. |
| `probe.txt` | One-second probe: tiny `git worktree add` latency vs. concurrent Jetski-hub `git status` processes and runnable tasks. |
| `status.time` | `git repo-worktree status` timing. |
| `df-*` | `df -B1` used/avail bytes before and after runs. |

Full `add*.json` envelopes (~470 KB each) are omitted; their per-project data
duplicates `history-runs.jsonl`.

## `studio-main/` (trees `studio-wt-report-A/B/C`, burst at 01:06 UTC)

| File | Content |
| :-- | :-- |
| `add{A,B,C}.json`, `.stderr`, `.time` | A was a pool hit (4.8 s); B and C were misses (40 s, 60 s). |
| `remove{A,B,C}.json`, `.err` | C and B returned 9 projects each; A returned 0 (pool full, real delete). |
| `df0`, `df-{A,B,C}` | Disk used (bytes) before the burst and after each add. |
| `pool-list-after.json` | `pool list --json` after the burst: 36 spares, all at the current source HEAD. |
| `pool-du-kb-after.txt` | `du -sx` (KB) per `.pool/<project>` after the burst. Nine of these projects aren't pool-mode for `add` (see the report's dead-spare table). |
| `trees-list.json` | `list --json`: the user's live trees and their population modes. |
| `history-add-remove-summary.jsonl` | Every add/remove in studio-main's history since 2026-10-12 (439 events), without per-project arrays. Used for the pool hit/miss statistics. |

The pool state **before** the burst (27 spares: one each for the 9 pool-mode
projects, two each for the 9 non-pool projects) wasn't saved as a file. It
appears in the report, derived from `pool list` at 01:09.
