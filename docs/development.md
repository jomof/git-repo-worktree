# Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

The build script generates `git-repo-worktree.1` and one page per subcommand
with `clap_mangen`. Completion scripts are generated from the same CLI
definition using the `completions` subcommand.

Some behavior needs more than the default test environment:

* Tests in `tests/repo_fidelity.rs` drive a real `repo` client. They need
  `repo` on `PATH` and `GRW_TEST_REPO_URL` naming a git-repo checkout to
  initialize from (`git clone https://gerrit.googlesource.com/git-repo`).
* Overlay tests run only on Linux with `fuse-overlayfs`. From macOS, run
  `scripts/linux-test.sh [cargo test args]` (requires Docker). It builds and tests
  in an Ubuntu container with FUSE enabled, as a non-root user, on an ext4
  volume without reflink support.

See [RELEASING.md](../RELEASING.md) for the GitHub-hosted release process.
