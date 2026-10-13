use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const FORMAT_VERSION: u32 = 3;

pub const MODE_WORKTREE: &str = "worktree";
pub const MODE_SHARED: &str = "symlink-shared";
pub const MODE_OVERLAY: &str = "overlay";

/// Projects tracking more than this many bytes are "heavy" and throttled
/// during population.
pub const HEAVY_PROJECT_BYTES: u64 = 500 * 1024 * 1024;

/// Checkout cost grows with file count (about 70 microseconds per file on
/// ext4), so projects with this many tracked files are pooled by default.
pub const POOL_MIN_FILES: u64 = 30_000;
/// Projects this large are pooled by default even with few files.
pub const POOL_MIN_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub fn default_heavy_jobs() -> usize {
    3
}

fn default_project_mode() -> String {
    "worktree".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockMeta {
    pub reason: String,
    pub at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MovingMeta {
    pub from: String,
    pub to: String,
    pub at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectMeta {
    pub path: String,
    pub name: String,
    pub sha: String,
    pub git_common_dir: String,
    pub state: String,
    pub reason: Option<String>,
    #[serde(default = "default_project_mode")]
    pub mode: String,
    #[serde(default)]
    pub clone_depth: Option<u32>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub est_bytes: Option<u64>,
    #[serde(default)]
    pub est_files: Option<u64>,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    /// Wall-clock milliseconds the last successful population took.
    #[serde(default)]
    pub populate_ms: Option<u64>,
}

impl ProjectMeta {
    /// Whether the tree owns an isolated Git worktree registration for this project.
    pub fn has_worktree(&self) -> bool {
        self.mode == MODE_WORKTREE || self.mode == MODE_OVERLAY
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeMeta {
    /// Common Git directory of the source manifest repository when the tree has
    /// its own linked manifest checkout.
    pub manifests_git_dir: Option<String>,
    /// Branch the tree's manifest checkout is on; repo derives the manifest
    /// revision from its tracking configuration.
    #[serde(default)]
    pub manifests_branch: Option<String>,
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectiveMeta {
    pub project: String,
    pub src: String,
    pub dest: String,
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeMeta {
    pub format_version: u32,
    pub tool_version: String,
    pub name: String,
    pub path: String,
    pub source_root: String,
    pub source_layout: String,
    pub created_at: String,
    pub state: String,
    pub branch: Option<String>,
    /// Projects that receive `branch`; `None` with a branch means every project
    /// (trees created before branch scoping).
    #[serde(default)]
    pub branch_projects: Option<Vec<String>>,
    pub submodules: bool,
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub full_projects: Vec<String>,
    #[serde(default)]
    pub default_modes: BTreeMap<String, String>,
    #[serde(default)]
    pub no_reflink: bool,
    #[serde(default)]
    pub no_overlay: bool,
    #[serde(default = "default_heavy_jobs")]
    pub heavy_jobs: usize,
    pub locked: Option<LockMeta>,
    pub moving: Option<MovingMeta>,
    /// Present when the tree carries a synthesized `.repo` bridge.
    #[serde(default)]
    pub repo_bridge: Option<BridgeMeta>,
    pub projects: Vec<ProjectMeta>,
    pub copyfiles: Vec<DirectiveMeta>,
    pub linkfiles: Vec<DirectiveMeta>,
}

impl TreeMeta {
    pub fn root(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }

    pub fn source(&self) -> PathBuf {
        PathBuf::from(&self.source_root)
    }

    pub fn registry_entry(&self) -> PathBuf {
        self.source().join(".repo/repo-worktrees").join(&self.name)
    }

    pub fn failed_count(&self) -> usize {
        self.projects.iter().filter(|p| p.state == "failed").count()
            + self
                .copyfiles
                .iter()
                .chain(self.linkfiles.iter())
                .filter(|d| d.state == "failed")
                .count()
    }

    pub fn skipped_count(&self) -> usize {
        self.projects
            .iter()
            .filter(|p| p.state == "skipped")
            .count()
    }

    pub fn shared_count(&self) -> usize {
        self.projects
            .iter()
            .filter(|project| project.mode == MODE_SHARED)
            .count()
    }

    /// Whether a project that ends up as a worktree uses the spare-worktree
    /// pool: an explicit `pool` default, or, with no default, a project slow to
    /// check out. Fixed-depth projects qualify too, because they become
    /// worktrees when their source checkout is too dirty to share.
    pub fn uses_pool(&self, project: &ProjectMeta) -> bool {
        match self.default_modes.get(&project.path).map(String::as_str) {
            Some("pool") => true,
            Some(_) => false,
            None => match project.est_files {
                Some(files) => {
                    files >= POOL_MIN_FILES || project.est_bytes.unwrap_or(0) >= POOL_MIN_BYTES
                }
                None => project.est_bytes.unwrap_or(0) > HEAVY_PROJECT_BYTES,
            },
        }
    }

    pub fn branch_for(&self, project: &str) -> Option<&str> {
        let branch = self.branch.as_deref()?;
        match &self.branch_projects {
            None => Some(branch),
            Some(projects) => projects
                .iter()
                .any(|path| path == project)
                .then_some(branch),
        }
    }

    pub fn done_count(&self) -> usize {
        self.projects.iter().filter(|p| p.state == "done").count()
    }

    pub fn total_non_skipped(&self) -> usize {
        self.projects
            .iter()
            .filter(|p| p.state != "skipped")
            .count()
    }

    pub fn is_complete(&self) -> bool {
        self.projects
            .iter()
            .all(|p| p.state == "done" || p.state == "skipped")
            && self
                .copyfiles
                .iter()
                .chain(self.linkfiles.iter())
                .all(|d| d.state == "done")
            && self.repo_bridge.as_ref().map_or(true, |bridge| {
                bridge.state == "done" || bridge.state == "unsupported"
            })
    }

    pub fn contains_project_dir(&self, root: &Path) -> bool {
        self.projects.iter().any(|project| {
            let path = root.join(&project.path);
            path.exists() || fs::symlink_metadata(path).is_ok()
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Breadcrumb {
    pub format_version: u32,
    pub name: String,
    pub source_root: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct JournalRecord {
    pub ts: String,
    pub step: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
