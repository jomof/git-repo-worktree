# Track 1 Technical Spike: Full `repo` & `repox` Fidelity and Missing Symlinks

This document records the deep-dive investigation into how `repo` (`project.py`, `manifest_xml.py`) and `repox` (`workspace.py`) interact with `.repo/` and per-project `.git` directories, why `repo` symlinks are missing today, and the results of our empirical test spikes in `/tmp/spike-repo-fidelity`.

---

## 1. Root Cause of "Missing Repo Symlinks"

### 1.1 Manifest `<linkfile>` and `<copyfile>` Status vs. `<extend-project>` Bug
In `studio-main/.repo/manifests/default.xml`, there are 13 `<linkfile>` entries and 3 `<copyfile>` entries:
* **`tools/base`**: 11 `<linkfile>` entries (`tools/BUILD.bazel`, `BUILD.bazel`, `MODULE.bazel`, `MODULE.bazel.lock`, `.bazelignore`, `.bazelversion`, `.bazelrc`, `.editorconfig`, `prebuilts/studio/sdk/BUILD`, `prebuilts/studio/sdk/README.md`, `tools/vendor.MODULE.bazel`) and 3 `<copyfile>` entries (`CLAUDE.md`, `AGENTS.md`, `GEMINI.md`).
* **`tools/buildSrc`**: 2 `<linkfile>` entries (`tools/build.gradle`, `tools/gradle.properties`).

