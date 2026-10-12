use crate::error::{AppError, AppResult};
use crate::model::{DirectiveMeta, ProjectMeta};
use crate::util::{normalize, path_is_safe_relative, truncated_stderr, write_atomic};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use roxmltree::Document;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub struct ParsedManifest {
    pub projects: Vec<ProjectMeta>,
    pub copyfiles: Vec<DirectiveMeta>,
    pub linkfiles: Vec<DirectiveMeta>,
    pub supplemented_checkouts: Vec<String>,
    pub malformed_clone_depth: Vec<String>,
}

#[derive(Clone, Debug)]
struct DirectiveDeclaration {
    src: Option<String>,
    dest: Option<String>,
}

#[derive(Clone, Debug)]
struct ProjectDeclaration {
    name: String,
    path: String,
    clone_depth: Option<u32>,
    malformed_clone_depth: bool,
    remote: Option<String>,
    upstream: Option<String>,
    copyfiles: Vec<DirectiveDeclaration>,
    linkfiles: Vec<DirectiveDeclaration>,
}

#[derive(Clone, Debug, Default)]
struct ManifestDefaults {
    remote: Option<String>,
    revision: Option<String>,
    dest_branch: Option<String>,
    remote_revisions: HashMap<String, String>,
}

impl ManifestDefaults {
    fn absorb(&mut self, node: roxmltree::Node<'_, '_>) {
        match node.tag_name().name() {
            "default" => {
                if let Some(remote) = node.attribute("remote") {
                    self.remote = Some(remote.to_string());
                }
                if let Some(revision) = node.attribute("revision") {
                    self.revision = Some(revision.to_string());
                }
                if let Some(dest_branch) = node.attribute("dest-branch") {
                    self.dest_branch = Some(dest_branch.to_string());
                }
            }
            "remote" => {
                if let (Some(name), Some(revision)) =
                    (node.attribute("name"), node.attribute("revision"))
                {
                    self.remote_revisions
                        .insert(name.to_string(), revision.to_string());
                }
            }
            _ => {}
        }
    }

    fn remote_for(&self, node: roxmltree::Node<'_, '_>) -> Option<String> {
        node.attribute("remote")
            .map(str::to_owned)
            .or_else(|| self.remote.clone())
    }

    /// The branch a new local branch should track, chosen like `repo start`
    /// does for a pinned project: the project's dest-branch, its recorded
    /// upstream, its symbolic revision, then manifest-wide defaults.
    fn upstream_for(&self, node: roxmltree::Node<'_, '_>, remote: Option<&str>) -> Option<String> {
        let symbolic = |value: Option<&str>| {
            value
                .filter(|value| !is_object_id(value))
                .map(str::to_owned)
        };
        symbolic(node.attribute("dest-branch"))
            .or_else(|| symbolic(node.attribute("upstream")))
            .or_else(|| symbolic(node.attribute("revision")))
            .or_else(|| symbolic(self.dest_branch.as_deref()))
            .or_else(|| {
                symbolic(
                    remote
                        .and_then(|remote| self.remote_revisions.get(remote))
                        .map(String::as_str),
                )
            })
            .or_else(|| symbolic(self.revision.as_deref()))
            .map(|value| {
                if value.starts_with("refs/") {
                    value
                } else {
                    format!("refs/heads/{value}")
                }
            })
    }
}

