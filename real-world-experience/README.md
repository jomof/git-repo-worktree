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

## Addendum

7. **[`addendum-env-bazelrc-and-cartfs.md`](addendum-env-bazelrc-and-cartfs.md)** — **Addendum: How `env.bazelrc` and CartFS `output_base` Are Generated in `studio-main`**
   - Traces `env.bazelrc` creation to `./build/bazel/env/setup` (`build/bazel/env/setup_dev_env.py`), explains how `startup --output_base=/google/cartfs/mount/<md5(workspace_root)>` is computed, clarifies that CartFS is a Google-internal FUSE filesystem (while `setup_dev_env.py` is in public AOSP), and shows how generic `<md5(source_root)>` -> `<md5(worktree_root)>` rewriting preserves both Bazel isolation and CartFS deduplication.
   - Explicitly spells out that the reviewing/implementing agent **does not need CartFS** in its environment: following 4 purely generic conventions (real `<wt>/.repo/`, non-symlinked project dirs, `<source_root>`/`<md5(source_root)>` string substitution in copied root configs, and hash-guarded `output_base` cleanup) automatically takes advantage of CartFS when present and can be tested in any standard `/tmp` directory.
