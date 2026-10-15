# Stateful fuzzing

`harness.py` builds a tiny world (three projects, a manifest repository, a
source client and a reference client made with real `repo init`/`repo sync`)
and drives it with random sequences of operations: upstream commits and
manifest changes, source syncs and commits, `add`, `remove`, `repair`,
`move`, defaults, crashes mid-`add` and mid-`move`, killed overlay daemons,
and work inside trees (`repo start`, commits, `repo sync`). After every step it
checks invariants, and `repo sync` in a tree is compared with a fresh client.
Hypothesis shrinks a failure to a minimal sequence; its commands are written
to `findings/`.

```sh
python3 -m venv /tmp/grw-fuzz-venv && /tmp/grw-fuzz-venv/bin/pip install hypothesis
cargo build --features test-hooks
GRW_BIN=target/debug/git-repo-worktree GRW_FUZZ_MAINTENANCE=1 GRW_FUZZ_CRASH=1 \
  /tmp/grw-fuzz-venv/bin/python fuzz/harness.py 40     # macOS or Linux
fuzz/run-linux.sh 4 40                                  # Linux in Docker, overlays on
```

Both need a git-repo checkout in `GRW_TEST_REPO_URL` (default
`/tmp/git-repo-src`). Run several workers with distinct `GRW_FUZZ_WORKER`
values to fuzz in parallel. Check `git-repo-worktree version` in the output:
it must be the build you meant to test. Turn each finding into a test in
`tests/` before fixing it.
