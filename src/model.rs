use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FORMAT_VERSION: u32 = 1;

pub fn default_heavy_jobs() -> usize {
    3
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
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub est_bytes: Option<u64>,
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
    pub submodules: bool,
    #[serde(default)]
    pub no_reflink: bool,
    #[serde(default = "default_heavy_jobs")]
    pub heavy_jobs: usize,
    pub locked: Option<LockMeta>,
    pub moving: Option<MovingMeta>,
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
    }

    pub fn contains_project_dir(&self, root: &Path) -> bool {
        self.projects.iter().any(|project| {
            let path = root.join(&project.path);
            path.exists()
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
