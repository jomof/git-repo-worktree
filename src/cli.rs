use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "git-repo-worktree",
    bin_name = "git repo-worktree",
    version,
    about = "Create and manage whole-tree linked worktrees for Google repo clients",
    propagate_version = true
)]
pub struct Cli {
    /// Emit one stable JSON envelope on stdout.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress summaries and warnings.
    #[arg(short = 'q', long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Show per-project details (repeat for future compatibility).
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Run as if started in this directory.
    #[arg(short = 'C', global = true, value_name = "DIR")]
    pub directory: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a whole-tree worktree or resume an interrupted creation.
    Add(AddArgs),
    /// List registered whole-tree worktrees.
    List(ListArgs),
    /// Remove a whole-tree worktree.
    Remove(RemoveArgs),
    /// Lock a tree against removal, move, and prune.
    Lock(LockArgs),
    /// Unlock a tree.
    Unlock(UnlockArgs),
    /// Move a tree on the same filesystem.
    Move(MoveArgs),
    /// Prune registry entries for missing trees.
    Prune(PruneArgs),
    /// Repair worktree registrations and manifest directives.
    Repair(RepairArgs),
    /// Show tool and runtime dependency versions.
    Version,
    /// Generate shell completions.
    Completions(CompletionsArgs),
    /// Show, install, update, or remove the bundled agent skill.
    Skill(SkillArgs),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Destination path, or a tree selector with --continue.
    pub path: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(short = 'b', value_name = "BRANCH")]
    pub branch: Option<String>,
    #[arg(long)]
    pub detach: bool,
    #[arg(long)]
    pub submodules: bool,
    #[arg(long)]
    pub lock: bool,
    #[arg(long, requires = "lock")]
    pub reason: Option<String>,
    #[arg(long = "continue")]
    pub resume: bool,
    /// Override only the concurrent repo-sync safety check.
    #[arg(long)]
    pub force: bool,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
    #[arg(long)]
    pub fail_fast: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(long, conflicts_with = "json")]
    pub porcelain: bool,
    #[arg(short = 'z', requires = "porcelain")]
    pub nul: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    pub tree: String,
    #[arg(long, action = ArgAction::Count)]
    pub force: u8,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Debug, Args)]
pub struct LockArgs {
    pub tree: String,
    #[arg(long, default_value = "")]
    pub reason: String,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Debug, Args)]
pub struct UnlockArgs {
    pub tree: String,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Debug, Args)]
pub struct MoveArgs {
    pub tree: String,
    pub new_path: PathBuf,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    #[arg(long)]
    pub expire: Option<String>,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Debug, Args)]
pub struct RepairArgs {
    pub trees: Vec<String>,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    pub shell: CompletionShell,
}

#[derive(Debug, Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub command: SkillCommand,
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Print the embedded skill without installation sentinels.
    Show,
    /// Probe and report every known skill target.
    List,
    /// Install the bundled skill.
    Add(SkillMutationArgs),
    /// Refresh every sentinel-bearing installed copy.
    Sync(SkillSyncArgs),
    /// Remove installed copies.
    Remove(SkillRemoveArgs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SkillScope {
    Auto,
    User,
    Project,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("selection")
        .args(["path", "target", "scope", "all"])
        .multiple(false)
))]
pub struct SkillMutationArgs {
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<SkillScope>,
    #[arg(long, action = ArgAction::Append, value_name = "ID")]
    pub target: Vec<String>,
    #[arg(long, value_name = "DIR")]
    pub path: Option<PathBuf>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SkillSyncArgs {
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("selection")
        .args(["path", "target", "scope", "all"])
        .multiple(false)
))]
pub struct SkillRemoveArgs {
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<SkillScope>,
    #[arg(long, action = ArgAction::Append, value_name = "ID")]
    pub target: Vec<String>,
    #[arg(long, value_name = "DIR")]
    pub path: Option<PathBuf>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub force: bool,
}

fn parse_jobs(value: &str) -> Result<usize, String> {
    let jobs = value
        .parse::<usize>()
        .map_err(|_| "jobs must be a positive integer".to_string())?;
    if jobs == 0 {
        Err("jobs must be at least 1".into())
    } else {
        Ok(jobs)
    }
}
