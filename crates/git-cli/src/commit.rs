use crate::clipboard;
use crate::commit_json;
use crate::commit_shared::{
    DiffNumstat, diff_numstat, git_output, git_status_success, git_stdout_trimmed_optional,
    is_lockfile, parse_name_status_z, trim_trailing_newlines,
};
use crate::prompt;
use anyhow::{Result, anyhow};
use nils_common::env as shared_env;
use nils_common::git::{self as common_git, GitContextError};
use nils_common::process;
use nils_common::shell::{AnsiStripMode, strip_ansi as strip_ansi_impl};
use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Clipboard,
    Stdout,
    Both,
}

struct ContextArgs {
    mode: OutputMode,
    no_color: bool,
    include_patterns: Vec<String>,
    extra_args: Vec<String>,
}

enum ParseOutcome<T> {
    Continue(T),
    Exit(i32),
}

enum CommitCommand {
    Context,
    ContextJson,
    ToStash,
}

pub fn dispatch(cmd_raw: &str, args: &[String]) -> i32 {
    match parse_command(cmd_raw) {
        Some(CommitCommand::Context) => run_context(args),
        Some(CommitCommand::ContextJson) => commit_json::run(args),
        Some(CommitCommand::ToStash) => run_to_stash(args),
        None => {
            eprintln!("Unknown commit command: {cmd_raw}");
            2
        }
    }
}

fn parse_command(raw: &str) -> Option<CommitCommand> {
    match raw {
        "context" => Some(CommitCommand::Context),
        "context-json" | "context_json" | "contextjson" | "json" => {
            Some(CommitCommand::ContextJson)
        }
        "to-stash" | "stash" => Some(CommitCommand::ToStash),
        _ => None,
    }
}

fn run_context(args: &[String]) -> i32 {
    if !ensure_git_work_tree() {
        return 1;
    }

    let parsed = match parse_context_args(args) {
        ParseOutcome::Continue(value) => value,
        ParseOutcome::Exit(code) => return code,
    };

    if !parsed.extra_args.is_empty() {
        eprintln!(
            "⚠️  Ignoring unknown arguments: {}",
            parsed.extra_args.join(" ")
        );
    }

    let diff_output = match git_output(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--cached",
        "--no-color",
    ]) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };
    let diff_raw = String::from_utf8_lossy(&diff_output.stdout).to_string();
    let diff = trim_trailing_newlines(&diff_raw);

    if diff.trim().is_empty() {
        eprintln!("⚠️  No staged changes to record");
        return 1;
    }

    if !git_scope_available() {
        eprintln!("❗ git-scope is required but was not found in PATH.");
        return 1;
    }

    let scope = match git_scope_output(parsed.no_color) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    let contents = match build_staged_contents(&parsed.include_patterns) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    let context = format!(
        "# Commit Context\n\n## Input expectations\n\n- Full-file reads are not required for commit message generation.\n- Base the message on staged diff, scope tree, and staged (index) version content.\n\n---\n\n## 📂 Scope and file tree:\n\n```text\n{scope}\n```\n\n## 📄 Git staged diff:\n\n```diff\n{diff}\n```\n\n  ## 📚 Staged file contents (index version):\n\n{contents}"
    );

    let context_with_newline = format!("{context}\n");

    match parsed.mode {
        OutputMode::Stdout => {
            println!("{context}");
        }
        OutputMode::Both => {
            println!("{context}");
            let _ = clipboard::set_clipboard_best_effort(&context_with_newline);
        }
        OutputMode::Clipboard => {
            let _ = clipboard::set_clipboard_best_effort(&context_with_newline);
            println!("✅ Commit context copied to clipboard with:");
            println!("  • Diff");
            println!("  • Scope summary (via git-scope staged)");
            println!("  • Staged file contents (index version)");
        }
    }

    0
}

