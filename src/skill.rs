use crate::cli::{SkillCommand, SkillMutationArgs, SkillRemoveArgs, SkillScope, SkillSyncArgs};
use crate::discovery::find_direct_client_root;
use crate::error::{AppError, AppResult};
use crate::util::{absolutize, write_atomic};
use serde::Serialize;
use serde_json::{json, Value};
use serde_yaml::Mapping;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const EMBEDDED_SKILL: &str = include_str!("../skill/SKILL.md");
const SKILL_NAME: &str = "git-repo-worktree";
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub id: String,
    pub scope: String,
    pub skills_dir: String,
    pub skill_path: String,
    pub eligible: bool,
    pub installed: bool,
    pub current: bool,
    pub installed_version: Option<String>,
    pub foreign: bool,
    pub unreadable: bool,
    #[serde(skip_serializing)]
    anchor: String,
    #[serde(skip_serializing)]
    evidence: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct SkillAction {
    id: String,
    skill_path: Option<String>,
    action: String,
    from_version: Option<String>,
    to_version: Option<String>,
}

pub fn run(command: &SkillCommand, cwd: &Path, json_output: bool, quiet: bool) -> AppResult<Value> {
    validate_embedded_skill()?;
    match command {
        SkillCommand::Show => show(json_output),
        SkillCommand::List => list(cwd, json_output),
        SkillCommand::Add(args) => add(args, cwd, quiet),
        SkillCommand::Sync(args) => sync(args, cwd, quiet),
        SkillCommand::Remove(args) => remove(args, cwd, quiet),
    }
}

fn show(json_output: bool) -> AppResult<Value> {
    if !json_output {
        print!("{EMBEDDED_SKILL}");
    }
    Ok(json!({
        "skill": EMBEDDED_SKILL,
        "name": SKILL_NAME,
        "version": current_version()
    }))
}

fn list(cwd: &Path, json_output: bool) -> AppResult<Value> {
    let candidates = probe_all(cwd)?;
    if !json_output {
        for candidate in &candidates {
            println!(
                "{}  {}  {}",
                candidate.id,
                candidate.skill_path,
                candidate_status(candidate)
            );
        }
    }
    Ok(json!({"candidates": candidates}))
}

fn candidate_status(candidate: &Candidate) -> String {
    if candidate.unreadable {
        "unreadable".into()
    } else if candidate.foreign {
        "foreign".into()
    } else if candidate.installed && candidate.current {
        "installed (current)".into()
    } else if candidate.installed {
        format!(
            "installed v{} (stale)",
            candidate.installed_version.as_deref().unwrap_or("unknown")
        )
    } else if candidate.eligible {
        "eligible".into()
    } else {
        "not-eligible".into()
    }
}

fn add(args: &SkillMutationArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    let candidates = probe_all(cwd)?;
    let (selected, mut actions) = select_add_candidates(args, &candidates, cwd, quiet)?;
    let mut foreign = Vec::new();
    for candidate in selected {
        let action = install_candidate(&candidate, args.force, args.dry_run)?;
        if action.action == "blocked" {
            foreign.push(candidate.skill_path.clone());
        }
        if !quiet {
            print_action(&action);
        }
        actions.push(action);
    }
    actions.sort_by_key(|action| {
        candidates
            .iter()
            .position(|candidate| candidate.id == action.id)
            .unwrap_or(usize::MAX)
    });
    let result = json!({"actions": actions});
    if foreign.is_empty() {
        Ok(result)
    } else {
        Err(AppError::operational(
            "E_SKILL_FOREIGN",
            format!(
                "refusing to overwrite foreign skill file(s): {}; pass --force",
                foreign.join(", ")
            ),
        )
        .with_result(result))
    }
}

fn sync(args: &SkillSyncArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    let candidates = probe_all(cwd)?;
    let mut actions = Vec::new();
    let mut seen = HashSet::new();
    for candidate in candidates {
        if !seen.insert(candidate.skill_path.clone()) {
            continue;
        }
        let action = if candidate.installed {
            install_candidate(&candidate, false, args.dry_run)?
        } else if candidate.foreign {
            SkillAction {
                id: candidate.id,
                skill_path: Some(candidate.skill_path),
                action: "skipped-foreign".into(),
                from_version: None,
                to_version: None,
            }
        } else {
            continue;
        };
        if !quiet {
            print_action(&action);
        }
        actions.push(action);
    }
    Ok(json!({"actions": actions}))
}

