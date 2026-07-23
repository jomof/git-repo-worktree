use crate::cli::{
    AddArgs, ListArgs, LockArgs, MoveArgs, PruneArgs, RemoveArgs, RepairArgs, UnlockArgs,
};
use crate::discovery::{check_git_version, check_repo_version, discover_client};
use crate::error::{AppError, AppResult};
use crate::manifest::{parse_snapshot_file, project_depth};
use crate::model::{DirectiveMeta, LockMeta, MovingMeta, ProjectMeta, TreeMeta, FORMAT_VERSION};
use crate::registry::{load_all, select_tree, with_registry_lock, LockMode};
use crate::util::{
    absolutize, append_journal, append_journal_batch, canonicalize_missing, copy_permissions,
    dedupe_paths, default_jobs, ensure_safe_destination, exit_reason, is_path_prefix,
    journal_has_done, journal_record, now, path_is_safe_relative, relative_path, sanitize_name,
    truncated_stderr, write_breadcrumb, write_meta,
};
use crate::INTERRUPTED;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const JOURNAL_BATCH_WINDOW: Duration = Duration::from_millis(10);

fn phase_start(quiet: bool, verbose: u8, phase: &str) {
    if !quiet && verbose > 0 {
        eprintln!("phase {phase}: starting");
    }
}

fn phase_done(quiet: bool, verbose: u8, phase: &str, started: Instant, detail: Option<&str>) {
    if quiet || verbose == 0 {
        return;
    }
    let elapsed = format_elapsed(started.elapsed());
    match detail {
        Some(detail) => eprintln!("phase {phase}: {elapsed} ({detail})"),
        None => eprintln!("phase {phase}: {elapsed}"),
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_secs() == 0 {
        format!("{}ms", elapsed.as_millis())
    } else {
        format!("{:.3}s", elapsed.as_secs_f64())
    }
}

pub fn add(args: &AddArgs, cwd: &Path, quiet: bool, verbose: u8) -> AppResult<Value> {
    let operation_started = Instant::now();
    let jobs = args.jobs.unwrap_or_else(default_jobs);
    if args.resume
        && (args.name.is_some()
            || args.branch.is_some()
            || args.submodules
            || args.lock
            || args.reason.is_some())
    {
        return Err(AppError::usage(
            "--name, -b, --submodules, --lock, and --reason are invalid with --continue",
        ));
    }
    let selector = args.path.as_deref().ok_or_else(|| {
        AppError::usage(if args.resume {
            "--continue requires a tree selector or path"
        } else {
            "add requires a destination path"
        })
    })?;
    check_git_version()?;
    let client = discover_client(cwd, false)?;
    if args.resume {
        return with_registry_lock(&client.root, LockMode::Exclusive, false, |registry| {
            let resume_started = Instant::now();
            phase_start(quiet, verbose, "resume");
            let mut meta = select_tree(registry, selector, cwd)?;
            if meta.state != "creating" && meta.state != "incomplete" {
                return Err(AppError::operational(
                    "E_NOT_RESUMABLE",
                    format!("tree {} is in state {}", meta.name, meta.state),
                ));
            }
            let entry = meta.registry_entry();
            if !journal_has_done(&entry, "pin")? {
                return Err(AppError::operational(
                    "E_NOT_RESUMABLE",
                    format!(
                        "tree {} was interrupted before a snapshot was pinned; remove it and run add again",
                        meta.name
                    ),
                ));
            }
            if meta.projects.is_empty() && meta.copyfiles.is_empty() && meta.linkfiles.is_empty() {
                let parsed =
                    parse_snapshot_file(&meta.source(), &entry.join("snapshot.xml"), jobs)?;
                meta.projects = parsed.projects;
                meta.copyfiles = parsed.copyfiles;
                meta.linkfiles = parsed.linkfiles;
                write_meta(&meta)?;
            }
            if !meta.root().exists() {
                fs::create_dir_all(meta.root()).map_err(|error| {
                    AppError::io(
                        format!("create destination {}", meta.root().display()),
                        error,
                    )
                })?;
                fs::set_permissions(meta.root(), fs::Permissions::from_mode(0o755)).map_err(
                    |error| AppError::io(format!("chmod {}", meta.root().display()), error),
                )?;
            }
            write_breadcrumb(&meta)?;
            if !journal_has_done(&entry, "mkdir-root")? {
                append_journal(&entry, "mkdir-root", "done", None, None, None)?;
            }
            phase_done(quiet, verbose, "resume", resume_started, None);
            run_add_phases(
                &mut meta,
                jobs,
                args.fail_fast,
                quiet,
                verbose,
                operation_started,
            )
        });
    }

    check_repo_version()?;
    with_registry_lock(&client.root, LockMode::Exclusive, true, |registry| {
        check_quiescence(&client.root, args.force, quiet)?;
        let destination = canonicalize_missing(&absolutize(Path::new(selector), cwd))?;
        validate_destination(&destination, &client.root, &load_all(registry)?)?;
        let name =
            match &args.name {
                Some(name) if valid_name(name) => name.clone(),
                Some(_) => return Err(AppError::operational(
                    "E_BAD_NAME",
                    "explicit tree names may contain only ASCII letters, digits, '.', '_', and '-'",
                )),
                None => destination
                    .file_name()
                    .map(sanitize_name)
                    .unwrap_or_default(),
            };
        if name.is_empty() {
            return Err(AppError::operational(
                "E_BAD_NAME",
                "tree name is empty after sanitization",
            ));
        }
        let entry = registry.join(&name);
        if entry.exists() {
            return Err(AppError::operational(
                "E_NAME_EXISTS",
                format!("a tree named {name} already exists; pass --name or use add --continue"),
            ));
        }
        fs::create_dir(&entry)
            .map_err(|error| AppError::io(format!("create {}", entry.display()), error))?;
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o755))
            .map_err(|error| AppError::io(format!("chmod {}", entry.display()), error))?;
        let mut meta = TreeMeta {
            format_version: FORMAT_VERSION,
            tool_version: env!("CARGO_PKG_VERSION").into(),
            name,
            path: destination.to_string_lossy().into_owned(),
            source_root: client.root.to_string_lossy().into_owned(),
            source_layout: client.layout.clone(),
            created_at: now(),
            state: "creating".into(),
            branch: args.branch.clone(),
            submodules: args.submodules,
            locked: args.lock.then(|| LockMeta {
                reason: args.reason.clone().unwrap_or_default(),
                at: now(),
            }),
            moving: None,
            projects: Vec::new(),
            copyfiles: Vec::new(),
            linkfiles: Vec::new(),
        };
        write_meta(&meta)?;

        let snapshot_started = Instant::now();
        phase_start(quiet, verbose, "snapshot");
        let snapshot = entry.join("snapshot.xml");
        let output = match Command::new("repo")
            .current_dir(&client.root)
            .args(["manifest", "-r", "-o"])
            .arg(&snapshot)
            .output()
        {
            Ok(output) => output,
            Err(error) => {
                let _ = fs::remove_dir_all(&entry);
                return Err(AppError::operational(
                    "E_REPO_MISSING",
                    format!("repo is not available on PATH: {error}"),
                ));
            }
        };
        if !output.status.success() {
            let error = crate::manifest::snapshot_command_failure(&output);
            let _ = fs::remove_dir_all(&entry);
            return Err(error);
        }
        let parsed = match parse_snapshot_file(&client.root, &snapshot, jobs) {
            Ok(parsed) => parsed,
            Err(error) => {
                let _ = fs::remove_dir_all(&entry);
                return Err(error);
            }
        };
        meta.projects = parsed.projects;
        meta.copyfiles = parsed.copyfiles;
        meta.linkfiles = parsed.linkfiles;
        append_journal(&entry, "pin", "done", None, None, None)?;
        write_meta(&meta)?;
        phase_done(
            quiet,
            verbose,
            "snapshot",
            snapshot_started,
            Some(&format!("{} projects", meta.projects.len())),
        );

        fs::create_dir_all(&destination).map_err(|error| {
            AppError::io(
                format!("create destination {}", destination.display()),
                error,
            )
        })?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
            .map_err(|error| AppError::io(format!("chmod {}", destination.display()), error))?;
        write_breadcrumb(&meta)?;
        append_journal(&entry, "mkdir-root", "done", None, None, None)?;

        if !quiet {
            for project in meta
                .projects
                .iter()
                .filter(|project| project.state == "skipped")
            {
                if verbose > 0 {
                    eprintln!(
                        "warning: skipped {} ({})",
                        project.path,
                        project.reason.as_deref().unwrap_or("unknown")
                    );
                }
            }
            if verbose == 0 {
                let skipped = meta.skipped_count();
                if skipped > 0 {
                    eprintln!("warning: {skipped} snapshot projects were skipped");
                }
            }
            if !parsed.orphan_checkouts.is_empty() {
                eprintln!(
                    "warning: {} checkout(s) are absent from the snapshot manifest",
                    parsed.orphan_checkouts.len()
                );
            }
        }
        run_add_phases(
            &mut meta,
            jobs,
            args.fail_fast,
            quiet,
            verbose,
            operation_started,
        )
    })
}

