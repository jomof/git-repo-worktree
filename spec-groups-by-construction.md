# git-repo-worktree — Groups by Construction and Non-Default Groups (Spec Addendum)

**Status:** Addendum to `git-repo-worktree-spec.md` v1.0-draft. Normative.
Clarifies the interaction between the **P6 Design Decision (Groups by-construction)** and the literal **§6.3 Pin Phase candidate-selection algorithm** when the source client utilizes non-default groups (such as `notdefault,platform-linux` commonly used in Android/Studio checkouts).

---

## 1. Problem Statement and Context

The main specification establishes two distinct rules that conflict when a developer works in an enlistment with non-default groups enabled:

1. **Policy P6 (Groups: by-construction):**
   The tool must replicate the active on-disk state of the source repository client, as synced by the user. It states: *"Groups: by-construction (enumerate what is on disk)."*
2. **Algorithm §6.3.3 (Candidate Selection):**
   Candidates for worktree creation are determined by the intersection of the flat snapshot and the project list:
   ```
   Candidates = Snapshot ∩ project.list
   ```
   Any project on disk (in `project.list`) that is absent from the snapshot is classified as an `orphan-checkout`, warned about, and ignored.

### 1.1 The Root Cause (Google `repo` Behavior)
When `git-repo-worktree` runs `repo manifest -r -o snapshot.xml` (as mandated by §6.3.1), Google's `repo` utility exports only the projects belonging to the **default** groups. It completely excludes projects marked with the `notdefault` attribute (e.g., `groups="notdefault,platform-linux"`), **even if those projects are active, synced on disk, and listed in `.repo/project.list`**.

Because these synced projects are missing from the generated `snapshot.xml`, they are dropped during the intersection step of the Pin Phase. The resulting worktree is incomplete, lacking critical directories (such as platform NDKs or prebuilts), which causes downstream build tools (such as Bazel or CMake) to fail with directory-read errors (e.g., `readdir() failed, not a directory`).

---

## 2. Clarification of Existing Behavior

Under the v1 specification:
* The current Rust implementation is **fully compliant with the literal algorithm defined in §6.3**. It correctly generates the snapshot, detects that the non-default projects are absent from it, emits `orphan-checkout` warnings, and ignores them.
* However, this implementation **fails to achieve the high-level P6 policy (by-construction)**, as it does not successfully replicate the actual state of the source client's on-disk enlistment.

---

## 3. Resolution for Future Compliance

To resolve this conflict and fully honor both the **by-construction** guarantee and the **Pin Phase**, future implementations must evolve the candidate-discovery process to treat the on-disk state (`.repo/project.list`) as the primary source of truth for project membership, rather than relying solely on `repo manifest -r`'s project list.

### 3.1 Updated Candidate Resolution
The candidate-discovery algorithm in **§6.3.3** is amended as follows:

1. **Determine Active Projects:** Read `.repo/project.list` to identify all projects currently synced and active on disk.
2. **Supplement Snapshot Parsing:** 
   * If a project is found in `.repo/project.list` but is absent from the XML output of `repo manifest -r` (due to being a non-default group), the tool must **not** ignore it.
   * Instead, it must resolve its pinned revision directly from the source checkout using:
     ```bash
     git -C <source-root>/<path> rev-parse HEAD
     ```
   * The project is added to the candidates list with state `pending` and its target SHA set to the resolved HEAD revision.
3. **Copyfile and Linkfile Replay:**
   * For projects added via this fallback mechanism, the tool should parse the original manifest source files under `.repo/manifests/` (or the symbolic link `.repo/manifest.xml`) to discover any associated `<copyfile>` or `<linkfile>` directives.
   * Alternatively, if the `repo` launcher version supports it, the tool can query the full manifest (including non-default groups) or reconstruct it by parsing the underlying XML declarations directly.

### 3.2 Benefits
This change ensures that:
1. Every directory and file present in the source client (including prebuilt toolchains and SDKs) is faithfully replicated in the worktree.
2. The created worktree behaves as a true "mirror image" of the source client's committed state, allowing heavy-duty build systems (like Bazel) to compile and test successfully.
3. The "by-construction" promise of P6 is upheld without violating the idempotency or durability guarantees of the journal.

---
— end of addendum —
