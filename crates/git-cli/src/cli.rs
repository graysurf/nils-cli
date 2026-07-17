use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
use nils_common::cli_contract::exit;
use std::ffi::OsString;

use crate::{branch, ci, commit, completion, open, reset, utils, worktree};

#[derive(Debug, Parser)]
#[command(
    name = "git-cli",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Git helper CLI",
    disable_help_subcommand = true,
    override_usage = "git-cli <group> <command> [args]"
)]
struct Cli {
    #[command(subcommand)]
    group: Option<Group>,
}

#[derive(Debug, Subcommand)]
enum Group {
    #[command(about = "Utility helpers")]
    Utils(UtilsGroup),
    #[command(about = "Reset helpers")]
    Reset(ResetGroup),
    #[command(about = "Commit helpers")]
    Commit(CommitGroup),
    #[command(about = "Branch helpers")]
    Branch(BranchGroup),
    #[command(about = "Worktree helpers")]
    Worktree(WorktreeGroup),
    #[command(about = "CI helpers")]
    Ci(CiGroup),
    #[command(about = "Open remote pages")]
    Open(OpenGroup),
    #[command(about = "Export shell completion script")]
    Completion(RawArgs),
    #[command(about = "Display help message for git-cli")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
struct UtilsGroup {
    #[command(subcommand)]
    command: Option<UtilsCommand>,
}

#[derive(Debug, Subcommand)]
enum UtilsCommand {
    #[command(about = "Create zip archive from HEAD")]
    Zip(RawArgs),
    #[command(
        name = "copy-staged",
        visible_alias = "copy",
        about = "Copy staged diff to clipboard"
    )]
    CopyStaged(RawArgs),
    #[command(about = "Jump to git root")]
    Root(RawArgs),
    #[command(
        name = "commit-hash",
        visible_alias = "hash",
        about = "Resolve commit hash"
    )]
    CommitHash(RawArgs),
    #[command(about = "Display help message for utils")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
struct ResetGroup {
    #[command(subcommand)]
    command: Option<ResetCommand>,
}

#[derive(Debug, Subcommand)]
enum ResetCommand {
    #[command(about = "Reset to HEAD~N (soft)")]
    Soft(RawArgs),
    #[command(about = "Reset to HEAD~N (mixed)")]
    Mixed(RawArgs),
    #[command(about = "Reset to HEAD~N (hard)")]
    Hard(RawArgs),
    #[command(about = "Undo last reset")]
    Undo(RawArgs),
    #[command(name = "back-head", about = "Checkout HEAD@{1}")]
    BackHead(RawArgs),
    #[command(name = "back-checkout", about = "Return to previous branch")]
    BackCheckout(RawArgs),
    #[command(about = "Reset to remote branch")]
    Remote(RawArgs),
    #[command(about = "Display help message for reset")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
struct CommitGroup {
    #[command(subcommand)]
    command: Option<CommitCommand>,
}

#[derive(Debug, Subcommand)]
enum CommitCommand {
    #[command(about = "Print commit context")]
    Context(RawArgs),
    #[command(
        name = "context-json",
        visible_aliases = ["context_json", "contextjson", "json"],
        about = "Print commit context as JSON"
    )]
    ContextJson(RawArgs),
    #[command(
        name = "to-stash",
        visible_alias = "stash",
        about = "Create stash from commit"
    )]
    ToStash(RawArgs),
    #[command(about = "Display help message for commit")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
struct BranchGroup {
    #[command(subcommand)]
    command: Option<BranchCommand>,
}

#[derive(Debug, Subcommand)]
enum BranchCommand {
    #[command(
        name = "cleanup",
        visible_alias = "delete-merged",
        about = "Delete merged branches"
    )]
    Cleanup(RawArgs),
    #[command(about = "Display help message for branch")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
struct WorktreeGroup {
    #[command(subcommand)]
    command: Option<WorktreeCommand>,
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    #[command(about = "Create a managed agent worktree")]
    Add(RawArgs),
    #[command(
        name = "adopt-dirty",
        about = "Adopt one challenged dirty checkout snapshot"
    )]
    AdoptDirty(RawArgs),
    #[command(
        name = "dirty-snapshot",
        about = "Hash the current dirty checkout state"
    )]
    DirtySnapshot(RawArgs),
    #[command(about = "List git worktrees")]
    List(RawArgs),
    #[command(about = "Remove a managed worktree by slug or path")]
    Remove(RawArgs),
    #[command(about = "Prune stale git worktree metadata")]
    Prune(RawArgs),
    #[command(name = "revoke-dirty", about = "Revoke a receipt-bound dirty adoption")]
    RevokeDirty(RawArgs),
    #[command(about = "Resolve a worktree path to cd into")]
    Go(RawArgs),
    #[command(about = "Display help message for worktree")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
struct CiGroup {
    #[command(subcommand)]
    command: Option<CiCommand>,
}