fn remove(args: &SkillRemoveArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    let candidates = probe_all(cwd)?;
    let selected = select_remove_candidates(args, &candidates, cwd)?;
    let single = selected.len() == 1 && (args.path.is_some() || args.target.len() == 1);
    let mut actions = Vec::new();
    let mut blocked = Vec::new();
    let mut absent = Vec::new();
    for candidate in selected {
        let action = remove_candidate(&candidate, args.force)?;
        if action.action == "blocked" {
            blocked.push(candidate.skill_path.clone());
        } else if action.action == "absent" {
            absent.push(candidate.skill_path.clone());
        }
        if !quiet {
            print_action(&action);
        }
        actions.push(action);
    }
    let result = json!({"actions": actions});
    if !blocked.is_empty() {
        Err(AppError::operational(
            "E_SKILL_FOREIGN",
            format!(
                "refusing to remove foreign skill file(s): {}; pass --force",
                blocked.join(", ")
            ),
        )
        .with_result(result))
    } else if single && !absent.is_empty() {
        Err(AppError::operational(
            "E_SKILL_NOT_INSTALLED",
            format!("skill is not installed at {}", absent.join(", ")),
        )
        .with_result(result))
    } else {
        Ok(result)
    }
}

fn print_action(action: &SkillAction) {
    let path = action.skill_path.as_deref().unwrap_or("-");
    eprintln!("{}  {}  {}", action.id, path, action.action);
}

fn select_add_candidates(
    args: &SkillMutationArgs,
    all: &[Candidate],
    cwd: &Path,
    quiet: bool,
) -> AppResult<(Vec<Candidate>, Vec<SkillAction>)> {
    if let Some(path) = &args.path {
        let directory = absolutize(path, cwd);
        if !directory.is_dir() {
            return Err(AppError::operational(
                "E_NO_SKILL_TARGET",
                format!("skills directory does not exist: {}", directory.display()),
            ));
        }
        return Ok((vec![custom_candidate("path", &directory)], Vec::new()));
    }
    if !args.target.is_empty() {
        let mut selected = Vec::new();
        for id in &args.target {
            let candidate = all
                .iter()
                .find(|candidate| candidate.id == *id && candidate.scope != "env")
                .ok_or_else(|| AppError::usage(format!("unknown skill target: {id}")))?;
            if !candidate.eligible {
                return Err(no_target_error(candidate));
            }
            selected.push(candidate.clone());
        }
        return Ok((selected, Vec::new()));
    }

    let scope = args.scope.unwrap_or(SkillScope::Auto);
    let mut selected = if args.all {
        all.iter()
            .filter(|candidate| candidate.eligible)
            .cloned()
            .collect::<Vec<_>>()
    } else {
        match scope {
            SkillScope::User => all
                .iter()
                .filter(|candidate| candidate.scope == "user" && candidate.eligible)
                .cloned()
                .collect(),
            SkillScope::Project => all
                .iter()
                .filter(|candidate| candidate.scope == "project" && candidate.eligible)
                .cloned()
                .collect(),
            SkillScope::Auto => {
                let mut values = all
                    .iter()
                    .filter(|candidate| candidate.scope == "env" && candidate.eligible)
                    .cloned()
                    .collect::<Vec<_>>();
                let project = all
                    .iter()
                    .filter(|candidate| candidate.scope == "project" && candidate.eligible)
                    .cloned()
                    .collect::<Vec<_>>();
                if project.is_empty() {
                    values.extend(
                        all.iter()
                            .filter(|candidate| candidate.scope == "user" && candidate.eligible)
                            .cloned(),
                    );
                } else {
                    values.extend(project);
                }
                values
            }
        }
    };
    if selected.is_empty() {
        return Err(no_selection_error(scope, all));
    }
    let skipped = alias_dedupe(&mut selected, quiet);
    Ok((selected, skipped))
}

