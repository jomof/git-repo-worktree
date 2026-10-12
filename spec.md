# git-repo-worktree — Implementation Specification

**Version:** 1.0-draft
**Status:** Ready for implementation
**Targets:** Linux and macOS only
**Language:** Rust, published to crates.io as `git-repo-worktree`

Addenda: [`spec-fast-add.md`](spec-fast-add.md) (fast default `add`) and
[`spec-v2.md`](spec-v2.md) (repo fidelity, scoped branches, inspection, pool and
overlay modes). Where an addendum conflicts with this document, the addendum wins.

This document is written so that an implementing agent can build the tool without
making policy decisions. Where a choice existed, it has been made and is recorded
here. If the implementer encounters a genuinely unspecified situation, the rule is:
**mirror the behavior of `git worktree` for the analogous single-repo case**, and
record the deviation in `IMPLEMENTATION_NOTES.md` in the repo root.

---

## 1. Purpose and scope

`git-repo-worktree` creates and manages *whole-tree worktrees* of a Google
`repo`-managed client (an "enlistment"): parallel directory trees in which every
project from the source client is checked out as a linked `git worktree` at a
pinned revision, with manifest `<copyfile>`/`<linkfile>` directives replayed.
The result is a cheap, isolated, disposable image of the entire source tree,
suitable for humans and coding agents to work in without disturbing the source
client.

### 1.1 Non-goals (v1)

1. ~~**No `.repo` in created trees.**~~ Retired by `spec-v2.md` §3: trees carry a
   synthesized `.repo` bridge so repo commands work inside them (`--git-only`
   opts out).
2. **No transport of dirty state.** Uncommitted/untracked changes in the source
   tree are never copied. Created trees materialize committed state only
   (mirrors `git worktree add`).
3. **No Windows support.** Do not add `cfg(windows)` code paths. The crate must
   compile only on unix (`#[cfg(unix)]`; fail the build elsewhere with a clear
   `compile_error!`).
4. **No cross-filesystem `move`.** See §7.5.
5. **No network prefetching.** Partial-clone lazy fetches happen naturally via
   git; the tool never runs `git fetch` itself.
6. **No mutation of repo internals.** The tool writes only: (a) its own registry
   under `.repo/repo-worktrees/`, (b) the destination tree, (c) worktree
   registration data that *git itself* writes as a side effect of `git worktree`
   commands, plus the bridge links inside those registrations and the per-tree
   manifest branch described in `spec-v2.md` §3. It never edits the source's
   manifests, project.list, repo config, or anything else under `.repo/`.

### 1.2 Fixed policy decisions (agreed with the commissioner)

| # | Topic | Decision |
|---|-------|----------|
| P1 | repo commands inside created tree | Supported via a synthesized `.repo` bridge (`spec-v2.md` §3) |
| P2 | Source layouts | Support both legacy layout and `repo init --worktree` layout; auto-detect |
| P3 | Dirty state | Mirror `git worktree`: not transported; source dirtiness does not block `add` |
| P4 | Branch model | Detached HEAD at pinned SHAs by default; optional `-b` scoped to `-p` projects or `--all-projects`, with upstream tracking (`spec-v2.md` §4) |
| P5 | Failure semantics | Journaled + idempotent resume (`add --continue`); never all-or-nothing rollback |
| P6 | Manifest features | copyfile/linkfile: full support. Partial clone & superproject sources: supported. local_manifests: supported via snapshot. Groups: by-construction (enumerate what is on disk). Nested projects: supported. Submodules: off by default, `--submodules` opt-in |
| P7 | Platforms | Linux + macOS |
| P8 | Commands | Full `git worktree` surface: add, list, lock, move, prune, remove, repair, unlock; plus `version`, `completions` |
| P9 | Registry location | `.repo/repo-worktrees/` in the source client |
| P10 | Concurrency | Mandatory `flock` self-exclusion; best-effort concurrent-sync detection (error, `--force` to override); pin-first ordering |
| P11 | Structured output | `list --porcelain [-z]` mirroring git field style; `--json` on every subcommand |
| P12 | Version floors | git ≥ 2.31, repo ≥ 2.30 (repo needed only by `add`) |

---

## 2. Definitions

* **Source client / source root** — a directory containing a `.repo/` directory
  created by `repo init` + `repo sync`.
* **Project** — one `<project>` element of the manifest; identified by its
  checkout **path** (relative to the client root) and its remote **name**.
* **Legacy layout** — the default repo layout: each project checkout's `.git` is
  a gitdir file/symlink pointing into `.repo/projects/<path>.git`, which shares
  objects with `.repo/project-objects/<name>.git` via alternates.
