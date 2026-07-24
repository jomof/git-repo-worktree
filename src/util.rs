use crate::error::{AppError, AppResult};
use crate::model::{Breadcrumb, JournalRecord, TreeMeta, FORMAT_VERSION};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Output;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub fn now() -> String {
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .expect("zero nanoseconds is valid")
        .format(&Rfc3339)
        .expect("RFC3339 formatting is infallible")
}

pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(32)
}

pub fn truncated_stderr(output: &Output) -> String {
    let bytes = &output.stderr[..output.stderr.len().min(500)];
    String::from_utf8_lossy(bytes).trim().to_string()
}

pub fn exit_reason(output: &Output) -> String {
    let code = output.status.code().unwrap_or(1);
    let stderr = truncated_stderr(output);
    let mut reason = format!("exit {code}: {stderr}");
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("promisor") || lower.contains("could not fetch") {
        reason.push_str(
            " (source uses partial clone; network/auth may be required — retry with 'add --continue')",
        );
    }
    reason
}

pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> AppResult<()> {
    let parent = path.parent().ok_or_else(|| {
        AppError::operational("E_IO", format!("path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| AppError::io(format!("create {}", parent.display()), error))?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("git-repo-worktree"),
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true).mode(mode);
    let mut file = options
        .open(&temp)
        .map_err(|error| AppError::io(format!("create {}", temp.display()), error))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| AppError::io(format!("write {}", temp.display()), error))?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
        .map_err(|error| AppError::io(format!("chmod {}", temp.display()), error))?;
    fs::rename(&temp, path)
        .map_err(|error| AppError::io(format!("replace {}", path.display()), error))
}

pub fn write_meta(meta: &TreeMeta) -> AppResult<()> {
    let path = meta.registry_entry().join("meta.json");
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|error| AppError::io("serialize meta.json", error))?;
    write_atomic(&path, &bytes, 0o644)
}

pub fn read_meta(path: &Path) -> AppResult<TreeMeta> {
    let bytes =
        fs::read(path).map_err(|error| AppError::io(format!("read {}", path.display()), error))?;
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
            format!(
                "{} uses a newer metadata format; upgrade git-repo-worktree",
                path.display()
            ),
        ));
    }
    serde_json::from_value(value)
        .map_err(|error| AppError::io(format!("parse {}", path.display()), error))
}

pub fn write_breadcrumb(meta: &TreeMeta) -> AppResult<()> {
    let breadcrumb = Breadcrumb {
        format_version: FORMAT_VERSION,
        name: meta.name.clone(),
        source_root: meta.source_root.clone(),
    };
    let bytes = serde_json::to_vec(&breadcrumb)
        .map_err(|error| AppError::io("serialize breadcrumb", error))?;
    write_atomic(&meta.root().join(".repo-worktree"), &bytes, 0o644)
}

pub fn append_journal(
    entry: &Path,
    step: &str,
    status: &str,
    project: Option<&str>,
    dest: Option<&str>,
    reason: Option<&str>,
) -> AppResult<()> {
    let record = journal_record(step, status, project, dest, reason);
    append_journal_batch(entry, &[record])
}

pub fn journal_record(
    step: &str,
    status: &str,
    project: Option<&str>,
    dest: Option<&str>,
    reason: Option<&str>,
) -> JournalRecord {
    JournalRecord {
        ts: now(),
        step: step.to_string(),
        status: status.to_string(),
        project: project.map(str::to_owned),
        dest: dest.map(str::to_owned),
        reason: reason.map(str::to_owned),
    }
}

pub fn append_journal_batch(entry: &Path, records: &[JournalRecord]) -> AppResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)
            .map_err(|error| AppError::io("serialize journal record", error))?;
        bytes.push(b'\n');
    }
    let path = entry.join("journal.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(&path)
        .map_err(|error| AppError::io(format!("open {}", path.display()), error))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| AppError::io(format!("append journal batch to {}", path.display()), error))
}

pub fn journal_has_done(entry: &Path, step: &str) -> AppResult<bool> {
    journal_has(entry, step, "done", None)
}

pub fn journal_has_project_status(
    entry: &Path,
    step: &str,
    status: &str,
    project: &str,
) -> AppResult<bool> {
    journal_has(entry, step, status, Some(project))
}

fn journal_has(entry: &Path, step: &str, status: &str, project: Option<&str>) -> AppResult<bool> {
    let path = entry.join("journal.jsonl");
    let mut contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(AppError::io(format!("read {}", path.display()), error)),
    };
    if !contents.is_empty() && !contents.ends_with('\n') {
        let valid_len = contents.rfind('\n').map_or(0, |index| index + 1);
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| AppError::io(format!("open {}", path.display()), error))?;
        file.set_len(valid_len as u64)
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                AppError::io(
                    format!("truncate incomplete journal record in {}", path.display()),
                    error,
                )
            })?;
        contents.truncate(valid_len);
    }
    for line in contents.lines() {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| AppError::io(format!("parse {}", path.display()), error))?;
        if value.get("step").and_then(serde_json::Value::as_str) == Some(step)
            && value.get("status").and_then(serde_json::Value::as_str) == Some(status)
            && project.map_or(true, |project| {
                value.get("project").and_then(serde_json::Value::as_str) == Some(project)
            })
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn absolutize(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    normalize(&joined)
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(value) => out.push(value),
        }
    }
    out
}