fn run_add_phases(
    meta: &mut TreeMeta,
    jobs: usize,
    fail_fast: bool,
    quiet: bool,
    verbose: u8,
    operation_started: Instant,
) -> AppResult<Value> {
    let entry = meta.registry_entry();
    let source = meta.source();
    let destination = meta.root();
    let branch = meta.branch.clone();
    let pool = ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|error| AppError::io("create worker pool", error))?;
    let project_started = Instant::now();
    let project_count = meta
        .projects
        .iter()
        .filter(|project| project.state != "done" && project.state != "skipped")
        .count();
    if !quiet && verbose > 0 {
        eprintln!("phase projects: starting {project_count} project(s) with {jobs} job(s)");
    }
    run_project_phase(
        meta,
        &pool,
        &source,
        &destination,
        branch.as_deref(),
        jobs,
        fail_fast,
        quiet,
        verbose,
    )?;
    phase_done(
        quiet,
        verbose,
        "projects",
        project_started,
        Some(&format!("{project_count} candidates")),
    );
    if INTERRUPTED.load(Ordering::SeqCst) {
        meta.state = "incomplete".into();
        write_meta(meta)?;
        return Err(AppError::partial(
            serde_json::to_value(&*meta).expect("tree metadata serializes"),
        ));
    }

    let directive_started = Instant::now();
    phase_start(quiet, verbose, "directives");
    process_directives(meta, &pool, quiet)?;
    phase_done(
        quiet,
        verbose,
        "directives",
        directive_started,
        Some(&format!(
            "{} copy/link directives",
            meta.copyfiles.len() + meta.linkfiles.len()
        )),
    );
    let submodule_started = Instant::now();
    phase_start(quiet, verbose, "submodules");
    process_submodules(meta, &pool, quiet)?;
    phase_done(quiet, verbose, "submodules", submodule_started, None);
    if meta.locked.is_some() {
        let lock_started = Instant::now();
        phase_start(quiet, verbose, "lock");
        let warnings = lock_existing_projects(meta, jobs)?;
        if warnings > 0 && !quiet {
            eprintln!("warning: {warnings} project worktree lock(s) could not be applied");
        }
        phase_done(quiet, verbose, "lock", lock_started, None);
    }
    let finalize_started = Instant::now();
    phase_start(quiet, verbose, "finalize");
    meta.state = if meta.is_complete() {
        "complete".into()
    } else {
        "incomplete".into()
    };
    append_journal(&entry, "finalize", "done", None, None, None)?;
    write_meta(meta)?;
    phase_done(quiet, verbose, "finalize", finalize_started, None);
    if !quiet {
        let failed = meta
            .projects
            .iter()
            .filter(|project| project.state != "done" && project.state != "skipped")
            .count()
            + meta
                .copyfiles
                .iter()
                .chain(meta.linkfiles.iter())
                .filter(|directive| directive.state != "done")
                .count();
        eprintln!(
            "created {} at {}: {} projects, {} skipped, {} failed",
            meta.name,
            meta.path,
            meta.done_count(),
            meta.skipped_count(),
            failed
        );
    }
    phase_done(quiet, verbose, "total", operation_started, None);
    let result = serde_json::to_value(&*meta).expect("tree metadata serializes");
    if meta.state == "incomplete" {
        Err(AppError::partial(result))
    } else {
        Ok(result)
    }
}

