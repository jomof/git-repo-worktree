# git-repo-worktree — `skill` Subcommand Family (Spec Addendum)

**Status:** Addendum to `git-repo-worktree-spec.md` v1.0-draft. Normative.
Extends the command surface (main spec §7) with a `skill` subcommand family
that installs the tool's agent skill (`SKILL.md`) into locations where agent
frameworks discover skills. Global conventions — flock, `--json`, exit codes,
error-code stability — carry over from the main spec (§3.3, §6.1, §10) except
where stated: `skill` subcommands do **not** require a repo client (§4.1 does
not apply) and do **not** take the registry flock (they never touch the
registry; they take no lock at all — installs are single-file atomic writes).

---

## 1. Design principles

1. **Single source of truth.** The skill text is embedded in the binary at
   compile time (`include_str!("../skill/SKILL.md")`). The installed skill can
   never drift from the binary except by being stale, which `skill sync`
   fixes. The repository's `skill/SKILL.md` is the only editable copy.
2. **Evidence-based discovery, never invention.** The tool installs only into
   directories belonging to agent frameworks that show evidence of being in
   use (their framework root — `.claude/`, `.gemini/`, `.agents/`, etc. —
   already exists). It never creates a framework root itself, in either
   scope — that would falsely signal the framework is configured. It may
   create the `skills/` child and the skill's own leaf directory beneath an
   existing framework root. The same principle governs the target table:
   a framework enters the table only with paths taken from that framework's
   own documentation, never guessed (see §2.3).
3. **Explicit beats implicit.** Auto-selection follows a documented, ordered
   probe; every result names the exact path written; `--path` and an
   environment variable escape-hatch cover frameworks the tool doesn't know.
4. **Own only what you installed.** Every installed copy carries a sentinel.
   Overwriting or removing a file without the sentinel requires `--force`,
   because that file belongs to the user or another tool.
5. **Idempotent everywhere.** Re-running any `skill` command must be safe and
   must converge (up-to-date installs are no-ops that report `unchanged`).

## 2. Known targets and the probe order

A **target** is a (scope, framework) pair resolving to a skills directory.
v1 ships this built-in target table; extending it is a minor-version change
(append-only; ids are stable strings). Rows are listed in **probe order**
within each scope:

| id | scope | skills dir | evidence required (must already exist) |
|----|-------|------------|----------------------------------------|
| `agents-project` | project | `<anchor>/.agents/skills/` | `<anchor>/.agents/` |
| `claude-code-project` | project | `<anchor>/.claude/skills/` | `<anchor>/.claude/` |
| `gemini-project` | project | `<anchor>/.gemini/skills/` | `<anchor>/.gemini/` |
| `agents-user` | user | `~/.agents/skills/` | `~/.agents/` |
| `claude-code-user` | user | `~/.claude/skills/` | `~/.claude/` |
| `gemini-user` | user | `~/.gemini/skills/` | `~/.gemini/` |
| `antigravity-user` | user | `~/.gemini/config/skills/` | `~/.gemini/antigravity/` **or** `~/.gemini/antigravity-cli/` |

There is deliberately **no `antigravity-project` id**: Antigravity's
documented workspace-scope skills location *is* `.agents/skills/`, so the
`agents-project` target already serves it (see §2.2). At user scope, however,
Antigravity does not reliably read `~/.agents/skills/` across its flavors,
hence the dedicated `antigravity-user` target (§2.3).

The `agents-*` targets are the Agent Skills interoperable locations
(`.agents/skills/`), which Gemini CLI documents as a first-class alias of its
own `.gemini/skills/` tiers and which other standard-compliant tools also
read. They are probed first within each scope precisely because one install
there can serve multiple frameworks.

`<anchor>` resolution for project scope: performed **per target**, walking up
from cwd (after `-C`); a target's anchor is the first directory containing
that target's evidence directory (`.agents/`, `.claude/`, `.gemini/`, ...),
stopping (inclusive) at the repo-client root if one is found on the way (main
spec §4.1 discovery is reused opportunistically, not required), and stopping
(exclusive) at `$HOME` and at the filesystem root. If a repo client root is
found but carries none of the evidence directories, it is still recorded as a
**candidate** anchor for every project-scope target (see `--scope project`
behavior below) — the client root is the natural home for a project-scoped
skill about that client.

