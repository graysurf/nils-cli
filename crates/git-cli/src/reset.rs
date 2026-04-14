use crate::prompt;
use nils_common::git as common_git;
use std::io::{self, BufRead, Write};
use std::process::Output;

pub fn dispatch(cmd: &str, args: &[String]) -> Option<i32> {
    match cmd {
        "soft" => Some(reset_by_count("soft", args)),
        "mixed" => Some(reset_by_count("mixed", args)),
        "hard" => Some(reset_by_count("hard", args)),
        "undo" => Some(reset_undo()),
        "back-head" => Some(back_head()),
        "back-checkout" => Some(back_checkout()),
        "remote" => Some(reset_remote(args)),
        _ => None,
    }
}

fn reset_by_count(mode: &str, args: &[String]) -> i32 {
    let count_arg = args.first();
    let extra_arg = args.get(1);
    if extra_arg.is_some() {
        eprintln!("❌ Too many arguments.");
        eprintln!("Usage: git-reset-{mode} [N]");
        return 2;
    }

    let count = match count_arg {
        Some(value) => match parse_positive_int(value) {
            Some(value) => value,
            None => {
                eprintln!("❌ Invalid commit count: {value} (must be a positive integer).");
                eprintln!("Usage: git-reset-{mode} [N]");
                return 2;
            }
        },
        None => 1,
    };

    let target = format!("HEAD~{count}");
    if !git_success(&["rev-parse", "--verify", "--quiet", &target]) {
        eprintln!("❌ Cannot resolve {target} (not enough commits?).");
        return 1;
    }

    let commit_label = if count > 1 {
        format!("last {count} commits")
    } else {
        "last commit".to_string()
    };

    let (preface, prompt, failure, success) = match mode {
        "soft" => (
            vec![
                format!("⚠️  This will rewind your {commit_label} (soft reset)"),
                "🧠 Your changes will remain STAGED. Useful for rewriting commit message."
                    .to_string(),
            ],
            format!("❓ Proceed with 'git reset --soft {target}'? [y/N] "),
            "❌ Soft reset failed.".to_string(),
            "✅ Reset completed. Your changes are still staged.".to_string(),
        ),
        "mixed" => (
            vec![
                format!("⚠️  This will rewind your {commit_label} (mixed reset)"),
                "🧠 Your changes will become UNSTAGED and editable in working directory."
                    .to_string(),
            ],
            format!("❓ Proceed with 'git reset --mixed {target}'? [y/N] "),
            "❌ Mixed reset failed.".to_string(),
            "✅ Reset completed. Your changes are now unstaged.".to_string(),
        ),
        "hard" => (
            vec![
                format!("⚠️  This will HARD RESET your repository to {target}."),
                "🔥 Tracked staged/unstaged changes will be OVERWRITTEN.".to_string(),
                format!("🧨 This is equivalent to: git reset --hard {target}"),
            ],
            "❓ Are you absolutely sure? [y/N] ".to_string(),
            "❌ Hard reset failed.".to_string(),
            format!("✅ Hard reset completed. HEAD moved back to {target}."),
        ),
        _ => {
            eprintln!("❌ Unknown reset mode: {mode}");
            return 2;
        }
    };

    for line in preface {
        println!("{line}");
    }
    println!("🧾 Commits to be rewound:");

    let output = match git_output(&[
        "log",
        "--no-color",
        "-n",
        &count.to_string(),
        "--date=format:%m-%d %H:%M",
        "--pretty=%h %ad %an  %s",
    ]) {
        Some(output) => output,
        None => return 1,
    };
    if !output.status.success() {
        emit_output(&output);
        return exit_code(&output);
    }
    emit_output(&output);

    if !confirm_or_abort(&prompt) {
        return 1;
    }

    let code = git_status(&["reset", &format!("--{mode}"), &target]).unwrap_or(1);
    if code != 0 {
        println!("{failure}");
        return 1;
    }

    println!("{success}");
    0
}