#[derive(Debug)]
struct ProjectSchedule {
    ready: VecDeque<usize>,
    children: Vec<Vec<usize>>,
    work_count: usize,
}

fn build_project_schedule(projects: &[ProjectMeta]) -> ProjectSchedule {
    let unfinished = projects
        .iter()
        .enumerate()
        .filter(|(_, project)| project.state != "done" && project.state != "skipped")
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let unfinished_by_path = unfinished
        .iter()
        .map(|index| (PathBuf::from(&projects[*index].path), *index))
        .collect::<HashMap<_, _>>();
    let mut children = vec![Vec::new(); projects.len()];
    let mut ready = VecDeque::new();
    for &child_index in &unfinished {
        let child_path = Path::new(&projects[child_index].path);
        let mut ancestor = child_path.parent();
        let mut parent = None;
        while let Some(path) = ancestor {
            if path.as_os_str().is_empty() {
                break;
            }
            if let Some(parent_index) = unfinished_by_path.get(path) {
                parent = Some(*parent_index);
                break;
            }
            ancestor = path.parent();
        }
        if let Some(parent_index) = parent {
            children[parent_index].push(child_index);
        } else {
            ready.push_back(child_index);
        }
    }
    ProjectSchedule {
        ready,
        children,
        work_count: unfinished.len(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_project_phase(
    meta: &mut TreeMeta,
    pool: &rayon::ThreadPool,
    source: &Path,
    destination: &Path,
    branch: Option<&str>,
    jobs: usize,
    fail_fast: bool,
    quiet: bool,
    verbose: u8,
) -> AppResult<()> {
    let ProjectSchedule {
        mut ready,
        children,
        work_count,
    } = build_project_schedule(&meta.projects);
    let total = meta.total_non_skipped();
    let mut settled = total.saturating_sub(work_count);
    let mut processed = 0usize;
    let mut running = 0usize;
    let mut stop = false;
    let mut completed_for_hook = 0usize;
    let (sender, receiver) = mpsc::channel::<(usize, Result<(), String>)>();

    while processed < work_count {
        if INTERRUPTED.load(Ordering::SeqCst) {
            stop = true;
        }
        while !stop && running < jobs {
            let Some(index) = ready.pop_front() else {
                break;
            };
            let project = meta.projects[index].clone();
            let source = source.to_path_buf();
            let destination = destination.to_path_buf();
            let branch = branch.map(str::to_owned);
            let sender = sender.clone();
            pool.spawn(move || {
                let result = process_project(&project, &source, &destination, branch.as_deref());
                let _ = sender.send((index, result));
            });
            running += 1;
        }
        if running == 0 {
            break;
        }

        let first = receiver
            .recv()
            .map_err(|error| AppError::io("receive project completion from worker pool", error))?;
        let mut batch = vec![first];
        let deadline = Instant::now() + JOURNAL_BATCH_WINDOW;
        while batch.len() < running {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match receiver.recv_timeout(remaining) {
                Ok(completion) => batch.push(completion),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(error) => {
                    return Err(AppError::io(
                        "receive project completion from worker pool",
                        error,
                    ));
                }
            }
        }
        running -= batch.len();

        let records = batch
            .iter()
            .map(|(index, result)| {
                let path = &meta.projects[*index].path;
                match result {
                    Ok(()) => journal_record("project-add", "done", Some(path), None, None),
                    Err(reason) => {
                        journal_record("project-add", "failed", Some(path), None, Some(reason))
                    }
                }
            })
            .collect::<Vec<_>>();
        append_journal_batch(&meta.registry_entry(), &records)?;

        for (index, result) in batch {
            processed += 1;
            settled += 1;
            let project_path = meta.projects[index].path.clone();
            match result {
                Ok(()) => {
                    meta.projects[index].state = "done".into();
                    meta.projects[index].reason = None;
                    completed_for_hook += 1;
                    maybe_crash_after(completed_for_hook);
                    if verbose > 0 && !quiet {
                        eprintln!("project {settled}/{total}: added {project_path}");
                    }
                }
                Err(reason) => {
                    meta.projects[index].state = "failed".into();
                    meta.projects[index].reason = Some(reason.clone());
                    if !quiet {
                        eprintln!(
                            "warning: project {settled}/{total} failed {project_path}: {reason}"
                        );
                    }
                    if fail_fast {
                        stop = true;
                    }
                }
            }
            for &child in &children[index] {
                ready.push_back(child);
            }
        }
        write_meta(meta)?;
    }

    if !stop && processed != work_count {
        return Err(AppError::operational(
            "E_SCHEDULE",
            "project dependency scheduler could not make progress",
        ));
    }
    Ok(())
}

fn process_project(
    project: &ProjectMeta,
    source_root: &Path,
    destination: &Path,
    branch: Option<&str>,
) -> Result<(), String> {
    let worktree = destination.join(&project.path);
    if worktree.join(".git").exists() {
        if let Ok(output) = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["rev-parse", "HEAD"])
            .output()
        {
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == project.sha
            {
                return Ok(());
            }
        }
    }
    if let Some(parent) = worktree.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let source_checkout = source_root.join(&project.path);
    let mut command = Command::new("git");
    if source_checkout.exists() {
        command.arg("-C").arg(&source_checkout);
    } else {
        command.arg(format!("--git-dir={}", project.git_common_dir));
    }
    command.args(["worktree", "add"]);
    if let Some(branch) = branch {
        command.arg("-b").arg(branch);
    } else {
        command.arg("--detach");
    }
    let output = command
        .arg(&worktree)
        .arg(&project.sha)
        .output()
        .map_err(|error| format!("run git worktree add: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = truncated_stderr(&output);
        if branch.is_some()
            && (stderr.contains("already exists") || stderr.contains("already checked out"))
        {
            Err("branch-exists".into())
        } else {
            Err(exit_reason(&output))
        }
    }
}

fn process_directives(meta: &mut TreeMeta, pool: &rayon::ThreadPool, quiet: bool) -> AppResult<()> {
    let done_projects = meta
        .projects
        .iter()
        .filter(|project| project.state == "done")
        .map(|project| project.path.clone())
        .collect::<HashSet<_>>();
    let root = meta.root();
    let entry = meta.registry_entry();
    let copy_work = meta
        .copyfiles
        .iter()
        .enumerate()
        .filter(|(_, directive)| {
            directive.reason.as_deref() != Some("malformed")
                && done_projects.contains(&directive.project)
                && directive.state != "done"
        })
        .map(|(index, directive)| (index, directive.clone()))
        .collect::<Vec<_>>();
    let copy_results = pool.install(|| {
        copy_work
            .par_iter()
            .map(|(index, directive)| (*index, apply_copy(&root, directive, false)))
            .collect::<Vec<_>>()
    });
    let copy_records = copy_results
        .iter()
        .map(|(index, result)| {
            directive_journal_record("copyfile", &meta.copyfiles[*index], result)
        })
        .collect::<Vec<_>>();
    append_journal_batch(&entry, &copy_records)?;
    for (index, result) in copy_results {
        apply_directive_result(&mut meta.copyfiles[index], result, "copyfile", quiet);
    }

    let link_work = meta
        .linkfiles
        .iter()
        .enumerate()
        .filter(|(_, directive)| {
            directive.reason.as_deref() != Some("malformed")
                && done_projects.contains(&directive.project)
                && directive.state != "done"
        })
        .map(|(index, directive)| (index, directive.clone()))
        .collect::<Vec<_>>();
    if !quiet {
        for (_, directive) in &link_work {
            if !root.join(&directive.project).join(&directive.src).exists() {
                eprintln!(
                    "warning: dangling-link {} -> {}/{}",
                    directive.dest, directive.project, directive.src
                );
            }
        }
    }
    let link_results = pool.install(|| {
        link_work
            .par_iter()
            .map(|(index, directive)| (*index, apply_link(&root, directive, false)))
            .collect::<Vec<_>>()
    });
    let link_records = link_results
        .iter()
        .map(|(index, result)| {
            directive_journal_record("linkfile", &meta.linkfiles[*index], result)
        })
        .collect::<Vec<_>>();
    append_journal_batch(&entry, &link_records)?;
    for (index, result) in link_results {
        apply_directive_result(&mut meta.linkfiles[index], result, "linkfile", quiet);
    }
    write_meta(meta)
}

fn directive_journal_record(
    step: &str,
    directive: &DirectiveMeta,
    result: &Result<(), String>,
) -> crate::model::JournalRecord {
    match result {
        Ok(()) => journal_record(
            step,
            "done",
            Some(&directive.project),
            Some(&directive.dest),
            None,
        ),
        Err(reason) => journal_record(
            step,
            "failed",
            Some(&directive.project),
            Some(&directive.dest),
            Some(reason),
        ),
    }
}

fn apply_directive_result(
    directive: &mut DirectiveMeta,
    result: Result<(), String>,
    step: &str,
    quiet: bool,
) {
    match result {
        Ok(()) => {
            directive.state = "done".into();
            directive.reason = None;
        }
        Err(reason) => {
            directive.state = "failed".into();
            directive.reason = Some(reason.clone());
            if !quiet {
                eprintln!("warning: {step} {} failed: {reason}", directive.dest);
            }
        }
    }
}

fn directive_paths(root: &Path, directive: &DirectiveMeta) -> Result<(PathBuf, PathBuf), String> {
    let src = Path::new(&directive.src);
    let dest = Path::new(&directive.dest);
    if !path_is_safe_relative(src) || !path_is_safe_relative(dest) {
        return Err("unsafe-path".into());
    }
    let source = root.join(&directive.project).join(src);
    let destination = ensure_safe_destination(root, dest).map_err(|_| "unsafe-path".to_string())?;
    Ok((source, destination))
}

fn apply_copy(root: &Path, directive: &DirectiveMeta, repair: bool) -> Result<(), String> {
    let (source, destination) = directive_paths(root, directive)?;
    let metadata = fs::metadata(&source).map_err(|_| "src-missing".to_string())?;
    if !metadata.is_file() {
        return Err("src-not-file".into());
    }
    if destination.exists() {
        if repair {
            return Ok(());
        }
        if let Ok(dest_metadata) = fs::metadata(&destination) {
            if dest_metadata.len() == metadata.len()
                && dest_metadata.modified().ok() >= metadata.modified().ok()
            {
                return Ok(());
            }
        }
    }
    fs::copy(&source, &destination).map_err(|error| format!("copy failed: {error}"))?;
    copy_permissions(&source, &destination).map_err(|error| error.message)?;
    Ok(())
}

fn apply_link(root: &Path, directive: &DirectiveMeta, repair: bool) -> Result<(), String> {
    let (source, destination) = directive_paths(root, directive)?;
    let parent = destination
        .parent()
        .ok_or_else(|| "unsafe-path".to_string())?;
    let target = relative_path(parent, &source);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current =
                fs::read_link(&destination).map_err(|error| format!("readlink failed: {error}"))?;
            if current == target {
                return Ok(());
            }
            if !repair {
                return Err("dest-occupied".into());
            }
            fs::remove_file(&destination)
                .map_err(|error| format!("replace link failed: {error}"))?;
        }
        Ok(metadata) => {
            if !repair || metadata.is_dir() {
                return Err("dest-occupied".into());
            }
            fs::remove_file(&destination)
                .map_err(|error| format!("replace destination failed: {error}"))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect destination failed: {error}")),
    }
    symlink(&target, &destination).map_err(|error| format!("symlink failed: {error}"))
}

fn process_submodules(meta: &mut TreeMeta, pool: &rayon::ThreadPool, quiet: bool) -> AppResult<()> {
    let root = meta.root();
    let candidates = meta
        .projects
        .iter()
        .enumerate()
        .filter(|(_, project)| {
            project.state == "done" && root.join(&project.path).join(".gitmodules").is_file()
        })
        .map(|(index, project)| (index, project.clone()))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(());
    }
    if !meta.submodules {
        if !quiet {
            let names = candidates
                .iter()
                .map(|(_, project)| project.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "warning: {} projects contain .gitmodules and were not populated\n         ({names}); use --submodules to populate",
                candidates.len()
            );
        }
        return Ok(());
    }
    let results = pool.install(|| {
        candidates
            .par_iter()
            .map(|(index, project)| {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(root.join(&project.path))
                    .args(["submodule", "update", "--init", "--recursive"])
                    .output();
                let result = match output {
                    Ok(output) if output.status.success() => Ok(()),
                    Ok(output) => Err(exit_reason(&output)),
                    Err(error) => Err(format!("run git submodule: {error}")),
                };
                (*index, project.path.clone(), result)
            })
            .collect::<Vec<_>>()
    });
    let records = results
        .iter()
        .map(|(_, path, result)| match result {
            Ok(()) => journal_record("submodule", "done", Some(path), None, None),
            Err(reason) => journal_record("submodule", "failed", Some(path), None, Some(reason)),
        })
        .collect::<Vec<_>>();
    append_journal_batch(&meta.registry_entry(), &records)?;
    for (index, path, result) in results {
        match result {
            Ok(()) => {}
            Err(reason) => {
                meta.projects[index].state = "failed".into();
                meta.projects[index].reason = Some(format!("submodule: {reason}"));
                if !quiet {
                    eprintln!("warning: submodule {path} failed: {reason}");
                }
            }
        }
    }
    write_meta(meta)
}

fn maybe_crash_after(_completed: usize) {
    #[cfg(feature = "test-hooks")]
    if std::env::var("GRW_TEST_CRASH_AFTER")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        == Some(_completed)
    {
        std::process::abort();
    }
}

fn check_quiescence(source_root: &Path, force: bool, quiet: bool) -> AppResult<()> {
    let lock_names = [
        "index.lock",
        "packed-refs.lock",
        "HEAD.lock",
        "shallow.lock",
    ];
    let detected_lock = lock_names
        .iter()
        .map(|name| source_root.join(".repo/manifests.git").join(name))
        .find(|path| path.exists());
    let detected_process = if detected_lock.is_none() {
        Command::new("ps")
            .args(["-Ao", "args="])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .find(|line| {
                        let normalized = line.replace('/', " ");
                        normalized.contains("repo sync") || normalized.contains("repo init")
                    })
                    .map(str::to_owned)
            })
    } else {
        None
    };
    if detected_lock.is_none() && detected_process.is_none() {
        return Ok(());
    }
    let detail = detected_lock
        .as_ref()
        .map(|path| {
            path.strip_prefix(source_root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .or(detected_process)
        .unwrap_or_else(|| "repo process".into());
    if force {
        if !quiet {
            eprintln!("warning: repo sync appears active ({detail}); proceeding due to --force");
        }
        Ok(())
    } else {
        Err(AppError::locked(
            "E_SYNC_ACTIVE",
            format!("repo sync appears to be active ({detail}); use --force to proceed"),
        ))
    }
}

fn validate_destination(
    destination: &Path,
    source_root: &Path,
    registered: &[TreeMeta],
) -> AppResult<()> {
    if destination.exists()
        && (!destination.is_dir()
            || fs::read_dir(destination)
                .map_err(|error| {
                    AppError::io(format!("read destination {}", destination.display()), error)
                })?
                .next()
                .is_some())
    {
        return Err(AppError::operational(
            "E_DEST_NOT_EMPTY",
            format!("destination is not empty: {}", destination.display()),
        ));
    }
    let source = canonicalize_missing(source_root)?;
    let destination = canonicalize_missing(destination)?;
    if is_path_prefix(&source, &destination) || is_path_prefix(&destination, &source) {
        return Err(AppError::operational(
            "E_DEST_NESTED",
            "destination and source client must not contain one another",
        ));
    }
    for tree in registered {
        let path = canonicalize_missing(Path::new(&tree.path))?;
        if is_path_prefix(&path, &destination) {
            return Err(AppError::operational(
                "E_DEST_NESTED",
                format!("destination is inside registered tree {}", tree.name),
            ));
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub fn list(args: &ListArgs, cwd: &Path, json_output: bool, verbose: u8) -> AppResult<Value> {
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Shared, false, |registry| {
        let trees = load_all(registry)?;
        if !json_output {
            if args.porcelain {
                print_porcelain(&trees, registry, args.nul)?;
            } else {
                for tree in &trees {
                    let locked = tree.locked.as_ref().map(|_| "  locked").unwrap_or("");
                    println!(
                        "{}  {}  {}/{} projects{}",
                        tree.path,
                        tree.state,
                        tree.done_count(),
                        tree.total_non_skipped(),
                        locked
                    );
                    if verbose > 0 {
                        let failed = tree
                            .projects
                            .iter()
                            .filter(|project| project.state == "failed")
                            .map(|project| project.path.as_str())
                            .collect::<Vec<_>>()
                            .join(",");
                        println!(
                            "  name={} created_at={} snapshot={} failed={}",
                            tree.name,
                            tree.created_at,
                            registry.join(&tree.name).join("snapshot.xml").display(),
                            failed
                        );
                    }
                }
            }
        }
        Ok(json!({"trees": trees}))
    })
}

fn print_porcelain(trees: &[TreeMeta], registry: &Path, nul: bool) -> AppResult<()> {
    let separator = if nul { "\0" } else { "\n" };
    let mut output = String::new();
    for tree in trees {
        let mut lines = vec![
            format!("worktree {}", tree.path),
            format!("name {}", tree.name),
            format!("state {}", tree.state),
            format!(
                "snapshot {}",
                registry.join(&tree.name).join("snapshot.xml").display()
            ),
            format!("projects {}", tree.done_count()),
            format!("skipped {}", tree.skipped_count()),
            format!("failed {}", tree.failed_count()),
        ];
        if let Some(locked) = &tree.locked {
            lines.push(format!("locked {}", locked.reason));
        }
        if !tree.root().exists() {
            lines.push("prunable path missing".into());
        }
        output.push_str(&lines.join(separator));
        output.push_str(separator);
        output.push_str(separator);
    }
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(output.as_bytes())
        .map_err(|error| AppError::io("write porcelain output", error))
}

pub fn remove(args: &RemoveArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    check_git_version()?;
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Exclusive, false, |registry| {
        let mut meta = select_tree(registry, &args.tree, cwd)?;
        if meta.state != "removing" {
            if args.force == 0 {
                let dirty = dirty_projects(&meta)?;
                if !dirty.is_empty() {
                    let shown = dirty.iter().take(10).cloned().collect::<Vec<_>>();
                    let extra = dirty.len().saturating_sub(shown.len());
                    return Err(AppError::operational(
                        "E_DIRTY",
                        format!(
                            "tree has dirty projects: {}{}",
                            shown.join(", "),
                            if extra > 0 {
                                format!(" (+{extra} more)")
                            } else {
                                String::new()
                            }
                        ),
                    ));
                }
            }
            if let Some(lock) = &meta.locked {
                if args.force < 2 {
                    return Err(AppError::operational(
                        "E_TREE_LOCKED",
                        format!(
                            "tree is locked ({}); pass --force --force to remove",
                            lock.reason
                        ),
                    ));
                }
            }
            meta.state = "removing".into();
            write_meta(&meta)?;
        }
        let pool = ThreadPoolBuilder::new()
            .num_threads(args.jobs.unwrap_or_else(default_jobs))
            .build()
            .map_err(|error| AppError::io("create worker pool", error))?;
        let mut projects = meta.projects.clone();
        projects.sort_by_key(|project| std::cmp::Reverse(project_depth(&project.path)));
        let mut start = 0;
        while start < projects.len() {
            let depth = project_depth(&projects[start].path);
            let end = projects[start..]
                .iter()
                .position(|project| project_depth(&project.path) != depth)
                .map(|offset| start + offset)
                .unwrap_or(projects.len());
            pool.install(|| {
                projects[start..end]
                    .par_iter()
                    .filter(|project| {
                        meta.root().join(&project.path).exists()
                            && !project.git_common_dir.is_empty()
                    })
                    .for_each(|project| {
                        let _ = Command::new("git")
                            .arg(format!("--git-dir={}", project.git_common_dir))
                            .args(["worktree", "remove", "--force"])
                            .arg(meta.root().join(&project.path))
                            .output();
                    });
            });
            start = end;
        }
        if meta.root().exists() {
            fs::remove_dir_all(meta.root()).map_err(|error| {
                AppError::io(format!("remove tree {}", meta.root().display()), error)
            })?;
        }
        let gitdirs = dedupe_paths(
            meta.projects
                .iter()
                .filter(|project| !project.git_common_dir.is_empty())
                .map(|project| &project.git_common_dir),
        );
        pool.install(|| {
            gitdirs.par_iter().for_each(|gitdir| {
                let _ = Command::new("git")
                    .arg(format!("--git-dir={gitdir}"))
                    .args(["worktree", "prune"])
                    .output();
            });
        });
        let entry = meta.registry_entry();
        if entry.exists() {
            fs::remove_dir_all(&entry)
                .map_err(|error| AppError::io(format!("remove {}", entry.display()), error))?;
        }
        if !quiet {
            eprintln!("removed {} ({})", meta.name, meta.path);
        }
        Ok(json!({"name": meta.name, "path": meta.path, "removed": true}))
    })
}

fn dirty_projects(meta: &TreeMeta) -> AppResult<Vec<String>> {
    let mut dirty = Vec::new();
    for project in meta
        .projects
        .iter()
        .filter(|project| project.state == "done")
    {
        let path = meta.root().join(&project.path);
        if !path.exists() {
            continue;
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["status", "--porcelain", "--untracked-files=normal"])
            .output()
            .map_err(|error| AppError::io(format!("inspect {}", path.display()), error))?;
        if !output.status.success() {
            return Err(AppError::operational(
                "E_IO",
                format!(
                    "git status failed for {}: {}",
                    project.path,
                    truncated_stderr(&output)
                ),
            ));
        }
        if !output.stdout.is_empty() {
            dirty.push(project.path.clone());
        }
    }
    Ok(dirty)
}

pub fn lock(args: &LockArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    check_git_version()?;
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Exclusive, false, |registry| {
        let mut meta = select_tree(registry, &args.tree, cwd)?;
        if meta.locked.is_some() {
            return Err(AppError::operational(
                "E_ALREADY_LOCKED",
                format!("tree {} is already locked", meta.name),
            ));
        }
        let warnings = apply_lock(&mut meta, &args.reason, args.jobs)?;
        write_meta(&meta)?;
        if !quiet {
            eprintln!("locked {}", meta.name);
        }
        Ok(json!({"name": meta.name, "locked": true, "project_warnings": warnings}))
    })
}

fn apply_lock(meta: &mut TreeMeta, reason: &str, jobs: Option<usize>) -> AppResult<usize> {
    meta.locked = Some(LockMeta {
        reason: reason.to_string(),
        at: now(),
    });
    write_meta(meta)?;
    lock_existing_projects(meta, jobs.unwrap_or_else(default_jobs))
}

fn lock_existing_projects(meta: &TreeMeta, jobs: usize) -> AppResult<usize> {
    let reason = meta
        .locked
        .as_ref()
        .map(|lock| lock.reason.as_str())
        .unwrap_or("");
    let lock_reason = format!("repo-worktree:{}: {reason}", meta.name);
    let root = meta.root();
    let pool = ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|error| AppError::io("create worker pool", error))?;
    let warnings = pool.install(|| {
        meta.projects
            .par_iter()
            .filter(|project| root.join(&project.path).exists())
            .filter(|project| {
                Command::new("git")
                    .arg(format!("--git-dir={}", project.git_common_dir))
                    .args(["worktree", "lock", "--reason"])
                    .arg(&lock_reason)
                    .arg(root.join(&project.path))
                    .output()
                    .map(|output| !output.status.success())
                    .unwrap_or(true)
            })
            .count()
    });
    Ok(warnings)
}

pub fn unlock(args: &UnlockArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    check_git_version()?;
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Exclusive, false, |registry| {
        let mut meta = select_tree(registry, &args.tree, cwd)?;
        if meta.locked.is_none() {
            return Err(AppError::operational(
                "E_NOT_LOCKED",
                format!("tree {} is not locked", meta.name),
            ));
        }
        meta.locked = None;
        write_meta(&meta)?;
        let root = meta.root();
        let pool = ThreadPoolBuilder::new()
            .num_threads(args.jobs.unwrap_or_else(default_jobs))
            .build()
            .map_err(|error| AppError::io("create worker pool", error))?;
        let warnings = pool.install(|| {
            meta.projects
                .par_iter()
                .filter(|project| project.state == "done")
                .filter(|project| {
                    Command::new("git")
                        .arg(format!("--git-dir={}", project.git_common_dir))
                        .args(["worktree", "unlock"])
                        .arg(root.join(&project.path))
                        .output()
                        .map(|output| !output.status.success())
                        .unwrap_or(true)
                })
                .count()
        });
        if !quiet {
            eprintln!("unlocked {}", meta.name);
        }
        Ok(json!({"name": meta.name, "locked": false, "project_warnings": warnings}))
    })
}

