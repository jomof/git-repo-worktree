use crate::error::{AppError, AppResult};
use crate::model::TreeMeta;
use crate::util::{absolutize, canonicalize_missing, read_meta};
use fd_lock::RwLock;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub enum LockMode {
    Shared,
    Exclusive,
}

pub fn registry_path(source_root: &Path) -> PathBuf {
    source_root.join(".repo/repo-worktrees")
}

pub fn with_registry_lock<T>(
    source_root: &Path,
    mode: LockMode,
    create: bool,
    operation: impl FnOnce(&Path) -> AppResult<T>,
) -> AppResult<T> {
    let registry = registry_path(source_root);
    if !registry.exists() {
        if !create {
            return operation(&registry);
        }
        fs::create_dir_all(&registry)
            .map_err(|error| AppError::io(format!("create {}", registry.display()), error))?;
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o755))
            .map_err(|error| AppError::io(format!("chmod {}", registry.display()), error))?;
    }
    let lock_path = registry.join("lock");
    let file = open_lock_file(&lock_path, create)?;
    let mut lock = RwLock::new(file);
    match mode {
        LockMode::Shared => {
            let _guard = lock.try_read().map_err(lock_error)?;
            operation(&registry)
        }
        LockMode::Exclusive => {
            let _guard = lock.try_write().map_err(lock_error)?;
            operation(&registry)
        }
    }
}

fn open_lock_file(path: &Path, create: bool) -> AppResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create).mode(0o644);
    options
        .open(path)
        .map_err(|error| AppError::io(format!("open registry lock {}", path.display()), error))
}

fn lock_error(error: std::io::Error) -> AppError {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        AppError::locked(
            "E_LOCKED",
            "another git-repo-worktree process holds the registry lock; retry shortly",
        )
    } else {
        AppError::io("acquire registry lock", error)
    }
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
    paths.iter().map(|path| read_meta(path)).collect()
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
