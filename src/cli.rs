use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "git-repo-worktree",
    bin_name = "git repo-worktree",
    version,
    about = "Create and manage whole-tree linked worktrees for Google repo clients",
    long_about = "Create and manage whole-tree linked worktrees for Google repo clients.\n\n\
A tree is a parallel directory with one linked Git worktree per project of the \
source client, pinned to the commit each project currently has checked out \
(its HEAD, as recorded by `repo manifest -r`). Trees carry a synthesized .repo \
directory, so repo commands such as `repo status`, `repo start`, `repo upload`, \
and `repo sync` work inside them as in any client of the same manifest.\n\n\
In a tree, avoid `repo init`, `repo wipe`, `repo gc`, and `repo sync --force-sync`: \
they rewrite the tree's .repo or delete data shared with the source client. Run \
repo from the tree root or a worktree project, not from inside a symlink-shared \
project, which is physically the source checkout.\n\n\
Use `git-repo-worktree <command> --help` (hyphenated) or `git repo-worktree \
<command> -h` when the manual page is not installed.",
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

    /// Seconds to wait for a busy tree or registry lock before exiting with
    /// code 4 (default 300; 0 fails immediately).
    #[arg(long, global = true, value_name = "SECONDS")]
    pub lock_timeout: Option<u64>,

    /// Do not run site hooks from .repo/repo-worktrees/hooks
    /// (also set by GIT_REPO_WORKTREE_NO_HOOKS=1).
    #[arg(long, global = true)]
    pub no_hooks: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a whole-tree worktree or resume an interrupted creation.
    #[command(long_about = ADD_LONG_ABOUT)]
    Add(AddArgs),
    /// List registered whole-tree worktrees (add -v for per-project detail).
    List(ListArgs),
    /// Show per-project HEAD, branch, and dirtiness for one tree.
    Status(StatusArgs),
    /// Detach checked-out branches so they can be checked out elsewhere.
    #[command(long_about = DETACH_LONG_ABOUT)]
    Detach(DetachArgs),
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
    /// Convert shared projects into isolated Git worktrees.
    Materialize(MaterializeArgs),
    /// Manage persistent per-project population defaults.
    Defaults(DefaultsArgs),
    /// Manage pre-populated spare worktrees for pool-mode projects.
    Pool(PoolArgs),
    /// Show the site hooks that run after add and before remove.
    #[command(long_about = HOOKS_LONG_ABOUT)]
    Hooks(HooksArgs),
    /// Summarize this client's local usage history and timings.
    #[command(long_about = REPORT_LONG_ABOUT)]
    Report(ReportArgs),
    /// Show tool and runtime dependency versions.
    Version,
    /// Generate shell completions.
    Completions(CompletionsArgs),
    /// Show, install, update, or remove the bundled agent skill.
    Skill(SkillArgs),
}

const ADD_LONG_ABOUT: &str = "Create a whole-tree worktree or resume an interrupted creation.\n\n\
Starting revisions: every project starts on a detached HEAD at the commit that \
project currently has checked out in the source client (`repo manifest -r`), \
including local commits and topic branches checked out there. `add` never \
fetches and never resets to the remote branch. To start a project from upstream \
instead, run `git checkout --detach <remote>/<branch>` inside it, or run \
`repo sync` in the tree to move every project to upstream as in any client.\n\n\
Uncommitted changes in the source client are never copied.\n\n\
Branches: `-b <branch>` creates the branch only in the projects named with \
`-p/--project` (repeatable; any path inside a project is accepted), or in every \
project with `--all-projects`. Created branches track the project's manifest \
upstream, like `repo start`.";

const DETACH_LONG_ABOUT: &str =
    "Detach checked-out branches so they can be checked out elsewhere.\n\n\
Git allows a branch to be checked out in only one worktree of a project. \
`detach` moves HEAD off the branch in place (`git checkout --detach`), keeping \
the working tree and index untouched.\n\n\
With `--branch` and no tree, every tree and the source client are searched for \
that branch. Otherwise name a tree (or run inside one), or pass `--source`.";

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Destination path, or a tree selector with --continue.
    pub path: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    /// Create BRANCH in the projects selected by --project or --all-projects.
    #[arg(short = 'b', value_name = "BRANCH")]
    pub branch: Option<String>,
    /// Project that receives the -b branch (repeatable; subpaths resolve to their project).
    #[arg(
        short = 'p',
        long = "project",
        action = ArgAction::Append,
        value_name = "PROJECT",
        requires = "branch",
        conflicts_with = "all_projects"
    )]
    pub projects: Vec<String>,
    /// Create the -b branch in every project.
    #[arg(long, requires = "branch")]
    pub all_projects: bool,
    /// Do not synthesize a .repo directory; repo commands will not work in the tree.
    #[arg(long)]
    pub git_only: bool,
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
    /// Materialize every project instead of sharing fixed-depth projects.
    #[arg(long)]
    pub full: bool,
    /// Materialize this project instead of sharing it (repeatable).
    #[arg(long = "full-project", action = ArgAction::Append, value_name = "PROJECT")]
    pub full_projects: Vec<String>,
    /// Disable copy-on-write population and always use Git checkout.
    #[arg(long)]
    pub no_reflink: bool,
    /// Share fixed-depth projects with symlinks instead of FUSE overlays
    /// (also set by GIT_REPO_WORKTREE_NO_OVERLAY=1).
    #[arg(long)]
    pub no_overlay: bool,
    /// Maximum concurrent projects larger than 500 MiB.
    #[arg(long, value_parser = parse_jobs)]
    pub heavy_jobs: Option<usize>,
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

