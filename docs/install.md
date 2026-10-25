# Installing git-repo-worktree

The runtime requires Git 2.31 or newer and, for `add` only, repo 2.30 or newer.
Linux and macOS (x86-64 and ARM64) are supported. Except for `cargo install`,
every method below installs the same prebuilt binary from the project's GitHub
releases. Git runs it as `git repo-worktree` once `git-repo-worktree` is on
`PATH`.

## With Cargo

```sh
cargo install git-repo-worktree    # build from source on crates.io
cargo binstall git-repo-worktree   # prebuilt binary, via cargo-binstall
```

Both install into `~/.cargo/bin`. Building from source needs a current stable
Rust toolchain, and the result's `version` output omits the commit hash.
`cargo binstall` is a separate Cargo plugin that downloads the release binary
instead of compiling; install it first with `cargo install cargo-binstall` (or
see [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) for faster
options).

## With a tool manager

```sh
mise use -g github:jomof/git-repo-worktree      # mise
ubi --project jomof/git-repo-worktree --in ~/.local/bin
eget jomof/git-repo-worktree --to ~/.local/bin --asset musl   # Linux
eget jomof/git-repo-worktree --to ~/.local/bin                # macOS
```

`eget` asks which Linux archive to use unless given `--asset musl` (the static
build) or `--asset gnu`. `mise` also accepts `ubi:jomof/git-repo-worktree`. Pin
a release with `@<version>` (mise) or `--tag v<version>` (ubi, eget). mise
holds back very new releases from `latest` for a short minimum release age, so
for a day or so after a release it still resolves the previous one unless the
version is pinned.

These install only the binary (as do the Cargo methods). Without the manual pages,
`git repo-worktree --help` cannot open a page; use `git repo-worktree -h` or
`git-repo-worktree --help`, or install the pages from the release archive
(`git-repo-worktree*.1`) into a `man1` directory on `MANPATH`.

## With the install script

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/jomof/git-repo-worktree/releases/latest/download/install.sh |
  sh
```

The installer selects the macOS or Linux archive for the current x86-64 or
ARM64 machine, verifies its SHA-256 checksum, and atomically installs it in
`~/.local/bin`, with manual pages in `~/.local/share/man` so
`git repo-worktree --help` works. If that directory is not the
`git-repo-worktree` Git would find, it configures a global `repo-worktree` Git
alias pointing at the installed binary. It never replaces a conflicting existing
alias. Git runs a `git-repo-worktree` found on `PATH` before any alias, so the
installer warns when another copy (for example an older `cargo install` in
`~/.cargo/bin`) would shadow the one it installed.

Rerun the same command to upgrade. To pin a release or choose another
directory, pass installer arguments after `sh -s --`:

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/jomof/git-repo-worktree/releases/download/v3.0.0/install.sh |
  sh -s -- --version v3.0.0 --install-dir "$HOME/bin"
```

Use `--no-git-alias` if the chosen directory is already managed on `PATH`.

## After installing

An optional shorter alias is:

```sh
git config --global alias.rwt '!git-repo-worktree'
```

Prebuilt release archives contain the binary, license, README, `SKILL.md`,
man pages, and bash, zsh, and fish completions. Linux releases have a GNU
archive and a static musl archive per architecture; the install script uses the
musl build.

On Linux, install `fuse-overlayfs` to let large prebuilt projects become
copy-on-write overlays (see [How trees are populated](population.md)).
