//! Local usage history: one JSON line per invocation in
//! `.repo/repo-worktrees/history.jsonl` of the source client.
//!
//! Records hold command and flag names, timings, outcomes, and project paths,
//! never flag values or file contents. Nothing leaves the machine. Writes are
//! unsynced appends, and the file is trimmed to its newest half when it grows
//! past [`MAX_BYTES`]. `GIT_REPO_WORKTREE_NO_HISTORY=1` disables recording.

use serde_json::{json, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const FILE_NAME: &str = "history.jsonl";
pub const RECORD_VERSION: u64 = 1;
const MAX_BYTES: u64 = 8 * 1024 * 1024;

struct Recorder {
    source_root: Option<PathBuf>,
    fields: Vec<(String, Value)>,
    phases: Vec<(String, u64)>,
}

static RECORDER: Mutex<Recorder> = Mutex::new(Recorder {
    source_root: None,
    fields: Vec::new(),
    phases: Vec::new(),
});

fn with<T>(operation: impl FnOnce(&mut Recorder) -> T) -> T {
    let mut recorder = RECORDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    operation(&mut recorder)
}

pub fn disabled() -> bool {
    std::env::var("GIT_REPO_WORKTREE_NO_HISTORY")
        .is_ok_and(|value| !value.is_empty() && value != "0")
}

pub fn history_path(source_root: &Path) -> PathBuf {
    source_root.join(".repo/repo-worktrees").join(FILE_NAME)
}

/// Remembers the source client the invocation acts on; the first one wins.
pub fn set_source(root: &Path) {
    with(|recorder| {
        if recorder.source_root.is_none() {
            recorder.source_root = Some(root.to_path_buf());
        }
    });
}

pub fn record(key: &str, value: Value) {
    with(|recorder| {
        recorder.fields.retain(|(existing, _)| existing != key);
        recorder.fields.push((key.to_string(), value));
    });
}

/// Appends `value` to the array field `key`.
pub fn push(key: &str, value: Value) {
    with(|recorder| {
        match recorder
            .fields
            .iter_mut()
            .find(|(existing, _)| existing == key)
        {
            Some((_, Value::Array(items))) => items.push(value),
            _ => recorder
                .fields
                .push((key.to_string(), Value::Array(vec![value]))),
        }
    });
}

pub fn phase(name: &str, milliseconds: u64) {
    with(|recorder| {
        match recorder
            .phases
            .iter_mut()
            .find(|(existing, _)| existing == name)
        {
            Some((_, total)) => *total += milliseconds,
            None => recorder.phases.push((name.to_string(), milliseconds)),
        }
    });
}

pub fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Reduces a command line to its shape: subcommand names and flag names.
/// Positional arguments and flag values are replaced or dropped.
pub fn argument_shape(arguments: &[String], subcommands: &[&str]) -> Vec<String> {
    let mut shape = Vec::new();
    let mut seen_subcommand = 0;
    for argument in arguments {
        if let Some(flag) = argument.strip_prefix("--") {
            if !flag.is_empty() {
                shape.push(format!("--{}", flag.split('=').next().unwrap_or(flag)));
            }
        } else if argument.starts_with('-') && argument.len() > 1 {
            shape.push(argument.chars().take(2).collect());
        } else if seen_subcommand < 2 && subcommands.contains(&argument.as_str()) {
            seen_subcommand += 1;
            shape.push(argument.clone());
        }
    }
    shape
}

/// Appends the invocation's record. Failures are ignored: history must never
/// change a command's outcome.
pub fn finish(
    command: &str,
    arguments: Vec<String>,
    milliseconds: u64,
    exit_code: i32,
    error: Option<&str>,
) {
    if disabled() {
        return;
    }
    let (source_root, fields, phases) = with(|recorder| {
        (
            recorder.source_root.take(),
            std::mem::take(&mut recorder.fields),
            std::mem::take(&mut recorder.phases),
        )
    });
    let Some(source_root) = source_root else {
        return;
    };
    let path = history_path(&source_root);
    if !path.parent().is_some_and(Path::is_dir) {
        return;
    }
    let mut event = Map::new();
    event.insert("v".into(), json!(RECORD_VERSION));
    event.insert("ts".into(), json!(crate::util::now()));
    event.insert("tool_version".into(), json!(env!("CARGO_PKG_VERSION")));
    event.insert("commit".into(), json!(option_env!("GRW_COMMIT")));
    event.insert("command".into(), json!(command));
    event.insert("args".into(), json!(arguments));
    event.insert("ms".into(), json!(milliseconds));
    event.insert("exit_code".into(), json!(exit_code));
    event.insert("error".into(), json!(error));
    event.insert(
        "via_git".into(),
        json!(std::env::var_os("GIT_EXEC_PATH").is_some()),
    );
    if let Ok(session) = std::env::var("GIT_REPO_WORKTREE_SESSION") {
        event.insert("session".into(), json!(session));
    }
    if !phases.is_empty() {
        event.insert(
            "phases".into(),
            Value::Object(
                phases
                    .into_iter()
                    .map(|(name, ms)| (name, json!(ms)))
                    .collect(),
            ),
        );
    }
    event.extend(fields);
    let Ok(mut line) = serde_json::to_vec(&Value::Object(event)) else {
        return;
    };
    line.push(b'\n');
    trim_if_large(&path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(&line);
    }
}

fn trim_if_large(path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() <= MAX_BYTES {
        return;
    }
    let Ok(contents) = fs::read(path) else {
        return;
    };
    let keep_from = contents.len() - (MAX_BYTES / 2) as usize;
    let start = contents[keep_from..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(contents.len(), |offset| keep_from + offset + 1);
    let _ = crate::util::write_atomic(path, &contents[start..], 0o644);
}

/// Facts about the machine that explain performance differences between runs.
pub fn environment(source_root: &Path, tree_root: &Path) -> Value {
    let kernel = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "kernel": kernel,
        "cpus": std::thread::available_parallelism().map(usize::from).ok(),
        "git": crate::discovery::program_version("git", &["version"]).ok(),
        "repo": crate::discovery::check_repo_version().ok(),
        "source_fs": filesystem_type(source_root),
        "tree_fs": filesystem_type(tree_root),
        "overlay_unavailable": crate::overlay::unavailable_reason(),
    })
}