#[derive(Debug, Subcommand)]
enum CiCommand {
    #[command(about = "Cherry-pick into CI branch")]
    Pick(RawArgs),
    #[command(about = "Display help message for ci")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_subcommand = true)]
struct OpenGroup {
    #[command(subcommand)]
    command: Option<OpenCommand>,
}

#[derive(Debug, Subcommand)]
enum OpenCommand {
    #[command(about = "Open repository page")]
    Repo(RawArgs),
    #[command(about = "Open branch tree page")]
    Branch(RawArgs),
    #[command(
        name = "default-branch",
        visible_alias = "default",
        about = "Open default branch tree page"
    )]
    DefaultBranch(RawArgs),
    #[command(about = "Open commit page")]
    Commit(RawArgs),
    #[command(about = "Open compare page")]
    Compare(RawArgs),
    #[command(
        name = "pr",
        visible_aliases = ["pull-request", "mr", "merge-request"],
        about = "Open pull or merge request page"
    )]
    Pr(RawArgs),
    #[command(
        visible_aliases = ["prs", "merge-requests", "mrs"],
        about = "Open pull or merge request list"
    )]
    Pulls(RawArgs),
    #[command(visible_alias = "issue", about = "Open issues list/page")]
    Issues(RawArgs),
    #[command(visible_alias = "action", about = "Open actions page")]
    Actions(RawArgs),
    #[command(visible_alias = "release", about = "Open releases list/page")]
    Releases(RawArgs),
    #[command(visible_alias = "tag", about = "Open tags list/page")]
    Tags(RawArgs),
    #[command(visible_alias = "history", about = "Open commit history page")]
    Commits(RawArgs),
    #[command(visible_alias = "blob", about = "Open file page")]
    File(RawArgs),
    #[command(about = "Open blame page")]
    Blame(RawArgs),
    #[command(about = "Display help message for open")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_flag = true)]
struct RawArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

pub fn run() -> i32 {
    if let Some(status) = worktree::dirty_checkout_adoption::run_internal_snapshot_worker() {
        return status;
    }
    // Short-circuit `COMPLETE=<shell> git-cli ...` dynamic-completion requests
    // before the hand-rolled dispatch. No-op when `COMPLETE` is unset, so
    // ordinary invocations are unaffected.
    completion::complete_env();
    run_from(std::env::args())
}

fn run_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => return print_parse_error(err),
    };

    match cli.group {
        Some(Group::Utils(group)) => run_utils(group),
        Some(Group::Reset(group)) => run_reset(group),
        Some(Group::Commit(group)) => run_commit(group),
        Some(Group::Branch(group)) => run_branch(group),
        Some(Group::Worktree(group)) => run_worktree(group),
        Some(Group::Ci(group)) => run_ci(group),
        Some(Group::Open(group)) => run_open(group),
        Some(Group::Completion(raw)) => run_completion(raw),
        Some(Group::Help) | None => print_root_help(),
    }
}

