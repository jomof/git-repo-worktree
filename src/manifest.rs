use crate::error::{AppError, AppResult};
use crate::model::{DirectiveMeta, ProjectMeta};
use crate::util::{normalize, path_is_safe_relative, truncated_stderr};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use roxmltree::Document;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub struct ParsedManifest {
    pub projects: Vec<ProjectMeta>,
    pub copyfiles: Vec<DirectiveMeta>,
    pub linkfiles: Vec<DirectiveMeta>,
    pub orphan_checkouts: Vec<String>,
}

pub fn parse_snapshot(
    xml: &str,
    source_root: &Path,
    project_list_text: &str,
    jobs: usize,
) -> AppResult<ParsedManifest> {
    let document = Document::parse(xml).map_err(|error| {
        AppError::operational("E_MANIFEST_PARSE", format!("invalid snapshot XML: {error}"))
    })?;
    let project_list: HashSet<String> = project_list_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let mut in_snapshot = HashSet::new();
    let mut projects = Vec::new();
    let mut copyfiles = Vec::new();
    let mut linkfiles = Vec::new();

    for node in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "project")
    {
        let name = node.attribute("name").ok_or_else(|| {
            AppError::operational("E_MANIFEST_PARSE", "project is missing required name")
        })?;
        let path = node.attribute("path").unwrap_or(name);
        let revision = node.attribute("revision").ok_or_else(|| {
            AppError::operational(
                "E_MANIFEST_PARSE",
                format!("project {path} is missing required revision"),
            )
        })?;
        in_snapshot.insert(path.to_string());
        let mut project = ProjectMeta {
            path: path.to_string(),
            name: name.to_string(),
            sha: revision.to_string(),
            git_common_dir: String::new(),
            state: "pending".into(),
            reason: None,
        };
        let path_safe = path_is_safe_relative(Path::new(path));
        if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            project.state = "skipped".into();
            project.reason = Some("unpinned-revision".into());
        } else if !path_safe {
            project.state = "skipped".into();
            project.reason = Some("unsafe-project-path".into());
        } else if !project_list.contains(path) {
            project.state = "skipped".into();
            project.reason = Some("not-synced".into());
        }

        if project.state == "pending" {
            for child in node.children().filter(|child| child.is_element()) {
                let kind = child.tag_name().name();
                if kind != "copyfile" && kind != "linkfile" {
                    continue;
                }
                let src = child.attribute("src");
                let dest = child.attribute("dest");
                let malformed = src.is_none() || dest.is_none();
                let directive = DirectiveMeta {
                    project: path.to_string(),
                    src: src.unwrap_or("").to_string(),
                    dest: dest.unwrap_or("").to_string(),
                    state: if malformed { "failed" } else { "pending" }.into(),
                    reason: malformed.then(|| "malformed".into()),
                };
                if kind == "copyfile" {
                    copyfiles.push(directive);
                } else {
                    linkfiles.push(directive);
                }
            }
        }
        projects.push(project);
    }

    projects.sort_by(|left, right| {
        depth(&left.path)
            .cmp(&depth(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });
    resolve_git_common_dirs(&mut projects, source_root, jobs)?;
    let orphan_checkouts = project_list
        .difference(&in_snapshot)
        .cloned()
        .collect::<Vec<_>>();
    Ok(ParsedManifest {
        projects,
        copyfiles,
        linkfiles,
        orphan_checkouts,
    })
}

pub fn parse_snapshot_file(
    source_root: &Path,
    snapshot: &Path,
    jobs: usize,
) -> AppResult<ParsedManifest> {
    let xml = fs::read_to_string(snapshot)
        .map_err(|error| AppError::io(format!("read {}", snapshot.display()), error))?;
    let project_list_path = source_root.join(".repo/project.list");
    let project_list = fs::read_to_string(&project_list_path).map_err(|error| {
        AppError::operational(
            "E_MANIFEST_PARSE",
            format!("read {}: {error}", project_list_path.display()),
        )
    })?;
    parse_snapshot(&xml, source_root, &project_list, jobs)
}

fn resolve_git_common_dirs(
    projects: &mut [ProjectMeta],
    source_root: &Path,
    jobs: usize,
) -> AppResult<()> {
    let candidates = projects
        .iter()
        .enumerate()
        .filter(|(_, project)| project.state == "pending")
        .map(|(index, project)| (index, project.path.clone()))
        .collect::<Vec<_>>();
    let pool = ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|error| AppError::io("create manifest worker pool", error))?;
    let resolutions = pool.install(|| {
        candidates
            .par_iter()
            .map(|(index, path)| (*index, resolve_git_common_dir(source_root, path)))
            .collect::<Vec<_>>()
    });
    for (index, resolution) in resolutions {
        match resolution {
            Some(gitdir) => {
                projects[index].git_common_dir = gitdir.to_string_lossy().into_owned();
            }
            None => {
                projects[index].state = "skipped".into();
                projects[index].reason = Some("no-gitdir".into());
            }
        }
    }
    Ok(())
}