* **Worktree layout** — the `repo init --worktree` layout: bare project repos
  live under `.repo/worktrees/`, and checkouts are linked git worktrees of them.
* **Tree** (or **whole-tree worktree**) — one directory created by this tool,
  containing one linked worktree per project.
* **Registry** — this tool's metadata directory: `<source-root>/.repo/repo-worktrees/`.
* **Snapshot manifest** — the pinned, flattened manifest produced by
  `repo manifest -r` at `add` time (all `<include>`s and local manifests merged,
  every project revision pinned to a concrete SHA).

---

## 3. Environment, dependencies, and crate layout

### 3.1 Runtime dependencies

* `git` ≥ 2.31 on `PATH`. Checked at startup of `add`, `move`, `repair`,
  `remove`, `prune`, `lock`, `unlock` by parsing `git version`. If below floor:
  exit code 1, error `E_GIT_TOO_OLD`.
* `repo` on `PATH` is required **only by `add`** (for the snapshot). If absent
  at `add` time: exit 1, error `E_REPO_MISSING`. Other subcommands must not
  invoke `repo`.
* `ps` (POSIX) — used best-effort by sync detection; absence is tolerated
  (detection silently degrades).

### 3.2 Crate and build

* Crate name and binary name: `git-repo-worktree` (so `cargo install
  git-repo-worktree` yields `git repo-worktree ...` via git's external-command
  dispatch).
* Rust edition 2021. MSRV: 1.75.
* Pinned major dependencies (do not substitute): `clap` v4 (derive) for CLI;
  `clap_complete` for the `completions` subcommand; `clap_mangen` (build-time
  man page generation); `roxmltree` for manifest XML parsing; `serde` +
  `serde_json` for registry files and `--json` output; `fd-lock` v4 for flock;
  `rayon` for the parallel job pool; `anyhow` for error propagation to the CLI
  boundary; `thiserror` for typed internal errors; `time` for RFC 3339
  timestamps.
* All timestamps written anywhere are UTC RFC 3339 with seconds precision,
  e.g. `2026-07-23T18:04:05Z`.
* Logging: human diagnostics go to **stderr**. Verbosity flags `-q` (errors
  only) and `-v` (live durable per-project completion plus phase start/completion
  timings). Default: summary + warnings.

### 3.3 Global CLI conventions

```
git repo-worktree <subcommand> [options] [args]
```

* Global flags accepted by every subcommand: `--json`, `-q`, `-v`,
  `-C <dir>` (run as if started in `<dir>`; mirrors git).
* When `--json` is given: stdout carries **exactly one JSON document** (§10);
  all human/progress output goes to stderr.
* Exit codes (uniform across subcommands):
  * `0` — success.
  * `1` — operational failure (nothing usable was accomplished, or a fatal
    error occurred).
  * `2` — CLI usage error (unknown flag, missing arg). Produced by clap.
  * `3` — partial success: the operation completed for some projects but not
    all; the tree is in state `incomplete` (only `add`, `repair`, and
    `--submodules` phases can produce this).
  * `4` — lock contention or quiescence check failure (registry flock held by
    another process, or sync-detection tripped without `--force`).

---

## 4. Source-client discovery and layout detection

### 4.1 Discovery

All subcommands locate the source client as follows:

1. Start at cwd (after applying `-C`).
2. Walk parent directories looking for a directory named `.repo` that contains
   a file `project.list` **or** a directory `manifests/`.
3. First hit wins; its parent is the **source root**.
4. If none found before filesystem root: exit 1, `E_NO_REPO_CLIENT`, message
   must state that the command must be run from within a repo client (or use
   `-C`).

Exception: `list`, `remove`, `move`, `lock`, `unlock`, `repair`, `prune` also
accept being run from **inside a created tree**. To support this, `add` writes a
breadcrumb file at the tree root: `.repo-worktree` (§5.4). If discovery step 2
fails, the tool additionally walks up looking for a `.repo-worktree` breadcrumb;
if found, it reads the source root from it and proceeds as if run there. If the
breadcrumb's recorded source root no longer contains a registry: exit 1,
`E_SOURCE_MISSING`.

### 4.2 Layout detection

Detect once per invocation and record in memory (and in `meta.json` at `add`):

* If `<root>/.repo/worktrees/` exists and is non-empty → **worktree layout**.
* Else → **legacy layout**.

The distinction is informational only; all git operations are performed via
`git -C <project-checkout>` or `git --git-dir=<recorded common dir>`, which
works identically in both layouts. Do not branch behavior on layout except in
messages and metadata.

---

## 5. On-disk data

### 5.1 Registry layout

