use crate::git;
use nils_common::git as common_git;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
use std::fs::File;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};

const EXIT_ERROR: i32 = 1;
const EXIT_NO_STAGED_CHANGES: i32 = 2;
const EXIT_MESSAGE_REQUIRED: i32 = 3;
const EXIT_VALIDATION_FAILED: i32 = 4;
const EXIT_DEPENDENCY_ERROR: i32 = 5;
const CAT_PAGER_ENV: [(&str, &str); 2] = [("GIT_PAGER", "cat"), ("PAGER", "cat")];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SummaryMode {
    GitScope,
    GitShow,
    None,
}

impl SummaryMode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "git-scope" => Some(Self::GitScope),
            "git-show" => Some(Self::GitShow),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct CommitOptions {
    message: Option<String>,
    message_file: Option<String>,
    message_out: Option<PathBuf>,
    summary_mode: SummaryMode,
    no_progress: bool,
    quiet: bool,
    automation: bool,
    validate_only: bool,
    dry_run: bool,
    auto_fix: bool,
    repo: Option<PathBuf>,
}

impl Default for CommitOptions {
    fn default() -> Self {
        Self {
            message: None,
            message_file: None,
            message_out: None,
            summary_mode: SummaryMode::GitScope,
            no_progress: false,
            quiet: false,
            automation: false,
            validate_only: false,
            dry_run: false,
            auto_fix: false,
            repo: None,
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut options = match parse_args(args) {
        Ok(options) => options,
        Err(code) => return code,
    };

    if !git::command_exists("git") {
        eprintln!("error: git is required (ensure it is installed and on PATH)");
        return EXIT_DEPENDENCY_ERROR;
    }

    if options.quiet {
        options.no_progress = true;
        options.summary_mode = SummaryMode::None;
    }

    let mut message_contents = match read_message_contents(&options) {
        Ok(contents) => contents,
        Err(code) => return code,
    };

    if options.auto_fix {
        message_contents = normalize_message(&message_contents);
    }

    if let Some(path) = options.message_out.as_deref()
        && let Err(err) = write_message_file(path, &message_contents)
    {
        eprintln!("error: failed to write --message-out file: {err}");
        return EXIT_ERROR;
    }

    let tmpfile = match tempfile::NamedTempFile::new() {
        Ok(file) => file,
        Err(_) => {
            eprintln!("error: failed to create temp file for commit message");
            return EXIT_ERROR;
        }
    };

    if let Err(err) = write_message_file(tmpfile.path(), &message_contents) {
        eprintln!("{err:#}");
        return EXIT_ERROR;
    }

    if let Err(code) = validate_commit_message(tmpfile.path()) {
        return code;
    }

    if options.validate_only {
        return 0;
    }

    if !git::is_inside_work_tree(options.repo.as_deref()) {
        eprintln!("error: must run inside a git work tree");
        return EXIT_ERROR;
    }

    match git::has_staged_changes(options.repo.as_deref()) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("error: no staged changes (stage files with git add first)");
            return EXIT_NO_STAGED_CHANGES;
        }
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    }

    if options.dry_run {
        return 0;
    }

    let show_progress = !options.no_progress && std::io::stderr().is_terminal();
    let progress = if show_progress {
        Some(Progress::spinner(
            ProgressOptions::default()
                .with_prefix("semantic-commit ")
                .with_finish(ProgressFinish::Clear),
        ))
    } else {
        None
    };

    if let Some(progress) = &progress {
        progress.set_message("git commit");
        progress.tick();
    }

    let status = git_commit(tmpfile.path(), options.repo.as_deref());

    if let Some(progress) = &progress {
        progress.finish_and_clear();
    }

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let rc = status.code().unwrap_or(EXIT_ERROR);
            eprintln!("error: git commit failed (exit code: {rc})");
            return rc;
        }
        Err(err) => {
            eprintln!("{err:#}");
            return EXIT_ERROR;
        }
    }

    print_summary(options.summary_mode, options.repo.as_deref())
}