fn resolve_git_common_dir(source_root: &Path, project_path: &str) -> Option<PathBuf> {
    let checkout = source_root.join(project_path);
    if checkout.exists() {
        let output = Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["rev-parse", "--git-common-dir"])
            .output()
            .ok()?;
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let path = PathBuf::from(raw);
            let absolute = if path.is_absolute() {
                path
            } else {
                checkout.join(path)
            };
            return Some(fs::canonicalize(&absolute).unwrap_or_else(|_| normalize(&absolute)));
        }
    }
    let fallback = source_root
        .join(".repo/projects")
        .join(format!("{project_path}.git"));
    fallback
        .is_dir()
        .then(|| fs::canonicalize(&fallback).unwrap_or_else(|_| normalize(&fallback)))
}

fn depth(path: &str) -> usize {
    Path::new(path).components().count()
}

pub fn project_depth(path: &str) -> usize {
    depth(path)
}

pub fn snapshot_command_failure(output: &std::process::Output) -> AppError {
    AppError::operational(
        "E_SNAPSHOT_FAILED",
        format!("repo manifest -r failed: {}", truncated_stderr(output)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha() -> &'static str {
        "0123456789abcdef0123456789abcdef01234567"
    }

    #[test]
    fn parses_projects_and_directives() {
        let temp = tempfile::tempdir().unwrap();
        let xml = format!(
            r#"<manifest><project name="p" path="a" revision="{}"><copyfile src="x" dest="y"/><linkfile src="z" dest="l"/></project></manifest>"#,
            sha()
        );
        fs::create_dir_all(temp.path().join(".repo/projects/a.git")).unwrap();
        let parsed = parse_snapshot(&xml, temp.path(), "a\n", 2).unwrap();
        assert_eq!(parsed.projects.len(), 1);
        assert_eq!(parsed.copyfiles[0].dest, "y");
        assert_eq!(parsed.linkfiles[0].dest, "l");
    }

    #[test]
    fn marks_non_hex_revision_skipped() {
        let xml = r#"<manifest><project name="p" revision="main"/></manifest>"#;
        let parsed = parse_snapshot(xml, Path::new("/missing"), "p\n", 2).unwrap();
        assert_eq!(
            parsed.projects[0].reason.as_deref(),
            Some("unpinned-revision")
        );
    }

    #[test]
    fn records_malformed_directive() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".repo/projects/p.git")).unwrap();
        let xml = format!(
            r#"<manifest><project name="p" revision="{}"><copyfile src="x"/></project></manifest>"#,
            sha()
        );
        let parsed = parse_snapshot(&xml, temp.path(), "p\n", 2).unwrap();
        assert_eq!(parsed.copyfiles[0].reason.as_deref(), Some("malformed"));
    }
}