fn reset_undo() -> i32 {
    if !git_success(&["rev-parse", "--is-inside-work-tree"]) {
        println!("❌ Not a git repository.");
        return 1;
    }

    let target_commit = match git_stdout_trimmed(&["rev-parse", "HEAD@{1}"]).and_then(non_empty) {
        Some(value) => value,
        None => {
            println!("❌ Cannot resolve HEAD@{{1}} (no previous HEAD position in reflog).");
            return 1;
        }
    };

    let op_warnings = detect_in_progress_ops();
    if !op_warnings.is_empty() {
        println!("🛡️  Detected an in-progress Git operation:");
        for warning in op_warnings {
            println!("   - {warning}");
        }
        println!("⚠️  Resetting during these operations can be confusing.");
        if !confirm_or_abort("❓ Still run git-reset-undo (move HEAD back)? [y/N] ") {
            return 1;
        }
    }

    let mut reflog_line_current =
        git_stdout_trimmed(&["reflog", "-1", "--pretty=%h %gs", "HEAD@{0}"]).and_then(non_empty);
    let mut reflog_subject_current =
        git_stdout_trimmed(&["reflog", "-1", "--pretty=%gs", "HEAD@{0}"]).and_then(non_empty);

    if reflog_line_current.is_none() || reflog_subject_current.is_none() {
        reflog_line_current =
            git_stdout_trimmed(&["reflog", "show", "-1", "--pretty=%h %gs", "HEAD"])
                .and_then(non_empty);
        reflog_subject_current =
            git_stdout_trimmed(&["reflog", "show", "-1", "--pretty=%gs", "HEAD"])
                .and_then(non_empty);
    }

    let mut reflog_line_target =
        git_stdout_trimmed(&["reflog", "-1", "--pretty=%h %gs", "HEAD@{1}"]).and_then(non_empty);
    if reflog_line_target.is_none() {
        reflog_line_target = reflog_show_line(2, "%h %gs").and_then(non_empty);
    }

    let line_current = reflog_line_current.unwrap_or_else(|| "(unavailable)".to_string());
    let line_target = reflog_line_target.unwrap_or_else(|| "(unavailable)".to_string());
    let subject_current = reflog_subject_current.unwrap_or_else(|| "(unavailable)".to_string());

    println!("🧾 Current HEAD@{{0}} (last action):");
    println!("   {line_current}");
    println!("🧾 Target  HEAD@{{1}} (previous HEAD position):");
    println!("   {line_target}");

    if line_current == "(unavailable)" || line_target == "(unavailable)" {
        println!(
            "ℹ️  Reflog display unavailable here; reset target is still the resolved SHA: {target_commit}"
        );
    }

    if subject_current != "(unavailable)" && !subject_current.starts_with("reset:") {
        println!("⚠️  The last action does NOT look like a reset operation.");
        println!("🧠 It may be from checkout/rebase/merge/pull, etc.");
        if !confirm_or_abort(
            "❓ Still proceed to move HEAD back to the previous HEAD position? [y/N] ",
        ) {
            return 1;
        }
    }

    println!("🕰  Target commit (resolved from HEAD@{{1}}):");
    let log_output = match git_output(&["log", "--oneline", "-1", &target_commit]) {
        Some(output) => output,
        None => return 1,
    };
    if !log_output.status.success() {
        emit_output(&log_output);
        return exit_code(&log_output);
    }
    emit_output(&log_output);

    let status_lines = match git_stdout_raw(&["status", "--porcelain"]) {
        Some(value) => value,
        None => return 1,
    };

    if status_lines.trim().is_empty() {
        println!("✅ Working tree clean. Proceeding with: git reset --hard {target_commit}");
        let code = git_status(&["reset", "--hard", &target_commit]).unwrap_or(1);
        if code != 0 {
            println!("❌ Hard reset failed.");
            return 1;
        }
        println!("✅ Repository reset back to previous HEAD: {target_commit}");
        return 0;
    }

    println!("⚠️  Working tree has changes:");
    print!("{status_lines}");
    if !status_lines.ends_with('\n') {
        println!();
    }
    println!();
    println!("Choose how to proceed:");
    println!(
        "  1) Keep changes + PRESERVE INDEX (staged vs new base)  (git reset --soft  {target_commit})"
    );
    println!(
        "  2) Keep changes + UNSTAGE ALL                          (git reset --mixed {target_commit})"
    );
    println!(
        "  3) Discard tracked changes                             (git reset --hard  {target_commit})"
    );
    println!("  4) Abort");

    let choice = match read_line("❓ Select [1/2/3/4] (default: 4): ") {
        Ok(value) => value,
        Err(_) => {
            println!("🚫 Aborted");
            return 1;
        }
    };

    match choice.as_str() {
        "1" => {
            println!(
                "🧷 Preserving INDEX (staged) and working tree. Running: git reset --soft {target_commit}"
            );
            println!(
                "⚠️  Note: The index is preserved, but what appears staged is relative to the new HEAD."
            );
            let code = git_status(&["reset", "--soft", &target_commit]).unwrap_or(1);
            if code != 0 {
                println!("❌ Soft reset failed.");
                return 1;
            }
            println!("✅ HEAD moved back while preserving index + working tree: {target_commit}");
            0
        }
        "2" => {
            println!(
                "🧷 Preserving working tree but clearing INDEX (unstage all). Running: git reset --mixed {target_commit}"
            );
            let code = git_status(&["reset", "--mixed", &target_commit]).unwrap_or(1);
            if code != 0 {
                println!("❌ Mixed reset failed.");
                return 1;
            }
            println!("✅ HEAD moved back; working tree preserved; index reset: {target_commit}");
            0
        }
        "3" => {
            println!("🔥 Discarding tracked changes. Running: git reset --hard {target_commit}");
            println!("⚠️  This overwrites tracked files in working tree + index.");
            println!("ℹ️  Untracked files are NOT removed by reset --hard.");
            if !confirm_or_abort("❓ Are you absolutely sure? [y/N] ") {
                return 1;
            }
            let code = git_status(&["reset", "--hard", &target_commit]).unwrap_or(1);
            if code != 0 {
                println!("❌ Hard reset failed.");
                return 1;
            }
            println!("✅ Repository reset back to previous HEAD: {target_commit}");
            0
        }
        _ => {
            println!("🚫 Aborted");
            1
        }
    }
}