```
<source-root>/.repo/repo-worktrees/
├── lock                      # 0-byte flock target
└── <name>/                   # one dir per tree, dir name == tree name
    ├── meta.json
    ├── snapshot.xml          # verbatim output of `repo manifest -r`
    └── journal.jsonl
```

The registry directory is created (mode 0755) on first `add`. Its presence with
no entries is normal.

### 5.2 `meta.json` schema

```json
{
  "format_version": 1,
  "tool_version": "<crate semver>",
  "name": "wt1",
  "path": "/abs/path/to/tree",
  "source_root": "/abs/path/to/client",
  "source_layout": "legacy",            // "legacy" | "worktree"
  "created_at": "2026-07-23T18:04:05Z",
  "state": "complete",                  // see §5.5
  "branch": null,                       // string if -b was used
  "submodules": false,                  // true if --submodules was used
  "locked": null,                       // or {"reason": "...", "at": "<ts>"}
  "moving": null,                       // or {"from": "...", "to": "...", "at": "<ts>"} during move
  "projects": [
    {
      "path": "vendor/foo",             // relative to tree root
      "name": "platform/vendor/foo",    // manifest project name
      "sha": "<40-hex>",
      "git_common_dir": "/abs/.repo/projects/vendor/foo.git",
      "state": "done",                  // "pending"|"done"|"failed"|"skipped"
      "reason": null                    // string when failed/skipped
    }
  ],
  "copyfiles": [ {"project": "vendor/foo", "src": "a", "dest": "b", "state": "done", "reason": null} ],
  "linkfiles": [ {"project": "vendor/foo", "src": "a", "dest": "b", "state": "done", "reason": null} ]
}
```

Rules:

* `meta.json` is rewritten atomically (write temp file in same dir + rename).
* Readers must reject `format_version` > 1 with exit 1, `E_FORMAT_TOO_NEW`,
  and a message telling the user to upgrade the tool.
* `git_common_dir` is captured at `add` time via
  `git -C <source-checkout> rev-parse --git-common-dir` (made absolute). It is
  the durable handle used by `prune`/`repair`/`remove` when checkouts are
  missing.

### 5.3 `journal.jsonl` schema

Append-only, one JSON object per line. Records completed within a short bounded
coalescing window may be written together and covered by one `fsync`. State,
progress output, and dependent scheduling must not advance until that batch is
durable. A crash before durability may replay any item in the batch; every operation
is idempotent, so this does not weaken resume guarantees. Recovery truncates only a
malformed, unterminated final line and fails on malformed durable lines. The journal
is used to make `add` resumable and to explain partial failures.

```json
{"ts":"...","step":"pin","status":"done"}
{"ts":"...","step":"mkdir-root","status":"done"}
{"ts":"...","step":"project-add","project":"vendor/foo","status":"done"}
{"ts":"...","step":"project-add","project":"vendor/bar","status":"failed","reason":"exit 128: <first 500 bytes of stderr>"}
{"ts":"...","step":"copyfile","project":"vendor/foo","dest":"Makefile","status":"done"}
{"ts":"...","step":"linkfile","project":"build","dest":"BUILD","status":"done"}
{"ts":"...","step":"submodule","project":"third_party/x","status":"failed","reason":"..."}
{"ts":"...","step":"finalize","status":"done"}
```

`step` ∈ {`pin`, `mkdir-root`, `project-add`, `copyfile`, `linkfile`,
`submodule`, `finalize`}. `status` ∈ {`done`, `failed`}. In-progress steps are
not journaled (a crash mid-step simply leaves no `done` line; resume re-executes
it — every step below is specified to be idempotent).

### 5.4 Breadcrumb file

`add` writes `<tree-root>/.repo-worktree` (regular file, mode 0644):

```json
{"format_version":1,"name":"wt1","source_root":"/abs/path/to/client"}
```

`move` rewrites it. `remove` deletes it with the tree.

### 5.5 Tree states

`meta.json.state` ∈:

* `creating` — `add` in progress (set before first project op).
* `incomplete` — `add`/`repair`/submodule phase finished with ≥1 failed item.
* `complete` — all projects `done` or `skipped`, all copy/link items `done`.
* `moving` — `move` in progress (`moving` field populated).
* `removing` — `remove` in progress.

A crashed process leaves `creating`/`moving`/`removing`; every subcommand that
opens an entry in one of those states and finds no live flock holder treats it
as its `incomplete`/interrupted equivalent and proceeds per its own rules
(`add --continue` resumes, `remove` cleans, `list` reports the raw state).

---

## 6. Common behaviors

### 6.1 Registry flock

