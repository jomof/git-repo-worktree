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