fn is_object_id(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn directive_children(
    node: roxmltree::Node<'_, '_>,
) -> (Vec<DirectiveDeclaration>, Vec<DirectiveDeclaration>) {
    let mut copyfiles = Vec::new();
    let mut linkfiles = Vec::new();
    for child in node.children().filter(|child| child.is_element()) {
        let directive = DirectiveDeclaration {
            src: child.attribute("src").map(str::to_owned),
            dest: child.attribute("dest").map(str::to_owned),
        };
        match child.tag_name().name() {
            "copyfile" => copyfiles.push(directive),
            "linkfile" => linkfiles.push(directive),
            _ => {}
        }
    }
    (copyfiles, linkfiles)
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
    let mut malformed_clone_depth = Vec::new();
    let mut defaults = ManifestDefaults::default();
    for node in document
        .root_element()
        .children()
        .filter(|node| node.is_element())
    {
        defaults.absorb(node);
    }

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
        let (clone_depth, malformed) = parse_clone_depth(node.attribute("clone-depth"));
        if malformed {
            malformed_clone_depth.push(path.to_string());
        }
        let remote = defaults.remote_for(node);
        let upstream = defaults.upstream_for(node, remote.as_deref());
        let mut project = ProjectMeta {
            path: path.to_string(),
            name: name.to_string(),
            sha: revision.to_string(),
            git_common_dir: String::new(),
            state: "pending".into(),
            reason: None,
            mode: "worktree".into(),
            clone_depth,
            strategy: None,
            est_bytes: None,
            remote,
            upstream,
            populate_ms: None,
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

    for node in document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "extend-project")
    {
        let Some(name) = node.attribute("name") else {
            continue;
        };
        let only_path = node.attribute("path");
        let (extra_copies, extra_links) = directive_children(node);
        for project in projects.iter_mut().filter(|project| {
            project.name == name && only_path.map_or(true, |path| project.path == path)
        }) {
            if let Some(upstream) = node
                .attribute("dest-branch")
                .or_else(|| node.attribute("upstream"))
                .filter(|value| !is_object_id(value))
            {
                project.upstream = Some(if upstream.starts_with("refs/") {
                    upstream.to_string()
                } else {
                    format!("refs/heads/{upstream}")
                });
            }
            if let Some(remote) = node.attribute("remote") {
                project.remote = Some(remote.to_string());
            }
            if project.state != "pending" {
                continue;
            }
            append_declared_directives(&mut copyfiles, &project.path, &extra_copies);
            append_declared_directives(&mut linkfiles, &project.path, &extra_links);
        }
    }

    let mut supplemented_checkouts = project_list
        .difference(&in_snapshot)
        .cloned()
        .collect::<Vec<_>>();
    supplemented_checkouts.sort();
    if !supplemented_checkouts.is_empty() {
        let declarations = load_active_manifest_declarations(source_root)?;
        for path in &supplemented_checkouts {
            let declaration = declarations.get(path);
            let name = declaration
                .map(|declaration| declaration.name.clone())
                .unwrap_or_else(|| path.clone());
            let clone_depth = declaration.and_then(|declaration| declaration.clone_depth);
            if declaration.is_some_and(|declaration| declaration.malformed_clone_depth) {
                malformed_clone_depth.push(path.clone());
            }
            let path_safe = path_is_safe_relative(Path::new(path));
            projects.push(ProjectMeta {
                path: path.clone(),
                name,
                sha: String::new(),
                git_common_dir: String::new(),
                state: if path_safe { "pending" } else { "skipped" }.into(),
                reason: (!path_safe).then(|| "unsafe-project-path".into()),
                mode: "worktree".into(),
                clone_depth,
                strategy: None,
                est_bytes: None,
                remote: declaration.and_then(|declaration| declaration.remote.clone()),
                upstream: declaration.and_then(|declaration| declaration.upstream.clone()),
                populate_ms: None,
            });
            if path_safe {
                let Some(declaration) = declaration else {
                    continue;
                };
                append_declared_directives(&mut copyfiles, path, &declaration.copyfiles);
                append_declared_directives(&mut linkfiles, path, &declaration.linkfiles);
            }
        }
    }

    projects.sort_by(|left, right| {
        depth(&left.path)
            .cmp(&depth(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });
    let supplemental_paths = supplemented_checkouts
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    resolve_git_common_dirs(&mut projects, source_root, &supplemental_paths, jobs)?;
    Ok(ParsedManifest {
        projects,
        copyfiles,
        linkfiles,
        supplemented_checkouts,
        malformed_clone_depth,
    })
}

fn parse_clone_depth(value: Option<&str>) -> (Option<u32>, bool) {
    let Some(value) = value else {
        return (None, false);
    };
    match value.parse::<u32>() {
        Ok(value) if value > 0 => (Some(value), false),
        _ => (None, true),
    }
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
    let parsed = parse_snapshot(&xml, source_root, &project_list, jobs)?;
    if !parsed.supplemented_checkouts.is_empty() {
        let augmented = augment_snapshot(&xml, &parsed)?;
        write_atomic(snapshot, augmented.as_bytes(), 0o644)?;
    }
    Ok(parsed)
}

fn append_declared_directives(
    output: &mut Vec<DirectiveMeta>,
    project: &str,
    declarations: &[DirectiveDeclaration],
) {
    output.extend(declarations.iter().map(|declaration| {
        let malformed = declaration.src.is_none() || declaration.dest.is_none();
        DirectiveMeta {
            project: project.to_string(),
            src: declaration.src.clone().unwrap_or_default(),
            dest: declaration.dest.clone().unwrap_or_default(),
            state: if malformed { "failed" } else { "pending" }.into(),
            reason: malformed.then(|| "malformed".into()),
        }
    }));
}

fn load_active_manifest_declarations(
    source_root: &Path,
) -> AppResult<HashMap<String, ProjectDeclaration>> {
    let repo_dir = source_root.join(".repo");
    let manifests_root = repo_dir.join("manifests");
    let mut declarations = HashMap::new();
    let mut visited = HashSet::new();
    let mut defaults = ManifestDefaults::default();
    let manifest = repo_dir.join("manifest.xml");
    let default_manifest = manifests_root.join("default.xml");
    if manifest.exists() {
        parse_manifest_source(
            &manifest,
            &manifests_root,
            &mut declarations,
            &mut defaults,
            &mut visited,
        )?;
    } else if default_manifest.exists() {
        parse_manifest_source(
            &default_manifest,
            &manifests_root,
            &mut declarations,
            &mut defaults,
            &mut visited,
        )?;
    }

    let local_root = repo_dir.join("local_manifests");
    let mut local_manifests = match fs::read_dir(&local_root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "xml"))
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(AppError::io(
                format!("read {}", local_root.display()),
                error,
            ));
        }
    };
    local_manifests.sort();
    for local_manifest in local_manifests {
        parse_manifest_source(
            &local_manifest,
            &manifests_root,
            &mut declarations,
            &mut defaults,
            &mut visited,
        )?;
    }
    Ok(declarations)
}