fn run_utils(group: UtilsGroup) -> i32 {
    match group.command {
        Some(UtilsCommand::Zip(raw)) => utils::dispatch("zip", &raw.args).unwrap_or(exit::USAGE),
        Some(UtilsCommand::CopyStaged(raw)) => {
            utils::dispatch("copy-staged", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(UtilsCommand::Root(raw)) => utils::dispatch("root", &raw.args).unwrap_or(exit::USAGE),
        Some(UtilsCommand::CommitHash(raw)) => {
            utils::dispatch("commit-hash", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(UtilsCommand::Help) | None => print_group_help("utils"),
    }
}

fn run_reset(group: ResetGroup) -> i32 {
    match group.command {
        Some(ResetCommand::Soft(raw)) => reset::dispatch("soft", &raw.args).unwrap_or(exit::USAGE),
        Some(ResetCommand::Mixed(raw)) => {
            reset::dispatch("mixed", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(ResetCommand::Hard(raw)) => reset::dispatch("hard", &raw.args).unwrap_or(exit::USAGE),
        Some(ResetCommand::Undo(raw)) => reset::dispatch("undo", &raw.args).unwrap_or(exit::USAGE),
        Some(ResetCommand::BackHead(raw)) => {
            reset::dispatch("back-head", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(ResetCommand::BackCheckout(raw)) => {
            reset::dispatch("back-checkout", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(ResetCommand::Remote(raw)) => {
            reset::dispatch("remote", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(ResetCommand::Help) | None => print_group_help("reset"),
    }
}

fn run_commit(group: CommitGroup) -> i32 {
    match group.command {
        Some(CommitCommand::Context(raw)) => commit::dispatch("context", &raw.args),
        Some(CommitCommand::ContextJson(raw)) => commit::dispatch("context-json", &raw.args),
        Some(CommitCommand::ToStash(raw)) => commit::dispatch("to-stash", &raw.args),
        Some(CommitCommand::Help) | None => print_group_help("commit"),
    }
}

fn run_branch(group: BranchGroup) -> i32 {
    match group.command {
        Some(BranchCommand::Cleanup(raw)) => {
            branch::dispatch("cleanup", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(BranchCommand::Help) | None => print_group_help("branch"),
    }
}

fn run_ci(group: CiGroup) -> i32 {
    match group.command {
        Some(CiCommand::Pick(raw)) => ci::dispatch("pick", &raw.args).unwrap_or(exit::USAGE),
        Some(CiCommand::Help) | None => print_group_help("ci"),
    }
}

fn run_worktree(group: WorktreeGroup) -> i32 {
    match group.command {
        Some(WorktreeCommand::Add(raw)) => {
            worktree::dispatch("add", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::AdoptDirty(raw)) => {
            worktree::dispatch("adopt-dirty", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::DirtySnapshot(raw)) => {
            worktree::dispatch("dirty-snapshot", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::List(raw)) => {
            worktree::dispatch("list", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::Remove(raw)) => {
            worktree::dispatch("remove", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::Prune(raw)) => {
            worktree::dispatch("prune", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::RevokeDirty(raw)) => {
            worktree::dispatch("revoke-dirty", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::Go(raw)) => {
            worktree::dispatch("go", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(WorktreeCommand::Help) | None => print_group_help("worktree"),
    }
}

fn run_open(group: OpenGroup) -> i32 {
    match group.command {
        Some(OpenCommand::Repo(raw)) => open::dispatch("repo", &raw.args).unwrap_or(exit::USAGE),
        Some(OpenCommand::Branch(raw)) => {
            open::dispatch("branch", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::DefaultBranch(raw)) => {
            open::dispatch("default-branch", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::Commit(raw)) => {
            open::dispatch("commit", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::Compare(raw)) => {
            open::dispatch("compare", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::Pr(raw)) => open::dispatch("pr", &raw.args).unwrap_or(exit::USAGE),
        Some(OpenCommand::Pulls(raw)) => open::dispatch("pulls", &raw.args).unwrap_or(exit::USAGE),
        Some(OpenCommand::Issues(raw)) => {
            open::dispatch("issues", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::Actions(raw)) => {
            open::dispatch("actions", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::Releases(raw)) => {
            open::dispatch("releases", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::Tags(raw)) => open::dispatch("tags", &raw.args).unwrap_or(exit::USAGE),
        Some(OpenCommand::Commits(raw)) => {
            open::dispatch("commits", &raw.args).unwrap_or(exit::USAGE)
        }
        Some(OpenCommand::File(raw)) => open::dispatch("file", &raw.args).unwrap_or(exit::USAGE),
        Some(OpenCommand::Blame(raw)) => open::dispatch("blame", &raw.args).unwrap_or(exit::USAGE),
        Some(OpenCommand::Help) | None => print_group_help("open"),
    }
}

fn run_completion(raw: RawArgs) -> i32 {
    let Some(shell) = raw.args.first() else {
        eprintln!("usage: git-cli completion <bash|zsh>");
        return exit::USAGE;
    };
    completion::dispatch(shell, &raw.args[1..])
}

fn print_parse_error(err: clap::Error) -> i32 {
    let kind = err.kind();
    if let Err(print_err) = err.print() {
        eprintln!("{print_err}");
        return exit::RUNTIME;
    }

    if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        exit::SUCCESS
    } else {
        exit::USAGE
    }
}

fn print_root_help() -> i32 {
    let mut command = Cli::command();
    if let Err(err) = command.print_help() {
        eprintln!("{err}");
        return exit::RUNTIME;
    }
    println!();
    exit::SUCCESS
}

fn print_group_help(group: &str) -> i32 {
    let mut command = Cli::command();
    let Some(group_command) = command.find_subcommand_mut(group) else {
        return exit::USAGE;
    };
    if let Err(err) = group_command.print_help() {
        eprintln!("{err}");
        return exit::RUNTIME;
    }
    println!();
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_parses_help_when_no_command() {
        let code = run_from(["git-cli"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_parses_version_flag() {
        let code = run_from(["git-cli", "-V"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_unknown_group_exits_usage() {
        let code = run_from(["git-cli", "nope"]);
        assert_eq!(code, exit::USAGE);
    }

    #[test]
    fn clap_unknown_nested_subcommand_exits_usage() {
        let code = run_from(["git-cli", "utils", "nope"]);
        assert_eq!(code, exit::USAGE);
    }

    #[test]
    fn clap_dispatches_nested_subcommand_raw_args() {
        let cli =
            Cli::try_parse_from(["git-cli", "utils", "copy-staged", "--both"]).expect("parse");

        let Some(Group::Utils(group)) = cli.group else {
            panic!("expected utils group");
        };
        let Some(UtilsCommand::CopyStaged(raw)) = group.command else {
            panic!("expected copy-staged command");
        };
        assert_eq!(raw.args, vec!["--both".to_string()]);
    }

    #[test]
    fn clap_dispatches_worktree_raw_args() {
        let cli =
            Cli::try_parse_from(["git-cli", "worktree", "add", "topic-one", "--from", "main"])
                .expect("parse");

        let Some(Group::Worktree(group)) = cli.group else {
            panic!("expected worktree group");
        };
        let Some(WorktreeCommand::Add(raw)) = group.command else {
            panic!("expected add command");
        };
        assert_eq!(
            raw.args,
            vec![
                "topic-one".to_string(),
                "--from".to_string(),
                "main".to_string()
            ]
        );
    }
}
