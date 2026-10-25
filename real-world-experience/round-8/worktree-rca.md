# Worktree Root-Cause Analysis

## Summary
The verdict is **agent error**. The issue was not caused by `git repo-worktree remove` leaking state or failing to clean up an old pool entry. Instead, `git repo-worktree add` is designed to snapshot the main source client's current HEADs. Because `~/projects/studio-main/tools/vendor/google` is currently on the `mother` branch (at `f0d61ab633`, which is 6 commits ahead of `goog/studio-main`), all newly created pool slots and worktrees inherit these 6 commits. The agent for `android-cli-1p` successfully followed the `repo-worktree` skill's instruction to explicitly `checkout goog/studio-main`, resolving the baseline. However, the agent for `v4-package` failed to execute this step, committing directly on top of the inherited `mother` branch. 

## Timeline
* **2026-10-25 14:23:15 UTC:** Agent for `android-cli-1p` correctly executed `checkout: moving from f0d61ab633... to goog/studio-main` (observed in `android-cli-1p` reflog), avoiding the issue.
* **2026-10-25 14:24:05 UTC:** A fresh pool slot (git worktree `google1`) was created by `git-repo-worktree` (`stat` birth time on `.repo/projects/tools/vendor/google.git/worktrees/google1`). It correctly started at `f0d61ab633` since that was the main client's HEAD.
* **2026-10-25 14:24:28 UTC:** `v4-package` worktree creation completed, adopting the freshly created `google1` pool slot. 
* **2026-10-25 14:28:30 UTC:** The `v4-package` agent committed `Add 1P package for android-cli` without first checking out `goog/studio-main` (observed in `v4-package` reflog).

## Root Cause
Agent error. The agent ignored the explicit instruction documented in `repo-worktree/SKILL.md` (section 3): "Because `add` snapshots the source client's current on-disk `HEAD`s, detach at `goog/studio-main` when you need a clean upstream baseline". `git-repo-worktree` functioned precisely as designed, inheriting the 6 commits from `~/projects/studio-main/tools/vendor/google` (which is on the `mother` branch).

## Blast Radius
Any new pool slots or worktrees created right now will inherit the 6 extra commits until the main checkout of `tools/vendor/google` is detached. I verified that the two current idle pool slots in `.repo/repo-worktrees/.pool/tools%2Fvendor%2Fgoogle/` (`1792938346578056453-1776552-6` and `1792938421396127992-1776552-13`) both contain these 6 extra commits (`git log --oneline goog/studio-main..HEAD` reports 6 commits). 

## Recommended Fix
**For agent briefs:** Strengthen the prompt to strictly require the baseline checkout. For example: 
> "Workspace: create a fresh isolated studio-main worktree with `git repo-worktree` (never ~/projects/studio-main; never Workspace share). **CRITICAL:** Immediately after creation, you MUST detach all relevant projects to `goog/studio-main` (e.g., `git checkout -q --detach goog/studio-main`) as `git repo-worktree` snapshots the user's potentially dirty local HEADs."