fn alias_dedupe(selected: &mut Vec<Candidate>, quiet: bool) -> Vec<SkillAction> {
    let agent_anchors = selected
        .iter()
        .filter(|candidate| candidate.id.starts_with("agents-"))
        .map(|candidate| (candidate.scope.clone(), candidate.anchor.clone()))
        .collect::<HashSet<_>>();
    let mut skipped = Vec::new();
    selected.retain(|candidate| {
        let covered = candidate.id.starts_with("gemini-")
            && agent_anchors.contains(&(candidate.scope.clone(), candidate.anchor.clone()));
        if covered {
            let agent_id = if candidate.scope == "user" {
                "agents-user"
            } else {
                "agents-project"
            };
            skipped.push(SkillAction {
                id: candidate.id.clone(),
                skill_path: None,
                action: "skipped-alias".into(),
                from_version: None,
                to_version: None,
            });
            if !quiet {
                eprintln!("{}  skipped (covered by {agent_id})", candidate.id);
            }
        }
        !covered
    });
    skipped
}

fn select_remove_candidates(
    args: &SkillRemoveArgs,
    all: &[Candidate],
    cwd: &Path,
) -> AppResult<Vec<Candidate>> {
    if let Some(path) = &args.path {
        let directory = absolutize(path, cwd);
        if !directory.is_dir() {
            return Err(AppError::operational(
                "E_NO_SKILL_TARGET",
                format!("skills directory does not exist: {}", directory.display()),
            ));
        }
        return Ok(vec![custom_candidate("path", &directory)]);
    }
    if !args.target.is_empty() {
        let mut selected = Vec::new();
        for id in &args.target {
            let candidate = all
                .iter()
                .find(|candidate| candidate.id == *id && candidate.scope != "env")
                .ok_or_else(|| AppError::usage(format!("unknown skill target: {id}")))?;
            if !candidate.eligible && !candidate.installed && !candidate.foreign {
                return Err(no_target_error(candidate));
            }
            selected.push(candidate.clone());
        }
        return Ok(selected);
    }
    if args.all {
        return Ok(all
            .iter()
            .filter(|candidate| candidate.eligible || candidate.installed || candidate.foreign)
            .cloned()
            .collect());
    }
    match args.scope.unwrap_or(SkillScope::Auto) {
        SkillScope::Auto => Ok(all
            .iter()
            .filter(|candidate| candidate.installed)
            .cloned()
            .collect()),
        SkillScope::User => Ok(all
            .iter()
            .filter(|candidate| candidate.scope == "user" && candidate.eligible)
            .cloned()
            .collect()),
        SkillScope::Project => Ok(all
            .iter()
            .filter(|candidate| candidate.scope == "project" && candidate.eligible)
            .cloned()
            .collect()),
    }
}

fn install_candidate(candidate: &Candidate, force: bool, dry_run: bool) -> AppResult<SkillAction> {
    let version = current_version();
    let from_version = candidate.installed_version.clone();
    let (action, should_write) = if candidate.current {
        ("unchanged", false)
    } else if candidate.foreign && !force {
        ("blocked", false)
    } else if candidate.foreign {
        (if dry_run { "would-replace" } else { "replaced" }, true)
    } else if candidate.installed {
        (if dry_run { "would-update" } else { "updated" }, true)
    } else {
        (
            if dry_run {
                "would-install"
            } else {
                "installed"
            },
            true,
        )
    };
    if should_write && !dry_run {
        let skills_dir = Path::new(&candidate.skills_dir);
        create_dir_all_0755(skills_dir)?;
        let leaf = skills_dir.join(SKILL_NAME);
        create_dir_all_0755(&leaf)?;
        let payload = installed_payload(&version)?;
        write_atomic(Path::new(&candidate.skill_path), payload.as_bytes(), 0o644)?;
    }
    Ok(SkillAction {
        id: candidate.id.clone(),
        skill_path: Some(candidate.skill_path.clone()),
        action: action.into(),
        from_version,
        to_version: (action != "blocked").then_some(version),
    })
}