Every subcommand that mutates registry or trees (`add`, `remove`, `move`,
`lock`, `unlock`, `prune`, `repair`) takes an **exclusive** flock on
`.repo/repo-worktrees/lock` for its whole duration. `list` takes a **shared**
flock. Non-blocking acquire; on contention exit 4, `E_LOCKED`, message includes
the hint that another `git-repo-worktree` process is running.

### 6.2 Quiescence (concurrent `repo sync`) detection — `add` only

Before the pin phase, `add` checks, in order:

1. Existence of any of: `.repo/manifests.git/index.lock`,
   `.repo/manifests.git/packed-refs.lock`, `.repo/manifests.git/HEAD.lock`,
   `.repo/manifests.git/shallow.lock`.
2. Best-effort process scan: run `ps -Ao args=`; if any line matches the regex
   `(^|/)repo\s+(sync|init)(\s|$)`, treat as positive. If `ps` fails, skip this
   check silently.

If positive: exit 4, `E_SYNC_ACTIVE`, message names what was detected and
states that `--force` overrides. With `--force`, print a warning and continue.
False positives (stale lock files) are acceptable; that is what `--force` is
for. No other subcommand performs this check.

### 6.3 Pin phase (`add` only)

1. Run `repo manifest -r -o <registry>/<name>/snapshot.xml` with cwd =
   source root. Nonzero exit → exit 1, `E_SNAPSHOT_FAILED`, include repo's
   stderr.
2. Parse `snapshot.xml` with `roxmltree`. Because `-r` output is flattened,
   the parser must handle exactly this subset and may error (`E_MANIFEST_PARSE`)
   on anything structurally unparseable:
   * `<project>` attributes used: `name` (required), `path` (default = `name`),
     `revision` (required; must be 40-hex — if not 40-hex, record the project as
     `skipped` with reason `unpinned-revision` and warn; do not fail the run),
     `groups` (ignored), `upstream`/`dest-branch`/`remote` (ignored).
   * Child elements used: `<copyfile src dest>`, `<linkfile src dest>`
     (both attributes required on each; missing attribute → that directive is
     recorded `failed` with reason `malformed`, warn, continue).
   * All other elements/attributes (`<remote>`, `<default>`, `<superproject>`,
     `<repo-hooks>`, `<contactinfo>`, `<notice>`, etc.) are ignored.
3. Read `.repo/project.list` (one path per line, `#` comments and blank lines
   ignored). Enumerate:
   * Projects in snapshot ∩ project.list → **candidates**.
   * In snapshot, not in project.list → `skipped`, reason `not-synced`, warn
     once per project at `-v`, summarized count otherwise.
   * In project.list, not in snapshot → warn (`orphan-checkout`), ignore.
4. For each candidate, resolve and record `git_common_dir` in the bounded parallel
   job pool:
   `git -C <root>/<path> rev-parse --git-common-dir`, absolutized. If the
   checkout directory is missing or the command fails, attempt the legacy
   fallback path `<root>/.repo/projects/<path>.git`; if that directory exists,
   use it; otherwise mark project `skipped`, reason `no-gitdir`, warn.

### 6.4 Path-safety rules (applied to every copyfile/linkfile)

Reject (record item `failed`, reason `unsafe-path`, warn, continue) any
directive where:

* `dest` or `src` is absolute;
* `dest` or `src` contains a `..` component;
* the fully resolved `dest` parent, after creating intermediate directories,
  would escape the tree root (verify by canonicalizing the deepest existing
  ancestor and prefix-checking against the canonicalized tree root);
* any existing intermediate component of `dest` inside the tree is a symlink
  (prevents symlink-swap escapes). Intermediate directories the tool itself
  creates are created with `mkdir` (never through symlinks) mode 0755.

### 6.5 Name and destination rules (`add`)

* Tree **name**: from `--name`, else the final component of the destination
  path sanitized to `[A-Za-z0-9._-]` (other bytes → `-`). Empty after
  sanitization → exit 1 `E_BAD_NAME`. Name must not collide with an existing
  registry entry → exit 1 `E_NAME_EXISTS` with hint to pass `--name` or use
  `add --continue` if resuming.
* **Destination** must be a nonexistent path or an existing empty directory
  (mirrors git). Violation → exit 1 `E_DEST_NOT_EMPTY` (with `--continue`, the
  destination is expected to exist).
* Destination must not be inside the source root; the source root must not be
  inside the destination; destination must not be inside any registered tree.
  Violation → exit 1 `E_DEST_NESTED`. (Comparison on canonicalized paths.)

### 6.6 Parallel job pool

