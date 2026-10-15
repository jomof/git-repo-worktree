# Real-World Experience & Architectural Review Packet (`git-repo-worktree`)

This directory contains empirical field reports, agent session post-mortems, benchmark spikes, and architectural plans gathered from real-world usage of `git-repo-worktree` across `studio-main` (57 Git projects, ~37 GB, ~660,000 files on Linux `ext4`).

It is intended for an engineering/review agent auditing the entire `git-repo-worktree` repository (`spec.md`, `spec-user-journeys.md`, `spec-fast-add.md`, `src/`, `skill/SKILL.md`, and `install.sh`) to address and smooth over Critical User Journeys (CUJs).

## Contents

1. **[`agent-rough-spots-report.md`](./agent-rough-spots-report.md)**
   * **Start here.** Comprehensive report of the 10 rough spots and failure modes agents have hit in production `studio-main` sessions, mapped onto 7 end-to-end Critical User Journeys (CUJs) with exact source-code locations and a verification checklist.
2. **[`industrial-strength-plan.md`](./industrial-strength-plan.md)**
   * Architectural plan for `git-repo-worktree` v2:
     * **Full `repo` & `repox` fidelity** inside worktrees via synthesized `.repo/` and per-project `.git -> ../../.repo/projects/<path>.git` links (Model E1).
     * **Eliminating `symlink-shared`** without sacrificing creation speed using Copy-on-Write FUSE overlay mounts (`fuse-overlayfs` + `--no-checkout` + index copy with `sd_dev` patching / `core.checkStat=minimal`, plus an optional Phase 2 built-in Rust FUSE daemon with Linux 6.9+ `FUSE_PASSTHROUGH`).
     * **Smoothing out all operational hoops** (`env.bazelrc` bootstrapping with `startup --output_base` isolation, spelling out by-design detached-`HEAD` starting revisions in `--help` and docs, scoped `-b` with upstream tracking, subpath-to-project resolution, `detach` command, idempotent `materialize`, unbranched detached-commit guards, and manpage installation).
3. **[`track1-repo-fidelity-and-symlinks-spike.md`](./track1-repo-fidelity-and-symlinks-spike.md)**
   * Deep-dive investigation of `repo` (`project.py`, `manifest_xml.py`) and `repox` (`workspace.py`) internals, why naive `.repo` symlinking fails, how `repo init --worktree` works, and empirical test spike results for Model E1 in `/tmp/spike-repo-fidelity`.
4. **[`track2-fuse-and-fast-isolation-benchmarks.md`](./track2-fuse-and-fast-isolation-benchmarks.md)**
   * Empirical benchmark spike results on `studio-main` (`tools/idea` with 274,366 files / 2.0 GB, `prebuilts/studio/jdk` with 7.2 GB, and all 56 active projects): comparing full `git worktree add`, hardlink farms (`cp -al`), `--no-checkout` + index copy, `fuse-overlayfs`, kernel `overlayfs`, and `FUSE_PASSTHROUGH`. Includes the root-cause analysis and fix for the FUSE `st_dev` Git index stat-cache mismatch.
5. **[`track3-skill-hoops-and-cli-audit.md`](./track3-skill-hoops-and-cli-audit.md)**
   * Complete audit of all operational hoops documented in the external `repo-worktree` skill (`~/.gemini/config/skills/repo-worktree/SKILL.md`), including the full text of the external skill and an exact discrepancy matrix comparing it against `skill/SKILL.md`, `src/cli.rs`, and `src/error.rs`.

## Core Design Principles Confirmed by the Project Owner

1. **Full `repo` & `repox` Fidelity**: The original non-goal (P1 in `spec.md`: *"No `.repo` in created trees"*) is retired. `repo` and `repox` commands (`repo status`, `repo start`, `repo branches`, `repo diff`, `repo forall`, `repo upload`, `repo sync -l`, `repox -hc`, `repox -hb`, `repox stack`) must work with full fidelity inside a worktree.
2. **Starting Revisions Are the Detached `HEAD`s of the Initiating Repo (By Design)**: A newly created worktree must be initialized to the detached `HEAD` commits of the initiating `repo` workspace (`repo manifest -r`), matching `git worktree add --detach` semantics. It must **not** reset projects to remote `goog/studio-main`. To remediate agent confusion, `--help` (`src/cli.rs`), manpages, `skill/SKILL.md`, and `spec*.md` must explicitly spell out this by-design behavior.
3. **Eliminate the `symlink-shared` Footgun Without Sacrificing Speed**: Raw directory symlinks into the live mutable source checkout (`<wt>/<proj> -> <source>/<proj>`) cause silent source corruption, Git state hijacking, and `realpath` escapes. Replace them with CoW FUSE overlay isolation (`fuse-overlayfs` / `FUSE_PASSTHROUGH`) backed by real `--no-checkout` Git worktrees.

## Round 2

8. **[`round-2-blindfold-test-plan.md`](round-2-blindfold-test-plan.md)**: plan for a blindfold subagent test run of the v2 build (`spec-v2.md`): scenarios that probe each round-1 rough edge, source-integrity checks, a studio-main performance matrix for the new pool and overlay defaults, and the deliverables for `round-2/`.

9. **[`round-3-blindfold-hooks-plan.md`](round-3-blindfold-hooks-plan.md)**: blindfold rerun after the round-2 fixes, adding site hooks (`post-add` runs `./build/bazel/env/setup`, `pre-remove` stops the tree's Bazel server) and testing hook failure, parallel agents without lock contention, and warm-pool timing.

## Round 4

10. **[`round-4-add-timing/report.md`](round-4-add-timing/report.md)**: real-world `add`/`remove` timing, disk, and pool behaviour on `~/projects/main` (AOSP, 1,084 projects) and `~/projects/studio-main`, with raw history, scripts, and ruled-out hypotheses for the AOSP slowdown. Headline: 132 GB of studio-main's 171 GB pool is spares `add` never consumes.

## Round 5

11. **[`round-5-pool-and-stalls-plan.md`](round-5-pool-and-stalls-plan.md)**: after the round-4 timing report, measures the orphaned-spare cleanup and background pool refill on studio-main, and attributes AOSP's stalled adds with a thread-level wait-channel sampler and a `-j` matrix.
12. **[`round-5/report.md`](round-5/report.md)**: results of the Round 5 test plan on `a9a71ee28036` (`pool clear --orphaned` reclaiming 132.39 GiB on `studio-main` in 19.8 s; background `pool maintain` refills, bursts, spaced adds, and concurrent Bazel builds; AOSP cold `add` speedup from 155–243 s down to 24.8–43.4 s; warm-pool AOSP `add` in 20.5 s; `remove` `prune` down from 0.2–32 s to 25–47 ms; and attribution of the remaining AOSP `projects` time to synchronous `projects-journal-sync` / `projects-meta-write` `fsync()` calls on the main scheduler thread).
## Round 6

13. **[`round-6-verify-round-5-fixes-plan.md`](round-6-verify-round-5-fixes-plan.md)**: repeats round 5's measurements against the round-5 fixes (journal writer thread, size-estimate cache, maintenance pausing during tree operations, cheaper `remove` on a full pool) and decides whether the default `-j` should change.
14. **[`round-6/report.md`](round-6/report.md)**: results of the Round 6 test plan on `ca1c36d6d15e` (scheduler idle slots down to <1s per worker; cold AOSP adds at 23.8–24.2 s across both `-j 16` and `-j 32`; size-estimate cache cutting `snapshot-estimates` to 0–3 ms; `SIGSTOP` pause protecting concurrent AOSP adds from 97.7 s down to 29.2 s; and cheaper `remove` on a full pool).