fn create_dir_all_0755(path: &Path) -> AppResult<()> {
    let mut cursor = path;
    let mut missing = Vec::new();
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor.parent().ok_or_else(|| {
            AppError::operational(
                "E_IO",
                format!("directory has no existing ancestor: {}", path.display()),
            )
        })?;
    }
    fs::create_dir_all(path)
        .map_err(|error| AppError::io(format!("create {}", path.display()), error))?;
    for directory in missing.into_iter().rev() {
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .map_err(|error| AppError::io(format!("chmod {}", directory.display()), error))?;
    }
    Ok(())
}

fn remove_candidate(candidate: &Candidate, force: bool) -> AppResult<SkillAction> {
    let (action, remove) = if candidate.installed {
        ("removed", true)
    } else if candidate.foreign && force {
        ("removed-foreign", true)
    } else if candidate.foreign {
        ("blocked", false)
    } else {
        ("absent", false)
    };
    if remove {
        let skill_path = Path::new(&candidate.skill_path);
        fs::remove_file(skill_path)
            .map_err(|error| AppError::io(format!("remove {}", skill_path.display()), error))?;
        if let Some(leaf) = skill_path.parent() {
            if fs::read_dir(leaf)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false)
            {
                fs::remove_dir(leaf)
                    .map_err(|error| AppError::io(format!("remove {}", leaf.display()), error))?;
            }
        }
    }
    Ok(SkillAction {
        id: candidate.id.clone(),
        skill_path: Some(candidate.skill_path.clone()),
        action: action.into(),
        from_version: candidate.installed_version.clone(),
        to_version: None,
    })
}

fn no_target_error(candidate: &Candidate) -> AppError {
    AppError::operational(
        "E_NO_SKILL_TARGET",
        format!(
            "{} is not eligible; missing evidence: {}",
            candidate.id,
            candidate.evidence.join(" or ")
        ),
    )
}

fn no_selection_error(scope: SkillScope, all: &[Candidate]) -> AppError {
    if scope == SkillScope::Project {
        let paths = all
            .iter()
            .filter(|candidate| candidate.scope == "project")
            .map(|candidate| candidate.skills_dir.as_str())
            .collect::<Vec<_>>();
        return AppError::operational(
            "E_NO_SKILL_TARGET",
            format!(
                "no project skill target exists; create one of {} (recommended interoperable choice), {}, or {} with mkdir -p, then re-run",
                paths.first().copied().unwrap_or(".agents/skills"),
                paths.get(1).copied().unwrap_or(".claude/skills"),
                paths.get(2).copied().unwrap_or(".gemini/skills")
            ),
        );
    }
    let rejected = all
        .iter()
        .filter(|candidate| !candidate.eligible)
        .map(|candidate| format!("{} ({})", candidate.id, candidate.evidence.join(" or ")))
        .collect::<Vec<_>>()
        .join(", ");
    AppError::operational(
        "E_NO_SKILL_TARGET",
        format!(
            "no eligible skill target; rejected: {rejected}. The tool will not create framework roots; use mkdir -p ~/.agents/skills (or a project equivalent), or pass --path"
        ),
    )
}