/// Load average and, on Linux, pressure stall averages (percent of time some
/// or all tasks waited on CPU, I/O, or memory), so slow runs on a busy
/// machine can be told apart from slow tools.
pub fn system_load() -> Value {
    let loadavg = fs::read_to_string("/proc/loadavg").ok().map(|text| {
        text.split_whitespace()
            .take(3)
            .filter_map(|value| value.parse::<f64>().ok())
            .collect::<Vec<_>>()
    });
    let mut pressure = Map::new();
    for resource in ["cpu", "io", "memory"] {
        let Ok(text) = fs::read_to_string(format!("/proc/pressure/{resource}")) else {
            continue;
        };
        let mut lines = Map::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let Some(kind) = fields.next() else {
                continue;
            };
            let averages = fields
                .filter_map(|field| field.split_once('='))
                .filter(|(name, _)| name.starts_with("avg"))
                .filter_map(|(name, value)| {
                    Some((name.to_string(), json!(value.parse::<f64>().ok()?)))
                })
                .collect::<Map<_, _>>();
            lines.insert(kind.to_string(), Value::Object(averages));
        }
        pressure.insert(resource.to_string(), Value::Object(lines));
    }
    json!({
        "loadavg": loadavg,
        "pressure": (!pressure.is_empty()).then_some(Value::Object(pressure)),
    })
}

/// The type of the filesystem containing `path`, from the mount table.
pub fn filesystem_type(path: &Path) -> Option<String> {
    let path = fs::canonicalize(path).ok()?;
    mounts()
        .into_iter()
        .filter(|(mount_point, _)| path.starts_with(mount_point))
        .max_by_key(|(mount_point, _)| mount_point.components().count())
        .map(|(_, kind)| kind)
}

#[cfg(target_os = "linux")]
fn mounts() -> Vec<(PathBuf, String)> {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    mountinfo
        .lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let mount_point = left.split(' ').nth(4)?.replace("\\040", " ");
            let kind = right.split(' ').next()?;
            Some((PathBuf::from(mount_point), kind.to_string()))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn mounts() -> Vec<(PathBuf, String)> {
    let Ok(output) = std::process::Command::new("mount").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once(" on ")?;
            let (mount_point, options) = rest.rsplit_once(" (")?;
            let kind = options.split(',').next()?.trim_end_matches(')');
            Some((PathBuf::from(mount_point), kind.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_shape_keeps_only_command_and_flag_names() {
        let arguments = [
            "-C",
            "/secret/client",
            "add",
            "--json",
            "--name=private",
            "-b",
            "topic",
            "-p",
            "tools/idea",
            "/secret/tree",
        ]
        .map(String::from);
        assert_eq!(
            argument_shape(&arguments, &["add", "remove"]),
            vec!["-C", "add", "--json", "--name", "-b", "-p"]
        );
    }

    #[test]
    fn trimming_keeps_whole_recent_lines() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE_NAME);
        let line = format!("{}\n", "x".repeat(1023));
        let count = (MAX_BYTES / 1024 + 10) as usize;
        fs::write(&path, line.repeat(count)).unwrap();
        trim_if_large(&path);
        let trimmed = fs::read_to_string(&path).unwrap();
        assert!(trimmed.len() as u64 <= MAX_BYTES / 2);
        assert!(trimmed.lines().all(|kept| kept.len() == 1023));
    }
}
