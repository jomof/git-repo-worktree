use crate::error::{AppError, AppResult};
use crate::model::TreeMeta;
use crate::util::{absolutize, canonicalize_missing, read_meta};
use fd_lock::{RwLock, RwLockWriteGuard};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// `Shared` operations only read metadata, which is always replaced
/// atomically, so they take no lock. `Exclusive` operations wait for the
/// registry-wide lock, which is held only for short cross-tree steps.
#[derive(Clone, Copy, Debug)]
pub enum LockMode {
    Shared,
    Exclusive,
}

const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(300);
static LOCK_TIMEOUT: OnceLock<Duration> = OnceLock::new();
static COMMAND: OnceLock<String> = OnceLock::new();

/// Sets how long writers wait for a lock and the command name recorded for
/// other waiters. The first call wins.
pub fn configure(timeout: Option<Duration>, command: &str) {
    let timeout = timeout
        .or_else(|| {
            std::env::var("GIT_REPO_WORKTREE_LOCK_TIMEOUT")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs)
        })
        .unwrap_or(DEFAULT_LOCK_TIMEOUT);
    let _ = LOCK_TIMEOUT.set(timeout);
    let _ = COMMAND.set(command.to_string());
}

fn lock_timeout() -> Duration {
    *LOCK_TIMEOUT.get().unwrap_or(&DEFAULT_LOCK_TIMEOUT)
}

pub fn registry_path(source_root: &Path) -> PathBuf {
    source_root.join(".repo/repo-worktrees")
}

/// An exclusive advisory lock, released when dropped.
pub struct Held {
    _guard: RwLockWriteGuard<'static, File>,
}

/// Acquires the exclusive lock on `path`, waiting up to the configured
/// timeout. Prints who holds it while waiting unless `quiet`.
pub fn acquire(path: &Path, what: &str, quiet: bool) -> AppResult<Held> {
    acquire_within(path, what, quiet, lock_timeout())
}

pub fn try_acquire(path: &Path, what: &str) -> AppResult<Option<Held>> {
    match acquire_within(path, what, true, Duration::ZERO) {
        Ok(held) => Ok(Some(held)),
        Err(error) if error.code == "E_LOCKED" => Ok(None),
        Err(error) => Err(error),
    }
}

fn acquire_within(path: &Path, what: &str, quiet: bool, timeout: Duration) -> AppResult<Held> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o644)
        .open(path)
        .map_err(|error| AppError::io(format!("open lock {}", path.display()), error))?;
    let started = Instant::now();
    let mut next_notice = Duration::from_secs(1);
    loop {
        let attempt = file
            .try_clone()
            .map_err(|error| AppError::io(format!("open lock {}", path.display()), error))?;
        let lock: &'static mut RwLock<File> = Box::leak(Box::new(RwLock::new(attempt)));
        match lock.try_write() {
            Ok(mut guard) => {
                let waited = crate::metrics::elapsed_ms(started);
                if waited > 0 {
                    crate::metrics::phase("lock-wait", waited);
                }
                let holder = serde_json::json!({
                    "pid": std::process::id(),
                    "command": COMMAND.get().map(String::as_str).unwrap_or("?"),
                    "since": crate::util::now(),
                });
                let _ = guard.set_len(0);
                let _ = guard.seek(SeekFrom::Start(0));
                let _ = guard.write_all(holder.to_string().as_bytes());
                return Ok(Held { _guard: guard });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(AppError::io(format!("lock {}", path.display()), error)),
        }
        let waited = started.elapsed();
        if waited >= timeout {
            return Err(AppError::locked(
                "E_LOCKED",
                format!(
                    "timed out after {}s waiting for {what}{}; retry later or raise --lock-timeout",
                    waited.as_secs(),
                    describe_holder(path)
                ),
            ));
        }
        if !quiet && waited >= next_notice {
            eprintln!("waiting for {what}{}", describe_holder(path));
            next_notice = waited + Duration::from_secs(30);
        }
        std::thread::sleep(Duration::from_millis(100).min(timeout - waited));
    }
}

fn describe_holder(path: &Path) -> String {
    let mut contents = String::new();
    if File::open(path)
        .and_then(|mut file| file.read_to_string(&mut contents))
        .is_err()
    {
        return String::new();
    }
    let Ok(holder) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return String::new();
    };
    format!(
        " (held by pid {}: {} since {})",
        holder["pid"],
        holder["command"].as_str().unwrap_or("?"),
        holder["since"].as_str().unwrap_or("?")
    )
}

