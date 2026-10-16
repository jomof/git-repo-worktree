# Fuzzing

Two drivers exercise git-repo-worktree against real repo clients in a small
world: projects `a` (copyfile and linkfile), `lib/b` with `lib/b/nested`
inside it, `prebuilts/pre` (clone-depth), `hooks` (repo-hooks with a
recording pre-upload hook), `sub` (with a submodule), `extra/grouped`
(notdefault, from an included manifest), and `c` (added and removed by
manifest changes), plus a source client, a reference client, and local bare
"gerrit" repositories that receive `repo upload`.

* `harness.py` (Hypothesis stateful testing) runs random sequences of
  upstream, manifest, source, tree, review, and fault operations, checks
  invariants after every step, compares `repo sync` in a tree with a fresh
  client, and shrinks a failure to a minimal sequence written to `findings/`.
  Modes are set by environment variables documented at the top of the file:
  world variant (`plain`, `partial`, `worktree`), overlays, background
  maintenance, crash hooks, random kills, faults, a full disk, and older
  releases that create trees (`fetch-old.sh` downloads them).
* `race.py` runs random batches of concurrent tool operations on one client,
  then checks every result and the world. Failing rounds are written with
  their seed; `race.py --seed <seed>` reruns one.

```sh
python3 -m venv /tmp/grw-fuzz-venv && /tmp/grw-fuzz-venv/bin/pip install hypothesis
CARGO_TARGET_DIR=/tmp/grw-fuzz-target cargo build --features test-hooks
GRW_BIN=/tmp/grw-fuzz-target/debug/git-repo-worktree PYTHON=/tmp/grw-fuzz-venv/bin/python \
  fuzz/run-local.sh 20          # four configurations on this machine
fuzz/run-linux.sh 40 60         # five workers in Docker, overlays and a tmpfs disk
```

Both need a git-repo checkout in `GRW_TEST_REPO_URL` (default
`/tmp/git-repo-src`) and `gh` for the older releases. Check the
`git-repo-worktree` version line in the output: it must be the build you meant
to test. Random kills make outcomes timing-dependent, so Hypothesis may report
flakiness in that mode. Turn each finding into a test in `tests/` before
fixing it.