pub fn move_tree(args: &MoveArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    check_git_version()?;
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Exclusive, false, |registry| {
        let mut meta = select_tree(registry, &args.tree, cwd)?;
        if meta.locked.is_some() && !args.force {
            return Err(AppError::operational(
                "E_TREE_LOCKED",
                format!("tree {} is locked; pass --force to move", meta.name),
            ));
        }
        let old_state = meta.state.clone();
        let old_path = meta.root();
        let new_path = canonicalize_missing(&absolutize(&args.new_path, cwd))?;
        validate_destination(&new_path, &meta.source(), &load_all(registry)?)?;
        meta.moving = Some(MovingMeta {
            from: old_path.to_string_lossy().into_owned(),
            to: new_path.to_string_lossy().into_owned(),
            at: now(),
        });
        meta.state = "moving".into();
        write_meta(&meta)?;
        if let Err(error) = fs::rename(&old_path, &new_path) {
            meta.moving = None;
            meta.state = old_state;
            write_meta(&meta)?;
            if error.raw_os_error() == Some(18) {
                return Err(AppError::operational(
                    "E_CROSS_DEVICE",
                    "cross-filesystem move is not supported; create a new worktree at the destination and remove this one",
                ));
            }
            return Err(AppError::io(
                format!("move {} to {}", old_path.display(), new_path.display()),
                error,
            ));
        }
        maybe_crash_after_move();
        meta.path = new_path.to_string_lossy().into_owned();
        write_breadcrumb(&meta)?;
        let warnings = repair_git_links(&mut meta, default_jobs());
        meta.moving = None;
        meta.state = if old_state == "complete" && warnings == 0 && meta.is_complete() {
            "complete".into()
        } else if old_state == "creating" {
            old_state
        } else {
            "incomplete".into()
        };
        write_meta(&meta)?;
        if !quiet {
            if warnings > 0 {
                eprintln!("warning: {warnings} project worktree repair(s) failed");
            }
            eprintln!(
                "moved {} from {} to {}",
                meta.name,
                old_path.display(),
                new_path.display()
            );
        }
        Ok(json!({"name": meta.name, "from": old_path, "to": new_path}))
    })
}

