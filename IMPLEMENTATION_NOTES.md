# Implementation notes

The specifications leave the following boundary cases open; these choices keep the
machine interface deterministic:

- `repair --json` returns the specified full tree object when exactly one tree is
  selected. When repairing all or multiple trees, `result` is
  `{"trees":[<full tree objects>]}`.
- The add-time repo version check reads the installed repo launcher's `VERSION`
  tuple. It never executes repo for version discovery, preserving the rule that add
  invokes repo only as `repo manifest -r`. If a custom launcher exposes no readable
  tuple, availability and compatibility are determined by the required manifest
  command.
- A normal snapshot-command or snapshot-parse failure removes the just-created empty
  registry entry so the same name can be retried. If the process is killed before
  pin completion, `add --continue` refuses the unpinned entry; `remove <name>` safely
  clears it. If a pinned snapshot exists but the process died before metadata or the
  breadcrumb was flushed, continuation reconstructs those pieces from the immutable
  snapshot without invoking repo again.
- Snapshot `git_common_dir` discovery uses the same bounded job count as project
  creation. Project creation uses prefix dependencies rather than global path-depth
  barriers, allowing unrelated deep paths to start immediately.
- `.repo/project.list` entries omitted by `repo manifest -r` are pinned from the
  corresponding checkout `HEAD`. Copy/link declarations are reconstructed by
  following the active `.repo/manifest.xml` include graph and applying sorted local
  manifests. The resulting pinned projects and directives are atomically appended
  to `snapshot.xml` before `pin done` is journaled, so crash recovery never re-pins
  them from a later source state.
- Project completions that arrive within a short coalescing window share one journal
  write and `fsync`. State changes, progress output, and dependent scheduling happen
  only after that durability boundary. If a process or machine fails before it,
  `add --continue` safely repeats the idempotent worktree check. A partial trailing
  journal record is truncated during recovery; malformed durable records still fail
  closed.