### 2.1 Alias-deduplication rule

Because Gemini CLI treats `.agents/skills/` as an alias of `.gemini/skills/`
in both scopes, installing to both under the same anchor (or in the same
`$HOME`) is redundant: Gemini would discover two copies and shadow one. In
`--scope auto|user|project` and `--all` selection, when an `agents-*` target
and its same-scope, same-anchor `gemini-*` counterpart are **both** eligible,
select only the `agents-*` target and report the `gemini-*` one with action
`skipped-alias` (JSON) / `skipped (covered by agents-…)` (human). An explicit
`--target gemini-user`/`--target gemini-project` (§4.3) overrides the rule
and installs there anyway. The rule never applies in the other direction and
never applies across different anchors. No analogous rule exists for
`claude-code-*` unless and until Claude Code documents the alias, at which
point adding it is a minor-version change.

### 2.2 Framework-support matrix (informative)

`claude-code-*` serves Claude Code. `gemini-*` serves Gemini CLI.
`agents-project` serves Gemini CLI (documented alias), **Google Antigravity**
(all three flavors — IDE, editor agent, and CLI — read
`<workspace>/.agents/skills/` as their workspace-scope location), and any
other tool implementing the Agent Skills standard's interoperable directory.
`agents-user` serves Gemini CLI and other standard-compliant tools, but not
reliably Antigravity. `antigravity-user` serves Antigravity's global scope.
The SKILL.md payload is identical for all targets — the format is the shared
Agent Skills standard (YAML frontmatter with `name`/`description` + Markdown
body) — so no per-framework content transformation exists or is permitted.

### 2.3 Antigravity user scope: chosen path and its rationale

Antigravity's global-scope skill discovery is inconsistently documented
(official docs, codelabs, and observed behavior name different directories:
`~/.gemini/antigravity/skills/`, `~/.gemini/config/skills/`, and per-flavor
built-in dirs). This spec pins `~/.gemini/config/skills/` because it is the
one location empirically recognized by **all three** Antigravity flavors at
the time of writing; `~/.gemini/antigravity/skills/` is documented but not
universally honored. Implementer rules that follow from this volatility:

1. The path lives only in the declarative target table, so tracking upstream
   changes is a data-only, minor-version edit (append a new target id such as
   `antigravity-user-v2` rather than mutating `antigravity-user`'s meaning if
   the old path must be retired — ids are append-only per §2).
2. The evidence rule is Antigravity-specific on purpose: `~/.gemini/` alone
   proves only Gemini-family tooling, so `antigravity-user` requires
   `~/.gemini/antigravity/` or `~/.gemini/antigravity-cli/` (Antigravity's
   own state directories) to exist. Given that evidence, creating the
   `config/skills/` chain beneath the existing `~/.gemini/` root is permitted
   (this is the one sanctioned two-level create; it does not violate §1.2
   because the framework root `~/.gemini/` and the Antigravity state dirs
   already exist).
3. `antigravity-user` and `gemini-user` are distinct directories under the
   same `~/.gemini/` root; both may be selected in the same `--scope user`
   run. No alias-dedupe relationship exists between them or between
   `antigravity-user` and `agents-user`.
4. README must carry a short note that Antigravity's global skills location
   has churned and that `skill list` shows exactly where this binary
   installs.

Escape hatches, both always available and probed **before** the built-in
table:

* `--path <dir>` — treat `<dir>` as the skills directory verbatim (evidence
  rule waived; `<dir>` must exist).
* `GRW_SKILL_DIRS` — colon-separated list of skills directories; each entry
  behaves like a `--path` candidate with id `env:<dir>`.

Installation layout inside any skills dir is always:

```
<skills-dir>/git-repo-worktree/SKILL.md
```

## 3. The sentinel