fn parse_context_args(args: &[String]) -> ParseOutcome<ContextArgs> {
    let mut mode = OutputMode::Clipboard;
    let mut no_color = false;
    let mut include_patterns: Vec<String> = Vec::new();
    let mut extra_args: Vec<String> = Vec::new();

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--stdout" | "-p" | "--print" => mode = OutputMode::Stdout,
            "--both" => mode = OutputMode::Both,
            "--no-color" | "no-color" => no_color = true,
            "--include" => {
                let value = iter.next().map(|v| v.to_string()).unwrap_or_default();
                if value.is_empty() {
                    eprintln!("❌ Missing value for --include");
                    return ParseOutcome::Exit(2);
                }
                include_patterns.push(value);
            }
            value if value.starts_with("--include=") => {
                include_patterns.push(value.trim_start_matches("--include=").to_string());
            }
            "--help" | "-h" => {
                print_context_usage();
                return ParseOutcome::Exit(0);
            }
            other => extra_args.push(other.to_string()),
        }
    }

    ParseOutcome::Continue(ContextArgs {
        mode,
        no_color,
        include_patterns,
        extra_args,
    })
}

fn print_context_usage() {
    println!(
        "Usage: git-cli commit context [-p|--stdout|--both] [--no-color] [--include <path/glob>]"
    );
    println!("  -p, --stdout, --print   Print commit context to stdout only");
    println!("  --both                  Print to stdout and copy to clipboard");
    println!("  --no-color              Disable ANSI colors (also via NO_COLOR)");
    println!("  --include               Show full content for selected paths (repeatable)");
}

fn git_scope_available() -> bool {
    if env::var("GIT_CLI_FIXTURE_GIT_SCOPE_MODE").ok().as_deref() == Some("missing") {
        return false;
    }
    process::cmd_exists("git-scope")
}

fn git_scope_output(no_color: bool) -> Result<String> {
    let mut args: Vec<&str> = vec!["staged"];
    if shared_env::no_color_requested(no_color) {
        args.push("--no-color");
    }

    let output = Command::new("git-scope")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| anyhow!("git-scope failed: {err}"))?;

    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    let stripped = strip_ansi(&raw);
    Ok(trim_trailing_newlines(&stripped))
}

fn strip_ansi(input: &str) -> String {
    strip_ansi_impl(input, AnsiStripMode::CsiSgrOnly).into_owned()
}

fn build_staged_contents(include_patterns: &[String]) -> Result<String> {
    let output = git_output(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--cached",
        "--name-status",
        "-z",
    ])?;

    let entries = parse_name_status_z(&output.stdout)?;
    let mut out = String::new();

    for entry in entries {
        let (display_path, content_path, head_path) = match &entry.old_path {
            Some(old) => (
                format!("{old} -> {}", entry.path),
                entry.path.clone(),
                old.to_string(),
            ),
            None => (entry.path.clone(), entry.path.clone(), entry.path.clone()),
        };

        out.push_str(&format!("### {display_path} ({})\n\n", entry.status_raw));

        let mut include_content = false;
        for pattern in include_patterns {
            if !pattern.is_empty() && pattern_matches(pattern, &content_path) {
                include_content = true;
                break;
            }
        }

        let lockfile = is_lockfile(&content_path);
        let diff = diff_numstat(&content_path).unwrap_or(DiffNumstat {
            added: None,
            deleted: None,
            binary: false,
        });

        let mut binary_file = diff.binary;
        let mut blob_type: Option<String> = None;

        let blob_ref = if entry.status_raw == "D" {
            format!("HEAD:{head_path}")
        } else {
            format!(":{content_path}")
        };

        if !binary_file
            && let Some(detected) = file_probe(&blob_ref)
            && detected.contains("charset=binary")
        {
            binary_file = true;
            blob_type = Some(detected);
        }

        if binary_file {
            let blob_size = git_stdout_trimmed_optional(&["cat-file", "-s", &blob_ref]);
            out.push_str("[Binary file content hidden]\n\n");
            if let Some(size) = blob_size {
                out.push_str(&format!("Size: {size} bytes\n"));
            }
            if let Some(blob_type) = blob_type {
                out.push_str(&format!("Type: {blob_type}\n"));
            }
            out.push('\n');
            continue;
        }

        if lockfile && !include_content {
            out.push_str("[Lockfile content hidden]\n\n");
            if let (Some(added), Some(deleted)) = (diff.added, diff.deleted) {
                out.push_str(&format!("Summary: +{added} -{deleted}\n"));
            }
            out.push_str(&format!(
                "Tip: use --include {content_path} to show full content\n\n"
            ));
            continue;
        }

        if entry.status_raw == "D" {
            if git_status_success(&["cat-file", "-e", &blob_ref]) {
                out.push_str("[Deleted file, showing HEAD version]\n\n");
                out.push_str("```ts\n");
                match git_output(&["show", &blob_ref]) {
                    Ok(output) => {
                        out.push_str(&String::from_utf8_lossy(&output.stdout));
                    }
                    Err(_) => {
                        out.push_str("[HEAD version not found]\n");
                    }
                }
                out.push_str("```\n\n");
            } else {
                out.push_str("[Deleted file, no HEAD version found]\n\n");
            }
            continue;
        }

        if entry.status_raw == "A"
            || entry.status_raw == "M"
            || entry.status_raw.starts_with('R')
            || entry.status_raw.starts_with('C')
        {
            out.push_str("```ts\n");
            let index_ref = format!(":{content_path}");
            match git_output(&["show", &index_ref]) {
                Ok(output) => {
                    out.push_str(&String::from_utf8_lossy(&output.stdout));
                }
                Err(_) => {
                    out.push_str("[Index version not found]\n");
                }
            }
            out.push_str("```\n\n");
            continue;
        }

        out.push_str(&format!("[Unhandled status: {}]\n\n", entry.status_raw));
    }

    Ok(trim_trailing_newlines(&out))
}