fn parse_args(args: &[String]) -> Result<CommitOptions, i32> {
    let mut options = CommitOptions::default();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_usage_stdout();
                return Err(0);
            }
            "--message" | "-m" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: {} requires a value", args[i]);
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                options.message = Some(value);
                i += 2;
            }
            "--message-file" | "-F" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: {} requires a path", args[i]);
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                options.message_file = Some(value);
                i += 2;
            }
            "--message-out" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --message-out requires a path");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                options.message_out = Some(PathBuf::from(value));
                i += 2;
            }
            "--summary" => {
                let value = match args.get(i + 1) {
                    Some(value) => value,
                    None => {
                        eprintln!("error: --summary requires a value");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };

                let Some(mode) = SummaryMode::parse(value) else {
                    eprintln!(
                        "error: invalid --summary value: {value} (expected: git-scope, git-show, none)"
                    );
                    print_usage_stderr();
                    return Err(EXIT_ERROR);
                };

                options.summary_mode = mode;
                i += 2;
            }
            "--no-summary" => {
                options.summary_mode = SummaryMode::None;
                i += 1;
            }
            "--no-progress" => {
                options.no_progress = true;
                i += 1;
            }
            "--quiet" => {
                options.quiet = true;
                i += 1;
            }
            "--automation" | "--non-interactive" => {
                options.automation = true;
                i += 1;
            }
            "--validate-only" => {
                options.validate_only = true;
                i += 1;
            }
            "--dry-run" => {
                options.dry_run = true;
                i += 1;
            }
            "--auto-fix" => {
                options.auto_fix = true;
                i += 1;
            }
            "--repo" => {
                let value = match args.get(i + 1) {
                    Some(value) => value.clone(),
                    None => {
                        eprintln!("error: --repo requires a path");
                        print_usage_stderr();
                        return Err(EXIT_ERROR);
                    }
                };
                options.repo = Some(PathBuf::from(value));
                i += 2;
            }
            other => {
                eprintln!("error: unknown argument: {other}");
                print_usage_stderr();
                return Err(EXIT_ERROR);
            }
        }
    }

    if options.message.is_some() && options.message_file.is_some() {
        eprintln!("error: use only one of --message or --message-file");
        return Err(EXIT_ERROR);
    }

    Ok(options)
}

fn read_message_contents(options: &CommitOptions) -> Result<String, i32> {
    let message_contents = match (&options.message, &options.message_file) {
        (Some(text), None) => text.clone(),
        (None, Some(path)) => match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(_) => {
                eprintln!("error: message file not found: {path}");
                return Err(EXIT_ERROR);
            }
        },
        (None, None) => {
            if options.automation {
                eprintln!(
                    "error: no commit message provided in automation mode (use --message or --message-file)"
                );
                return Err(EXIT_MESSAGE_REQUIRED);
            }

            if std::io::stdin().is_terminal() {
                eprintln!(
                    "error: no commit message provided (use stdin, --message, or --message-file)"
                );
                print_usage_stderr();
                return Err(EXIT_MESSAGE_REQUIRED);
            }

            let mut buf = String::new();
            if let Err(err) = std::io::stdin().read_to_string(&mut buf) {
                eprintln!("{err:#}");
                return Err(EXIT_ERROR);
            }
            buf
        }
        (Some(_), Some(_)) => unreachable!("validated above"),
    };

    if message_contents.trim().is_empty() {
        eprintln!("error: commit message is empty");
        return Err(EXIT_MESSAGE_REQUIRED);
    }

    Ok(message_contents)
}

fn run_git_output_with_pager(repo: Option<&Path>, args: &[&str]) -> std::io::Result<Output> {
    match repo {
        Some(repo) => common_git::run_output_in_with_env(repo, args, &CAT_PAGER_ENV),
        None => common_git::run_output_with_env(args, &CAT_PAGER_ENV),
    }
}

fn run_git_status_inherit_with_pager(
    repo: Option<&Path>,
    args: &[&str],
) -> std::io::Result<ExitStatus> {
    match repo {
        Some(repo) => common_git::run_status_inherit_in_with_env(repo, args, &CAT_PAGER_ENV),
        None => common_git::run_status_inherit_with_env(args, &CAT_PAGER_ENV),
    }
}

fn git_commit(
    message_path: &Path,
    repo: Option<&Path>,
) -> anyhow::Result<std::process::ExitStatus> {
    let message_path = message_path.to_string_lossy();
    let output = run_git_output_with_pager(repo, &["commit", "-F", message_path.as_ref()])?;

    if !output.stderr.is_empty() {
        std::io::stderr().write_all(&output.stderr)?;
    }

    Ok(output.status)
}