fn back_head() -> i32 {
    let prev_head = match git_stdout_trimmed(&["rev-parse", "HEAD@{1}"]).and_then(non_empty) {
        Some(value) => value,
        None => {
            println!("❌ Cannot find previous HEAD in reflog.");
            return 1;
        }
    };

    println!("⏪ This will move HEAD back to the previous position (HEAD@{{1}}):");
    if let Some(oneline) = git_stdout_trimmed(&["log", "--oneline", "-1", &prev_head]) {
        println!("🔁 {oneline}");
    }
    if !confirm_or_abort("❓ Proceed with 'git checkout HEAD@{1}'? [y/N] ") {
        return 1;
    }

    let code = git_status(&["checkout", "HEAD@{1}"]).unwrap_or(1);
    if code != 0 {
        println!("❌ Checkout failed (likely due to local changes or invalid reflog state).");
        return 1;
    }

    println!("✅ Restored to previous HEAD (HEAD@{{1}}): {prev_head}");
    0
}

fn back_checkout() -> i32 {
    let current_branch =
        match git_stdout_trimmed(&["rev-parse", "--abbrev-ref", "HEAD"]).and_then(non_empty) {
            Some(value) => value,
            None => {
                println!("❌ Cannot determine current branch.");
                return 1;
            }
        };

    if current_branch == "HEAD" {
        println!(
            "❌ You are in a detached HEAD state. This function targets branch-to-branch checkouts."
        );
        println!(
            "🧠 Tip: Use `git reflog` to find the branch/commit you want, then `git checkout <branch>`."
        );
        return 1;
    }

    let from_branch = match find_previous_checkout(&current_branch) {
        Some(value) => value,
        None => {
            println!("❌ Could not find a previous checkout that switched to {current_branch}.");
            return 1;
        }
    };

    if from_branch.len() >= 7
        && from_branch.len() <= 40
        && from_branch.chars().all(|c| c.is_ascii_hexdigit())
    {
        println!(
            "❌ Previous 'from' looks like a commit SHA ({from_branch}). Refusing to checkout to avoid detached HEAD."
        );
        println!("🧠 Use `git reflog` to choose the correct branch explicitly.");
        return 1;
    }

    if !git_success(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{from_branch}"),
    ]) {
        println!("❌ '{from_branch}' is not an existing local branch.");
        println!("🧠 If it's a remote branch, try: git checkout -t origin/{from_branch}");
        return 1;
    }

    println!("⏪ This will move HEAD back to previous branch: {from_branch}");
    if !confirm_or_abort(&format!(
        "❓ Proceed with 'git checkout {from_branch}'? [y/N] "
    )) {
        return 1;
    }

    let code = git_status(&["checkout", &from_branch]).unwrap_or(1);
    if code != 0 {
        println!("❌ Checkout failed (likely due to local changes or conflicts).");
        return 1;
    }

    println!("✅ Restored to previous branch: {from_branch}");
    0
}

