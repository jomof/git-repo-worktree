use crate::model::{ProjectMeta, TreeMeta, MODE_OVERLAY};
use crate::overlay;
use crate::util::exit_reason;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Returns the Git directory backing a checkout (`<checkout>/.git` itself when
/// it is a directory or a symlink to one, otherwise the `gitdir:` target).
pub fn checkout_git_dir(checkout: &Path) -> Option<PathBuf> {
    let dot_git = checkout.join(".git");
    let metadata = fs::metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return fs::canonicalize(&dot_git).ok();
    }
    let contents = fs::read_to_string(&dot_git).ok()?;
    let target = contents.strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        checkout.join(target)
    };
    fs::canonicalize(absolute).ok()
}

/// Reads the branch checked out in a checkout, without spawning Git when the
/// HEAD file is a plain symbolic ref.
pub fn checkout_branch(checkout: &Path) -> Option<String> {
    let git_dir = checkout_git_dir(checkout)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        if reference != "refs/heads/.invalid" {
            return reference.strip_prefix("refs/heads/").map(str::to_owned);
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["symbolic-ref", "-q", "--short", "HEAD"])
            .output()
            .ok()?;
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return (output.status.success() && !branch.is_empty()).then_some(branch);
    }
    None
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ProjectInspection {
    pub path: String,
    pub mode: String,
    pub pinned: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub modified: usize,
    pub untracked: usize,
    pub unbranched_commits: usize,
    /// Populated submodules with uncommitted changes of their own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub submodule_changes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounted: Option<bool>,
    /// The tree's recorded state and reason, when the tree does not record
    /// the project as done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded: Option<String>,
    /// Milliseconds the inspection took.
    pub ms: u64,
    pub dirty: bool,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A problem that does not make the project dirty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl ProjectInspection {
    fn finish(mut self) -> Self {
        if self.modified > 0 {
            self.reasons.push("modified".into());
        }
        if self.untracked > 0 {
            self.reasons.push("untracked".into());
        }
        if self.unbranched_commits > 0 {
            self.reasons.push("unbranched-commits".into());
        }
        if self.error.is_some() {
            self.reasons.push("unverifiable".into());
        }
        self.dirty = !self.reasons.is_empty();
        self
    }
}

/// Inspects every populated worktree-backed project of a tree in parallel.
pub fn inspect_tree(meta: &TreeMeta, jobs: usize) -> Vec<ProjectInspection> {
    let root = meta.root();
    let entry = meta.registry_entry();
    let candidates = meta
        .projects
        .iter()
        .filter(|project| project.has_worktree() && project.state != "skipped")
        .filter(|project| root.join(&project.path).exists())
        .collect::<Vec<_>>();
    let inspect = || {
        candidates
            .par_iter()
            .map(|project| {
                let started = std::time::Instant::now();
                let children = nested_children(meta, project);
                let mut inspection = inspect_project(&root, &entry, project, &children);
                inspection.ms = crate::metrics::elapsed_ms(started);
                inspection
            })
            .collect::<Vec<_>>()
    };
    match ThreadPoolBuilder::new().num_threads(jobs.max(1)).build() {
        Ok(pool) => pool.install(inspect),
        Err(_) => inspect(),
    }
}

/// Paths, relative to `project`, of the tree's projects nested inside it. Git
/// reports each as untracked in the parent, as it does in any repo client.
pub fn nested_children(meta: &TreeMeta, project: &ProjectMeta) -> Vec<String> {
    let prefix = format!("{}/", project.path);
    meta.projects
        .iter()
        .filter(|other| other.state != "skipped")
        .filter_map(|other| other.path.strip_prefix(&prefix).map(str::to_owned))
        .collect()
}

pub fn inspect_project(
    root: &Path,
    entry: &Path,
    project: &ProjectMeta,
    children: &[String],
) -> ProjectInspection {
    let checkout = root.join(&project.path);
    let mut inspection = ProjectInspection {
        path: project.path.clone(),
        mode: project.mode.clone(),
        pinned: project.sha.clone(),
        recorded: (project.state != "done").then(|| match &project.reason {
            Some(reason) => format!("{}: {reason}", project.state),
            None => project.state.clone(),
        }),
        ..Default::default()
    };
    if project.mode == MODE_OVERLAY {
        let mounted = overlay::is_mounted(&checkout);
        let dead = mounted && overlay::is_dead(&checkout);
        inspection.mounted = Some(mounted && !dead);
        let upper = overlay::upper_dir(entry, &project.path);
        if dead {
            inspection.warning = Some(
                "overlay mount is dead (its daemon exited); run `git repo-worktree repair --mounts-only`"
                    .into(),
            );
        }
        if !mounted || dead {
            match overlay::upper_has_changes(&upper) {
                Ok(true) => {
                    inspection.error = Some(
                        if dead {
                            "overlay mount is dead and the overlay has changes; run `git repo-worktree repair --mounts-only`"
                        } else {
                            "overlay is not mounted; run `git repo-worktree repair --mounts-only`"
                        }
                        .into(),
                    )
                }
                Ok(false) => {}
                Err(error) => {
                    inspection.error.get_or_insert(error);
                }
            }
            inspection.head = overlay::admin_head(&upper, &checkout);
            inspection.unbranched_commits =
                count_unbranched(&project.git_common_dir, inspection.head.as_deref(), project)
                    .unwrap_or_else(|error| {
                        inspection.error.get_or_insert(error);
                        0
                    });
            return inspection.finish();
        }
        if matches!(overlay::upper_has_changes(&upper), Ok(false)) {
            inspection.head = overlay::admin_head(&upper, &checkout);
            inspection.branch = checkout_branch(&checkout);
            inspection.unbranched_commits =
                count_unbranched(&project.git_common_dir, inspection.head.as_deref(), project)
                    .unwrap_or_else(|error| {
                        inspection.error.get_or_insert(error);
                        0
                    });
            return inspection.finish();
        }
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=normal",
        ])
        .output();
    let output = match output {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            inspection.error = Some(format!("git status failed: {}", exit_reason(&output)));
            return inspection.finish();
        }
        Err(error) => {
            inspection.error = Some(format!("run git status: {error}"));
            return inspection.finish();
        }
    };
    parse_status_v2(&output.stdout, &mut inspection, children);
    match count_unbranched(&project.git_common_dir, inspection.head.as_deref(), project) {
        Ok(count) => inspection.unbranched_commits = count,
        Err(error) => inspection.error = Some(error),
    }
    inspection.finish()
}