pub fn canonicalize_missing(path: &Path) -> AppResult<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            AppError::operational("E_IO", format!("cannot resolve path {}", path.display()))
        })?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or_else(|| AppError::operational("E_IO", "path has no existing ancestor"))?
            .to_path_buf();
    }
    let mut resolved = fs::canonicalize(&existing)
        .map_err(|error| AppError::io(format!("resolve {}", existing.display()), error))?;
    for name in suffix.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

pub fn is_path_prefix(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent)
}

pub fn sanitize_name(value: &OsStr) -> String {
    value
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn path_is_safe_relative(path: &Path) -> bool {
    !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

pub fn ensure_safe_destination(tree_root: &Path, relative: &Path) -> AppResult<PathBuf> {
    if !path_is_safe_relative(relative) {
        return Err(AppError::operational(
            "E_UNSAFE_PATH",
            "unsafe relative path",
        ));
    }
    let canonical_root = canonicalize_missing(tree_root)?;
    let dest = tree_root.join(relative);
    let parent = dest.parent().unwrap_or(tree_root);

    let mut cursor = tree_root.to_path_buf();
    if let Ok(rest) = parent.strip_prefix(tree_root) {
        for component in rest.components() {
            cursor.push(component.as_os_str());
            if cursor.exists() {
                let metadata = fs::symlink_metadata(&cursor).map_err(|error| {
                    AppError::io(format!("inspect {}", cursor.display()), error)
                })?;
                if metadata.file_type().is_symlink() {
                    return Err(AppError::operational(
                        "E_UNSAFE_PATH",
                        format!("symlink in destination path: {}", cursor.display()),
                    ));
                }
            } else {
                if let Err(error) = fs::create_dir(&cursor) {
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(AppError::io(format!("create {}", cursor.display()), error));
                    }
                    let metadata = fs::symlink_metadata(&cursor).map_err(|error| {
                        AppError::io(format!("inspect {}", cursor.display()), error)
                    })?;
                    if metadata.file_type().is_symlink() {
                        return Err(AppError::operational(
                            "E_UNSAFE_PATH",
                            format!("symlink in destination path: {}", cursor.display()),
                        ));
                    }
                }
                fs::set_permissions(&cursor, fs::Permissions::from_mode(0o755))
                    .map_err(|error| AppError::io(format!("chmod {}", cursor.display()), error))?;
            }
        }
    }
    let resolved_parent = fs::canonicalize(parent)
        .map_err(|error| AppError::io(format!("resolve {}", parent.display()), error))?;
    if !resolved_parent.starts_with(&canonical_root) {
        return Err(AppError::operational(
            "E_UNSAFE_PATH",
            format!("destination escapes tree: {}", dest.display()),
        ));
    }
    Ok(dest)
}

pub fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from.len() {
        result.push("..");
    }
    for component in &to[common..] {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

pub fn copy_permissions(source: &Path, destination: &Path) -> AppResult<()> {
    let mode = fs::metadata(source)
        .map_err(|error| AppError::io(format!("stat {}", source.display()), error))?
        .permissions()
        .mode();
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .map_err(|error| AppError::io(format!("chmod {}", destination.display()), error))
}

pub fn dedupe_paths<'a>(paths: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths
        .filter(|value| seen.insert((*value).clone()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn sanitizes_names() {
        assert_eq!(
            sanitize_name(OsStr::new("hello world/ok")),
            "hello-world-ok"
        );
        assert_eq!(sanitize_name(OsStr::new("a_b.c-1")), "a_b.c-1");
    }

    #[test]
    fn computes_relative_paths() {
        assert_eq!(
            relative_path(Path::new("/tree/out"), Path::new("/tree/p/src")),
            PathBuf::from("../p/src")
        );
        assert_eq!(
            relative_path(Path::new("/tree"), Path::new("/tree/a")),
            PathBuf::from("a")
        );
    }

    #[test]
    fn rejects_parent_and_absolute_paths() {
        assert!(!path_is_safe_relative(Path::new("../escape")));
        assert!(!path_is_safe_relative(Path::new("/escape")));
        assert!(path_is_safe_relative(Path::new("safe/file")));
    }

    #[test]
    fn rejects_symlink_intermediate_destination() {
        let tree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), tree.path().join("escape")).unwrap();
        let error = ensure_safe_destination(tree.path(), Path::new("escape/file")).unwrap_err();
        assert_eq!(error.code, "E_UNSAFE_PATH");
        assert!(!outside.path().join("file").exists());
    }

    #[test]
    fn appends_and_fsyncs_journal_records_as_one_batch() {
        let entry = tempfile::tempdir().unwrap();
        let records = vec![
            journal_record("project-add", "done", Some("a"), None, None),
            journal_record(
                "project-add",
                "failed",
                Some("b"),
                None,
                Some("checkout failed"),
            ),
        ];
        append_journal_batch(entry.path(), &records).unwrap();
        let contents = fs::read_to_string(entry.path().join("journal.jsonl")).unwrap();
        assert_eq!(contents.lines().count(), 2);
        assert!(contents.ends_with('\n'));
    }

    #[test]
    fn ignores_only_an_incomplete_trailing_journal_record() {
        let entry = tempfile::tempdir().unwrap();
        append_journal(entry.path(), "pin", "done", None, None, None).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(entry.path().join("journal.jsonl"))
            .unwrap();
        file.write_all(br#"{"step":"project-add""#).unwrap();
        assert!(journal_has_done(entry.path(), "pin").unwrap());
        let path = entry.path().join("journal.jsonl");
        assert!(fs::read(&path).unwrap().ends_with(b"\n"));
        append_journal(entry.path(), "project-add", "done", Some("a"), None, None).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 2);
    }
}
