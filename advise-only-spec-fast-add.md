# git-repo-worktree — Fast `add`: Population Strategies (Advisory Draft)

**Status:** Advisory design record only. Superseded by `spec-fast-add.md`;
none of this document is normative.
Redefines how `add` populates project checkouts so that the **default** `add`
is fast on enlistments dominated by large prebuilts (e.g. `studio-main`),
while preserving the main spec's contracts: committed-state-only transport
(P3), journaled idempotent resume (P5), and no mutation of repo internals
(§1.1.6). Registry `format_version` is bumped to **2** (§8 of this addendum).

Terminology from the main spec applies. New terms:

* **Population mode** (per project): `worktree` (a real linked git worktree)
  or `symlink-shared` (a symlink into the source client; no git worktree
  exists for it in this tree).
* **Population strategy** (per `worktree`-mode project): `reflink` (§4) or
  `checkout` (§5). Chosen automatically; not user-visible except in metadata.

---

## 1. Mode selection (the clone-depth rule)

At pin time (main spec §6.3.2), the parser additionally reads the
`clone-depth` attribute of each `<project>` in the snapshot manifest.

A candidate project's mode is `symlink-shared` iff **all** of:

1. Its snapshot `<project>` element carries `clone-depth="<n>"` where `<n>`
   parses as a positive integer (any other value: ignore the attribute, warn
   `malformed clone-depth`).
2. `--full` was not given, and the project is not named by `--full-project`.
3. The project is **not** a prefix-directory parent of any other candidate
   project (sharing a parent would alias the nested child's path into the
   source client; such parents are always materialized — warn
   `shared-parent-demoted` at `-v`).
4. Its source checkout directory `<root>/<ppath>` exists (a missing checkout
   follows the normal `skipped`/fallback rules of §6.3.4 and, if a gitdir is
   found, is materialized, never shared).

**Guardrail:** if *every* candidate project satisfies rule 1, assume the
attribute reflects a client-wide shallow init rather than a deliberate
per-project marking; disable sharing for the whole run (all projects
materialize) and warn once:
`all projects carry clone-depth; ignoring it for sharing (use --full to silence)`.

All other candidates have mode `worktree`.

Rationale (informative): `clone-depth` is a declarative, manifest-author
signal meaning "history is irrelevant here", which in practice marks
prebuilts and toolchains that are never developed in. It requires no size
probing and no user labeling, satisfying the commissioner's constraint that
the default needs no configuration.

## 2. New `add` flags

* `--full` — force mode `worktree` for every project.
* `--full-project <ppath>` (repeatable) — force mode `worktree` for the named
  project path(s); unknown path → exit 2.
* `--no-reflink` — force strategy `checkout` for all materialized projects
  (diagnostic escape hatch).
* `--heavy-jobs <n>` — override the heavy-task concurrency cap (§6);
  default 3.

Per the main spec's `--continue` rule, all four are rejected with `--continue`
(exit 2); the values recorded in `meta.json` govern resumption.

## 3. `symlink-shared` mechanics

For each shared project, `add` (journal step `project-share`):

1. Creates parent directories under the tree root (mode 0755).
2. Creates the symlink `<dest>/<ppath>` → **absolute** target
   `<source-root>/<ppath>` via `symlink(2)`. Absolute, so the link survives
   `move` of the tree unchanged; if the *source client* is ever moved, every
   registered tree is broken and `repair` cannot fix it (document in README).
3. Idempotency: an existing symlink with the exact target → `done`; existing
   symlink with a different target → delete and relink; existing non-symlink
   → `failed`, reason `dest-occupied` (never delete real data).

No `git worktree` registration exists for shared projects. Consequences,
each normative:

* **`remove`**: shared projects are excluded from the dirty check entirely
  and from the per-project `git worktree remove`/`prune` calls. The
  recursive root deletion **must not follow symlinks** (delete the link
  inode only; Rust's `std::fs::remove_dir_all` semantics — add an explicit
  test, §10.2, because following the link would delete source-client data).
* **`repair`**: re-verify each shared link per §3.3's idempotency rules.
* **`lock`/`unlock`**: skip shared projects (nothing to lock); the tree-level
  lock in `meta.json` still applies.
* **`prune`**: unchanged (operates on registry entries and recorded
  `git_common_dir`s of materialized projects only).
* **copyfile/linkfile** (extends §6.4): a directive **dest** may not resolve
  to a path having any component inside a shared project (that namespace
  belongs to the source client) → item `failed`, reason `dest-in-shared`.
  A directive **src** inside a shared project is permitted and resolves
  through the link (shared semantics: the reader sees the source's current
  file). After `materialize` (§7) converts a project, its `dest-in-shared`
  items become retryable.
* **Writes through a shared link mutate the source client.** The tool cannot
  prevent this; it must disclose it: `add`'s summary line appends
  `, <n> shared (symlinked to source — treat as read-only)` when n > 0, and
  the SKILL.md gains the §9 rule.
