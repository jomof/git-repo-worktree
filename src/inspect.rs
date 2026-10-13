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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounted: Option<bool>,
    /// Milliseconds the inspection took.
    pub ms: u64,
    pub dirty: bool,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
        .filter(|project| project.has_worktree() && project.state == "done")
        .filter(|project| root.join(&project.path).exists())
        .collect::<Vec<_>>();
    let inspect = || {
        candidates
            .par_iter()
            .map(|project| {
                let started = std::time::Instant::now();
                let mut inspection = inspect_project(&root, &entry, project);
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

pub fn inspect_project(root: &Path, entry: &Path, project: &ProjectMeta) -> ProjectInspection {
    let checkout = root.join(&project.path);
    let mut inspection = ProjectInspection {
        path: project.path.clone(),
        mode: project.mode.clone(),
        pinned: project.sha.clone(),
        ..Default::default()
    };
    if project.mode == MODE_OVERLAY {
        let mounted = overlay::is_mounted(&checkout);
        inspection.mounted = Some(mounted);
        let upper = overlay::upper_dir(entry, &project.path);
        if !mounted {
            match overlay::upper_has_changes(&upper) {
                Ok(true) => inspection.error = Some("overlay is not mounted; run repair".into()),
                Ok(false) => {}
                Err(error) => inspection.error = Some(error),
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
    parse_status_v2(&output.stdout, &mut inspection);
    match count_unbranched(&project.git_common_dir, inspection.head.as_deref(), project) {
        Ok(count) => inspection.unbranched_commits = count,
        Err(error) => inspection.error = Some(error),
    }
    inspection.finish()
}

fn parse_status_v2(stdout: &[u8], inspection: &mut ProjectInspection) {
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
        } else if text.starts_with("2 ") {
            inspection.modified += 1;
            records.next();
        } else if text.starts_with("? ") {
            inspection.untracked += 1;
        }
    }
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

/// Processes whose working directory is inside `root` or whose arguments name
/// it, such as build servers started from a tree. Linux only; elsewhere empty.
pub fn processes_referencing(root: &Path) -> Vec<serde_json::Value> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let me = std::process::id();
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
        if pid == me {
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
        parse_status_v2(stdout, &mut inspection);
        assert_eq!(inspection.branch.as_deref(), Some("topic"));
        assert_eq!(inspection.modified, 2);
        assert_eq!(inspection.untracked, 2);
        let mut detached = ProjectInspection::default();
        parse_status_v2(
            b"# branch.oid abc\0# branch.head (detached)\0",
            &mut detached,
        );
        assert_eq!(detached.branch, None);
        assert_eq!(detached.head.as_deref(), Some("abc"));
    }
}