`-j/--jobs N` on `add`, `remove`, `repair`, `lock`, `unlock`, `prune`.
Default: `min(logical CPUs, 32)`. Project-granular tasks run on a rayon pool of
that size. Per-project git operations touch distinct gitdirs and distinct
destination subtrees, so no additional synchronization is required beyond:
journal batches and `meta.json` rewrites are serialized; the parent-before-child
ordering constraint in §6.7. Snapshot `git_common_dir` discovery uses this pool too.

### 6.7 Nested-project ordering

Sort candidate projects by ascending path depth (number of components), stable
by path. A project whose path is a prefix-directory of another's must complete
(`done`, `failed`, or `skipped`) before the nested one starts. Implement this as
a prefix-dependency graph: unrelated projects are eligible immediately regardless
of path depth; global depth waves are not acceptable. Release a child only after
its parent's completion record is durable. If a parent project **failed**, still
attempt the child (its directory can be created independently); the parent's
failure is already journaled.

---

## 7. Subcommands

Every subcommand: takes locks per §6.1; supports `--json` per §10; uses exit
codes per §3.3. "Tree selector" below means: a positional argument that is
either a registered name or a path (absolute or relative) whose canonical form
equals a registered tree's path. Ambiguity is impossible (names contain no
`/`; treat an argument containing `/` or equal to `.`/`..` as a path,
otherwise try name first, then path). No match → exit 1 `E_NO_SUCH_TREE`.

### 7.1 `add`

```
git repo-worktree add [options] <path>
```

Flags: `--name <name>`, `-b <branch>`, `--detach` (accepted, no-op — detached
is the default; documented for git-parity muscle memory), `--submodules`,
`--lock [--reason <string>]` (tree is created locked, per §7.4 semantics),
`--continue`, `--force` (overrides §6.2 only), `-j`.

Fresh-run algorithm:

1. Preconditions: git/repo versions (§3.1); flock (§6.1); quiescence (§6.2);
   name/destination rules (§6.5).
2. Create registry entry dir; write `meta.json` with `state:"creating"` and
   empty `projects`; journal nothing yet.
3. **Pin phase** (§6.3); journal `pin done`; rewrite `meta.json` with full
   `projects`/`copyfiles`/`linkfiles` arrays (all `pending`).
4. `mkdir -p` destination; write breadcrumb (§5.4); journal `mkdir-root done`.
5. **Project phase** (parallel, §6.6, ordering §6.7). For each candidate:
   * Command (detached default):
     `git -C <root>/<ppath> worktree add --detach <dest>/<ppath> <sha>`
     (when the checkout is missing, substitute
     `git --git-dir=<git_common_dir> worktree add --detach ...`).
   * With `-b <branch>`:
     `git ... worktree add -b <branch> <dest>/<ppath> <sha>`. If git fails
     because the branch already exists in that project (stderr match on
     `already exists` / `already checked out`), record `failed`, reason
     `branch-exists`; do not force.
   * Success → journal + meta `done`. Failure → journal + meta `failed` with
     `exit <code>: <first 500 bytes of stderr>`; continue with other projects
     (unless `--fail-fast`, a supported flag that stops scheduling new tasks
     after the first failure; running tasks finish).
   * Idempotency: before running, if `<dest>/<ppath>/.git` exists and
     `git -C <dest>/<ppath> rev-parse HEAD` equals `<sha>`, mark `done`
     without re-running (this is what makes `--continue` and crash-rerun safe).