fn maybe_crash_after_move() {
    #[cfg(feature = "test-hooks")]
    if std::env::var_os("GRW_TEST_CRASH_AFTER_MOVE").is_some() {
        std::process::abort();
    }
}

pub fn prune(args: &PruneArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    check_git_version()?;
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Exclusive, false, |registry| {
        let trees = load_all(registry)?;
        let mut pruned = Vec::new();
        let mut gitdirs = Vec::new();
        for tree in trees {
            let root = tree.root();
            let prunable = !root.exists()
                || (!root.join(".repo-worktree").exists() && !tree.contains_project_dir(&root));
            if !prunable || tree.locked.is_some() {
                continue;
            }
            if args.dry_run {
                println!("would prune {} ({})", tree.name, tree.path);
            } else {
                gitdirs.extend(
                    tree.projects
                        .iter()
                        .filter(|project| !project.git_common_dir.is_empty())
                        .map(|project| project.git_common_dir.clone()),
                );
                let entry = tree.registry_entry();
                if entry.exists() {
                    fs::remove_dir_all(&entry).map_err(|error| {
                        AppError::io(format!("remove {}", entry.display()), error)
                    })?;
                }
                if !quiet {
                    eprintln!("pruned {} ({})", tree.name, tree.path);
                }
            }
            pruned.push(json!({"name": tree.name, "path": tree.path}));
        }
        if !args.dry_run {
            let mut unique = HashSet::new();
            let gitdirs = gitdirs
                .into_iter()
                .filter(|path| unique.insert(path.clone()))
                .collect::<Vec<_>>();
            let pool = ThreadPoolBuilder::new()
                .num_threads(args.jobs.unwrap_or_else(default_jobs))
                .build()
                .map_err(|error| AppError::io("create worker pool", error))?;
            pool.install(|| {
                gitdirs.par_iter().for_each(|gitdir| {
                    let mut command = Command::new("git");
                    command
                        .arg(format!("--git-dir={gitdir}"))
                        .args(["worktree", "prune"]);
                    if let Some(expire) = &args.expire {
                        command.arg("--expire").arg(expire);
                    }
                    let _ = command.output();
                });
            });
        }
        Ok(json!({"pruned": pruned, "dry_run": args.dry_run}))
    })
}

