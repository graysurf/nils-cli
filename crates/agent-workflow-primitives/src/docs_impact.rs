use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Args, Parser, Subcommand, ValueHint};
use serde::Serialize;
use serde_json::json;

use crate::common::{
    CliError, OutputFormat, absolute_path, display_path, render_error, render_success,
};
use crate::completion::{self, CompletionShell};

const SCAN_SCHEMA_VERSION: &str = "cli.docs-impact.scan.v1";
const SCAN_COMMAND: &str = "docs-impact scan";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(err) => return crate::common::handle_parse_error("docs-impact", argv, err),
    };

    match cli.command {
        Command::Scan(args) => run_scan(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "docs-impact"),
    }
}

fn run_scan(args: ScanArgs) -> i32 {
    let format = args.format;
    match scan(&args) {
        Ok(result) => render_success(
            SCAN_SCHEMA_VERSION,
            SCAN_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(SCAN_SCHEMA_VERSION, SCAN_COMMAND, format, err),
    }
}

fn scan(args: &ScanArgs) -> Result<ScanResult, CliError> {
    let repo = absolute_path(&args.repo)?;
    ensure_git_repo(&repo)?;

    let mut changed = BTreeSet::new();
    if let Some(base) = args.base.as_deref() {
        let range = format!("{base}...HEAD");
        for path in git_lines(
            &repo,
            &["diff", "--name-only", "--diff-filter=ACMRTUXB", &range],
        )? {
            changed.insert(path);
        }
    } else {
        for argv in [
            ["diff", "--name-only", "--diff-filter=ACMRTUXB"].as_slice(),
            ["diff", "--cached", "--name-only", "--diff-filter=ACMRTUXB"].as_slice(),
        ] {
            for path in git_lines(&repo, argv)? {
                changed.insert(path);
            }
        }
    }

    if args.include_untracked {
        for path in git_lines(&repo, &["ls-files", "--others", "--exclude-standard"])? {
            changed.insert(path);
        }
    }

    let mut docs_files = Vec::new();
    let mut non_docs_files = Vec::new();
    for path in changed {
        if is_docs_path(&path) {
            docs_files.push(path);
        } else {
            non_docs_files.push(path);
        }
    }

    let mut suggested_review = Vec::new();
    if !non_docs_files.is_empty() && docs_files.is_empty() {
        suggested_review.push("non-doc changes detected without changed docs".to_string());
        suggested_review.push(
            "check README.md, docs/, runbooks, changelog, and skill docs for stale guidance"
                .to_string(),
        );
    }
    if !docs_files.is_empty() {
        suggested_review
            .push("docs changed; run markdown/docs validation before delivery".to_string());
    }

    Ok(ScanResult {
        repo: display_path(&repo),
        base: args.base.clone(),
        include_untracked: args.include_untracked,
        docs_changed: !docs_files.is_empty(),
        non_docs_changed: !non_docs_files.is_empty(),
        docs_files,
        non_docs_files,
        suggested_review,
    })
}

fn ensure_git_repo(repo: &Path) -> Result<(), CliError> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|err| {
            CliError::runtime("git-unavailable", format!("failed to run git: {err}"), None)
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "not-git-repo",
            format!("{} is not a Git worktree", repo.display()),
            Some(json!({ "repo": display_path(repo) })),
        ));
    }
    Ok(())
}

fn git_lines(repo: &Path, args: &[&str]) -> Result<Vec<String>, CliError> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| {
            CliError::runtime(
                "git-failed",
                format!("failed to run git {}: {err}", args.join(" ")),
                Some(json!({ "args": args })),
            )
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "git-failed",
            format!("git {} failed", args.join(" ")),
            Some(json!({
                "args": args,
                "stderr": String::from_utf8_lossy(&output.stderr).trim(),
            })),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn is_docs_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("docs/") || lower.contains("/docs/") {
        return true;
    }
    let file_name = Path::new(&lower)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    matches!(
        file_name,
        "readme.md" | "agents.md" | "development.md" | "changelog.md" | "license" | "license.md"
    ) || matches!(
        Path::new(&lower)
            .extension()
            .and_then(|value| value.to_str()),
        Some("md" | "mdx" | "rst" | "adoc" | "txt")
    )
}

#[derive(Debug, Parser)]
#[command(
    name = "docs-impact",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Scan Git changes for documentation impact.",
    long_about = "Scan Git changes and classify whether implementation work requires documentation updates.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  docs-impact scan --include-untracked\n  docs-impact scan --repo . --base origin/main --format json\n  docs-impact completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Scan changed files and classify documentation impact.
    Scan(ScanArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct ScanArgs {
    /// Git worktree to scan.
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    repo: PathBuf,

    /// Base ref for committed branch comparison (`<base>...HEAD`).
    #[arg(long, value_name = "REF")]
    base: Option<String>,

    /// Include untracked files from `git ls-files --others --exclude-standard`.
    #[arg(long)]
    include_untracked: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    shell: CompletionShell,
}

#[derive(Debug, Serialize)]
struct ScanResult {
    repo: String,
    base: Option<String>,
    include_untracked: bool,
    docs_changed: bool,
    non_docs_changed: bool,
    docs_files: Vec<String>,
    non_docs_files: Vec<String>,
    suggested_review: Vec<String>,
}

impl ScanResult {
    fn text_summary(&self) -> String {
        format!(
            "docs-impact: docs_changed={} non_docs_changed={} docs_files={} non_docs_files={}",
            self.docs_changed,
            self.non_docs_changed,
            self.docs_files.len(),
            self.non_docs_files.len()
        )
    }
}
