pub const NOT_GIT_REPO: &str = "❗ Not a Git repository. Run this command inside a Git project.";
pub const UNKNOWN_COMMAND_HINT: &str = "Run 'git-lock help' for usage.";
pub const COPY_USAGE: &str = "❗ Usage: git-lock copy <source-label> <target-label>";
pub const TARGET_LABEL_MISSING: &str = "❗ Target label is missing";
pub const NO_GIT_LOCKS_FOUND: &str = "❌ No git-locks found";
pub const DIFF_USAGE: &str = "❗ Usage: git-lock diff <label1> <label2> [--no-color]";
pub const TAG_USAGE: &str =
    "❗ Usage: git-lock tag <git-lock-label> <tag-name> [-m <tag-message>] [--push]";

pub fn subcommand_help(subcmd: &str) -> Option<&'static str> {
    let text = match subcmd {
        "lock" => {
            "Usage: git-lock lock [label] [note] [commit]\n  Save HEAD (or <commit>) to a lock label."
        }
        "unlock" => {
            "Usage: git-lock unlock [label]\n  Reset HEAD to the commit recorded in the lock."
        }
        "list" => "Usage: git-lock list\n  Show every lock for the current repository.",
        "copy" => {
            "Usage: git-lock copy <source-label> <target-label>\n  Duplicate a lock under a new label."
        }
        "delete" => {
            "Usage: git-lock delete [label]\n  Remove the lock entry; defaults to the latest label."
        }
        "diff" => {
            "Usage: git-lock diff <label1> <label2> [--no-color]\n  Show the commit diff between two locks."
        }
        "tag" => {
            "Usage: git-lock tag <label> <tag-name> [-m <message>] [--push]\n  Create a git tag pointing at the lock's commit."
        }
        "completion" => {
            "Usage: git-lock completion <bash|zsh>\n  Print the shell completion script."
        }
        _ => return None,
    };
    Some(text)
}

pub fn unknown_command(cmd: &str) -> String {
    format!("❗ Unknown command: '{cmd}'")
}

pub fn print_help() {
    println!("Save and restore named Git commit locks.");
    println!();
    println!("Usage: git-lock <command> [args]");
    println!();
    println!("Commands:");
    println!(
        "  {:<16}  Save commit hash to lock",
        "lock [label] [note] [commit]"
    );
    println!("  {:<16}  Reset to a saved commit", "unlock [label]");
    println!("  {:<16}  Show all locks for repo", "list");
    println!("  {:<16}  Duplicate a lock label", "copy <from> <to>");
    println!("  {:<16}  Remove a lock", "delete [label]");
    println!(
        "  {:<16}  Compare commits between two locks",
        "diff <l1> <l2> [--no-color]"
    );
    println!(
        "  {:<16}  Create git tag from a lock",
        "tag <label> <tag> [-m msg]"
    );
    println!(
        "  {:<16}  Export shell completion script",
        "completion <shell>"
    );
    println!("  {:<16}  Show version", "-V, --version");
    println!();
    println!("EXAMPLES:");
    println!("  git-lock lock release-point");
    println!("  git-lock list");
    println!("  git-lock diff before after");
    println!("  git-lock completion zsh");
    println!();
    println!("ENVIRONMENT:");
    println!("  ZSH_CACHE_DIR  Base cache directory for lock storage.");
    println!();
    println!("EXIT CODES:");
    println!("  0   success");
    println!("  1   runtime error");
    println!("  64  command-line usage error");
    println!();
}