fn print_summary(summary_mode: SummaryMode, repo: Option<&Path>) -> i32 {
    match summary_mode {
        SummaryMode::None => 0,
        SummaryMode::GitShow => print_git_show_summary(repo),
        SummaryMode::GitScope => {
            if run_git_scope_summary(repo) {
                0
            } else {
                eprintln!(
                    "warning: git-scope summary unavailable; falling back to git show --name-status"
                );
                print_git_show_summary(repo)
            }
        }
    }
}

fn run_git_scope_summary(repo: Option<&Path>) -> bool {
    let mut command = Command::new("git-scope");
    if let Some(repo) = repo {
        command.current_dir(repo);
    }

    let status = command
        .args(["commit", "HEAD", "--no-color"])
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(status) if status.success() => true,
        Ok(status) => {
            let rc = status.code().unwrap_or(EXIT_ERROR);
            eprintln!("warning: git-scope commit failed (exit code: {rc})");
            false
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("warning: git-scope not found on PATH");
            false
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("warning: git-scope is not executable");
            false
        }
        Err(err) => {
            eprintln!("warning: git-scope commit failed: {err}");
            false
        }
    }
}

fn print_git_show_summary(repo: Option<&Path>) -> i32 {
    let status = run_git_status_inherit_with_pager(
        repo,
        &["show", "-1", "--name-status", "--oneline", "--no-color"],
    );

    match status {
        Ok(status) if status.success() => 0,
        Ok(status) => {
            let rc = status.code().unwrap_or(EXIT_ERROR);
            eprintln!("error: git show summary failed (exit code: {rc})");
            rc
        }
        Err(err) => {
            eprintln!("error: git show summary failed: {err}");
            EXIT_ERROR
        }
    }
}

fn write_message_file(path: &Path, contents: &str) -> anyhow::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn validate_commit_message(path: &Path) -> Result<(), i32> {
    let file = File::open(path).map_err(|_| {
        eprintln!("error: commit message validation failed");
        EXIT_VALIDATION_FAILED
    })?;

    let reader = BufReader::new(file);
    let mut lines: Vec<String> = Vec::new();
    for line in reader.lines() {
        match line {
            Ok(line) => lines.push(line),
            Err(_) => {
                return fail_validation("commit message validation failed");
            }
        }
    }

    if lines.is_empty() {
        return fail_validation("commit message is empty");
    }

    let header = &lines[0];
    if header.is_empty() {
        return fail_validation("commit header is empty");
    }
    if header.chars().count() > MAX_LINE_WIDTH {
        return fail_validation("commit header exceeds 100 characters (max 100)");
    }
    if !is_valid_header(header) {
        return fail_validation(
            "invalid header format (expected 'type(scope): subject' or 'type: subject' with lowercase type)",
        );
    }

    let body_exists = lines.iter().skip(1).any(|line| !line.is_empty());
    if body_exists {
        if lines.get(1).is_some_and(|line| !line.is_empty()) {
            return fail_validation("commit body must be separated from header by a blank line");
        }

        let mut prev_was_body_line = false;
        for (idx, line) in lines.iter().enumerate().skip(2) {
            let line_no = idx + 1;
            if line.is_empty() {
                return fail_validation(&format!(
                    "commit body line {line_no} is empty; body lines must start with '- ' followed by uppercase letter (or '  ' to continue the previous bullet)"
                ));
            }
            if line.chars().count() > MAX_LINE_WIDTH {
                return fail_validation(&format!(
                    "commit body line {line_no} exceeds 100 characters (max 100)"
                ));
            }

            let is_bullet = line.starts_with("- ")
                && line.chars().nth(2).is_some_and(|c| c.is_ascii_uppercase());
            let is_continuation = prev_was_body_line
                && line.starts_with("  ")
                && line.chars().nth(2).is_some_and(|c| !c.is_whitespace());

            if !is_bullet && !is_continuation {
                return fail_validation(&format!(
                    "commit body line {line_no} must start with '- ' followed by uppercase letter (or '  ' to continue the previous bullet)"
                ));
            }
            prev_was_body_line = true;
        }
    }

    Ok(())
}

fn fail_validation(message: &str) -> Result<(), i32> {
    eprintln!("error: {message}");
    Err(EXIT_VALIDATION_FAILED)
}

