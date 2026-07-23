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