pub fn repair(args: &RepairArgs, cwd: &Path, quiet: bool) -> AppResult<Value> {
    check_git_version()?;
    let client = discover_client(cwd, true)?;
    with_registry_lock(&client.root, LockMode::Exclusive, false, |registry| {
        let mut trees = if args.trees.is_empty() {
            load_all(registry)?
        } else {
            args.trees
                .iter()
                .map(|selector| select_tree(registry, selector, cwd))
                .collect::<AppResult<Vec<_>>>()?
        };
        let jobs = args.jobs.unwrap_or_else(default_jobs);
        let mut incomplete = INTERRUPTED.load(Ordering::SeqCst);
        for meta in &mut trees {
            if INTERRUPTED.load(Ordering::SeqCst) {
                incomplete = true;
                break;
            }
            recover_move(meta)?;
            if !meta.root().exists() {
                if !quiet {
                    eprintln!("prunable {} ({})", meta.name, meta.path);
                }
                continue;
            }
            repair_git_links(meta, jobs);
            repair_directives(meta, jobs, quiet)?;
            meta.state = if meta.is_complete() {
                "complete".into()
            } else {
                incomplete = true;
                "incomplete".into()
            };
            write_meta(meta)?;
            if INTERRUPTED.load(Ordering::SeqCst) {
                meta.state = "incomplete".into();
                write_meta(meta)?;
                incomplete = true;
                break;
            }
        }
        let result = if trees.len() == 1 {
            serde_json::to_value(&trees[0]).expect("tree metadata serializes")
        } else {
            json!({"trees": trees})
        };
        if incomplete {
            Err(AppError::partial(result))
        } else {
            Ok(result)
        }
    })
}