pub fn with_registry_lock<T>(
    source_root: &Path,
    mode: LockMode,
    create: bool,
    operation: impl FnOnce(&Path) -> AppResult<T>,
) -> AppResult<T> {
    let registry = registry_path(source_root);
    if matches!(mode, LockMode::Shared) {
        return operation(&registry);
    }
    if !registry.exists() {
        if !create {
            return operation(&registry);
        }
        fs::create_dir_all(&registry)
            .map_err(|error| AppError::io(format!("create {}", registry.display()), error))?;
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o755))
            .map_err(|error| AppError::io(format!("chmod {}", registry.display()), error))?;
    }
    let _held = acquire(&registry.join("lock"), "the registry lock", false)?;
    operation(&registry)
}

pub fn tree_lock_path(entry: &Path) -> PathBuf {
    entry.join("lock")
}

/// Whether another process holds the lock on `path`. Never waits, and never
/// creates the file.
pub fn is_locked(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    let mut lock = RwLock::new(file);
    let busy = lock.try_write().is_err();
    busy
}

/// Selects a tree, waits for its own lock, and runs `operation` with freshly
/// read metadata. Operations on different trees run concurrently.
pub fn with_tree_lock<T>(
    source_root: &Path,
    selector: &str,
    cwd: &Path,
    quiet: bool,
    operation: impl FnOnce(&Path, TreeMeta) -> AppResult<T>,
) -> AppResult<T> {
    let registry = registry_path(source_root);
    let selected = select_tree(&registry, selector, cwd)?;
    let (_held, fresh) = lock_tree(&selected, quiet)?;
    operation(&registry, fresh)
}

/// Locks a selected tree and re-reads its metadata. A tree removed while this
/// waited (its registry entry, lock file included, deleted) is
/// `E_NO_SUCH_TREE`.
pub fn lock_tree(selected: &TreeMeta, quiet: bool) -> AppResult<(Held, TreeMeta)> {
    let entry = selected.registry_entry();
    let removed = || {
        AppError::operational(
            "E_NO_SUCH_TREE",
            format!(
                "tree {} was removed while waiting for its lock",
                selected.name
            ),
        )
    };
    let held = acquire(
        &tree_lock_path(&entry),
        &format!("tree {}", selected.name),
        quiet,
    )
    .map_err(|error| if entry.is_dir() { error } else { removed() })?;
    let fresh = read_meta(&entry.join("meta.json")).map_err(|_| removed())?;
    Ok((held, fresh))
}

pub fn load_all(registry: &Path) -> AppResult<Vec<TreeMeta>> {
    if !registry.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(registry)
        .map_err(|error| AppError::io(format!("read {}", registry.display()), error))?
    {
        let entry =
            entry.map_err(|error| AppError::io(format!("read {}", registry.display()), error))?;
        if entry
            .file_type()
            .map_err(|error| AppError::io("inspect registry entry", error))?
            .is_dir()
        {
            let meta = entry.path().join("meta.json");
            if meta.is_file() {
                paths.push(meta);
            }
        }
    }
    paths.sort();
    let mut trees = Vec::with_capacity(paths.len());
    for path in paths {
        match read_meta(&path) {
            Ok(tree) => trees.push(tree),
            Err(_) if !path.exists() => {}
            Err(error) => return Err(error),
        }
    }
    Ok(trees)
}

/// Selects a tree by name or path, or the tree containing `cwd` when no
/// selector is given.
pub fn select_tree_or_current(
    registry: &Path,
    selector: Option<&str>,
    cwd: &Path,
) -> AppResult<TreeMeta> {
    if let Some(selector) = selector {
        return select_tree(registry, selector, cwd);
    }
    containing_tree(registry, cwd)?
        .ok_or_else(|| AppError::usage("name a tree, or run this command inside one"))
}

pub fn containing_tree(registry: &Path, cwd: &Path) -> AppResult<Option<TreeMeta>> {
    for tree in load_all(registry)? {
        let root = canonicalize_missing(Path::new(&tree.path))?;
        if cwd.starts_with(&root) {
            return Ok(Some(tree));
        }
    }
    Ok(None)
}

pub fn select_tree(registry: &Path, selector: &str, cwd: &Path) -> AppResult<TreeMeta> {
    let trees = load_all(registry)?;
    let looks_like_path = selector.contains('/') || selector == "." || selector == "..";
    if !looks_like_path {
        if let Some(tree) = trees.iter().find(|tree| tree.name == selector) {
            return Ok(tree.clone());
        }
    }
    let requested = canonicalize_missing(&absolutize(Path::new(selector), cwd))?;
    for tree in trees {
        let registered = canonicalize_missing(Path::new(&tree.path))?;
        if requested == registered {
            return Ok(tree);
        }
    }
    Err(AppError::operational(
        "E_NO_SUCH_TREE",
        format!("no registered tree matches {selector}"),
    ))
}