fn pattern_matches(pattern: &str, text: &str) -> bool {
    wildcard_match(pattern, text)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut pi = 0;
    let mut ti = 0;
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_idx = Some(pi);
            match_idx = ti;
            pi += 1;
        } else if let Some(star) = star_idx {
            pi = star + 1;
            match_idx += 1;
            ti = match_idx;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

fn file_probe(blob_ref: &str) -> Option<String> {
    if env::var("GIT_CLI_FIXTURE_FILE_MODE").ok().as_deref() == Some("missing") {
        return None;
    }

    if !process::cmd_exists("file") {
        return None;
    }

    if !git_status_success(&["cat-file", "-e", blob_ref]) {
        return None;
    }

    let blob = git_output(&["cat-file", "-p", blob_ref]).ok()?;
    let sample_len = blob.stdout.len().min(8192);
    let sample = &blob.stdout[..sample_len];

    let mut child = Command::new("file")
        .args(["-b", "--mime", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(sample);
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }

    let out = String::from_utf8_lossy(&output.stdout).to_string();
    let out = trim_trailing_newlines(&out);
    if out.is_empty() { None } else { Some(out) }
}

fn run_to_stash(args: &[String]) -> i32 {
    if !ensure_git_work_tree() {
        return 1;
    }

    let commit_ref = args.first().map(|s| s.as_str()).unwrap_or("HEAD");
    let commit_sha = match git_stdout_trimmed_optional(&[
        "rev-parse",
        "--verify",
        &format!("{commit_ref}^{{commit}}"),
    ]) {
        Some(value) => value,
        None => {
            eprintln!("❌ Cannot resolve commit: {commit_ref}");
            return 1;
        }
    };

    let mut parent_sha =
        match git_stdout_trimmed_optional(&["rev-parse", "--verify", &format!("{commit_sha}^")]) {
            Some(value) => value,
            None => {
                eprintln!("❌ Commit {commit_sha} has no parent (root commit).");
                eprintln!("🧠 Converting a root commit to stash is ambiguous; aborting.");
                return 1;
            }
        };

    if is_merge_commit(&commit_sha) {
        println!("⚠️  Target commit is a merge commit (multiple parents).");
        println!(
            "🧠 This tool will use the FIRST parent to compute the patch: {commit_sha}^1..{commit_sha}"
        );
        if prompt::confirm_or_abort("❓ Proceed? [y/N] ").is_err() {
            return 1;
        }
        if let Some(value) =
            git_stdout_trimmed_optional(&["rev-parse", "--verify", &format!("{commit_sha}^1")])
        {
            parent_sha = value;
        } else {
            return 1;
        }
    }

    let branch_name = git_stdout_trimmed_optional(&["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|| "(unknown)".to_string());
    let subject = git_stdout_trimmed_optional(&["log", "-1", "--pretty=%s", &commit_sha])
        .unwrap_or_else(|| "(no subject)".to_string());

    let short_commit = short_sha(&commit_sha);
    let short_parent = short_sha(&parent_sha);
    let stash_msg = format!(
        "c2s: commit={short_commit} parent={short_parent} branch={branch_name} \"{subject}\""
    );

    let commit_oneline = git_stdout_trimmed_optional(&["log", "-1", "--oneline", &commit_sha])
        .unwrap_or_else(|| commit_sha.clone());

    println!("🧾 Convert commit → stash");
    println!("   Commit : {commit_oneline}");
    println!("   Parent : {short_parent}");
    println!("   Branch : {branch_name}");
    println!("   Message: {stash_msg}");
    println!();
    println!("This will:");
    println!("  1) Create a stash entry containing the patch: {short_parent}..{short_commit}");
    println!("  2) Optionally drop the commit from branch history by resetting to parent.");

    if prompt::confirm_or_abort("❓ Proceed to create stash? [y/N] ").is_err() {
        return 1;
    }

    let stash_result = create_stash_for_commit(&commit_sha, &parent_sha, &branch_name, &stash_msg);

    let stash_created = match stash_result {
        Ok(result) => result,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    if stash_created.fallback_failed {
        return 1;
    }

    if !stash_created.fallback_used {
        let stash_line = git_stdout_trimmed_optional(&["stash", "list", "-1"]).unwrap_or_default();
        println!("✅ Stash created: {stash_line}");
    }

    if commit_ref != "HEAD"
        && git_stdout_trimmed_optional(&["rev-parse", "HEAD"]).as_deref()
            != Some(commit_sha.as_str())
    {
        println!("ℹ️  Not dropping commit automatically because target is not HEAD.");
        println!(
            "🧠 If you want to remove it, do so explicitly (e.g., interactive rebase) after verifying stash."
        );
        return 0;
    }

    println!();
    println!("Optional: drop the commit from current branch history?");
    println!("  This would run: git reset --hard {short_parent}");
    println!("  (Your work remains in stash; untracked files are unaffected.)");

    match prompt::confirm("❓ Drop commit from history now? [y/N] ") {
        Ok(true) => {}
        Ok(false) => {
            println!("✅ Done. Commit kept; stash saved.");
            return 0;
        }
        Err(_) => return 1,
    }

    let upstream =
        git_stdout_trimmed_optional(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .unwrap_or_default();

    if !upstream.is_empty()
        && git_status_success(&["merge-base", "--is-ancestor", &commit_sha, &upstream])
    {
        println!("⚠️  This commit appears to be reachable from upstream ({upstream}).");
        println!(
            "🧨 Dropping it rewrites history and may require force push; it can affect others."
        );
        match prompt::confirm("❓ Still drop it? [y/N] ") {
            Ok(true) => {}
            Ok(false) => {
                println!("✅ Done. Commit kept; stash saved.");
                return 0;
            }
            Err(_) => return 1,
        }
    }

    let final_prompt =
        format!("❓ Final confirmation: run 'git reset --hard {short_parent}'? [y/N] ");
    match prompt::confirm(&final_prompt) {
        Ok(true) => {}
        Ok(false) => {
            println!("✅ Done. Commit kept; stash saved.");
            return 0;
        }
        Err(_) => return 1,
    }

    if !git_status_success(&["reset", "--hard", &parent_sha]) {
        println!("❌ Failed to reset branch to parent.");
        println!(
            "🧠 Your stash is still saved. You can manually recover the commit via reflog if needed."
        );
        return 1;
    }

    let stash_line = git_stdout_trimmed_optional(&["stash", "list", "-1"]).unwrap_or_default();
    println!("✅ Commit dropped from history. Your work is in stash:");
    println!("   {stash_line}");

    0
}

fn is_merge_commit(commit_sha: &str) -> bool {
    let output = match git_output(&["rev-list", "--parents", "-n", "1", commit_sha]) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let line = String::from_utf8_lossy(&output.stdout).to_string();
    let parts: Vec<&str> = line.split_whitespace().collect();
    parts.len() > 2
}

struct StashResult {
    fallback_used: bool,
    fallback_failed: bool,
}

fn create_stash_for_commit(
    commit_sha: &str,
    parent_sha: &str,
    branch_name: &str,
    stash_msg: &str,
) -> Result<StashResult> {
    let force_fallback = env::var("GIT_CLI_FORCE_STASH_FALLBACK")
        .ok()
        .map(|v| {
            let v = v.to_lowercase();
            !(v == "0" || v == "false" || v.is_empty())
        })
        .unwrap_or(false);

    let stash_sha = if force_fallback {
        None
    } else {
        synthesize_stash_object(commit_sha, parent_sha, branch_name, stash_msg)
    };

    if let Some(stash_sha) = stash_sha {
        if !git_status_success(&["stash", "store", "-m", stash_msg, &stash_sha]) {
            return Err(anyhow!("❌ Failed to store stash object."));
        }
        return Ok(StashResult {
            fallback_used: false,
            fallback_failed: false,
        });
    }

    println!("⚠️  Failed to synthesize stash object without touching worktree.");
    println!("🧠 Fallback would require touching the working tree.");
    if prompt::confirm_or_abort("❓ Fallback by temporarily checking out parent and applying patch (will modify worktree)? [y/N] ").is_err() {
        return Ok(StashResult {
            fallback_used: true,
            fallback_failed: true,
        });
    }

    let status = git_stdout_trimmed_optional(&["status", "--porcelain"]).unwrap_or_default();
    if !status.trim().is_empty() {
        println!("❌ Working tree is not clean; fallback requires clean state.");
        println!("🧠 Commit/stash your current changes first, then retry.");
        return Ok(StashResult {
            fallback_used: true,
            fallback_failed: true,
        });
    }

    let current_head = match git_stdout_trimmed_optional(&["rev-parse", "HEAD"]) {
        Some(value) => value,
        None => {
            return Ok(StashResult {
                fallback_used: true,
                fallback_failed: true,
            });
        }
    };

    if !git_status_success(&["checkout", "--detach", parent_sha]) {
        println!("❌ Failed to checkout parent for fallback.");
        return Ok(StashResult {
            fallback_used: true,
            fallback_failed: true,
        });
    }

    if !git_status_success(&["cherry-pick", "-n", commit_sha]) {
        println!("❌ Failed to apply commit patch in fallback mode.");
        println!("🧠 Attempting to restore original HEAD.");
        let _ = git_status_success(&["cherry-pick", "--abort"]);
        let _ = git_status_success(&["checkout", &current_head]);
        return Ok(StashResult {
            fallback_used: true,
            fallback_failed: true,
        });
    }

    if !git_status_success(&["stash", "push", "-m", stash_msg]) {
        println!("❌ Failed to stash changes in fallback mode.");
        let _ = git_status_success(&["reset", "--hard"]);
        let _ = git_status_success(&["checkout", &current_head]);
        return Ok(StashResult {
            fallback_used: true,
            fallback_failed: true,
        });
    }

    let _ = git_status_success(&["reset", "--hard"]);
    let _ = git_status_success(&["checkout", &current_head]);

    let stash_line = git_stdout_trimmed_optional(&["stash", "list", "-1"]).unwrap_or_default();
    println!("✅ Stash created (fallback): {stash_line}");

    Ok(StashResult {
        fallback_used: true,
        fallback_failed: false,
    })
}

fn synthesize_stash_object(
    commit_sha: &str,
    parent_sha: &str,
    branch_name: &str,
    stash_msg: &str,
) -> Option<String> {
    let base_tree =
        git_stdout_trimmed_optional(&["rev-parse", "--verify", &format!("{parent_sha}^{{tree}}")])?;
    let commit_tree =
        git_stdout_trimmed_optional(&["rev-parse", "--verify", &format!("{commit_sha}^{{tree}}")])?;

    let index_msg = format!("index on {branch_name}: {stash_msg}");
    let index_commit = git_stdout_trimmed_optional(&[
        "commit-tree",
        &base_tree,
        "-p",
        parent_sha,
        "-m",
        &index_msg,
    ])?;

    let wip_commit = git_stdout_trimmed_optional(&[
        "commit-tree",
        &commit_tree,
        "-p",
        parent_sha,
        "-p",
        &index_commit,
        "-m",
        stash_msg,
    ])?;

    Some(wip_commit)
}

fn short_sha(value: &str) -> String {
    value.chars().take(7).collect()
}

fn ensure_git_work_tree() -> bool {
    match common_git::require_work_tree() {
        Ok(()) => true,
        Err(GitContextError::GitNotFound) => {
            eprintln!("❗ git is required but was not found in PATH.");
            false
        }
        Err(GitContextError::NotRepository) => {
            eprintln!("❌ Not a git repository.");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommitCommand, OutputMode, ParseOutcome, dispatch, file_probe, git_scope_available,
        parse_command, parse_context_args, pattern_matches, short_sha, strip_ansi, wildcard_match,
    };
    use nils_test_support::{CwdGuard, EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_command_supports_aliases() {
        assert!(matches!(
            parse_command("context"),
            Some(CommitCommand::Context)
        ));
        assert!(matches!(
            parse_command("context-json"),
            Some(CommitCommand::ContextJson)
        ));
        assert!(matches!(
            parse_command("context_json"),
            Some(CommitCommand::ContextJson)
        ));
        assert!(matches!(
            parse_command("json"),
            Some(CommitCommand::ContextJson)
        ));
        assert!(matches!(
            parse_command("stash"),
            Some(CommitCommand::ToStash)
        ));
        assert!(parse_command("unknown").is_none());
    }

    #[test]
    fn parse_context_args_supports_modes_and_include_forms() {
        let args = vec![
            "--both".to_string(),
            "--no-color".to_string(),
            "--include".to_string(),
            "src/*.rs".to_string(),
            "--include=README.md".to_string(),
            "--extra".to_string(),
        ];

        match parse_context_args(&args) {
            ParseOutcome::Continue(parsed) => {
                assert_eq!(parsed.mode, OutputMode::Both);
                assert!(parsed.no_color);
                assert_eq!(
                    parsed.include_patterns,
                    vec!["src/*.rs".to_string(), "README.md".to_string()]
                );
                assert_eq!(parsed.extra_args, vec!["--extra".to_string()]);
            }
            ParseOutcome::Exit(code) => panic!("unexpected early exit: {code}"),
        }
    }

    #[test]
    fn parse_context_args_reports_missing_include_value() {
        let args = vec!["--include".to_string()];
        match parse_context_args(&args) {
            ParseOutcome::Exit(code) => assert_eq!(code, 2),
            ParseOutcome::Continue(_) => panic!("expected usage exit"),
        }
    }

    #[test]
    fn wildcard_matching_handles_star_and_question_mark() {
        assert!(wildcard_match("src/*.rs", "src/main.rs"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(wildcard_match("*commit*", "git-commit"));
        assert!(!wildcard_match("src/*.rs", "src/main.ts"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(pattern_matches("docs/**", "docs/plans/test.md"));
    }

    #[test]
    fn short_sha_truncates_to_seven_chars() {
        assert_eq!(short_sha("abcdef123456"), "abcdef1");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn parse_context_args_help_exits_zero() {
        let args = vec!["--help".to_string()];
        match parse_context_args(&args) {
            ParseOutcome::Exit(code) => assert_eq!(code, 0),
            ParseOutcome::Continue(_) => panic!("expected help exit"),
        }
    }

    #[test]
    fn git_scope_available_honors_fixture_override() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "GIT_CLI_FIXTURE_GIT_SCOPE_MODE", "missing");
        assert!(!git_scope_available());
    }

    #[test]
    fn file_probe_respects_missing_file_fixture() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::set(&lock, "GIT_CLI_FIXTURE_FILE_MODE", "missing");
        assert_eq!(file_probe("HEAD:README.md"), None);
    }

    #[test]
    fn strip_ansi_removes_sgr_sequences() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
    }

    #[test]
    fn dispatch_context_and_stash_fail_fast_outside_git_repo() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let _cwd = CwdGuard::set(&lock, dir.path()).expect("cwd");
        assert_eq!(dispatch("context", &[]), 1);
        assert_eq!(dispatch("stash", &[]), 1);
    }
}