fn reset_remote(args: &[String]) -> i32 {
    let mut want_help = false;
    let mut want_yes = false;
    let mut want_fetch = true;
    let mut want_prune = false;
    let mut want_clean = false;
    let mut want_set_upstream = false;
    let mut remote_arg: Option<String> = None;
    let mut branch_arg: Option<String> = None;
    let mut ref_arg: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-h" | "--help" => {
                want_help = true;
            }
            "-y" | "--yes" => {
                want_yes = true;
            }
            "-r" | "--remote" => {
                let Some(value) = args.get(i + 1) else {
                    return 2;
                };
                remote_arg = Some(value.to_string());
                i += 1;
            }
            "-b" | "--branch" => {
                let Some(value) = args.get(i + 1) else {
                    return 2;
                };
                branch_arg = Some(value.to_string());
                i += 1;
            }
            "--ref" => {
                let Some(value) = args.get(i + 1) else {
                    return 2;
                };
                ref_arg = Some(value.to_string());
                i += 1;
            }
            "--no-fetch" => {
                want_fetch = false;
            }
            "--prune" => {
                want_prune = true;
            }
            "--clean" => {
                want_clean = true;
            }
            "--set-upstream" => {
                want_set_upstream = true;
            }
            _ => {}
        }
        i += 1;
    }

    if want_help {
        print_reset_remote_help();
        return 0;
    }

    let mut remote = remote_arg.clone().unwrap_or_default();
    let mut remote_branch = branch_arg.clone().unwrap_or_default();

    if let Some(reference) = ref_arg {
        let Some((remote_ref, branch_ref)) = reference.split_once('/') else {
            eprintln!("❌ --ref must look like '<remote>/<branch>' (got: {reference})");
            return 2;
        };
        remote = remote_ref.to_string();
        remote_branch = branch_ref.to_string();
    }

    if !git_success(&["rev-parse", "--git-dir"]) {
        eprintln!("❌ Not inside a Git repository.");
        return 1;
    }

    let current_branch = match git_stdout_trimmed(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .and_then(non_empty)
    {
        Some(value) => value,
        None => {
            eprintln!("❌ Detached HEAD. Switch to a branch first.");
            return 1;
        }
    };

    let upstream =
        git_stdout_trimmed(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .unwrap_or_default();

    if remote.is_empty()
        && let Some((remote_ref, _)) = upstream.split_once('/')
    {
        remote = remote_ref.to_string();
    }
    if remote.is_empty() {
        remote = "origin".to_string();
    }

    if remote_branch.is_empty() {
        if let Some((_, branch_ref)) = upstream.split_once('/')
            && branch_ref != "HEAD"
        {
            remote_branch = branch_ref.to_string();
        }
        if remote_branch.is_empty() {
            remote_branch = current_branch.clone();
        }
    }

    let target_ref = format!("{remote}/{remote_branch}");

    if want_fetch {
        let fetch_args = if want_prune {
            vec!["fetch", "--prune", "--", &remote]
        } else {
            vec!["fetch", "--", &remote]
        };
        let code = git_status(&fetch_args).unwrap_or(1);
        if code != 0 {
            return code;
        }
    }

    if !git_success(&[
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/remotes/{remote}/{remote_branch}"),
    ]) {
        eprintln!("❌ Remote-tracking branch not found: {target_ref}");
        eprintln!("   Try: git fetch --prune -- {remote}");
        eprintln!("   Or verify: git branch -r | rg -n -- \"^\\\\s*{remote}/{remote_branch}$\"");
        return 1;
    }

    let status_porcelain = git_stdout_raw(&["status", "--porcelain"]).unwrap_or_default();
    if !want_yes {
        println!("⚠️  This will OVERWRITE local branch '{current_branch}' with '{target_ref}'.");
        if !status_porcelain.trim().is_empty() {
            println!("🔥 Tracked staged/unstaged changes will be DISCARDED by --hard.");
            println!("🧹 Untracked files will be kept (use --clean to remove).");
        }
        if !confirm_or_abort(&format!(
            "❓ Proceed with: git reset --hard {target_ref} ? [y/N] "
        )) {
            return 1;
        }
    }

    let code = git_status(&["reset", "--hard", &target_ref]).unwrap_or(1);
    if code != 0 {
        return code;
    }

    if want_clean {
        if !want_yes {
            println!("⚠️  Next: git clean -fd (removes untracked files/dirs)");
            let ok = prompt::confirm("❓ Proceed with: git clean -fd ? [y/N] ").unwrap_or_default();
            if !ok {
                println!("ℹ️  Skipped git clean -fd");
                want_clean = false;
            }
        }
        if want_clean {
            let code = git_status(&["clean", "-fd"]).unwrap_or(1);
            if code != 0 {
                return code;
            }
        }
    }

    if want_set_upstream || upstream.is_empty() {
        let _ = git_status(&["branch", "--set-upstream-to", &target_ref, &current_branch]);
    }

    println!("✅ Done. '{current_branch}' now matches '{target_ref}'.");
    0
}

fn parse_positive_int(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let value = raw.parse::<i64>().ok()?;
    if value <= 0 { None } else { Some(value) }
}

fn detect_in_progress_ops() -> Vec<String> {
    let mut warnings = Vec::new();
    if git_path_exists("MERGE_HEAD", true) {
        warnings.push("merge in progress (suggest: git merge --abort)".to_string());
    }
    if git_path_exists("rebase-apply", false) || git_path_exists("rebase-merge", false) {
        warnings.push("rebase in progress (suggest: git rebase --abort)".to_string());
    }
    if git_path_exists("CHERRY_PICK_HEAD", true) {
        warnings.push("cherry-pick in progress (suggest: git cherry-pick --abort)".to_string());
    }
    if git_path_exists("REVERT_HEAD", true) {
        warnings.push("revert in progress (suggest: git revert --abort)".to_string());
    }
    if git_path_exists("BISECT_LOG", true) {
        warnings.push("bisect in progress (suggest: git bisect reset)".to_string());
    }
    warnings
}

fn git_path_exists(name: &str, is_file: bool) -> bool {
    let output = git_stdout_trimmed(&["rev-parse", "--git-path", name]);
    let Some(path) = output else {
        return false;
    };
    let path = std::path::Path::new(&path);
    if is_file {
        path.is_file()
    } else {
        path.is_dir()
    }
}

fn reflog_show_line(index: usize, pretty: &str) -> Option<String> {
    let output = git_stdout_raw(&[
        "reflog",
        "show",
        "-2",
        &format!("--pretty={pretty}"),
        "HEAD",
    ])?;
    output.lines().nth(index - 1).map(|line| line.to_string())
}

fn find_previous_checkout(current_branch: &str) -> Option<String> {
    let output = git_stdout_raw(&["reflog", "--format=%gs"])?;
    for line in output.lines() {
        if !line.starts_with("checkout: moving from ") {
            continue;
        }
        if !line.ends_with(&format!(" to {current_branch}")) {
            continue;
        }
        let mut value = line.trim_start_matches("checkout: moving from ");
        value = value.trim_end_matches(&format!(" to {current_branch}"));
        return Some(value.to_string());
    }
    None
}

fn confirm_or_abort(prompt: &str) -> bool {
    prompt::confirm_or_abort(prompt).is_ok()
}

fn read_line(prompt: &str) -> io::Result<String> {
    let mut output = io::stdout();
    output.write_all(prompt.as_bytes())?;
    output.flush()?;
    let mut input = String::new();
    io::stdin().lock().read_line(&mut input)?;
    Ok(input.trim_end_matches(['\n', '\r']).to_string())
}

fn git_output(args: &[&str]) -> Option<Output> {
    common_git::run_output(args).ok()
}

fn git_status(args: &[&str]) -> Option<i32> {
    common_git::run_status_inherit(args)
        .ok()
        .map(|status| status.code().unwrap_or(1))
}

fn git_success(args: &[&str]) -> bool {
    matches!(git_output(args), Some(output) if output.status.success())
}

fn git_stdout_trimmed(args: &[&str]) -> Option<String> {
    let output = git_output(args)?;
    if !output.status.success() {
        return None;
    }
    Some(trim_trailing_newlines(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn git_stdout_raw(args: &[&str]) -> Option<String> {
    let output = git_output(args)?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn trim_trailing_newlines(input: &str) -> String {
    input.trim_end_matches(['\n', '\r']).to_string()
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn emit_output(output: &Output) {
    let _ = io::stdout().write_all(&output.stdout);
    let _ = io::stderr().write_all(&output.stderr);
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(1)
}

fn print_reset_remote_help() {
    println!(
        "git-reset-remote: overwrite current local branch with a remote-tracking branch (DANGEROUS)"
    );
    println!();
    println!("Usage:");
    println!("  git-reset-remote  # reset current branch to its upstream (or origin/<branch>)");
    println!("  git-reset-remote --ref origin/main");
    println!("  git-reset-remote -r origin -b main");
    println!();
    println!("Options:");
    println!("  -r, --remote <name>        Remote name (default: from upstream, else origin)");
    println!(
        "  -b, --branch <name>        Remote branch name (default: from upstream, else current branch)"
    );
    println!("      --ref <remote/branch>  Shortcut for --remote/--branch");
    println!("      --no-fetch             Skip 'git fetch' (uses existing remote-tracking refs)");
    println!("      --prune                Use 'git fetch --prune'");
    println!("      --set-upstream         Set upstream of current branch to <remote>/<branch>");
    println!(
        "      --clean                After reset, optionally run 'git clean -fd' (removes untracked)"
    );
    println!("  -y, --yes                  Skip confirmations");
}

#[cfg(test)]
mod tests {
    use super::{dispatch, non_empty, parse_positive_int, trim_trailing_newlines};
    use nils_test_support::{CwdGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;

    #[test]
    fn dispatch_returns_none_for_unknown_subcommand() {
        assert_eq!(dispatch("unknown", &[]), None);
    }

    #[test]
    fn parse_positive_int_accepts_digits_only() {
        assert_eq!(parse_positive_int("1"), Some(1));
        assert_eq!(parse_positive_int("42"), Some(42));
        assert_eq!(parse_positive_int("001"), Some(1));
    }

    #[test]
    fn parse_positive_int_rejects_invalid_values() {
        assert_eq!(parse_positive_int(""), None);
        assert_eq!(parse_positive_int("0"), None);
        assert_eq!(parse_positive_int("-1"), None);
        assert_eq!(parse_positive_int("1.0"), None);
        assert_eq!(parse_positive_int("abc"), None);
    }

    #[test]
    fn trim_trailing_newlines_only_removes_line_endings() {
        assert_eq!(trim_trailing_newlines("line\n"), "line");
        assert_eq!(trim_trailing_newlines("line\r\n"), "line");
        assert_eq!(trim_trailing_newlines("line  "), "line  ");
    }

    #[test]
    fn non_empty_returns_none_for_empty_string() {
        assert_eq!(non_empty(String::new()), None);
        assert_eq!(non_empty("value".to_string()), Some("value".to_string()));
    }

    #[test]
    fn reset_by_count_modes_return_usage_errors_for_invalid_arguments() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let _cwd = CwdGuard::set(&lock, dir.path()).expect("cwd");

        let args = vec!["1".to_string(), "2".to_string()];
        assert_eq!(dispatch("soft", &args), Some(2));
        assert_eq!(dispatch("mixed", &args), Some(2));
        assert_eq!(dispatch("hard", &args), Some(2));

        let args = vec!["abc".to_string()];
        assert_eq!(dispatch("soft", &args), Some(2));
    }

    #[test]
    fn reset_by_count_returns_runtime_error_when_target_commit_missing() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let _cwd = CwdGuard::set(&lock, dir.path()).expect("cwd");
        let args = vec!["999999".to_string()];
        assert_eq!(dispatch("soft", &args), Some(1));
    }

    #[test]
    fn reset_remote_argument_parsing_covers_help_and_usage_failures() {
        let help_args = vec!["--help".to_string()];
        assert_eq!(dispatch("remote", &help_args), Some(0));

        let bad_ref_args = vec!["--ref".to_string(), "invalid".to_string()];
        assert_eq!(dispatch("remote", &bad_ref_args), Some(2));

        let missing_remote_value = vec!["--remote".to_string()];
        assert_eq!(dispatch("remote", &missing_remote_value), Some(2));
    }
}
