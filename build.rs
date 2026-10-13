use clap::CommandFactory;
use std::fs::{self, File};
use std::path::PathBuf;

#[path = "src/cli.rs"]
mod cli;

/// Exposes the short commit hash as `GRW_COMMIT` when building from a Git
/// checkout, so `version` distinguishes builds that share a crate version.
fn record_commit() {
    let head = std::path::Path::new(".git/HEAD");
    if !head.is_file() {
        return;
    }
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(contents) = fs::read_to_string(head) {
        if let Some(reference) = contents.trim().strip_prefix("ref: ") {
            let path = std::path::Path::new(".git").join(reference);
            if path.is_file() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    if std::path::Path::new(".git/packed-refs").is_file() {
        println!("cargo:rerun-if-changed=.git/packed-refs");
    }
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output();
    if let Ok(output) = output {
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && !commit.is_empty() {
            println!("cargo:rustc-env=GRW_COMMIT={commit}");
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src/cli.rs");
    println!("cargo:rerun-if-changed=skill/SKILL.md");
    record_commit();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let man_dir = out.join("man");
    let _ = fs::remove_dir_all(&man_dir);
    fs::create_dir_all(&man_dir).expect("create man output directory");
    let mut command = cli::Cli::command();
    command.build();
    for subcommand in command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
    {
        let title: &'static str =
            Box::leak(format!("git-repo-worktree-{}", subcommand.get_name()).into_boxed_str());
        let mut file = File::create(man_dir.join(format!("{title}.1"))).expect("create man page");
        let page = subcommand
            .clone()
            .name(title)
            .bin_name(format!("git repo-worktree {}", subcommand.get_name()));
        clap_mangen::Man::new(page)
            .title(title)
            .render(&mut file)
            .expect("render man page");
    }
    let mut file = File::create(man_dir.join("git-repo-worktree.1")).expect("create man page");
    clap_mangen::Man::new(command)
        .render(&mut file)
        .expect("render man page");
}