The embedded SKILL.md's YAML frontmatter is extended **at install time** with
two generated fields (the embedded copy itself does not contain them):

```yaml
x-installed-by: git-repo-worktree
x-installed-version: "<crate semver>"
```

A file **bears the sentinel** iff its frontmatter parses and
`x-installed-by == "git-repo-worktree"`. A file is **current** iff it bears
the sentinel, `x-installed-version` equals the running binary's version, and
the body (everything after frontmatter) is byte-identical to the embedded
body. Frontmatter fields other than the two generated ones are compared as
part of the body-equivalence check (i.e., current ⇔ the file is exactly what
this binary would write).

## 4. Subcommands

### 4.1 `skill show`

```
git repo-worktree skill show
```

Print the embedded SKILL.md (without sentinel fields) to stdout verbatim.
Exit 0. With `--json`: `result` = `{"skill": "<full text>", "name":
"git-repo-worktree", "version": "<crate semver>"}`. This is the zero-install
path: an orchestrator can pipe it anywhere.

### 4.2 `skill list`

```
git repo-worktree skill list [--json]
```

Probe every candidate (env entries, then built-in table) and report, without
mutating anything. Human format: one line per candidate,
`<id>  <path>  <status>` where status ∈ `not-eligible` (evidence missing),
`eligible` (evidence present, not installed), `installed vX.Y.Z (stale)`,
`installed (current)`, `foreign` (file exists, no sentinel), `unreadable`.
`--json` `result`:

```json
{"candidates":[{"id":"claude-code-user","scope":"user",
  "skills_dir":"/home/me/.claude/skills",
  "skill_path":"/home/me/.claude/skills/git-repo-worktree/SKILL.md",
  "eligible":true,"installed":true,"current":false,
  "installed_version":"1.1.0","foreign":false}]}
```

Exit 0 always (listing cannot fail except for I/O errors → exit 1).

### 4.3 `skill add`

```
git repo-worktree skill add [--scope auto|user|project] [--target <id>]...
                            [--path <dir>] [--all] [--force] [--dry-run]
```

Selection (exactly one mode applies; `--path`, `--target`, `--scope`, and
`--all` are mutually exclusive, violation → exit 2). Since multiple agent
frameworks routinely coexist on one machine, scope-based selection installs
to **every** eligible target in that scope (after §2.1 alias dedupe), not
just the first — "set up my agents" should mean all of them:

* `--path <dir>` → install to `<dir>/git-repo-worktree/SKILL.md`. `<dir>`
  missing → exit 1 `E_NO_SKILL_TARGET`.
* `--target <id>` (repeatable) → install to exactly the named built-in
  target(s); ids per §2 table. Unknown id → exit 2. Known-but-ineligible
  (evidence missing) → exit 1 `E_NO_SKILL_TARGET` naming the missing
  evidence directory. `--target` bypasses the §2.1 alias-dedupe rule.
* `--scope user` → every eligible user-scope built-in target (alias-deduped).
  None eligible → exit 1 `E_NO_SKILL_TARGET`, message lists each user-scope
  framework root probed (`~/.agents/`, `~/.claude/`, `~/.gemini/`, and the
  Antigravity evidence dirs `~/.gemini/antigravity{,-cli}/`) with the
  note that the tool will not create framework roots; create one (e.g.
  `mkdir -p ~/.agents/skills`) or use `--path`.
* `--scope project` → every eligible project-scope built-in target
  (alias-deduped), each at its own resolved anchor. If none is eligible but
  a repo-client root was found → exit 1 `E_NO_SKILL_TARGET` with a message
  naming the creatable options under the client root
  (`<client-root>/.agents/skills/`, `<client-root>/.claude/skills/`,
  `<client-root>/.gemini/skills/`), recommending `.agents/skills/` as the
  interoperable choice, and instructing to `mkdir -p` one and re-run — the
  tool refuses to create framework roots per §1.2.