All 13 `<linkfile>` and 3 `<copyfile>` entries ARE created (`state: "done"`) across all 14 existing `studio-main-*` worktrees. However, we found two real symlink gaps:
1. **`<extend-project>` Directives Are Silently Dropped ([`src/manifest.rs:327-365`](../src/manifest.rs#L327-L365))**:
   * `extract_links_and_copies` in `src/manifest.rs` only inspects XML elements where `node.tag_name().name() == "project"`.
   * In `repo`'s manifest format (`manifest_xml.py`), `<extend-project name="...">` elements in submanifests or local overlays can also declare `<copyfile>` and `<linkfile>` children. Currently `git-repo-worktree` ignores `<extend-project>` completely.
2. **Missing `.repo/` and Per-Project `.git -> ../../.repo/projects/<path>.git` Symlinks**:
   * In a standard `repo` client (`studio-main`), every project's `<path>/.git` is a **relative symlink** to `../../.repo/projects/<path>.git`, and inside `.repo/projects/<path>.git`, `objects`, `description`, `hooks`, `info`, and `rr-cache` are symlinks into `.repo/project-objects/<name>.git/*`.
   * In `git-repo-worktree`, `<wt>/.repo` is completely omitted (`spec.md` P1), and `<wt>/<path>/.git` is a regular file containing `gitdir: <source>/.repo/projects/<path>.git/worktrees/<id>`.

---

## 2. Exact Mechanics of `repo` and `repox`

### 2.1 How `repo` Locates `.repo/` and Reads Project State
1. **Top-level discovery (`main.py` / `/usr/bin/repo`)**:
   * Walks parent directories from `cwd` looking for `.repo/repo/main.py` and `.repo/manifest.xml`.
2. **Project path mapping (`manifest_xml.py:GetProjectPaths`, lines 2025–2075)**:
   * For each `<project name="..." path="..."/>`, `GetProjectPaths` sets:
     * `worktree = <topdir>/<path>`
     * `gitdir = <topdir>/.repo/projects/<path>.git`
     * `objdir = <topdir>/.repo/project-objects/<name>.git`
     * *(unless `.repo/projects/<path>.git` does not exist AND `UseGitWorktrees` is enabled in `.repo/manifests.git/config`, in which case `gitdir = objdir = <topdir>/.repo/worktrees/<name>.git`)*.
3. **How `repo` reads `HEAD`, branches, and working tree status (`project.py:744-815`)**:
   * `self.work_git = _GitGetByExec(self, bare=False, gitdir=self.gitdir)` — executes `git` with `cwd=self.worktree` and `GIT_DIR=self.gitdir`!
   * `self.bare_git = _GitGetByExec(self, bare=True, gitdir=self.gitdir)` — executes `git --bare` with `GIT_DIR=self.gitdir`.
   * `self.bare_ref = GitRefs(self.gitdir)` — **directly reads files on disk inside `self.gitdir` (`<wt>/.repo/projects/<path>.git/HEAD`, `refs/`, `packed-refs`) in Python without calling `git`!**
   * Specifically, `Project._GetHead()` (`project.py:800-812`) does:
     ```python
     if not self.use_git_worktrees and self.bare_ref.is_loaded:
         head = self.bare_ref.head
         if head:
             return head
     return self.work_git.GetHead()
     ```
   * **Critical consequence**: Whenever `use_git_worktrees` is `False` (the default for standard `repo` clients like `studio-main`), `repo branches`, `repo status`, `repo start`, and `repo upload` read `<wt>/.repo/projects/<path>.git/HEAD` directly via `GitRefs` AND pass `GIT_DIR=<wt>/.repo/projects/<path>.git` to every `git` subprocess!
4. **How `repo sync -l` validates `<wt>/<path>/.git` (`project.py:4709-4728`)**:
   * During `Sync_LocalHalf`, `_InitWorkTree()` calls `_removeBadGitDirLink(dotgit)`:
     ```python
     dotgit_path = os.path.realpath(dotgit)
     if os.path.realpath(self.gitdir) != dotgit_path:
         platform_utils.remove(dotgit)
     ```
   * If `realpath(<wt>/<path>/.git) != realpath(<wt>/.repo/projects/<path>.git)`, `repo sync -l` **deletes `<wt>/<path>/.git`** and replaces it with a symlink to `self.gitdir`!

### 2.2 How `repox` Locates the Workspace and Git Roots
In `/usr/local/google/home/jomof/projects/repox/repox/workspace.py`:
1. `_find_repo_root` (`lines 108-110`) walks up `self.start_dir.parents` checking `(parent / ".repo").exists()`.
2. `discover_git_roots` (`lines 189-199`) runs `os.walk(repo_root)` (with default `followlinks=False`):
   * If `".git" in dirs or ".git" in files`, it adds `Path(root)` to `roots` and sets `dirs[:] = []`.
   * Because `os.walk` uses `scandir`/`lstat` to separate directories from files, a directory symlink `<wt>/tools/idea -> <source>/tools/idea` is placed in `dirs`, **not** recursed into when `followlinks=False`, so `repox` never sees `.git` inside `symlink-shared` projects!
   * Conversely, for a real directory `<wt>/proj_a` where `<wt>/proj_a/.git` is a symlink to a directory (`../.repo/projects/proj_a.git`), Python's `os.walk` places `.git` in `dirs`, which matches `if ".git" in dirs` and registers `<wt>/proj_a`!

---

## 3. Summary of Spike Models Tested in `/tmp/spike-repo-fidelity`

| Model | Description | Result | Why |
| :--- | :--- | :--- | :--- |
| **Model A** | `wt/.repo -> source/.repo` (direct symlink) | **FAILED** | `repo branches`/`upload` read `source/.repo/projects/<path>.git/HEAD` (wrong branch); `repo sync -l` deletes `wt/<path>/.git` gitfile and links it to `source`! |
| **Model B** | `wt/.repo/projects/<path>.git/HEAD -> .../worktrees/<id>/HEAD` (symlinked `HEAD`) | **FAILED** | Git's `setup.c:is_git_directory()` uses `lstat("HEAD")` and rejects any gitdir where `HEAD` is a symlink (`!S_ISREG`) unless it's a symref to `refs/*`. |
| **Model D** | Synthesized `wt/.repo/projects/<path>.git/` directory with its own regular-file `HEAD` | **PARTIAL** | Works for `repo`, but `git checkout` via `worktrees/<id>/HEAD` vs `.repo/projects/<path>.git/HEAD` creates two separate `HEAD` files (split-brain between `git worktree list` and `repo`). |
| **Model E1** | **`wt/.repo/projects/<path>.git -> source/.repo/projects/<path>.git/worktrees/<id>`** (with shared symlinks inside `worktrees/<id>`) + **`wt/<path>/.git -> ../../.repo/projects/<path>.git`** | **100% PASSED** | Single unified regular-file `HEAD` and `index` in `worktrees/<id>/`; `realpath(wt/<path>/.git) == realpath(wt/.repo/projects/<path>.git)`. |

---

## 4. Verification Matrix for Model E1 in `/tmp/spike-repo-fidelity`

Under Model E1:
1. `<source>/.repo/projects/<path>.git/worktrees/<id>/` already contains regular files `HEAD`, `index`, `commondir` (`../..`), `gitdir`, and `logs/` created by `git worktree add`.
2. We add relative symlinks inside `worktrees/<id>/` pointing to `../../<entry>` for:
   * `config`, `objects`, `refs`, `packed-refs`, `hooks`, `rr-cache`, `info`, `description`, `.repo_config.json` (whichever exist in `<source>/.repo/projects/<path>.git/`).
3. We symlink `<wt>/.repo/projects/<path>.git` $\rightarrow$ `<source>/.repo/projects/<path>.git/worktrees/<id>`.
4. We replace `<wt>/<path>/.git` (the `gitdir:` file) with a relative symlink `../../.repo/projects/<path>.git`, and update `worktrees/<id>/gitdir` to `<wt>/<path>/.git\n`.

### Command Verification Results (Model E1)
* `git status` inside `<wt>/proj_a`: **PASS (Exit 0)** — `On branch branch3, nothing to commit`.
* `git checkout -b test_branch` inside `<wt>/proj_a`: **PASS (Exit 0)** — updates `worktrees/<id>/HEAD`, immediately visible to both `git` and `repo`.
* `repo start branch3 proj_a` inside `<wt>`: **PASS (Exit 0)** — creates `branch3` with `branch.branch3.remote` and `branch.branch3.merge` configured.
* `repo status` inside `<wt>`: **PASS (Exit 0)** — `project proj_a/ branch branch3`.
* `repo branches` inside `<wt>` vs `<source>`: **PASS (Exit 0)** — shows `* branch3` in `<wt>`, and does NOT mark `branch3` active in `<source>`.
* `repo diff` inside `<wt>`: **PASS (Exit 0)** — shows exact working tree diff in `<wt>`.
* `repo forall -c ...` inside `<wt>`: **PASS (Exit 0)** — iterates over all worktree projects with correct `$REPO_PROJECT`, `$REPO_PATH`, `$REPO_REMOTE`.
* `repo upload --dry-run --yes` inside `<wt>`: **PASS** — selects `branch3` (1 commit) in `proj_a` to upload to `main`.
* `repo sync -l` inside `<wt>`: **PASS (Exit 0)** — `Syncing: 100% (2/2), done in 0.308s`. `<wt>/proj_a/.git` symlink is preserved intact!
* `git -C <source>/proj_a worktree list`: **PASS (Exit 0)** — lists both `<source>` and `/tmp/spike-repo-fidelity/wt1/proj_a [branch3]`.
* `git -C <source>/proj_a checkout branch3`: **PASS (Exit 128)** — `fatal: 'branch3' is already used by worktree at '/tmp/spike-repo-fidelity/wt1/proj_a'`.
* `remove` / `repair` handling: Because `git worktree remove` and `git worktree repair` validate that `<wt>/<path>/.git` is a regular file (`gitdir: ...`), `git-repo-worktree` simply writes `gitdir: <source>/.repo/projects/<path>.git/worktrees/<id>\n` to `<wt>/<path>/.git` right before calling `git worktree remove` or `git worktree repair` (and restores the relative symlink after `repair`).
