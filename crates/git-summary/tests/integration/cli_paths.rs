use crate::common;
use common::{git, init_repo, run_git_summary};
use std::fs;
use std::path::Path;

const SEPARATOR: &str = "----------------------------------------------------------------------------------------------------------------------------------------";

fn summary_table_header() -> String {
    format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Name", "Email", "Added", "Deleted", "Net", "Commits", "First", "Last"
    )
}

fn seed_single_commit(root: &Path) {
    fs::write(root.join("seed.txt"), "seed\n").expect("write seed file");
    git(root, &["add", "seed.txt"]);
    git(root, &["commit", "-m", "seed"]);
}

#[test]
fn no_args_and_help_aliases_print_usage() {
    let temp = tempfile::TempDir::new().unwrap();

    let no_args_output = run_git_summary(temp.path(), &[], &[]);
    assert!(no_args_output.contains("Usage: git-summary <command> [args]"));
    assert!(no_args_output.contains("Commands:"));
    assert!(no_args_output.contains("Entire history"));

    for args in [&["help"][..], &["--help"][..], &["-h"][..]] {
        let output = run_git_summary(temp.path(), args, &[]);
        assert!(output.contains("Usage: git-summary <command> [args]"));
        assert!(output.contains("Commands:"));
        assert!(output.contains("Custom date range (YYYY-MM-DD)"));
    }
}

#[test]
fn shortcut_commands_print_stable_headers() {
    let repo = init_repo();
    let root = repo.path();
    seed_single_commit(root);

    let table_header = summary_table_header();
    let cases = [
        ("all", "Git summary for all commits"),
        ("today", "Git summary for today:"),
        ("yesterday", "Git summary for yesterday:"),
        ("this-month", "Git summary for this month:"),
        ("last-month", "Git summary for last month:"),
        ("this-week", "Git summary for this week:"),
        ("last-week", "Git summary for last week:"),
    ];

    for (cmd, expected_header) in cases {
        let output = run_git_summary(root, &[cmd], &[]);
        assert!(
            output.contains(expected_header),
            "missing header for `{cmd}`: {output}"
        );
        assert!(
            output.contains(&table_header),
            "missing table header for `{cmd}`: {output}"
        );
        assert!(
            output.contains(SEPARATOR),
            "missing separator for `{cmd}`: {output}"
        );
    }
}

#[test]
fn reports_missing_git_in_path() {
    let temp = tempfile::TempDir::new().unwrap();

    let stub = tempfile::TempDir::new().unwrap();
    let git_path = stub.path().join("git");
    fs::write(&git_path, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&git_path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&git_path, perms).unwrap();
    }

    let path_env = stub.path().to_string_lossy().to_string();
    let (code, output) =
        common::run_git_summary_allow_fail(temp.path(), &["all"], &[("PATH", path_env.as_str())]);

    assert_ne!(code, 0);
    assert!(output.contains("git is required but was not found in PATH."));
}