* `git_common_dir` is still resolved and recorded for shared projects (it
  enables `materialize`).

## 4. Strategy `reflink` (default population for `worktree` mode)

Reflink population produces a byte-identical result to a normal checkout at
the pinned SHA — it is an optimization, not a semantic change — by cloning
file extents from the source checkout instead of inflating objects.

Preconditions, checked per project; any failure falls through to strategy
`checkout` (§5) with a `-v` note, never an error:

1. Source checkout `<root>/<ppath>` exists and
   `git -C <root>/<ppath> rev-parse HEAD` equals the pinned `<sha>`.
2. Reflink is supported between the source and destination filesystems,
   memoized per `(st_dev(source-root), st_dev(dest-root))` pair for the run:
   the first attempted clone that fails with `EOPNOTSUPP`, `EXDEV`, or
   `EINVAL` marks the pair unsupported.
3. `--no-reflink` was not given.

Algorithm (journal step `project-add`, `strategy:"reflink"` recorded):

1. `git -C <root>/<ppath> worktree add --no-checkout [--detach|-b <branch>]
   <dest>/<ppath> <sha>` (gitdir fallback form as in main spec §7.1.5).
   Registers the worktree and writes `.git`; populates nothing.
2. Snapshot the source's tracked deviations:
   `git -C <root>/<ppath> status --porcelain=v1 -z -uno` → the **deviant
   set** (modified, deleted, typechanged, staged paths).
3. Enumerate `git -C <root>/<ppath> ls-files -z --stage`. For each entry not
   in the deviant set:
   * mode `120000` (symlink): recreate the symlink with the same target via
     `symlink(2)` (never reflink through it).
   * mode `160000` (gitlink/submodule): create the empty directory only
     (the submodule phase governs population).
   * other modes: reflink-copy `<root>/<ppath>/<file>` →
     `<dest>/<ppath>/<file>` (Linux: `FICLONE` ioctl; macOS: `clonefile(2)`;
     use the pinned crate `reflink-copy`), then set permission bits to the
     index mode (0644/0755).
4. `git -C <dest>/<ppath> read-tree <sha>`.
5. For each path in the deviant set: `git -C <dest>/<ppath> checkout -- <path>`
   (restores the committed content from the object DB; this is what upholds
   P3 — dirty source content is never transported).
6. The index is left **unrefreshed** (no stat data). This is deliberate: a
   refresh would re-read every byte just copied. Correctness is unaffected;
   the first `git status`/`git diff` a user runs in that project performs a
   one-time content refresh. Record this in README ("first status in a fresh
   tree may be slow in huge projects").

**Resume semantics (replaces the §7.1.5 HEAD-equality idempotency check for
all strategies):** a project is `done` iff the journal contains its
`project-add done` line. On `--continue`, for a project without that line
whose `<dest>/<ppath>/.git` already exists:
`git -C <dest>/<ppath> reset --hard <sha>` with the §5 checkout-workers
config (correct from any partial state; forgoes reflink savings in the rare
resume case), then journal `done`. The journal line is written only after
step 5 completes.

## 5. Strategy `checkout` (fallback population)

Used when reflink preconditions fail or `--no-reflink` is given. As main
spec §7.1.5, with git's native parallel checkout enabled per invocation
(config is passed with `-c`, **never written** to any repo — §1.1.6):

```
git -C <root>/<ppath> -c checkout.workers=8 -c checkout.thresholdForParallelism=100 \
    worktree add [--detach|-b <branch>] <dest>/<ppath> <sha>
```

(and the same `-c` pair on every `reset --hard` issued by §4-resume or
`materialize`). The constant 8 is fixed; do not scale it by `-j` (the two
multiply).

## 6. Scheduling

Extends main spec §6.6/§6.7; the parent-before-child constraint is unchanged
and takes precedence over everything below.

1. At pin time, compute `est_bytes` per candidate: the summed `st_size` of
   the source checkout's directory walk (rayon-parallel; do not descend into
   `.git`). Record in `meta.json`. Failures → `est_bytes: null`, treated
   as 0.
2. Shared projects are scheduled first (they are ~free).
3. Materialized projects are dispatched in **descending `est_bytes`** order
   (longest-first minimizes makespan).
4. A project with `est_bytes > 500 * 1024 * 1024` is **heavy**. At most
   `--heavy-jobs` (default 3) heavy projects run concurrently (semaphore);
   light projects fill the remaining pool up to `-j`.

## 7. New subcommand: `materialize`

```
git repo-worktree materialize [-j N] <tree> <ppath>...
```

Converts shared projects to real worktrees in place (the escape hatch when a
user or agent must edit a prebuilt). Per named path:

1. Must exist in `meta.projects` with mode `symlink-shared` → else exit 1,
   new stable error code `E_NOT_SHARED` (naming the project's actual mode).
2. The on-disk path must be a symlink to the expected target → else that
   project `failed`, reason `dest-occupied`; continue with others.
3. Unlink the symlink; populate via §4/§5 at the recorded `sha`; set mode
   `worktree`; journal step `project-materialize`.
4. If the tree is locked, immediately `git worktree lock` the new project
   worktree with the tree's recorded reason (preserves the §7.4 invariant).
5. Retry this project's directives previously failed with `dest-in-shared`.

Exit 0 if all named projects converted, 3 if any failed. `materialize` is
permitted on locked trees (the lock guards lifecycle operations, not
content). Registry flock and `--json` per main-spec conventions;
`result` = the updated tree object. There is no inverse command in v1
(re-sharing a materialized project = `remove` + `add`).

## 8. Metadata, output, and format version

* `meta.json` `format_version` becomes **2**. v2 binaries read v1 entries
  (projects lacking `mode` default to `worktree`, `strategy` to
  `"checkout"`); v1 binaries refuse v2 per the existing `E_FORMAT_TOO_NEW`
  rule — desired, since a v1 binary would dirty-check *through* shared
  symlinks during `remove`.
* Per-project record gains: `"mode": "worktree"|"symlink-shared"`,
  `"strategy": "reflink"|"checkout"|null` (null iff shared),
  `"est_bytes": <int|null>`.
* Tree record gains: `"full": <bool>`, `"full_projects": [<ppath>...]`.
* `list` porcelain gains one line per tree: `shared <n>` (count of
  symlink-shared projects), placed after `skipped`. Human `list` appends
  `(<n> shared)` when n > 0. `--json` inherits the meta shape as always.

## 9. SKILL.md additions (normative text)

Append to the "Rules that prevent damage" section of the embedded SKILL.md:

> - Some project directories in a tree may be **symlink-shared** with the
>   source client (typically large prebuilts). Writing anything under them
>   modifies the source client for everyone. Before editing under paths like
>   `prebuilts/`, check `git repo-worktree list --json` — projects with
>   `"mode": "symlink-shared"` are read-only; to edit one, first run
>   `git repo-worktree materialize <tree> <project-path>`.

And to the flags mentioned in the create-workflow section: note `--full`
(fully materialized tree, slower) and `materialize` (convert one project
later).

## 10. Testing requirements (extends main spec §10)

1. Mode selection: fixture manifest with `clone-depth` on two projects →
   default `add` shares exactly those; `--full` shares none;
   `--full-project` demotes one; all-projects-clone-depth guardrail fires;
   nested parent with `clone-depth` is demoted (`shared-parent-demoted`).
2. Symlink safety: `remove` of a tree with shared projects deletes the link
   but **asserts every file in the source checkout still exists**; dirty
   files inside a shared dir do not block `remove`; a directive with dest
   inside a shared project fails `dest-in-shared`.
3. Reflink correctness (run on macOS APFS in CI; on Linux, create and mount
   a loopback XFS or Btrfs image — the default ext4 runners exercise only
   the fallback): populate via reflink, then `git -C <proj> status` is
   clean and `git diff <sha>` is empty; a file dirty in the source arrives
   with **committed** content in the tree (P3); a file deleted in the
   source arrives present; source symlinks arrive as symlinks with
   identical targets; permissions match index modes.
4. Fallback: on ext4 (or with `--no-reflink`) the same fixture produces an
   identical tree via strategy `checkout`; the memoization performs at most
   one failed clone attempt per filesystem pair (count via strace-able test
   hook or injected counter).
5. Resume: kill mid-copy (extend `GRW_TEST_CRASH_AFTER` to count reflinked
   files) → `--continue` completes via `reset --hard`; resulting tree
   identical to an uninterrupted run; journal-line (not HEAD) governs
   done-ness.
6. `materialize`: shared project → real worktree at the pinned SHA;
   `dest-in-shared` directives retried; `E_NOT_SHARED` on a worktree-mode
   path; locked-tree materialize locks the new project worktree.
7. Scheduling: with `-j 4 --heavy-jobs 1` and three heavy fixtures,
   instrument that no two heavy checkouts overlap and that dispatch order
   is descending `est_bytes` (test hook records dispatch timestamps).
8. Format version: v1 `meta.json` read by v2 binary lists/removes correctly
   with implied modes; v2 meta rejected by a reader pinned to v1 behavior
   (`E_FORMAT_TOO_NEW` golden test updated).

## 11. Expected impact (informative, not a conformance target)

On a `studio-main`-shaped enlistment (57 projects, ~37GB, ~660k files, four
projects > 5GB carrying `clone-depth`): sharing removes ~31GB of writes;
reflink turns the remaining bulk into metadata operations on APFS/XFS/Btrfs;
longest-first + heavy-cap removes the tail collision. Anticipated `add`
latency: seconds-to-tens-of-seconds on reflink-capable filesystems; on ext4,
the shared tier plus parallel checkout should still yield a several-fold
improvement over the measured 3m10s baseline.

— end of addendum —