fn recover_move(meta: &mut TreeMeta) -> AppResult<()> {
    let Some(moving) = meta.moving.clone() else {
        return Ok(());
    };
    let from = PathBuf::from(&moving.from);
    let to = PathBuf::from(&moving.to);
    if !to.exists() && from.exists() {
        fs::rename(&from, &to).map_err(|error| {
            if error.raw_os_error() == Some(18) {
                AppError::operational(
                    "E_CROSS_DEVICE",
                    "cross-filesystem move is not supported; create a new worktree at the destination and remove this one",
                )
            } else {
                AppError::io(
                    format!("finish move {} to {}", from.display(), to.display()),
                    error,
                )
            }
        })?;
    }
    if to.exists() {
        meta.path = to.to_string_lossy().into_owned();
        write_breadcrumb(meta)?;
        repair_git_links(meta, default_jobs());
    }
    meta.moving = None;
    meta.state = if meta.is_complete() {
        "complete".into()
    } else {
        "incomplete".into()
    };
    write_meta(meta)
}

fn repair_git_links(meta: &mut TreeMeta, jobs: usize) -> usize {
    let root = meta.root();
    let pool = match ThreadPoolBuilder::new().num_threads(jobs).build() {
        Ok(pool) => pool,
        Err(_) => return 1,
    };
    let results = pool.install(|| {
        meta.projects
            .par_iter()
            .enumerate()
            .filter(|(_, project)| project.state != "skipped" && root.join(&project.path).exists())
            .map(|(index, project)| {
                let success = Command::new("git")
                    .arg(format!("--git-dir={}", project.git_common_dir))
                    .args(["worktree", "repair"])
                    .arg(root.join(&project.path))
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false);
                (index, success)
            })
            .collect::<Vec<_>>()
    });
    let failures = results.iter().filter(|(_, success)| !success).count();
    for (index, success) in results {
        if !success && meta.projects[index].state == "done" {
            meta.projects[index].state = "failed".into();
            meta.projects[index].reason = Some("repair-failed".into());
        }
    }
    failures
}