fn probe_all(cwd: &Path) -> AppResult<Vec<Candidate>> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| AppError::operational("E_NO_SKILL_TARGET", "HOME is not set"))?;
    let home = absolutize(&home, cwd);
    let mut candidates = Vec::new();
    if let Some(value) = std::env::var_os("GRW_SKILL_DIRS") {
        for path in std::env::split_paths(&value) {
            if path.as_os_str().is_empty() {
                continue;
            }
            let path = absolutize(&path, cwd);
            candidates.push(probe_candidate(Candidate {
                id: format!("env:{}", path.display()),
                scope: "env".into(),
                skills_dir: path.to_string_lossy().into_owned(),
                skill_path: skill_path(&path).to_string_lossy().into_owned(),
                eligible: path.is_dir(),
                installed: false,
                current: false,
                installed_version: None,
                foreign: false,
                unreadable: false,
                anchor: path.to_string_lossy().into_owned(),
                evidence: vec![path.to_string_lossy().into_owned()],
            }));
        }
    }

    for (id, framework) in [
        ("agents-project", ".agents"),
        ("claude-code-project", ".claude"),
        ("gemini-project", ".gemini"),
    ] {
        let (anchor, evidence, eligible) = project_target(cwd, &home, framework);
        let skills = anchor.join(framework).join("skills");
        candidates.push(probe_candidate(Candidate {
            id: id.into(),
            scope: "project".into(),
            skills_dir: skills.to_string_lossy().into_owned(),
            skill_path: skill_path(&skills).to_string_lossy().into_owned(),
            eligible,
            installed: false,
            current: false,
            installed_version: None,
            foreign: false,
            unreadable: false,
            anchor: anchor.to_string_lossy().into_owned(),
            evidence: vec![evidence.to_string_lossy().into_owned()],
        }));
    }

    for (id, framework) in [
        ("agents-user", ".agents"),
        ("claude-code-user", ".claude"),
        ("gemini-user", ".gemini"),
    ] {
        let root = home.join(framework);
        let skills = root.join("skills");
        candidates.push(probe_candidate(Candidate {
            id: id.into(),
            scope: "user".into(),
            skills_dir: skills.to_string_lossy().into_owned(),
            skill_path: skill_path(&skills).to_string_lossy().into_owned(),
            eligible: root.is_dir(),
            installed: false,
            current: false,
            installed_version: None,
            foreign: false,
            unreadable: false,
            anchor: home.to_string_lossy().into_owned(),
            evidence: vec![root.to_string_lossy().into_owned()],
        }));
    }
    let anti_a = home.join(".gemini/antigravity");
    let anti_b = home.join(".gemini/antigravity-cli");
    let anti_skills = home.join(".gemini/config/skills");
    candidates.push(probe_candidate(Candidate {
        id: "antigravity-user".into(),
        scope: "user".into(),
        skills_dir: anti_skills.to_string_lossy().into_owned(),
        skill_path: skill_path(&anti_skills).to_string_lossy().into_owned(),
        eligible: anti_a.is_dir() || anti_b.is_dir(),
        installed: false,
        current: false,
        installed_version: None,
        foreign: false,
        unreadable: false,
        anchor: home.to_string_lossy().into_owned(),
        evidence: vec![
            anti_a.to_string_lossy().into_owned(),
            anti_b.to_string_lossy().into_owned(),
        ],
    }));
    Ok(candidates)
}

fn project_target(cwd: &Path, home: &Path, framework: &str) -> (PathBuf, PathBuf, bool) {
    let client_root = find_direct_client_root(cwd);
    for ancestor in cwd.ancestors() {
        if ancestor == home || ancestor.parent().is_none() {
            break;
        }
        let evidence = ancestor.join(framework);
        if evidence.is_dir() {
            return (ancestor.to_path_buf(), evidence, true);
        }
        if client_root.as_deref() == Some(ancestor) {
            return (ancestor.to_path_buf(), evidence, false);
        }
    }
    let anchor = client_root.unwrap_or_else(|| cwd.to_path_buf());
    let evidence = anchor.join(framework);
    (anchor, evidence, false)
}

fn custom_candidate(id: &str, skills_dir: &Path) -> Candidate {
    probe_candidate(Candidate {
        id: id.into(),
        scope: "custom".into(),
        skills_dir: skills_dir.to_string_lossy().into_owned(),
        skill_path: skill_path(skills_dir).to_string_lossy().into_owned(),
        eligible: skills_dir.is_dir(),
        installed: false,
        current: false,
        installed_version: None,
        foreign: false,
        unreadable: false,
        anchor: skills_dir.to_string_lossy().into_owned(),
        evidence: vec![skills_dir.to_string_lossy().into_owned()],
    })
}

fn skill_path(skills_dir: &Path) -> PathBuf {
    skills_dir.join(SKILL_NAME).join("SKILL.md")
}

