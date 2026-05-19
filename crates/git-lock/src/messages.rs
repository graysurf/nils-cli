pub const NOT_GIT_REPO: &str = "❗ Not a Git repository. Run this command inside a Git project.";
pub const UNKNOWN_COMMAND_HINT: &str = "Run 'git-lock help' for usage.";
pub const COPY_USAGE: &str = "❗ Usage: git-lock copy <source-label> <target-label>";
pub const TARGET_LABEL_MISSING: &str = "❗ Target label is missing";
pub const NO_GIT_LOCKS_FOUND: &str = "❌ No git-locks found";
pub const DIFF_USAGE: &str = "❗ Usage: git-lock diff <label1> <label2> [--no-color]";
pub const TAG_USAGE: &str =
    "❗ Usage: git-lock tag <git-lock-label> <tag-name> [-m <tag-message>] [--push]";

pub fn unknown_command(cmd: &str) -> String {
    format!("❗ Unknown command: '{cmd}'")
}