* `--scope auto` (default) → env entries (`GRW_SKILL_DIRS`) first, always;
  then every eligible project-scope target; if no project-scope target was
  eligible, every eligible user-scope target (both alias-deduped). Nothing
  eligible anywhere → exit 1 `E_NO_SKILL_TARGET`, message lists every
  candidate and why it was rejected, and shows the remedies
  (`mkdir -p ~/.agents/skills`, or the project-scope equivalent, or
  `--path`).
* `--all` → install to **every** eligible candidate (env + built-ins,
  alias-deduped).

Install algorithm per selected target (idempotent):

1. `mkdir -p` the `skills/` child and the `git-repo-worktree/` leaf (mode
   0755) — permitted because the framework root's existence was verified.
2. Compare existing file, if any:
   * absent → write (temp file + rename, mode 0644), report `installed`.
   * current (§3) → no write, report `unchanged`.
   * sentinel + stale → overwrite, report `updated <old> → <new>`.
   * **no sentinel** → without `--force`: exit 1 `E_SKILL_FOREIGN`, path
     named, nothing written (with `--all`, other targets still proceed and
     the overall exit is 1 with per-target results in JSON). With `--force`:
     overwrite, report `replaced (foreign)`.
3. `--dry-run`: perform all checks and report the verdict per target
   (`would install` / `unchanged` / `would update` / `foreign`), write
   nothing, exit 0 (or 1 if a foreign file would block a non-`--force` run).

`--json` `result`: `{"actions":[{"id":"...","skill_path":"...",
"action":"installed|unchanged|updated|replaced|blocked|skipped-alias",
"from_version":null,"to_version":"..."}]}`. (`skill_path` is `null` for
`skipped-alias` and for `unavailable` targets surfaced by `list`.)

### 4.4 `skill sync`

```
git repo-worktree skill sync [--dry-run] [--json]
```

Refresh every installed copy after a tool upgrade: probe all candidates
(§4.2), and for each that is `installed` with the sentinel, apply §4.3 step 2
(stale → update, current → unchanged). Foreign files are reported and skipped
(never an error for `sync`). Installs nothing new. Exit 0 if no I/O failures.
This is the command upgrade documentation tells users and agents to run;
`add` on an existing sentinel install is equivalent for a single target.

### 4.5 `skill remove`

```
git repo-worktree skill remove [--scope auto|user|project] [--target <id>]...
                               [--path <dir>] [--all] [--force] [--json]
```

Selection mirrors §4.3 (including `--target`), with two removal-specific
differences: `--scope auto` means "every candidate where our sentinel is
installed, across **all** frameworks and both scopes" (removal should
default to thorough, not first-hit), and the §2.1 alias-dedupe rule does not
apply (if copies exist in both `.agents/skills/` and `.gemini/skills/`,
remove both). Per target: file absent → exit 1 `E_SKILL_NOT_INSTALLED`
(single-target mode) or report `absent` (`--all`/auto). Sentinel present →
delete `SKILL.md`, then delete the `git-repo-worktree/` leaf directory iff
now empty; never delete the `skills/` parent. No sentinel → refuse without
`--force` (`E_SKILL_FOREIGN`), delete with it.

## 5. New stable error codes

`E_NO_SKILL_TARGET`, `E_SKILL_FOREIGN`, `E_SKILL_NOT_INSTALLED`. Rules from
main spec §10 apply (append-only; never rename).

## 6. Build integration

* `skill/SKILL.md` lives in the crate repository; CI must fail if it exceeds
  500 lines or its frontmatter lacks `name`/`description` or contains either
  sentinel field (they are generated, never authored).
* The crate embeds it with `include_str!`; `skill show` and all installs
  derive from that single embedded copy.
* Release artifacts additionally ship `SKILL.md` alongside the binary for
  humans installing without cargo.

## 7. Testing requirements (extends main spec §10)

1. `skill list`/`add`/`remove`/`sync` matrix over fabricated `$HOME` and
   project trees (set `HOME` in tests): no framework roots anywhere →
   `E_NO_SKILL_TARGET` with remedies; `~/.claude` only → auto installs
   `claude-code-user` only; `~/.claude` + `~/.gemini` → `--scope user`
   installs both; project `.claude/` present → auto installs project scope
   and skips user scope; `--all` installs everything eligible.
