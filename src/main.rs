#![cfg_attr(not(unix), allow(dead_code))]

#[cfg(not(unix))]
compile_error!("git-repo-worktree supports Linux and macOS only");

mod cli;
mod discovery;
mod error;
mod manifest;
mod model;
mod registry;
mod skill;
mod tree;
mod util;

use clap::{error::ErrorKind, CommandFactory, Parser};
use cli::{Cli, Command, CompletionShell, SkillCommand};
use error::{AppError, AppResult, Envelope};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static SIGNAL_HANDLER: OnceLock<()> = OnceLock::new();

fn install_signal_handler() {
    SIGNAL_HANDLER.get_or_init(|| {
        let _ = ctrlc::set_handler(|| {
            INTERRUPTED.store(true, Ordering::SeqCst);
        });
    });
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Add(_) => "add",
        Command::List(_) => "list",
        Command::Remove(_) => "remove",
        Command::Lock(_) => "lock",
        Command::Unlock(_) => "unlock",
        Command::Move(_) => "move",
        Command::Prune(_) => "prune",
        Command::Repair(_) => "repair",
        Command::Version => "version",
        Command::Completions(_) => "completions",
        Command::Skill(args) => match &args.command {
            SkillCommand::Show => "skill show",
            SkillCommand::List => "skill list",
            SkillCommand::Add(_) => "skill add",
            SkillCommand::Sync(_) => "skill sync",
            SkillCommand::Remove(_) => "skill remove",
        },
    }
}

fn cwd(cli: &Cli) -> AppResult<PathBuf> {
    let current = std::env::current_dir().map_err(|error| AppError::io("read cwd", error))?;
    let selected = cli
        .directory
        .as_ref()
        .map_or(current.clone(), |path| util::absolutize(path, &current));
    std::fs::canonicalize(&selected)
        .map_err(|error| AppError::io(format!("enter {}", selected.display()), error))
}

fn execute(cli: &Cli, cwd: &std::path::Path) -> AppResult<Value> {
    match &cli.command {
        Command::Add(args) => tree::add(args, cwd, cli.quiet, cli.verbose),
        Command::List(args) => tree::list(args, cwd, cli.json, cli.verbose),
        Command::Remove(args) => tree::remove(args, cwd, cli.quiet),
        Command::Lock(args) => tree::lock(args, cwd, cli.quiet),
        Command::Unlock(args) => tree::unlock(args, cwd, cli.quiet),
        Command::Move(args) => tree::move_tree(args, cwd, cli.quiet),
        Command::Prune(args) => tree::prune(args, cwd, cli.quiet),
        Command::Repair(args) => tree::repair(args, cwd, cli.quiet),
        Command::Version => version(cli.verbose, cli.json),
        Command::Completions(args) => completions(args.shell, cli.json),
        Command::Skill(args) => skill::run(&args.command, cwd, cli.json, cli.quiet),
    }
}

fn version(verbose: u8, json_output: bool) -> AppResult<Value> {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let git = discovery::program_version("git", &["version"]).ok();
    let repo = discovery::program_version("repo", &["--version"]).ok();
    if !json_output {
        if verbose > 0 {
            println!("git-repo-worktree {version}");
            println!("git {}", git.as_deref().unwrap_or("not found"));
            println!("repo {}", repo.as_deref().unwrap_or("not found"));
        } else {
            println!("git-repo-worktree {version}");
        }
    }
    Ok(json!({"version": version, "git": git, "repo": repo}))
}

fn completions(shell: CompletionShell, json_output: bool) -> AppResult<Value> {
    let mut command = Cli::command();
    let mut bytes = Vec::new();
    match shell {
        CompletionShell::Bash => clap_complete::generate(
            clap_complete::Shell::Bash,
            &mut command,
            "git-repo-worktree",
            &mut bytes,
        ),
        CompletionShell::Zsh => clap_complete::generate(
            clap_complete::Shell::Zsh,
            &mut command,
            "git-repo-worktree",
            &mut bytes,
        ),
        CompletionShell::Fish => clap_complete::generate(
            clap_complete::Shell::Fish,
            &mut command,
            "git-repo-worktree",
            &mut bytes,
        ),
    }
    let script = String::from_utf8(bytes)
        .map_err(|error| AppError::io("generate completion script", error))?;
    if !json_output {
        print!("{script}");
    }
    Ok(json!({"shell": format!("{shell:?}").to_ascii_lowercase(), "script": script}))
}

fn main() -> ExitCode {
    install_signal_handler();
    let raw_args = std::env::args_os().collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(&raw_args) {
        Ok(cli) => cli,
        Err(error) => {
            let json_output = raw_args.iter().any(|value| value == "--json");
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                if json_output {
                    let envelope = Envelope::success(
                        infer_command(&raw_args),
                        json!({"text": error.to_string()}),
                    );
                    println!(
                        "{}",
                        serde_json::to_string(&envelope).expect("envelope serializes")
                    );
                    return ExitCode::SUCCESS;
                }
                let code = error.exit_code();
                let _ = error.print();
                return ExitCode::from(code as u8);
            }
            if json_output {
                let app_error = AppError::usage(error.to_string());
                let envelope = Envelope::failure(infer_command(&raw_args), &app_error);
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("envelope serializes")
                );
                return ExitCode::from(2);
            }
            let code = error.exit_code();
            let _ = error.print();
            return ExitCode::from(code as u8);
        }
    };
    let command = command_name(&cli.command);
    let mut result = cwd(&cli).and_then(|cwd| execute(&cli, &cwd));
    if INTERRUPTED.load(Ordering::SeqCst)
        && result.is_ok()
        && !matches!(&cli.command, Command::Add(_) | Command::Repair(_))
    {
        result = Err(AppError::operational(
            "E_INTERRUPTED",
            "operation was interrupted",
        ));
    }

    let exit_code = match result {
        Ok(result) => {
            if cli.json {
                let envelope = Envelope::success(command, result);
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("envelope serializes")
                );
            }
            0
        }
        Err(error) => {
            if cli.json {
                let envelope = Envelope::failure(command, &error);
                println!(
                    "{}",
                    serde_json::to_string(&envelope).expect("envelope serializes")
                );
            } else {
                let _ = writeln!(std::io::stderr(), "error: {}", error.message);
            }
            error.exit_code
        }
    };
    ExitCode::from(exit_code as u8)
}

fn infer_command(args: &[std::ffi::OsString]) -> String {
    let values = args
        .iter()
        .skip(1)
        .filter_map(|value| value.to_str())
        .collect::<Vec<_>>();
    if let Some(index) = values.iter().position(|value| *value == "skill") {
        if let Some(subcommand) = values.get(index + 1) {
            return format!("skill {subcommand}");
        }
        return "skill".into();
    }
    for name in [
        "add",
        "list",
        "remove",
        "lock",
        "unlock",
        "move",
        "prune",
        "repair",
        "version",
        "completions",
    ] {
        if values.contains(&name) {
            return name.into();
        }
    }
    "cli".into()
}