fn repair_directives(meta: &mut TreeMeta, jobs: usize, quiet: bool) -> AppResult<()> {
    let done = meta
        .projects
        .iter()
        .filter(|project| project.state == "done")
        .map(|project| project.path.clone())
        .collect::<HashSet<_>>();
    let root = meta.root();
    let pool = ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|error| AppError::io("create worker pool", error))?;
    let copy_results = pool.install(|| {
        meta.copyfiles
            .par_iter()
            .enumerate()
            .filter(|(_, directive)| done.contains(&directive.project))
            .map(|(index, directive)| (index, apply_copy(&root, directive, true)))
            .collect::<Vec<_>>()
    });
    for (index, result) in copy_results {
        match result {
            Ok(()) => {
                meta.copyfiles[index].state = "done".into();
                meta.copyfiles[index].reason = None;
            }
            Err(reason) => {
                meta.copyfiles[index].state = "failed".into();
                meta.copyfiles[index].reason = Some(reason);
            }
        }
    }
    let link_results = pool.install(|| {
        meta.linkfiles
            .par_iter()
            .enumerate()
            .filter(|(_, directive)| done.contains(&directive.project))
            .map(|(index, directive)| (index, apply_link(&root, directive, true)))
            .collect::<Vec<_>>()
    });
    for (index, result) in link_results {
        match result {
            Ok(()) => {
                meta.linkfiles[index].state = "done".into();
                meta.linkfiles[index].reason = None;
            }
            Err(reason) => {
                if !quiet {
                    eprintln!(
                        "warning: repair linkfile {} failed: {reason}",
                        meta.linkfiles[index].dest
                    );
                }
                meta.linkfiles[index].state = "failed".into();
                meta.linkfiles[index].reason = Some(reason);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(path: &str, state: &str) -> ProjectMeta {
        ProjectMeta {
            path: path.into(),
            name: path.into(),
            sha: "0123456789abcdef0123456789abcdef01234567".into(),
            git_common_dir: format!("/git/{path}"),
            state: state.into(),
            reason: None,
        }
    }

    #[test]
    fn scheduler_waits_only_for_actual_project_ancestors() {
        let projects = vec![
            project("a", "pending"),
            project("unrelated/deep", "pending"),
            project("a/b", "pending"),
            project("a/b/c", "pending"),
        ];
        let schedule = build_project_schedule(&projects);
        assert_eq!(schedule.ready.into_iter().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(schedule.children[0], vec![2]);
        assert_eq!(schedule.children[2], vec![3]);
    }

    #[test]
    fn scheduler_treats_completed_ancestors_as_already_released() {
        let projects = vec![project("a", "done"), project("a/b", "pending")];
        let schedule = build_project_schedule(&projects);
        assert_eq!(schedule.ready.into_iter().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn porcelain_has_stable_fields() {
        let meta = TreeMeta {
            format_version: 1,
            tool_version: "1.0.0".into(),
            name: "wt".into(),
            path: "/tmp/wt".into(),
            source_root: "/tmp/source".into(),
            source_layout: "legacy".into(),
            created_at: "2026-07-23T18:04:05Z".into(),
            state: "complete".into(),
            branch: None,
            submodules: false,
            locked: None,
            moving: None,
            projects: vec![],
            copyfiles: vec![],
            linkfiles: vec![],
        };
        let envelope = serde_json::to_value(&meta).unwrap();
        assert_eq!(envelope["format_version"], 1);
        assert_eq!(meta.done_count(), 0);
    }
}