const REPORT_LONG_ABOUT: &str = "Summarize this client's local usage history and timings.\n\n\
Every invocation appends one line to .repo/repo-worktrees/history.jsonl: the \
command and flag names (never values), duration, outcome, phase timings, and, \
for commands that populate projects, per-project modes and timings. The file \
stays on this machine and is capped at 8 MiB. Set GIT_REPO_WORKTREE_SESSION to \
tag events with a label, or GIT_REPO_WORKTREE_NO_HISTORY=1 to stop recording.";

const HOOKS_LONG_ABOUT: &str = "Show the site hooks that run after add and before remove.\n\n\
Hooks are executables in .repo/repo-worktrees/hooks of the source client. \
`post-add` runs once a tree is complete (and again from `add --continue` or \
`repair` until it succeeds); `pre-remove` runs before a tree is deleted. Each \
runs in the tree root with GIT_REPO_WORKTREE_EVENT, GIT_REPO_WORKTREE_TREE, \
GIT_REPO_WORKTREE_NAME, and GIT_REPO_WORKTREE_SOURCE set. Output goes to \
stderr. A failing hook is reported and recorded but never fails the command. \
GIT_REPO_WORKTREE_HOOK_TIMEOUT (seconds, default 600) bounds each run; \
--no-hooks or GIT_REPO_WORKTREE_NO_HOOKS=1 skips them.";

#[derive(Debug, Args)]
pub struct HooksArgs {
    #[command(subcommand)]
    pub command: HooksCommand,
}

#[derive(Debug, Subcommand)]
pub enum HooksCommand {
    /// List hook events and whether each is installed.
    List,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Only include events tagged with this GIT_REPO_WORKTREE_SESSION label.
    #[arg(long, value_name = "LABEL")]
    pub session: Option<String>,
    /// Only include the most recent N events.
    #[arg(long, value_name = "N", value_parser = parse_jobs)]
    pub last: Option<usize>,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Tree name or path (default: the tree containing the current directory).
    pub tree: Option<String>,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Debug, Args)]
pub struct DetachArgs {
    /// Tree name or path.
    #[arg(conflicts_with = "source")]
    pub tree: Option<String>,
    /// Detach in the source client instead of a tree.
    #[arg(long)]
    pub source: bool,
    /// Only detach this branch.
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,
    /// Only consider this project (repeatable; subpaths resolve to their project).
    #[arg(short = 'p', long = "project", action = ArgAction::Append, value_name = "PROJECT")]
    pub projects: Vec<String>,
    /// Report what would be detached without changing anything.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    pub tree: String,
    #[arg(long, action = ArgAction::Count)]
    pub force: u8,
    /// Delete pool-mode projects instead of returning clean ones to the pool.
    #[arg(long)]
    pub no_pool: bool,
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

#[derive(Debug, Args)]
pub struct MaterializeArgs {
    pub tree: String,
    #[arg(required = true)]
    pub projects: Vec<String>,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Debug, Args)]
pub struct DefaultsArgs {
    #[command(subcommand)]
    pub command: DefaultsCommand,
}

#[derive(Debug, Subcommand)]
pub enum DefaultsCommand {
    /// List persistent project defaults.
    List,
    /// Prefer source-sharing for future trees.
    Symlink(DefaultProjectsArgs),
    /// Prefer isolated worktrees for future trees.
    Worktree(DefaultProjectsArgs),
    /// Prefer isolated worktrees taken from a pool of pre-populated spares.
    Pool(DefaultProjectsArgs),
    /// Prefer copy-on-write overlays of the source checkout (Linux, fuse-overlayfs).
    Overlay(DefaultProjectsArgs),
    /// Remove persistent defaults and return to automatic selection.
    Auto(DefaultProjectsArgs),
}

#[derive(Debug, Args)]
pub struct DefaultProjectsArgs {
    #[arg(required = true)]
    pub projects: Vec<String>,
}

#[derive(Debug, Args)]
pub struct PoolArgs {
    #[command(subcommand)]
    pub command: PoolCommand,
}

#[derive(Debug, Subcommand)]
pub enum PoolCommand {
    /// List spares per project, the pool depth, and orphaned spares.
    #[command(visible_alias = "status")]
    List,
    /// Show or set how many spares each pooled project keeps (default 2).
    Size(PoolSizeArgs),
    /// Create spare worktrees at the source checkout's current HEAD.
    Fill(PoolFillArgs),
    /// Delete spare worktrees.
    Clear(PoolClearArgs),
    /// Delete orphaned spares and refill pooled projects up to the pool size
    /// (run in the background after add and remove).
    #[command(hide = true)]
    Maintain,
}

#[derive(Debug, Args)]
pub struct PoolSizeArgs {
    /// New number of spares per project.
    #[arg(value_parser = parse_jobs)]
    pub spares: Option<usize>,
}

#[derive(Debug, Args)]
pub struct PoolFillArgs {
    /// Projects to fill (default: every pooled project).
    pub projects: Vec<String>,
    /// Spares to keep per project (default: the pool size).
    #[arg(long, value_parser = parse_jobs)]
    pub count: Option<usize>,
    #[arg(short = 'j', long, value_parser = parse_jobs)]
    pub jobs: Option<usize>,
}

#[derive(Debug, Args)]
pub struct PoolClearArgs {
    /// Projects to clear (default: all).
    pub projects: Vec<String>,
    /// Delete only spares no default add would use.
    #[arg(long)]
    pub orphaned: bool,
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