6. **Copy/link phase** (parallel, after all projects settle). Only directives
   whose owning project is `done`. Apply §6.4, then:
   * `linkfile`: compute the symlink target as the **relative** path from
     `dirname(<dest>/<lf.dest>)` to `<dest>/<project-path>/<lf.src>`; create
     with `symlink(2)`, creating parent dirs first. If a symlink already exists
     at dest with the exact same target → `done` (idempotent). Exists with a
     different target or as a non-symlink → `failed`, reason `dest-occupied`.
     Missing `src` in the worktree → create the (dangling) link anyway, mark
     `done`, warn `dangling-link` (mirrors repo, which links unconditionally).
   * `copyfile`: `src` must exist and be a regular file (repo's rule); else
     `failed`, reason `src-missing`/`src-not-file`. Copy bytes + permission
     bits. If dest exists with identical size and mtime ≥ worktree file's
     mtime → treat as `done` (idempotent resume); if dest exists otherwise,
     overwrite (fresh `add` owns the tree).
7. **Submodule phase** (only with `--submodules`; parallel). For each `done`
   project whose new checkout contains a `.gitmodules` at its root:
   `git -C <dest>/<ppath> submodule update --init --recursive`. Journal per
   project (`submodule done|failed`). Without the flag: detect the same set and
   print one warning listing affected projects, stating that submodules were
   not populated and `--submodules` exists.
8. **Finalize.** If `--lock`, apply §7.4 locking now. Compute final state:
   any `failed` project/copy/link/submodule item → `incomplete`, exit 3;
   otherwise `complete`, exit 0. Journal `finalize done`; rewrite meta.
   Human summary line (stderr):
   `created <name> at <path>: <D> projects, <S> skipped, <F> failed`.

`--continue <tree-selector or path>` algorithm: entry must exist and be in
state `creating` or `incomplete` (else exit 1 `E_NOT_RESUMABLE`). Skip §6.2
and the pin phase entirely (**never re-pin** — the snapshot is authoritative
for the tree's lifetime). Re-run phases 5–8 with the idempotency rules above,
retrying every `failed` item and executing every `pending` one. Flags `-b`,
`--name`, `--submodules`, `--lock` are rejected on `--continue` (exit 2);
the values recorded in `meta.json` govern.

### 7.2 `list`

```
git repo-worktree list [--porcelain [-z]] [--json] [-v]
```

Default human format: one line per tree,
`<path>  <state>  <D>/<T> projects  [locked]` (aligned columns), where T =
total non-skipped projects. `-v` adds a second line per tree with name,
created_at, snapshot path, and failed-project list.

`--porcelain` format (stable; `-z` replaces the terminating LF of each line
with NUL and the blank separator line with an extra NUL — mirroring
`git worktree list --porcelain -z` framing):

```
worktree /abs/path
name wt1
state complete
snapshot /abs/.repo/repo-worktrees/wt1/snapshot.xml
projects 412
skipped 3
failed 0
locked reason text        # present only if locked; reason may be empty
prunable path missing     # present only if the root directory is gone

```

Attribute names are fixed as above; unknown-to-the-consumer lines must be
ignorable, and new attributes may be appended in future minor versions.
`list` never mutates anything; a tree whose directory is missing is reported
with `prunable`, not auto-pruned.

### 7.3 `remove`

```
git repo-worktree remove [--force]... [-j N] <tree>
```

1. Dirty check (skipped entirely if ≥1 `--force`): for every project in state
   `done` whose directory exists, run `git -C <wt-project> status --porcelain
   --untracked-files=normal`; any output ⇒ dirty. If any project is dirty:
   exit 1 `E_DIRTY`, listing up to 10 dirty project paths (+ count). Mirrors
   `git worktree remove`.
2. Locked check: if the tree is locked, require **two** `--force` flags
   (mirrors git). One `--force` on a locked tree → exit 1 `E_TREE_LOCKED`.
3. Set `state:"removing"`, rewrite meta.
4. Unregister (parallel): for each project record with an existing directory:
   `git --git-dir=<git_common_dir> worktree remove --force <wt-project-path>`.
   On failure (e.g. git refuses, path half-deleted): fall through — step 6
   covers it.
5. Delete the tree root recursively (after step 4, only non-project residue,
   copyfiles, linkfiles, breadcrumb, and failed-remove leftovers remain).
6. Cleanup pass: for each project record:
   `git --git-dir=<git_common_dir> worktree prune`. (Safe and idempotent;
   reaps anything step 4 missed.)
7. Delete the registry entry directory. Exit 0.
   Interruption at any point leaves `state:"removing"`; re-running `remove`
   on it must succeed (every step is idempotent; missing dirs are fine).

### 7.4 `lock` / `unlock`

```
git repo-worktree lock [--reason <string>] <tree>
git repo-worktree unlock <tree>
```

`lock`: if already locked → exit 1 `E_ALREADY_LOCKED` (mirrors git). Set
`meta.locked = {reason, at}` (reason `""` if absent). Then per project
(parallel): `git --git-dir=<gcd> worktree lock <wt-project-path>` with
`--reason "repo-worktree:<name>: <reason>"`. Per-project failures (already
locked, missing) → warn, count, but the tree lock stands; exit 0 unless the
registry write itself failed.

`unlock`: not locked → exit 1 `E_NOT_LOCKED`. Clear `meta.locked`; per project
run `git --git-dir=<gcd> worktree unlock <wt-project-path>`, warning on
failures; exit 0.

Effect of locked state elsewhere: `remove` per §7.3.2; `move` refuses locked
trees without `--force` (single); `prune` never touches a locked tree's
registry entry even if its directory is missing (mirrors git's lock-protects-
from-prune semantics; report it in `list` as `locked` + `prunable`).

### 7.5 `move`

```
git repo-worktree move [--force] <tree> <new-path>
```

1. Refuse locked tree unless `--force` (exit 1 `E_TREE_LOCKED`).
2. Validate `<new-path>` with §6.5 destination rules (nonexistent or empty
   dir; nesting rules).
3. Write `meta.moving = {from, to, at}`, `state:"moving"`.
4. `rename(2)` old root → new root. On `EXDEV`: exit 1 `E_CROSS_DEVICE` with
   message "cross-filesystem move is not supported; create a new worktree at
   the destination and remove this one" — and clear `moving` (nothing changed).
5. Rewrite breadcrumb with new path; update `meta.path`.
6. Per project (parallel): `git --git-dir=<gcd> worktree repair
   <new-root>/<ppath>`.
7. Linkfiles are relative (§7.1.6) and survive rename; verification is
   `repair`'s job, not `move`'s. Clear `moving`, restore prior state
   (`complete`/`incomplete`), exit 0.
   Crash between 4 and 7: `meta.moving` names both paths; the next `repair`
   run (§7.7) must detect `moving`, determine which path exists, finish steps
   5–7, and clear the marker.

### 7.6 `prune`

```
git repo-worktree prune [-n|--dry-run] [-v] [--expire <time>]
```

For each registry entry: prunable iff its root directory does not exist (or
breadcrumb missing **and** no project dirs exist under the recorded root) and
it is not locked. For each prunable entry: delete the registry entry dir; then
for each of its recorded `git_common_dir`s run
`git --git-dir=<gcd> worktree prune` (append `--expire <time>` verbatim if
given). `-n` prints what would be removed (one line per entry:
`would prune <name> (<path>)`) and performs nothing, exit 0. Duplicated
`git_common_dir`s across entries are deduplicated before invoking git.

### 7.7 `repair`

```
git repo-worktree repair [-j N] [<tree>...]
```

Default: all registered trees. Per tree:

1. If `meta.moving` is set: complete the move per §7.5 crash-recovery first.
2. Root directory missing → report `prunable`, skip (repair never deletes).
3. Per project (parallel): if the worktree project dir exists, run
   `git --git-dir=<gcd> worktree repair <root>/<ppath>`; nonzero → mark that
   project `failed` reason `repair-failed` (previously-`done` projects only —
   never resurrect `skipped`).
4. Re-verify directives: recreate any linkfile that is missing or whose target
   differs from the computed one (delete + relink). Recreate a copyfile
   **only if dest is missing** — never overwrite an existing file during
   repair (the user may have edited it).
5. Recompute state (`complete` iff nothing failed), rewrite meta. Exit 0 if
   all repaired trees are `complete`, else 3.

### 7.8 `version`, `completions`

`version`: print `git-repo-worktree <semver>` (plus `git` and `repo` versions
detected, one per line, at `-v`). `completions <bash|zsh|fish>`: emit the
clap_complete script to stdout. Both exit 0 and support `--json`
(`{"version": "...", "git": "...", "repo": "..."}` for `version`).

---

## 8. JSON output (`--json`)

Envelope for every subcommand:

```json
{
  "format_version": 1,
  "command": "add",
  "ok": true,                       // ok=true iff exit code 0
  "exit_code": 0,
  "error": null,                    // or {"code":"E_...","message":"..."}
  "result": { ... }                 // command-specific, below
}
```

`result` payloads:

* `add` / `repair`: the full tree object — identical shape to `meta.json`
  (§5.2) — so an agent can read failure reasons per project/directive without
  touching the registry. On exit 3, `ok:false`, `error.code:"E_PARTIAL"`, and
  `result` still carries the tree object.
* `list`: `{"trees": [ <meta.json shape>, ... ]}` (always the full objects;
  `--porcelain` and `--json` are mutually exclusive, exit 2 if both given).
* `remove`: `{"name":"...","path":"...","removed":true}`.
* `lock`/`unlock`: `{"name":"...","locked":<bool>,"project_warnings":<int>}`.
* `move`: `{"name":"...","from":"...","to":"..."}`.
* `prune`: `{"pruned":[{"name":"...","path":"..."}],"dry_run":<bool>}`.

Error codes are the stable strings introduced throughout this spec
(`E_NO_REPO_CLIENT`, `E_GIT_TOO_OLD`, `E_REPO_MISSING`, `E_SNAPSHOT_FAILED`,
`E_MANIFEST_PARSE`, `E_BAD_NAME`, `E_NAME_EXISTS`, `E_DEST_NOT_EMPTY`,
`E_DEST_NESTED`, `E_NO_SUCH_TREE`, `E_NOT_RESUMABLE`, `E_DIRTY`,
`E_TREE_LOCKED`, `E_ALREADY_LOCKED`, `E_NOT_LOCKED`, `E_CROSS_DEVICE`,
`E_LOCKED`, `E_SYNC_ACTIVE`, `E_SOURCE_MISSING`, `E_FORMAT_TOO_NEW`,
`E_PARTIAL`). New codes may be added; existing codes must not be renamed.

---

## 9. Behavior notes the implementer must honor

1. **Never re-pin.** A tree's snapshot manifest is immutable after `add` phase
   3. `--continue`, `repair`, `move` all operate from the recorded snapshot
   and meta only.
2. **Never run `repo`** except `repo manifest -r` during `add` phase 3.
3. **Never write inside `.repo/`** outside `repo-worktrees/`.
4. **Detached HEAD everywhere** unless `-b`; never create or delete branches
   except via `git worktree add -b`, and never with `--force` variants that
   steal branches.
5. **Partial clone**: expect `git worktree add` to perform lazy fetches; a
   network/auth failure surfaces as a per-project `failed` with git's stderr —
   no special-casing beyond the message hint: when stderr contains
   `promisor` or `could not fetch`, append the hint
   `(source uses partial clone; network/auth may be required — retry with 'add --continue')`.
6. **Signals**: install a SIGINT/SIGTERM handler that stops scheduling new
   tasks, lets in-flight git processes finish (do not kill children), flushes
   the journal, rewrites meta, and exits 3 (during `add`/`repair`) or 1.
7. **stderr truncation**: everywhere git stderr is recorded, store at most 500
   bytes, lossy-UTF8.

---

## 10. Testing requirements

CI: GitHub Actions on `ubuntu-latest` and `macos-latest`, with `repo`
installed (curl from `storage.googleapis.com/git-repo-downloads/repo`). All
integration fixtures are network-free: local bare "remote" repos + a manifest
repo, served via `file://`, then `repo init -u file://... && repo sync`.

Required unit tests: snapshot-manifest parser (all §6.3.2 cases incl.
malformed directives, non-40-hex revisions); path-safety (§6.4) including
`..`, absolute, symlink-intermediate escape attempts; name sanitization;
porcelain and JSON serialization golden tests; relative-symlink computation.

Required integration scenarios (each asserted on both legacy and — where the
installed repo supports it — `--worktree` clients):

1. Happy path: 5-project client with nested projects (`a` and `a/b`),
   copyfile, linkfile (incl. one dangling) → `add`, assert tree contents,
   detached HEADs at pinned SHAs, link targets relative, exit 0; `list`
   (all three formats); `remove` clean.
2. `-b` branch mode incl. a pre-existing branch collision in one project →
   exit 3, journal reason `branch-exists`, `add --continue` after deleting the
   branch succeeds.
3. Interruption: kill -9 the tool mid-`add` (inject via env var
   `GRW_TEST_CRASH_AFTER=<n>` that aborts after n project-adds; test-only,
   compiled under `cfg(test)`/feature `test-hooks`) → state `creating`;
   `add --continue` completes; result identical to scenario 1.
4. Dirty remove: touch a file in one project → `remove` exits 1 `E_DIRTY`;
   `remove --force` succeeds; locked tree requires `--force --force`.
5. `move` same-filesystem: works, git worktrees functional after; simulated
   crash between rename and repair (test hook) → `repair` completes the move.
6. `prune` after `rm -rf <tree>`: registry entry reaped, per-gitdir
   `git worktree prune` leaves no stale registrations
   (`git --git-dir=... worktree list` clean).
7. Partial clone source (`repo init` with clone filter, or a project cloned
   `--filter=blob:none` with `uploadpack.allowFilter` on the fixture remote):
   `add` succeeds online; with fetches blocked (point the promisor remote URL
   at an invalid path), `add` exits 3 with the §9.5 hint; unblock;
   `add --continue` → complete.
8. Submodules: fixture project with a `.gitmodules` → default `add` warns and
   leaves unpopulated; `--submodules` populates recursively.
9. Quiescence: create `.repo/manifests.git/index.lock` → `add` exits 4;
   `--force` proceeds.
10. Flock: two concurrent `add`s → second exits 4.
11. `E_FORMAT_TOO_NEW`: hand-write `format_version: 2` meta → every
    subcommand refuses with the correct code.

---

## 11. Packaging and docs

* `cargo install git-repo-worktree` must be sufficient on a machine with git +
  repo. README documents this, plus the optional alias
  `git config --global alias.rwt '!git-repo-worktree'`.
* GitHub Releases with prebuilt binaries for `x86_64/aarch64` ×
  `linux-gnu/macos`, compatible with `cargo-binstall` metadata in Cargo.toml.
* Man page (`clap_mangen`) and completions shipped in release artifacts.
* README must state the v1 limitations verbatim: git-only trees (no repo
  commands inside), no dirty-state transport, no Windows, no cross-filesystem
  move, submodules opt-in.

— end of specification —