2. Alias dedupe (§2.1): `~/.agents` + `~/.gemini` both present → auto/`--all`
   install `agents-user` only and report `gemini-user` as `skipped-alias`;
   `--target gemini-user` installs there anyway; same pair at a project
   anchor behaves identically; `~/.agents` + `~/.claude` → **both** install
   (no dedupe). `remove --scope auto` with copies in both alias dirs removes
   both.
3. Sentinel semantics: hand-written foreign file blocks `add` and `remove`
   without `--force`; passes with it; `sync` skips it with a report.
   Repeat once under a Gemini-only fixture to pin that sentinel logic is
   target-independent.
4. Idempotence: second `add` reports `unchanged`, zero writes (assert mtime
   unchanged); version bump (inject via test hook overriding reported
   version) → `sync` reports `updated` in **every** installed location
   across frameworks in one invocation.
5. Evidence rule: `--scope project` inside a repo client with no framework
   roots names all three creatable directories (recommending
   `.agents/skills/`) in the error; the tool never creates `.agents/`,
   `.claude/`, or `.gemini/` (assert all absent after run).
6. Antigravity target: fixture with `~/.gemini/antigravity/` present but no
   `~/.gemini/config/` → `--scope user` installs to
   `~/.gemini/config/skills/git-repo-worktree/SKILL.md` (the sanctioned
   two-level create, §2.3.2); fixture with `~/.gemini/` only (no antigravity
   state dirs) → `antigravity-user` reported `not-eligible` and
   `--target antigravity-user` exits 1 `E_NO_SKILL_TARGET`; fixture with
   both `~/.gemini/` and `~/.gemini/antigravity-cli/` → `--scope user`
   selects `gemini-user` **and** `antigravity-user` (no dedupe between
   them, §2.3.3).
7. `GRW_SKILL_DIRS` and `--path` precedence over built-ins in `auto`.
8. `skill show` output round-trips: installing its stdout via `--path` then
   `skill list` reports `foreign` (no sentinel) — pinning that `show` output
   is clean and that sentinel detection keys on frontmatter, not body.

## 8. User journeys (normative examples)

**J-S1 — Agent bootstraps itself on a fresh machine [agent]:**
`git repo-worktree skill list --json` → no eligible target →
`git repo-worktree skill show` and inject the text into its own context;
optionally `mkdir -p ~/.agents/skills && git repo-worktree skill add --scope
user` (the interoperable directory serves Claude Code companions, Gemini
CLI, and other standard-compliant tools alike).

**J-S2 — Human sets up a shared client for the team:** from the client root,
`mkdir -p .agents/skills && git repo-worktree skill add --scope project`,
commit `.agents/skills/git-repo-worktree/SKILL.md` to the manifest-adjacent
tooling repo so every enlistment's agents — Claude Code companions via the
standard, Gemini CLI, and all Antigravity flavors, whichever each teammate
runs — discover it. A Claude-only team can use `.claude/skills` identically.

**J-S3 — After upgrading the tool:** `cargo install git-repo-worktree` then
`git repo-worktree skill sync` — every installed copy, across all
frameworks and scopes, reports `updated`.

**J-S4 — Mixed-framework machine:** a developer with both `~/.claude/` and
`~/.gemini/` runs `git repo-worktree skill add --scope user` → installs to
`claude-code-user` and `gemini-user` in one command (or, if `~/.agents/`
also exists, to `claude-code-user` + `agents-user`, with `gemini-user`
reported `skipped-alias` per §2.1).

**J-S5 — Antigravity user:** an Antigravity IDE user (so
`~/.gemini/antigravity/` exists) runs `git repo-worktree skill add --target
antigravity-user` → installed at `~/.gemini/config/skills/git-repo-worktree/
SKILL.md`, discoverable by all three Antigravity flavors; their project
trees are covered by J-S2's `.agents/skills/` install without any
Antigravity-specific step.

— end of addendum —