fn is_valid_header(header: &str) -> bool {
    // Regex parity: ^[a-z][a-z0-9-]*(\([a-z0-9._-]+\))?: .+$
    let Some((prefix, subject)) = header.split_once(": ") else {
        return false;
    };
    if subject.is_empty() {
        return false;
    }

    let (typ, scope) = if let Some((t, rest)) = prefix.split_once('(') {
        let Some(scope_end) = rest.strip_suffix(')') else {
            return false;
        };
        (t, Some(scope_end))
    } else {
        (prefix, None)
    };

    let mut chars = typ.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return false;
    }

    if let Some(scope) = scope {
        if scope.is_empty() {
            return false;
        }
        if !scope.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
        }) {
            return false;
        }
    }

    true
}

const MAX_LINE_WIDTH: usize = 100;

fn normalize_message(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.is_empty() {
        return input.to_string();
    }

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 2);
    out.push(normalize_header(lines[0]));

    let body: Vec<&str> = lines
        .iter()
        .skip(1)
        .filter(|line| !line.is_empty())
        .copied()
        .collect();

    if !body.is_empty() {
        out.push(String::new());
        for line in body {
            let cased = capitalize_bullet_first_char(line);
            for wrapped in wrap_body_line(&cased, MAX_LINE_WIDTH) {
                out.push(wrapped);
            }
        }
    }

    let mut result = out.join("\n");
    if input.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn normalize_header(header: &str) -> String {
    let Some((prefix, subject)) = header.split_once(": ") else {
        return header.to_string();
    };

    let (typ, scope) = if let Some((t, rest)) = prefix.split_once('(') {
        let Some(scope_inner) = rest.strip_suffix(')') else {
            return header.to_string();
        };
        (t, Some(scope_inner))
    } else {
        (prefix, None)
    };

    let prefix_norm = match scope {
        Some(s) => format!("{}({})", typ.to_ascii_lowercase(), s.to_ascii_lowercase()),
        None => typ.to_ascii_lowercase(),
    };

    format!("{prefix_norm}: {subject}")
}

fn capitalize_bullet_first_char(line: &str) -> String {
    if !line.starts_with("- ") {
        return line.to_string();
    }
    let mut chars = line.chars();
    let _ = chars.next();
    let _ = chars.next();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {
            let rest: String = chars.collect();
            format!("- {}{rest}", c.to_ascii_uppercase())
        }
        _ => line.to_string(),
    }
}

fn wrap_body_line(line: &str, max: usize) -> Vec<String> {
    if line.chars().count() <= max {
        return vec![line.to_string()];
    }

    let (first_prefix, cont_prefix, content): (&str, &str, String) = if line.starts_with("- ") {
        ("- ", "  ", line.chars().skip(2).collect())
    } else if line.starts_with("  ") {
        ("  ", "  ", line.chars().skip(2).collect())
    } else {
        ("", "", line.to_string())
    };

    let chars: Vec<char> = content.chars().collect();
    let first_budget = max.saturating_sub(first_prefix.chars().count());
    let cont_budget = max.saturating_sub(cont_prefix.chars().count());

    if first_budget == 0 || cont_budget == 0 {
        return vec![line.to_string()];
    }

    let mut result: Vec<String> = Vec::new();
    let mut idx = 0;
    let mut is_first = true;

    while idx < chars.len() {
        let budget = if is_first { first_budget } else { cont_budget };
        let prefix = if is_first { first_prefix } else { cont_prefix };
        let window_end = (idx + budget).min(chars.len());

        let break_at = if window_end == chars.len() {
            window_end
        } else {
            (idx..window_end)
                .rev()
                .find(|&i| chars[i] == ' ' && i > idx)
                .unwrap_or(window_end)
        };

        let chunk: String = chars[idx..break_at].iter().collect();
        result.push(format!("{prefix}{}", chunk.trim_end()));

        idx = if break_at < chars.len() && chars[break_at] == ' ' {
            let mut next = break_at + 1;
            while next < chars.len() && chars[next] == ' ' {
                next += 1;
            }
            next
        } else {
            break_at
        };
        is_first = false;
    }

    if result.is_empty() {
        result.push(first_prefix.to_string());
    }

    result
}

fn print_usage_stdout() {
    print_usage(false);
}

fn print_usage_stderr() {
    print_usage(true);
}

