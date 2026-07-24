# Releasing

GitHub hosts the installer, archives, checksum manifest, provenance
attestations, and release notes. No package server or GitHub Pages site is
required.

## One-time repository setup

Enable immutable releases in the repository's release settings. The workflow
creates a draft, uploads every asset, and publishes only after all platform
builds and smoke tests succeed, which is the sequence required by immutable
releases.

## Publish a release

1. Update the version in `Cargo.toml`, regenerate `Cargo.lock` if needed, and
   merge the change after CI passes.
2. Create and push a `v<version>` tag pointing at that commit. The workflow
   rejects a tag that does not exactly match the Cargo package version.
3. Watch the `Release` workflow. It builds GNU and static musl Linux archives
   plus native Intel and ARM macOS archives, smoke-tests all four installer
   assets, creates SHA-256 checksums and attestations, then publishes the draft.
4. Test the public entry point:

   ```sh
   curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
     https://github.com/jomof/git-repo-worktree/releases/latest/download/install.sh |
     sh
   git repo-worktree version
   ```

The workflow refuses to replace assets on an already published release. Fixes
therefore require a new version and tag.

To verify a downloaded archive's GitHub provenance:

```sh
gh attestation verify git-repo-worktree-<target>.tar.gz \
  --repo jomof/git-repo-worktree
```