fn parse_manifest_source(
    path: &Path,
    manifests_root: &Path,
    declarations: &mut HashMap<String, ProjectDeclaration>,
    defaults: &mut ManifestDefaults,
    visited: &mut HashSet<PathBuf>,
) -> AppResult<()> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| normalize(path));
    if !visited.insert(canonical.clone()) {
        return Ok(());
    }
    let xml = fs::read_to_string(&canonical)
        .map_err(|error| AppError::io(format!("read {}", canonical.display()), error))?;
    let document = Document::parse(&xml).map_err(|error| {
        AppError::operational(
            "E_MANIFEST_PARSE",
            format!("invalid source manifest {}: {error}", canonical.display()),
        )
    })?;
    for node in document
        .root_element()
        .children()
        .filter(|node| node.is_element())
    {
        match node.tag_name().name() {
            "include" => {
                let name = node.attribute("name").ok_or_else(|| {
                    AppError::operational(
                        "E_MANIFEST_PARSE",
                        format!("include in {} has no name", canonical.display()),
                    )
                })?;
                let relative = Path::new(name);
                if !path_is_safe_relative(relative) {
                    return Err(AppError::operational(
                        "E_MANIFEST_PARSE",
                        format!("unsafe manifest include {name}"),
                    ));
                }
                let sibling = canonical.parent().unwrap_or(manifests_root).join(relative);
                let include = if sibling.exists() {
                    sibling
                } else {
                    manifests_root.join(relative)
                };
                parse_manifest_source(&include, manifests_root, declarations, defaults, visited)?;
            }
            "default" | "remote" => defaults.absorb(node),
            "project" => {
                let name = node.attribute("name").ok_or_else(|| {
                    AppError::operational(
                        "E_MANIFEST_PARSE",
                        format!("project in {} has no name", canonical.display()),
                    )
                })?;
                let path = node.attribute("path").unwrap_or(name);
                let (copyfiles, linkfiles) = directive_children(node);
                let remote = defaults.remote_for(node);
                let upstream = defaults.upstream_for(node, remote.as_deref());
                let declaration = ProjectDeclaration {
                    name: name.to_string(),
                    path: path.to_string(),
                    clone_depth: parse_clone_depth(node.attribute("clone-depth")).0,
                    malformed_clone_depth: parse_clone_depth(node.attribute("clone-depth")).1,
                    remote,
                    upstream,
                    copyfiles,
                    linkfiles,
                };
                declarations.insert(declaration.path.clone(), declaration);
            }
            "extend-project" => {
                let Some(name) = node.attribute("name") else {
                    continue;
                };
                let only_path = node.attribute("path");
                let (copyfiles, linkfiles) = directive_children(node);
                let matching = declarations
                    .iter()
                    .filter(|(path, declaration)| {
                        declaration.name == name
                            && only_path.map_or(true, |only| only == path.as_str())
                    })
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>();
                for path in matching {
                    let Some(mut declaration) = declarations.remove(&path) else {
                        continue;
                    };
                    declaration.copyfiles.extend(copyfiles.iter().cloned());
                    declaration.linkfiles.extend(linkfiles.iter().cloned());
                    if let Some(remote) = node.attribute("remote") {
                        declaration.remote = Some(remote.to_string());
                    }
                    if let Some(dest_path) = node.attribute("dest-path") {
                        declaration.path = dest_path.to_string();
                    }
                    declarations.insert(declaration.path.clone(), declaration);
                }
            }
            "remove-project" => {
                let path = node.attribute("path");
                let name = node.attribute("name");
                declarations.retain(|project_path, declaration| {
                    path != Some(project_path.as_str()) && name != Some(declaration.name.as_str())
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn augment_snapshot(xml: &str, parsed: &ParsedManifest) -> AppResult<String> {
    let insert_at = xml.rfind("</manifest>").ok_or_else(|| {
        AppError::operational(
            "E_MANIFEST_PARSE",
            "snapshot manifest has no closing </manifest>",
        )
    })?;
    let supplemented = parsed
        .supplemented_checkouts
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut additions = String::new();
    for project in parsed
        .projects
        .iter()
        .filter(|project| supplemented.contains(&project.path) && project.sha.len() == 40)
    {
        let copyfiles = parsed
            .copyfiles
            .iter()
            .filter(|directive| directive.project == project.path)
            .collect::<Vec<_>>();
        let linkfiles = parsed
            .linkfiles
            .iter()
            .filter(|directive| directive.project == project.path)
            .collect::<Vec<_>>();
        additions.push_str("\n  <project name=\"");
        additions.push_str(&escape_xml(&project.name));
        additions.push_str("\" path=\"");
        additions.push_str(&escape_xml(&project.path));
        additions.push_str("\" revision=\"");
        additions.push_str(&project.sha);
        if let Some(clone_depth) = project.clone_depth {
            additions.push_str("\" clone-depth=\"");
            additions.push_str(&clone_depth.to_string());
        }
        if let Some(remote) = &project.remote {
            additions.push_str("\" remote=\"");
            additions.push_str(&escape_xml(remote));
        }
        if let Some(upstream) = &project.upstream {
            additions.push_str("\" upstream=\"");
            additions.push_str(&escape_xml(upstream));
        }
        if copyfiles.is_empty() && linkfiles.is_empty() {
            additions.push_str("\" />");
            continue;
        }
        additions.push_str("\">");
        for directive in copyfiles {
            additions.push_str("\n    <copyfile src=\"");
            additions.push_str(&escape_xml(&directive.src));
            additions.push_str("\" dest=\"");
            additions.push_str(&escape_xml(&directive.dest));
            additions.push_str("\" />");
        }
        for directive in linkfiles {
            additions.push_str("\n    <linkfile src=\"");
            additions.push_str(&escape_xml(&directive.src));
            additions.push_str("\" dest=\"");
            additions.push_str(&escape_xml(&directive.dest));
            additions.push_str("\" />");
        }
        additions.push_str("\n  </project>");
    }
    additions.push('\n');
    let mut augmented = String::with_capacity(xml.len() + additions.len());
    augmented.push_str(&xml[..insert_at]);
    augmented.push_str(&additions);
    augmented.push_str(&xml[insert_at..]);
    Ok(augmented)
}

fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn resolve_git_common_dirs(
    projects: &mut [ProjectMeta],
    source_root: &Path,
    supplemental_paths: &HashSet<String>,
    jobs: usize,
) -> AppResult<()> {
    let candidates = projects
        .iter()
        .enumerate()
        .filter(|(_, project)| project.state == "pending")
        .map(|(index, project)| {
            (
                index,
                project.path.clone(),
                supplemental_paths.contains(&project.path),
            )
        })
        .collect::<Vec<_>>();
    let pool = ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|error| AppError::io("create manifest worker pool", error))?;
    let resolutions = pool.install(|| {
        candidates
            .par_iter()
            .map(|(index, path, needs_head)| {
                (
                    *index,
                    *needs_head,
                    resolve_git_state(source_root, path, *needs_head),
                )
            })
            .collect::<Vec<_>>()
    });
    for (index, needs_head, resolution) in resolutions {
        match resolution {
            Ok((head, gitdir)) => {
                if let Some(head) = head {
                    projects[index].sha = head;
                }
                projects[index].git_common_dir = gitdir.to_string_lossy().into_owned();
            }
            Err(reason) if needs_head => {
                return Err(AppError::operational(
                    "E_HEAD_RESOLVE",
                    format!(
                        "resolve pinned HEAD for active project {}: {reason}",
                        projects[index].path
                    ),
                ));
            }
            Err(_) => {
                projects[index].state = "skipped".into();
                projects[index].reason = Some("no-gitdir".into());
            }
        }
    }
    let estimates = pool.install(|| {
        projects
            .par_iter()
            .enumerate()
            .filter(|(_, project)| project.state == "pending")
            .map(|(index, project)| {
                (
                    index,
                    estimate_tracked_bytes(Path::new(&project.git_common_dir), &project.sha),
                )
            })
            .collect::<Vec<_>>()
    });
    for (index, estimate) in estimates {
        projects[index].est_bytes = estimate;
    }
    Ok(())
}

fn estimate_tracked_bytes(git_common_dir: &Path, sha: &str) -> Option<u64> {
    let output = Command::new("git")
        .arg(format!("--git-dir={}", git_common_dir.display()))
        .args(["ls-tree", "-rlz", sha])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_tracked_bytes(&output.stdout))
}

fn parse_tracked_bytes(output: &[u8]) -> u64 {
    output
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let header = record.split(|byte| *byte == b'\t').next()?;
            let mut fields = header
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|field| !field.is_empty());
            let _mode = fields.next()?;
            (fields.next()? == b"blob").then_some(())?;
            let _object = fields.next()?;
            let size = fields.next()?;
            std::str::from_utf8(size).ok()?.parse::<u64>().ok()
        })
        .fold(0u64, u64::saturating_add)
}

