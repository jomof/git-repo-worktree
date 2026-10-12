use crate::error::{AppError, AppResult};
use crate::model::{Breadcrumb, FORMAT_VERSION};
use crate::util::truncated_stderr;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Client {
    pub root: PathBuf,
    pub layout: String,
}

pub fn find_direct_client_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|directory| is_client_root(directory))
        .map(Path::to_path_buf)
}

fn is_client_root(directory: &Path) -> bool {
    let repo = directory.join(".repo");
    repo.is_dir() && (repo.join("project.list").is_file() || repo.join("manifests").is_dir())
}

/// Locates the source client. A tree root (marked by its `.repo-worktree`
/// breadcrumb) is checked before `.repo` at the same level, because trees carry
/// a synthesized `.repo` that must never be mistaken for a source client.
pub fn discover_client(start: &Path, allow_breadcrumb: bool) -> AppResult<Client> {
    for directory in start.ancestors() {
        let path = directory.join(".repo-worktree");
        if path.is_file() {
            if !allow_breadcrumb {
                return Err(AppError::operational(
                    "E_NO_REPO_CLIENT",
                    format!(
                        "{} is a git-repo-worktree tree; run this command from the source client (or use -C)",
                        directory.display()
                    ),
                ));
            }
            let bytes = fs::read(&path)
                .map_err(|error| AppError::io(format!("read {}", path.display()), error))?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|error| AppError::io(format!("parse {}", path.display()), error))?;
            if value
                .get("format_version")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                > FORMAT_VERSION as u64
            {
                return Err(AppError::operational(
                    "E_FORMAT_TOO_NEW",
                    "the tree breadcrumb uses a newer format; upgrade git-repo-worktree",
                ));
            }
            let breadcrumb: Breadcrumb = serde_json::from_value(value)
                .map_err(|error| AppError::io(format!("parse {}", path.display()), error))?;
            let root = PathBuf::from(breadcrumb.source_root);
            if !root.join(".repo/repo-worktrees").is_dir() {
                return Err(AppError::operational(
                    "E_SOURCE_MISSING",
                    format!(
                        "the source client recorded by {} is missing or has no registry: {}",
                        path.display(),
                        root.display()
                    ),
                ));
            }
            crate::metrics::set_source(&root);
            return Ok(Client {
                layout: detect_layout(&root),
                root,
            });
        }
        if is_client_root(directory) {
            crate::metrics::set_source(directory);
            return Ok(Client {
                layout: detect_layout(directory),
                root: directory.to_path_buf(),
            });
        }
    }
    Err(AppError::operational(
        "E_NO_REPO_CLIENT",
        "this command must be run from within a repo client (or use -C)",
    ))
}

fn detect_layout(root: &Path) -> String {
    let worktrees = root.join(".repo/worktrees");
    let non_empty = fs::read_dir(worktrees)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if non_empty {
        "worktree".into()
    } else {
        "legacy".into()
    }
}

pub fn program_version(program: &str, args: &[&str]) -> AppResult<String> {
    let output = Command::new(program).args(args).output().map_err(|error| {
        let code = if program == "repo" {
            "E_REPO_MISSING"
        } else {
            "E_IO"
        };
        AppError::operational(code, format!("{program} is not available on PATH: {error}"))
    })?;
    if !output.status.success() {
        return Err(AppError::operational(
            if program == "repo" {
                "E_REPO_MISSING"
            } else {
                "E_IO"
            },
            format!(
                "{program} version check failed: {}",
                truncated_stderr(&output)
            ),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let value = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    Ok(value.lines().next().unwrap_or(value).to_string())
}

fn parse_semverish(value: &str) -> Option<(u64, u64)> {
    for token in value.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if token.is_empty() {
            continue;
        }
        let mut parts = token.split('.');
        if let (Some(major), Some(minor)) = (
            parts.next().and_then(|part| part.parse().ok()),
            parts.next().and_then(|part| part.parse().ok()),
        ) {
            return Some((major, minor));
        }
    }
    None
}

pub fn check_git_version() -> AppResult<String> {
    let version = program_version("git", &["version"])?;
    let parsed = parse_semverish(&version).ok_or_else(|| {
        AppError::operational(
            "E_GIT_TOO_OLD",
            format!("could not parse git version: {version}"),
        )
    })?;
    if parsed < (2, 31) {
        return Err(AppError::operational(
            "E_GIT_TOO_OLD",
            format!("git 2.31 or newer is required (found {version})"),
        ));
    }
    Ok(version)
}

pub fn check_repo_version() -> AppResult<String> {
    let executable = find_on_path("repo")
        .ok_or_else(|| AppError::operational("E_REPO_MISSING", "repo is not available on PATH"))?;
    let contents = fs::read_to_string(&executable).unwrap_or_default();
    let parsed = contents.lines().find_map(|line| {
        let line = line.trim();
        let value = line
            .strip_prefix("VERSION = (")
            .or_else(|| line.strip_prefix("VERSION=("))?;
        let value = value.split(')').next()?;
        let mut values = value.split(',').map(str::trim);
        Some((values.next()?.parse().ok()?, values.next()?.parse().ok()?))
    });
    if let Some(parsed) = parsed {
        if parsed < (2, 30) {
            return Err(AppError::operational(
                "E_REPO_TOO_OLD",
                format!(
                    "repo 2.30 or newer is required (found {}.{})",
                    parsed.0, parsed.1
                ),
            ));
        }
    }
    Ok(parsed
        .map(|(major, minor)| format!("{major}.{minor}"))
        .unwrap_or_else(|| "unknown".into()))
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path: OsString = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_text() {
        assert_eq!(parse_semverish("git version 2.31.4"), Some((2, 31)));
        assert_eq!(parse_semverish("repo version v2.54"), Some((2, 54)));
    }
}