fn parse_status_v2(stdout: &[u8], inspection: &mut ProjectInspection, children: &[String]) {
    let mut records = stdout.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        if let Some(oid) = text.strip_prefix("# branch.oid ") {
            if oid != "(initial)" {
                inspection.head = Some(oid.to_string());
            }
        } else if let Some(head) = text.strip_prefix("# branch.head ") {
            if head != "(detached)" {
                inspection.branch = Some(head.to_string());
            }
        } else if text.starts_with("1 ") || text.starts_with("u ") {
            inspection.modified += 1;
            note_submodule_changes(&text, 9, inspection);
        } else if text.starts_with("2 ") {
            inspection.modified += 1;
            note_submodule_changes(&text, 10, inspection);
            records.next();
        } else if let Some(path) = text.strip_prefix("? ") {
            let path = path.trim_end_matches('/');
            let nested = children.iter().any(|child| {
                child == path
                    || child
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/'))
            });
            if !nested {
                inspection.untracked += 1;
            }
        }
    }
}

/// Records a changed-entry line's path when it is a submodule with tracked or
/// untracked changes inside it (`S.M.`, `S..U`); a moved submodule commit is
/// visible in the parent and needs no note.
fn note_submodule_changes(text: &str, fields: usize, inspection: &mut ProjectInspection) {
    let parts = text.splitn(fields, ' ').collect::<Vec<_>>();
    let (Some(sub), Some(path)) = (parts.get(2), parts.get(fields - 1)) else {
        return;
    };
    let sub = sub.as_bytes();
    if sub.len() == 4 && sub[0] == b'S' && (sub[2] == b'M' || sub[3] == b'U') {
        inspection.submodule_changes.push((*path).to_string());
    }
}

/// Seconds since the epoch of the newest sign of work in a tree: its metadata,
/// each project's HEAD, index, and HEAD reflog, and files Git reports as
/// modified or untracked. Never refreshes an index, so inspecting a tree does
/// not make it look active.
pub fn last_activity(meta: &TreeMeta, jobs: usize) -> u64 {
    let root = meta.root();
    let modified = |path: &Path| {
        fs::symlink_metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_secs())
    };
    let projects = meta
        .projects
        .iter()
        .filter(|project| project.has_worktree() && project.state != "skipped")
        .filter(|project| root.join(&project.path).exists())
        .collect::<Vec<_>>();
    let newest_in = |project: &&ProjectMeta| -> u64 {
        let checkout = root.join(&project.path);
        let mut newest = 0;
        if let Some(git_dir) = checkout_git_dir(&checkout) {
            for name in ["HEAD", "index", "logs/HEAD"] {
                newest = newest.max(modified(&git_dir.join(name)));
            }
        }
        let Ok(output) = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
            .output()
        else {
            return newest;
        };
        let mut records = output.stdout.split(|byte| *byte == 0);
        let mut seen = 0;
        while let Some(record) = records.next() {
            if record.len() < 4 || seen == 2_000 {
                continue;
            }
            seen += 1;
            if record[0] == b'R' || record[0] == b'C' {
                records.next();
            }
            let path = String::from_utf8_lossy(&record[3..]);
            newest = newest.max(modified(&checkout.join(path.trim_end_matches('/'))));
        }
        newest
    };
    let from_projects = match ThreadPoolBuilder::new().num_threads(jobs.max(1)).build() {
        Ok(pool) => pool.install(|| projects.par_iter().map(newest_in).max()),
        Err(_) => projects.iter().map(newest_in).max(),
    };
    modified(&meta.registry_entry().join("meta.json")).max(from_projects.unwrap_or(0))
}

