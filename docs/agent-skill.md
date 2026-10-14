# Agent skill

The binary embeds [the agent skill](../skill/SKILL.md) as its single source of truth:

```sh
git repo-worktree skill show
git repo-worktree skill list
git repo-worktree skill add --scope user
git repo-worktree skill add --scope project
git repo-worktree skill sync
git repo-worktree skill remove
```

Automatic installation only uses framework roots that already exist. It can create
the `skills/git-repo-worktree` children, but never invents `.agents`, `.claude`, or
`.gemini`. Use `--path <existing-skills-dir>` for another framework or set the
colon-separated `GRW_SKILL_DIRS`.

When both `.agents/skills` and `.gemini/skills` are eligible at the same scope,
`.agents/skills` is installed once as Gemini's interoperable alias. An explicit
`--target gemini-user` or `--target gemini-project` overrides that deduplication.

Antigravity's global skills location has changed across releases. This version uses
`~/.gemini/config/skills` only when Antigravity-specific state is present.
`git repo-worktree skill list` is the authoritative report of the exact path this
binary will use.

Installed copies carry ownership and version sentinels. Foreign files are never
overwritten or removed without `--force`; `skill sync` updates every sentinel-bearing
copy after a tool upgrade.