fn resolve_git_state(
    source_root: &Path,
    project_path: &str,
    needs_head: bool,
) -> Result<(Option<String>, PathBuf), String> {
    let checkout = source_root.join(project_path);
    if checkout.exists() {
        let mut command = Command::new("git");
        command.arg("-C").arg(&checkout).arg("rev-parse");
        if needs_head {
            command.arg("HEAD");
        }
        let output = command
            .arg("--git-common-dir")
            .output()
            .map_err(|error| format!("run git rev-parse: {error}"))?;
        if output.status.success() {
            let values = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let (head, raw_gitdir) = if needs_head {
                let head = values
                    .first()
                    .ok_or_else(|| "missing HEAD output".to_string())?;
                if head.len() != 40 || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(format!("HEAD is not a 40-hex object ID: {head}"));
                }
                (
                    Some(head.clone()),
                    values
                        .get(1)
                        .ok_or_else(|| "missing git common directory output".to_string())?,
                )
            } else {
                (
                    None,
                    values
                        .first()
                        .ok_or_else(|| "missing git common directory output".to_string())?,
                )
            };
            let path = PathBuf::from(raw_gitdir);
            let absolute = if path.is_absolute() {
                path
            } else {
                checkout.join(path)
            };
            return Ok((
                head,
                fs::canonicalize(&absolute).unwrap_or_else(|_| normalize(&absolute)),
            ));
        }
        if needs_head {
            return Err(format!(
                "git rev-parse failed: {}",
                truncated_stderr(&output)
            ));
        }
    } else if needs_head {
        return Err(format!("source checkout {} is missing", checkout.display()));
    }
    let fallback = source_root
        .join(".repo/projects")
        .join(format!("{project_path}.git"));
    if fallback.is_dir() {
        Ok((
            None,
            fs::canonicalize(&fallback).unwrap_or_else(|_| normalize(&fallback)),
        ))
    } else {
        Err("no git common directory".into())
    }
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
    fn records_remote_and_tracking_upstream_like_repo_start() {
        let temp = tempfile::tempdir().unwrap();
        for path in ["a", "b", "c"] {
            fs::create_dir_all(temp.path().join(format!(".repo/projects/{path}.git"))).unwrap();
        }
        let xml = format!(
            r#"<manifest><remote name="goog" fetch=".."/><remote name="aosp" fetch=".." revision="aosp-main"/><default remote="goog" revision="studio-main"/>
<project name="a" revision="{sha}" upstream="studio-main"/>
<project name="b" revision="{sha}" remote="aosp" dest-branch="refs/heads/release"/>
<project name="c" revision="{sha}" remote="aosp"/></manifest>"#,
            sha = sha()
        );
        let parsed = parse_snapshot(&xml, temp.path(), "a\nb\nc\n", 2).unwrap();
        let by_path = |path: &str| {
            parsed
                .projects
                .iter()
                .find(|p| p.path == path)
                .unwrap()
                .clone()
        };
        assert_eq!(by_path("a").remote.as_deref(), Some("goog"));
        assert_eq!(
            by_path("a").upstream.as_deref(),
            Some("refs/heads/studio-main")
        );
        assert_eq!(by_path("b").remote.as_deref(), Some("aosp"));
        assert_eq!(by_path("b").upstream.as_deref(), Some("refs/heads/release"));
        assert_eq!(
            by_path("c").upstream.as_deref(),
            Some("refs/heads/aosp-main")
        );
    }

    #[test]
    fn extend_project_directives_attach_to_the_base_project() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".repo/projects/a.git")).unwrap();
        let xml = format!(
            r#"<manifest><project name="p" path="a" revision="{}"/><extend-project name="p"><linkfile src="x" dest="X"/><copyfile src="y" dest="Y"/></extend-project></manifest>"#,
            sha()
        );
        let parsed = parse_snapshot(&xml, temp.path(), "a\n", 2).unwrap();
        assert_eq!(parsed.linkfiles[0].project, "a");
        assert_eq!(parsed.linkfiles[0].dest, "X");
        assert_eq!(parsed.copyfiles[0].dest, "Y");
    }

    #[test]
    fn supplemented_declarations_honor_extend_project() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".repo/manifests")).unwrap();
        fs::write(
            temp.path().join(".repo/manifest.xml"),
            r#"<manifest><default remote="r" revision="main"/><project name="tools/p" path="p"/><extend-project name="tools/p"><linkfile src="a" dest="b"/></extend-project></manifest>"#,
        )
        .unwrap();
        let declarations = load_active_manifest_declarations(temp.path()).unwrap();
        let declaration = &declarations["p"];
        assert_eq!(declaration.linkfiles.len(), 1);
        assert_eq!(declaration.remote.as_deref(), Some("r"));
        assert_eq!(declaration.upstream.as_deref(), Some("refs/heads/main"));
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

    #[test]
    fn parses_positive_clone_depth_and_reports_malformed_values() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".repo/projects/a.git")).unwrap();
        fs::create_dir_all(temp.path().join(".repo/projects/b.git")).unwrap();
        let xml = format!(
            r#"<manifest><project name="a" revision="{}" clone-depth="2"/><project name="b" revision="{}" clone-depth="zero"/></manifest>"#,
            sha(),
            sha()
        );
        let parsed = parse_snapshot(&xml, temp.path(), "a\nb\n", 2).unwrap();
        assert_eq!(parsed.projects[0].clone_depth, Some(2));
        assert_eq!(parsed.projects[1].clone_depth, None);
        assert_eq!(parsed.malformed_clone_depth, vec!["b"]);
    }

    #[test]
    fn supplemented_project_preserves_clone_depth_in_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".repo/manifests")).unwrap();
        let project = temp.path().join("prebuilt");
        fs::create_dir(&project).unwrap();
        for args in [
            &["init"][..],
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "Test"][..],
        ] {
            assert!(Command::new("git")
                .current_dir(&project)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        }
        fs::write(project.join("payload"), "fixed\n").unwrap();
        for args in [&["add", "payload"][..], &["commit", "-m", "initial"][..]] {
            assert!(Command::new("git")
                .current_dir(&project)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        }
        fs::write(temp.path().join(".repo/project.list"), "prebuilt\n").unwrap();
        fs::write(
            temp.path().join(".repo/manifest.xml"),
            r#"<manifest><project name="tools/prebuilt" path="prebuilt" clone-depth="7"/></manifest>"#,
        )
        .unwrap();
        let snapshot = temp.path().join("snapshot.xml");
        fs::write(&snapshot, "<manifest></manifest>").unwrap();

        let parsed = parse_snapshot_file(temp.path(), &snapshot, 2).unwrap();
        assert_eq!(parsed.projects[0].clone_depth, Some(7));
        assert!(fs::read_to_string(snapshot)
            .unwrap()
            .contains(r#"clone-depth="7""#));
    }

    #[test]
    fn parses_tracked_sizes_conservatively() {
        let records = b"100644 blob a 12\tone\0\
100755 blob b 34\ttwo\0\
040000 tree c -\tdir\0\
160000 commit d -\tsubmodule\0\
100644 blob e malformed\tbad\0";
        assert_eq!(parse_tracked_bytes(records), 46);
    }

    #[test]
    fn tracked_size_sum_saturates() {
        let records = format!(
            "100644 blob a {}\tone\0\
100644 blob b 1\ttwo\0",
            u64::MAX
        );
        assert_eq!(parse_tracked_bytes(records.as_bytes()), u64::MAX);
    }
}
