use clap::CommandFactory;
use std::fs::{self, File};
use std::path::PathBuf;

#[path = "src/cli.rs"]
mod cli;

fn main() {
    println!("cargo:rerun-if-changed=src/cli.rs");
    println!("cargo:rerun-if-changed=skill/SKILL.md");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let man_dir = out.join("man");
    fs::create_dir_all(&man_dir).expect("create man output directory");
    let mut file = File::create(man_dir.join("git-repo-worktree.1")).expect("create man page");
    clap_mangen::Man::new(cli::Cli::command())
        .render(&mut file)
        .expect("render man page");
}