fn probe_candidate(mut candidate: Candidate) -> Candidate {
    let path = Path::new(&candidate.skill_path);
    match fs::read_to_string(path) {
        Ok(contents) => {
            if let Some(version) = sentinel_version(&contents) {
                candidate.installed = true;
                candidate.installed_version = version;
                candidate.current = installed_payload(&current_version())
                    .map(|expected| expected == contents)
                    .unwrap_or(false);
            } else {
                candidate.foreign = true;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => candidate.unreadable = true,
    }
    candidate
}

fn installed_payload(version: &str) -> AppResult<String> {
    let (frontmatter, body) = split_frontmatter(EMBEDDED_SKILL).ok_or_else(|| {
        AppError::operational(
            "E_SKILL_INVALID",
            "embedded SKILL.md has invalid YAML frontmatter",
        )
    })?;
    Ok(format!(
        "---\n{frontmatter}x-installed-by: git-repo-worktree\nx-installed-version: \"{version}\"\n---\n{body}"
    ))
}

fn sentinel_version(contents: &str) -> Option<Option<String>> {
    let (frontmatter, _) = split_frontmatter(contents)?;
    let mapping: Mapping = serde_yaml::from_str(frontmatter).ok()?;
    let installed_by = mapping
        .get(ValueKey::installed_by())
        .and_then(serde_yaml::Value::as_str)?;
    if installed_by != SKILL_NAME {
        return None;
    }
    Some(
        mapping
            .get(ValueKey::installed_version())
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_owned),
    )
}

struct ValueKey;

impl ValueKey {
    fn installed_by() -> &'static serde_yaml::Value {
        static KEY: std::sync::OnceLock<serde_yaml::Value> = std::sync::OnceLock::new();
        KEY.get_or_init(|| serde_yaml::Value::String("x-installed-by".into()))
    }

    fn installed_version() -> &'static serde_yaml::Value {
        static KEY: std::sync::OnceLock<serde_yaml::Value> = std::sync::OnceLock::new();
        KEY.get_or_init(|| serde_yaml::Value::String("x-installed-version".into()))
    }
}

fn split_frontmatter(contents: &str) -> Option<(&str, &str)> {
    let rest = contents.strip_prefix("---\n")?;
    let marker = rest.find("\n---\n")?;
    let frontmatter = &rest[..marker + 1];
    let body = &rest[marker + 5..];
    Some((frontmatter, body))
}

fn validate_embedded_skill() -> AppResult<()> {
    if EMBEDDED_SKILL.lines().count() > 500 {
        return Err(AppError::operational(
            "E_SKILL_INVALID",
            "embedded SKILL.md exceeds 500 lines",
        ));
    }
    let (frontmatter, _) = split_frontmatter(EMBEDDED_SKILL).ok_or_else(|| {
        AppError::operational("E_SKILL_INVALID", "embedded SKILL.md lacks frontmatter")
    })?;
    let mapping: Mapping = serde_yaml::from_str(frontmatter)
        .map_err(|error| AppError::operational("E_SKILL_INVALID", error.to_string()))?;
    for key in ["name", "description"] {
        if !mapping.contains_key(serde_yaml::Value::String(key.into())) {
            return Err(AppError::operational(
                "E_SKILL_INVALID",
                format!("embedded SKILL.md lacks {key}"),
            ));
        }
    }
    if mapping.contains_key(ValueKey::installed_by())
        || mapping.contains_key(ValueKey::installed_version())
    {
        return Err(AppError::operational(
            "E_SKILL_INVALID",
            "sentinel fields must not be authored in the embedded skill",
        ));
    }
    Ok(())
}

fn current_version() -> String {
    #[cfg(feature = "test-hooks")]
    if let Ok(version) = std::env::var("GRW_TEST_VERSION") {
        return version;
    }
    VERSION.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_adds_and_detects_sentinel() {
        let payload = installed_payload("9.8.7").unwrap();
        assert_eq!(sentinel_version(&payload), Some(Some("9.8.7".to_string())));
        assert!(!EMBEDDED_SKILL.contains("x-installed-by"));
    }

    #[test]
    fn show_payload_is_foreign() {
        assert_eq!(sentinel_version(EMBEDDED_SKILL), None);
    }
}