/// Counts commits reachable from HEAD but not from the pinned revision or any
/// branch, remote-tracking ref, or tag. Removing a tree discards its per-worktree
/// reflog, so these commits would otherwise be silently stranded.
fn count_unbranched(
    git_common_dir: &str,
    head: Option<&str>,
    project: &ProjectMeta,
) -> Result<usize, String> {
    let Some(head) = head else {
        return Ok(0);
    };
    if head == project.sha {
        return Ok(0);
    }
    let output = Command::new("git")
        .arg(format!("--git-dir={git_common_dir}"))
        .args(["rev-list", "--count", head, "--not", &project.sha])
        .args(["--branches", "--remotes", "--tags"])
        .output()
        .map_err(|error| format!("run git rev-list: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not verify commits on HEAD: {}",
            exit_reason(&output)
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "could not parse git rev-list output".to_string())
}

/// `pid` and its ancestors. A shell that ran this command often mentions the
/// tree path in its own arguments, but it is not left behind by the tree.
fn ancestors(pid: u32) -> Vec<u32> {
    let mut chain = vec![pid];
    let mut current = pid;
    while current > 1 && chain.len() < 64 {
        let Ok(stat) = fs::read_to_string(format!("/proc/{current}/stat")) else {
            break;
        };
        let Some(parent) = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|field| field.parse::<u32>().ok())
        else {
            break;
        };
        if parent == 0 || chain.contains(&parent) {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain
}

/// Processes whose working directory is inside `root` or whose arguments name
/// it, such as build servers started from a tree. Linux only; elsewhere empty.
pub fn processes_referencing(root: &Path) -> Vec<serde_json::Value> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let excluded = ancestors(std::process::id());
    let root_text = root.to_string_lossy().into_owned();
    let nested = format!("{root_text}/");
    let mentions = |argument: &str| argument.ends_with(&root_text) || argument.contains(&nested);
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if excluded.contains(&pid) {
            continue;
        }
        let cwd_inside = fs::read_link(entry.path().join("cwd"))
            .map(|cwd| cwd.starts_with(root))
            .unwrap_or(false);
        let arguments = fs::read(entry.path().join("cmdline"))
            .unwrap_or_default()
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8_lossy(argument).into_owned())
            .collect::<Vec<_>>();
        let named = arguments.iter().any(|argument| mentions(argument));
        if !cwd_inside && !named {
            continue;
        }
        let overlay_daemon = arguments
            .first()
            .and_then(|program| Path::new(program).file_name())
            .is_some_and(|program| program == "fuse-overlayfs");
        if overlay_daemon {
            continue;
        }
        let mut command = arguments.join(" ");
        if command.len() > 120 {
            let mut end = 120;
            while !command.is_char_boundary(end) {
                end -= 1;
            }
            command.truncate(end);
        }
        found.push(serde_json::json!({
            "pid": pid,
            "command": command,
            "reason": if cwd_inside { "cwd" } else { "arguments" },
        }));
        if found.len() == 20 {
            break;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2_records_including_renames() {
        let mut inspection = ProjectInspection::default();
        let stdout =
            b"# branch.oid 0123456789abcdef0123456789abcdef01234567\0# branch.head topic\0\
1 .M N... 100644 100644 100644 a b file\0\
2 R. N... 100644 100644 100644 a b R100 new\0old\0\
? untracked\0? other\0";
        parse_status_v2(stdout, &mut inspection, &[]);
        assert_eq!(inspection.branch.as_deref(), Some("topic"));
        assert_eq!(inspection.modified, 2);
        assert_eq!(inspection.untracked, 2);
        let mut detached = ProjectInspection::default();
        parse_status_v2(
            b"# branch.oid abc\0# branch.head (detached)\0",
            &mut detached,
            &[],
        );
        assert_eq!(detached.branch, None);
        assert_eq!(detached.head.as_deref(), Some("abc"));
    }

    #[test]
    fn submodules_with_changes_of_their_own_are_noted() {
        let mut inspection = ProjectInspection::default();
        parse_status_v2(
            b"1 .M S.M. 160000 160000 160000 a a sub/edited\0\
1 .M S..U 160000 160000 160000 a a sub/untracked\0\
1 M. SC.. 160000 160000 160000 a b sub/moved\0\
1 .M N... 100644 100644 100644 a a plain file\0",
            &mut inspection,
            &[],
        );
        assert_eq!(inspection.modified, 4);
        assert_eq!(
            inspection.submodule_changes,
            ["sub/edited", "sub/untracked"]
        );
    }

    #[test]
    fn nested_projects_are_not_untracked_files_of_their_parent() {
        let mut inspection = ProjectInspection::default();
        parse_status_v2(
            b"? nested/\0? tools/\0? scratch.txt\0? toolbox/\0",
            &mut inspection,
            &["nested".into(), "tools/deep".into()],
        );
        assert_eq!(inspection.untracked, 2);
    }
}
