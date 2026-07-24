# git-repo-worktree — Safe Fast `add` (Spec Addendum)

**Status:** Normative addendum to `spec.md` v1.0-draft.

This addendum speeds up project population without changing the main spec's
isolation model. Every successful project remains a real linked Git worktree
at its pinned SHA. Source working-tree changes are never transported, and no
created path aliases or writes through to the source client.

The sharing and `materialize` ideas in `advise-only-spec-fast-add.md` are
explicitly out of scope.

## 1. CLI

`add` gains:

* `--no-reflink`: disable reflink attempts and always use Git checkout.
* `--heavy-jobs <n>`: allow at most `n` heavy projects to populate
  concurrently. `n` must be positive and defaults to 3.

Both flags are rejected with `--continue` (exit 2). Their original values are
recorded in `meta.json` and govern resume. `-j/--jobs` continues to control
project-level concurrency.

## 2. Metadata compatibility

The registry format remains version 1 because every project remains a normal
worktree and older binaries can safely list, repair, move, lock, unlock, and
remove these trees.

Each project record gains:

```json
"strategy": "reflink" | "checkout" | null,
"est_bytes": 1234 | null
```

`strategy` is null until population succeeds. Tree metadata gains:

```json
"no_reflink": false,
"heavy_jobs": 3
```

Readers default absent `strategy` and `est_bytes` to null, `no_reflink` to
false, and `heavy_jobs` to 3 so existing version-1 entries remain readable.

## 3. Tracked-size estimation

After Git common-directory and supplemented-project HEAD discovery, pinning
computes `est_bytes` for each pending project from the pinned tree, not by
walking its live checkout:

```text
git --git-dir=<git_common_dir> ls-tree -rlz <sha>
```

Sum the reported blob sizes with saturating arithmetic. Trees, gitlinks,
malformed entries, and entries without a numeric size contribute zero. Run
estimates in the existing manifest worker pool. A command or object-availability
failure produces `est_bytes: null` and does not fail `add`.

This estimate excludes untracked and ignored build output, is immutable for
the snapshot, and also works when the source checkout directory is absent.

## 4. Scheduling and heavy throttling

The parent-before-child project dependency rule remains authoritative.
Among currently ready projects, dispatch the largest `est_bytes` first;
null estimates sort as zero, with project path as the deterministic tie-break.

A project is heavy when:

```text
est_bytes > 500 * 1024 * 1024
```

At most `heavy_jobs` heavy projects may run simultaneously. Light ready
projects may fill all remaining slots up to `jobs`; a throttled heavy project
must not block a ready light project. The effective heavy limit never exceeds
`jobs`.

## 5. Native parallel checkout

Every Git command that populates a worktree through checkout, including
normal `worktree add` and resume/fallback `reset --hard`, receives per-command
configuration:

```text
-c checkout.workers=8
-c checkout.thresholdForParallelism=100
```

These constants do not scale with `-j`; project concurrency and Git checkout
worker concurrency multiply. The configuration is never written to a
repository.

## 6. Conservative reflink eligibility

Unless `--no-reflink` was recorded, each project may attempt reflink
population. Every failed or ambiguous precondition silently selects checkout
and emits only a `-v` diagnostic.

Immediately before registering the destination, all of these must hold:

1. `<source-root>/<project-path>` exists as a Git checkout.
2. Its `HEAD` exactly equals the pinned SHA.
3. Sparse checkout is not enabled.
4. `git status --porcelain=v1 -z --untracked-files=all` is empty. This
   deliberately rejects staged, tracked, untracked, and ignored-by-index
   deviations that Git reports.
5. The source/destination filesystem pair has not already failed a reflink
   during this run.

No Git lock or source file is changed by this optimization.

## 7. Reflink population and validation

For an eligible project:

1. Run `git ... worktree add --no-checkout [--detach|-b <branch>]
   <destination> <sha>`.
2. Initialize the worktree index with `git -C <destination> read-tree <sha>`,
   then enumerate the pinned tree recursively with
   `git --git-dir=<git_common_dir> ls-tree -rz <sha>`.
3. For regular entries, create parent directories and use the pinned
   `reflink-copy` crate's `reflink` operation from the source checkout to the
   destination. Set permissions from the Git mode (0644 or 0755).
4. Recreate mode `120000` entries as symlinks with the source link target.
   Create mode `160000` entries as empty directories for the existing
   submodule phase. Any missing, unexpected, or unsafe entry aborts the
   optimization.
5. Re-check source `HEAD`, sparse mode, and full cleanliness.
6. Verify the destination against the pinned index with:

   ```text
   git -C <destination> update-index --refresh
   git -C <destination> diff-files --quiet
   git -C <destination> diff-index --cached --quiet <sha>
   ```

Only a project passing all checks records `strategy: "reflink"`. Thus even a
source modification racing the copy cannot be accepted as committed output.

If any step after registration fails, run the configured parallel
`git -C <destination> reset --hard <sha>` in the existing worktree. Success
records `strategy: "checkout"`; failure reports the project failed normally.
A reflink failure marks that source/destination device pair unsupported for
the rest of the run, preventing repeated expensive failures.

## 8. Ordinary checkout

Ineligible projects run:

```text
git -C <source-project> \
  -c checkout.workers=8 \
  -c checkout.thresholdForParallelism=100 \
  worktree add [--detach|-b <branch>] <destination> <sha>
```

The main spec's `--git-dir` fallback remains unchanged. Success records
`strategy: "checkout"`.

## 9. Resume and durability

Project completion is reported only after its `project-add done` record has
been included in a durable journal batch. Metadata is written after the
batch.

An unfinished project whose destination already contains `.git` is never
accepted from `HEAD` equality alone. `add --continue` runs the configured
parallel `reset --hard <sha>` and records checkout success. This repairs
crashes during `--no-checkout`, reflink copying, validation, or journal/meta
handoff. Replaying a project whose journal record was durable but whose
metadata update was interrupted is safe and idempotent.

The original `no_reflink`, `heavy_jobs`, project estimates, and pinned SHAs
remain authoritative on resume.

## 10. User and agent guidance

README and the embedded `SKILL.md` must state:

* Fast add still creates isolated real worktrees; there are no shared project
  symlinks.
* Scheduling is longest-ready-first with a default three-heavy-project cap.
* `--no-reflink` is a diagnostic fallback.
* `--heavy-jobs` may be lowered on storage-constrained machines.
* `--continue` repairs and resumes an interrupted optimized checkout.

## 11. Tests

Required coverage:

1. Tracked-size parsing ignores non-blobs/malformed sizes and saturates.
2. Dependency scheduling remains parent-before-child, orders ready projects
   largest first, and starts light work while heavy work is throttled.
3. Checkout invocations and resume resets carry both parallel-checkout
   configuration values.
4. A clean source on a reflink-capable filesystem yields a clean destination
   with `strategy: "reflink"`; unsupported filesystems and `--no-reflink`
   yield an identical `strategy: "checkout"` tree.
5. Dirty, untracked, wrong-HEAD, or sparse sources never use reflink and never
   transport source-only content.
6. Symlinks, executable modes, and gitlinks preserve normal checkout
   semantics.
7. A partial registered worktree with pinned `HEAD` is reset on resume rather
   than incorrectly accepted as complete.
8. Existing version-1 metadata without the new fields remains readable.

— end of addendum —