fn print_usage(stderr: bool) {
    let out: &mut dyn std::io::Write = if stderr {
        &mut std::io::stderr()
    } else {
        &mut std::io::stdout()
    };

    let _ = writeln!(out, "Usage:");
    let _ = writeln!(
        out,
        "  semantic-commit commit [--message <text>|--message-file <path>] [options]"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Options:");
    let _ = writeln!(out, "  -m, --message <text>         Commit message text");
    let _ = writeln!(
        out,
        "  -F, --message-file <path>    Read commit message from file"
    );
    let _ = writeln!(
        out,
        "      --message-out <path>     Save prepared message for recovery"
    );
    let _ = writeln!(
        out,
        "      --summary <mode>         Summary mode: git-scope | git-show | none"
    );
    let _ = writeln!(
        out,
        "      --no-summary             Equivalent to --summary none"
    );
    let _ = writeln!(
        out,
        "      --repo <path>            Run git commands against repo path"
    );
    let _ = writeln!(
        out,
        "      --automation             Disallow stdin message fallback"
    );
    let _ = writeln!(
        out,
        "      --validate-only          Validate message format only"
    );
    let _ = writeln!(
        out,
        "      --dry-run                Validate + staged checks, skip git commit"
    );
    let _ = writeln!(
        out,
        "      --auto-fix               Normalize body wrap, bullet/type/scope case before validation"
    );
    let _ = writeln!(
        out,
        "      --no-progress            Disable progress spinner"
    );
    let _ = writeln!(
        out,
        "      --quiet                  Suppress progress and summary output"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Examples:");
    let _ = writeln!(out, "  cat <<'MSG' | semantic-commit commit");
    let _ = writeln!(out, "  feat(core): add thing");
    let _ = writeln!(out);
    let _ = writeln!(out, "  - Add thing");
    let _ = writeln!(out, "  MSG");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  semantic-commit commit -F ./message.txt --summary git-show"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn message_file(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("create temp message file");
        file.write_all(contents.as_bytes())
            .expect("write temp message file");
        file
    }

    #[test]
    fn summary_mode_parse_supports_known_values() {
        assert_eq!(SummaryMode::parse("git-scope"), Some(SummaryMode::GitScope));
        assert_eq!(SummaryMode::parse("git-show"), Some(SummaryMode::GitShow));
        assert_eq!(SummaryMode::parse("none"), Some(SummaryMode::None));
        assert_eq!(SummaryMode::parse("other"), None);
    }

    #[test]
    fn validate_commit_message_accepts_valid_header_without_body() {
        let file = message_file("feat: add parser coverage\n");
        assert!(validate_commit_message(file.path()).is_ok());
    }

    #[test]
    fn validate_commit_message_accepts_valid_scoped_header_with_body() {
        let file = message_file("fix(parser-2): handle edge case\n\n- Handle malformed input\n");
        assert!(validate_commit_message(file.path()).is_ok());
    }

    #[test]
    fn validate_commit_message_rejects_header_over_100_chars() {
        let subject = "a".repeat(95);
        let file = message_file(&format!("feat: {subject}\n"));

        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn validate_commit_message_rejects_uppercase_scope() {
        let file = message_file("feat(Core): add parser coverage\n");
        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn validate_commit_message_rejects_empty_line_inside_body() {
        let file = message_file("feat: add parser coverage\n\n- First line\n\n- Second line\n");
        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn validate_commit_message_rejects_body_line_over_100_chars() {
        let long_line = "A".repeat(99);
        let file = message_file(&format!("feat: add parser coverage\n\n- {long_line}\n"));

        assert_eq!(
            validate_commit_message(file.path()),
            Err(EXIT_VALIDATION_FAILED)
        );
    }

    #[test]
    fn is_valid_header_enforces_shape_rules() {
        assert!(is_valid_header("fix(core_2): handle edge case"));
        assert!(is_valid_header("chore: update fixtures"));
        assert!(!is_valid_header("Fix: uppercase type"));
        assert!(!is_valid_header("fix(core!): invalid scope character"));
        assert!(!is_valid_header("fix(scope):"));
        assert!(!is_valid_header("fix(scope): "));
        assert!(!is_valid_header("fix(scope) missing colon"));
    }

    #[test]
    fn normalize_header_lowercases_type_and_scope() {
        assert_eq!(
            normalize_header("Feat(Core): add thing"),
            "feat(core): add thing"
        );
        assert_eq!(normalize_header("FIX: bug"), "fix: bug");
    }

    #[test]
    fn normalize_header_preserves_subject_case() {
        assert_eq!(
            normalize_header("feat(core): Add THING with MixedCase"),
            "feat(core): Add THING with MixedCase"
        );
    }

    #[test]
    fn normalize_header_passes_through_invalid_shape() {
        assert_eq!(normalize_header("no colon here"), "no colon here");
        assert_eq!(
            normalize_header("feat(unclosed: subj"),
            "feat(unclosed: subj"
        );
    }

    #[test]
    fn capitalize_bullet_first_char_upcases_lowercase_ascii() {
        assert_eq!(capitalize_bullet_first_char("- add thing"), "- Add thing");
    }

    #[test]
    fn capitalize_bullet_first_char_leaves_already_uppercase() {
        assert_eq!(capitalize_bullet_first_char("- Add thing"), "- Add thing");
    }

    #[test]
    fn capitalize_bullet_first_char_leaves_non_bullets() {
        assert_eq!(capitalize_bullet_first_char("  cont"), "  cont");
        assert_eq!(capitalize_bullet_first_char("plain"), "plain");
        assert_eq!(
            capitalize_bullet_first_char("- 1.0 release"),
            "- 1.0 release"
        );
    }

    #[test]
    fn wrap_body_line_short_unchanged() {
        assert_eq!(
            wrap_body_line("- Short bullet", 100),
            vec!["- Short bullet"]
        );
    }

    #[test]
    fn wrap_body_line_breaks_bullet_at_last_space() {
        let line = format!("- {}", "word ".repeat(30).trim_end());
        let out = wrap_body_line(&line, 40);
        assert!(out.len() > 1, "expected wrapping, got {out:?}");
        assert!(out[0].starts_with("- "));
        for cont in &out[1..] {
            assert!(
                cont.starts_with("  "),
                "continuation must start with two spaces: {cont:?}"
            );
            assert!(
                cont.chars().nth(2).is_some_and(|c| !c.is_whitespace()),
                "continuation third char must be non-whitespace: {cont:?}"
            );
        }
        for l in &out {
            assert!(l.chars().count() <= 40, "line exceeds budget: {l:?}");
        }
    }

    #[test]
    fn wrap_body_line_continuation_keeps_two_space_prefix() {
        let line = format!("  {}", "word ".repeat(30).trim_end());
        let out = wrap_body_line(&line, 40);
        for cont in &out {
            assert!(
                cont.starts_with("  "),
                "expected two-space prefix: {cont:?}"
            );
        }
    }

    #[test]
    fn wrap_body_line_hard_breaks_when_no_whitespace() {
        let line = format!("- {}", "a".repeat(120));
        let out = wrap_body_line(&line, 100);
        assert!(out.len() >= 2, "expected hard break, got {out:?}");
        for l in &out {
            assert!(l.chars().count() <= 100, "line exceeds budget: {l:?}");
        }
    }

    #[test]
    fn wrap_body_line_handles_cjk_codepoint_break() {
        let line = format!("- {}", "字".repeat(110));
        let out = wrap_body_line(&line, 100);
        assert!(out.len() >= 2, "expected wrap for long CJK, got {out:?}");
        for l in &out {
            assert!(l.chars().count() <= 100, "line exceeds budget: {l:?}");
        }
    }

    #[test]
    fn normalize_message_inserts_missing_blank_separator() {
        let out = normalize_message("feat: add thing\n- Add thing\n");
        assert_eq!(out, "feat: add thing\n\n- Add thing\n");
    }

    #[test]
    fn normalize_message_drops_empty_body_lines() {
        let out = normalize_message("feat: add thing\n\n- First\n\n- Second\n");
        assert_eq!(out, "feat: add thing\n\n- First\n- Second\n");
    }

    #[test]
    fn normalize_message_passes_through_already_valid() {
        let input = "feat(core): add thing\n\n- Add thing\n";
        assert_eq!(normalize_message(input), input);
    }

    #[test]
    fn normalize_message_does_not_truncate_overlength_header() {
        let header = format!("feat: {}", "a".repeat(120));
        let input = format!("{header}\n");
        let out = normalize_message(&input);
        let header_line = out.lines().next().unwrap();
        assert_eq!(header_line.chars().count(), header.chars().count());
    }

    #[test]
    fn normalize_message_makes_invalid_input_pass_validator() {
        let input = "Feat(Core): subject\n- lowercase bullet that is way too long because it has many words and exceeds the limit by a lot of chars\n";
        let normalized = normalize_message(input);
        let file = message_file(&normalized);
        assert!(
            validate_commit_message(file.path()).is_ok(),
            "normalized message failed validation:\n{normalized}"
        );
    }
}
