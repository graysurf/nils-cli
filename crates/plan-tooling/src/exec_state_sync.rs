//! `plan-tooling exec-state-sync` subcommand.
//!
//! On-demand repair surface over [`crate::exec_state`]: writes the tracking
//! issue URL and/or terminal-state header bullets into a bundle's
//! `*-execution-state.md`. The same byte-preserving routine that `plan-issue
//! record open` / `record close` call automatically, exposed so existing bundles
//! (issue exists, but the local Markdown lacks the URL or is frozen at a
//! mid-flight status) can be brought to their final state without any provider
//! lookup.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::exec_state::{self, ExecStateError, SyncReport, TerminalState};

const SCHEMA_VERSION: &str = "plan-tooling.exec-state-sync.v1";

const USAGE: &str = r#"Usage:
  plan-tooling exec-state-sync \
    (--bundle <dir> | --execution-state <path>) \
    [--issue-url <url>] [--status <text>] [--last-updated <date>] \
    [--branch-commit-pr <text>] [--current-task <text>] \
    [--next-task <text>] [--handoff <markdown>] [--dry-run] [--format json|text]

Purpose:
  Repair a plan bundle's `*-execution-state.md` by writing the tracking issue
  URL and/or terminal-state header bullets, offline and byte-preserving. The
  `## Task Ledger` rows are never touched.

Options:
  --bundle <dir>            Plan bundle directory containing `*-execution-state.md`
  --execution-state <path>  Explicit execution-state file (overrides --bundle lookup)
  --issue-url <url>         Tracking issue URL to record (autolinked)
  --status <text>           Terminal `Status` value (e.g. `complete; tracking issue closed`)
  --last-updated <date>     `Last updated` stamp (e.g. 2026-06-01)
  --branch-commit-pr <text> `Branch/commit/PR` value (e.g. merged PR ref/URL)
  --current-task <text>     `Current task` value
  --next-task <text>        `Next task` value
  --handoff <markdown>      Replacement body for the `## Handoff` section
  --dry-run                 Report the change set without writing the file
  --format <fmt>            text (default) or json
  -h, --help                Show help

Exit:
  0: sync report emitted
  1: bundle / execution-state read or write error
  2: usage error
"#;

#[derive(Debug, Default)]
struct Config {
    bundle: Option<PathBuf>,
    execution_state: Option<PathBuf>,
    issue_url: Option<String>,
    status: Option<String>,
    last_updated: Option<String>,
    branch_commit_pr: Option<String>,
    current_task: Option<String>,
    next_task: Option<String>,
    handoff: Option<String>,
    dry_run: bool,
    format: String,
}

#[derive(Debug, Serialize)]
struct Output {
    schema_version: &'static str,
    ok: bool,
    operation: &'static str,
    dry_run: bool,
    execution_state_file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<SyncReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: String,
    message: String,
}

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die(msg: &str) -> i32 {
    eprintln!("plan-tooling exec-state-sync: {msg}");
    print_usage();
    2
}

pub fn run(args: &[String]) -> i32 {
    let mut cfg = Config {
        format: "text".to_string(),
        ..Config::default()
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bundle" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--bundle requires a directory");
                };
                cfg.bundle = Some(PathBuf::from(value));
                i += 2;
            }
            "--execution-state" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--execution-state requires a path");
                };
                cfg.execution_state = Some(PathBuf::from(value));
                i += 2;
            }
            "--issue-url" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--issue-url requires a URL");
                };
                cfg.issue_url = Some(value.to_string());
                i += 2;
            }
            "--status" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--status requires text");
                };
                cfg.status = Some(value.to_string());
                i += 2;
            }
            "--last-updated" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--last-updated requires a date");
                };
                cfg.last_updated = Some(value.to_string());
                i += 2;
            }
            "--branch-commit-pr" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--branch-commit-pr requires text");
                };
                cfg.branch_commit_pr = Some(value.to_string());
                i += 2;
            }
            "--current-task" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--current-task requires text");
                };
                cfg.current_task = Some(value.to_string());
                i += 2;
            }
            "--next-task" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--next-task requires text");
                };
                cfg.next_task = Some(value.to_string());
                i += 2;
            }
            "--handoff" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--handoff requires markdown");
                };
                cfg.handoff = Some(value.to_string());
                i += 2;
            }
            "--dry-run" => {
                cfg.dry_run = true;
                i += 1;
            }
            "--format" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--format requires a value");
                };
                cfg.format = value.to_string();
                i += 2;
            }
            "-h" | "--help" => {
                let _ = std::io::stdout().write_all(USAGE.as_bytes());
                return 0;
            }
            other => return die(&format!("unknown argument: {other}")),
        }
    }

    if cfg.format != "text" && cfg.format != "json" {
        return die(&format!(
            "invalid --format (expected text|json): {}",
            cfg.format
        ));
    }
    if cfg.issue_url.is_none()
        && cfg.status.is_none()
        && cfg.last_updated.is_none()
        && cfg.branch_commit_pr.is_none()
        && cfg.current_task.is_none()
        && cfg.next_task.is_none()
        && cfg.handoff.is_none()
    {
        return die("nothing to sync: pass at least one terminal field option");
    }

    let exec_state_path = match resolve_execution_state(&cfg) {
        Ok(path) => path,
        Err(code) => return code,
    };

    let state = TerminalState {
        status: cfg.status.clone(),
        last_updated: cfg.last_updated.clone(),
        branch_commit_pr: cfg.branch_commit_pr.clone(),
        tracking_issue_url: cfg.issue_url.clone(),
        current_task: cfg.current_task.clone(),
        next_task: cfg.next_task.clone(),
        handoff: cfg.handoff.clone(),
    };

    match exec_state::writeback_terminal(&exec_state_path, &state, cfg.dry_run) {
        Ok(report) => {
            let output = Output {
                schema_version: SCHEMA_VERSION,
                ok: true,
                operation: "exec-state-sync",
                dry_run: cfg.dry_run,
                execution_state_file: exec_state_path.to_string_lossy().to_string(),
                report: Some(report),
                error: None,
            };
            print_output(&output, &cfg.format);
            0
        }
        Err(err) => emit_error(&cfg, &exec_state_path.to_string_lossy(), err),
    }
}

/// Resolve the execution-state file: explicit `--execution-state`, else the
/// single `*-execution-state.md` inside `--bundle`.
fn resolve_execution_state(cfg: &Config) -> Result<PathBuf, i32> {
    if let Some(path) = &cfg.execution_state {
        return Ok(path.clone());
    }
    let Some(bundle) = &cfg.bundle else {
        return Err(die(
            "either --execution-state <path> or --bundle <dir> is required",
        ));
    };
    match find_execution_state(bundle) {
        Some(path) => Ok(path),
        None => Err(die(&format!(
            "no `*-execution-state.md` found in bundle {}",
            bundle.display()
        ))),
    }
}

fn find_execution_state(bundle: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(bundle).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("-execution-state.md"))
                .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

fn emit_error(cfg: &Config, exec_state_file: &str, err: ExecStateError) -> i32 {
    let output = Output {
        schema_version: SCHEMA_VERSION,
        ok: false,
        operation: "exec-state-sync",
        dry_run: cfg.dry_run,
        execution_state_file: exec_state_file.to_string(),
        report: None,
        error: Some(ErrorBody {
            code: err.code().to_string(),
            message: err.to_string(),
        }),
    };
    print_output(&output, &cfg.format);
    eprintln!("plan-tooling exec-state-sync: {err}");
    1
}

fn print_output(output: &Output, format: &str) {
    if format == "json" {
        match serde_json::to_string_pretty(output) {
            Ok(s) => println!("{s}"),
            Err(err) => eprintln!("plan-tooling exec-state-sync: failed to serialize JSON: {err}"),
        }
        return;
    }
    if let Some(err) = &output.error {
        eprintln!(
            "plan-tooling exec-state-sync: {} ({})",
            err.message, err.code
        );
        return;
    }
    let mode = if output.dry_run { "dry-run" } else { "write" };
    let report = output.report.as_ref();
    let changed = report.map(|r| r.changed).unwrap_or(false);
    println!(
        "exec-state-sync ({mode}) for {}: {}",
        output.execution_state_file,
        if changed { "changed" } else { "no change" }
    );
    if let Some(report) = report {
        for bullet in &report.bullets {
            let prev = bullet.previous.as_deref().unwrap_or("<none>");
            println!(
                "  [{:?}] {}: {} -> {}",
                bullet.action, bullet.label, prev, bullet.value
            );
        }
        for section in &report.sections {
            let prev = section.previous.as_deref().unwrap_or("<none>");
            println!(
                "  [{:?}] {}: {} -> {}",
                section.action, section.heading, prev, section.value
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "## Execution State\n\n- Status: tracking issue opened\n- Tracking issue: not yet opened\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | done | x | y | z |\n";

    fn temp_bundle() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo-execution-state.md");
        fs::write(&path, SAMPLE).unwrap();
        (dir, path)
    }

    #[test]
    fn repair_writes_terminal_state_and_returns_zero() {
        let (dir, path) = temp_bundle();
        let code = run(&[
            "--bundle".into(),
            dir.path().to_string_lossy().into_owned(),
            "--issue-url".into(),
            "https://github.com/sympoies/nils-cli/issues/716".into(),
            "--status".into(),
            "complete; tracking issue closed".into(),
            "--format".into(),
            "json".into(),
        ]);
        assert_eq!(code, 0);
        let out = fs::read_to_string(&path).unwrap();
        assert!(
            out.contains("- Tracking issue: <https://github.com/sympoies/nils-cli/issues/716>")
        );
        assert!(out.contains("- Status: complete; tracking issue closed"));
        // Task Ledger preserved.
        assert!(out.contains("| 1.1 | done | x | y | z |"));
    }

    #[test]
    fn repair_writes_terminal_tasks_and_handoff() {
        let (dir, path) = temp_bundle();
        fs::write(
            &path,
            format!("{SAMPLE}\n## Handoff\n\n- Merge the PR and close the tracker.\n"),
        )
        .unwrap();
        let code = run(&[
            "--bundle".into(),
            dir.path().to_string_lossy().into_owned(),
            "--current-task".into(),
            "none; tracking issue closed".into(),
            "--next-task".into(),
            "none; tracking issue closed".into(),
            "--handoff".into(),
            "- Tracking issue closed; no action remains.".into(),
            "--format".into(),
            "json".into(),
        ]);
        assert_eq!(code, 0);
        let written = fs::read_to_string(path).unwrap();
        assert!(written.contains("- Current task: none; tracking issue closed"));
        assert!(written.contains("- Next task: none; tracking issue closed"));
        assert!(written.contains("## Handoff\n\n- Tracking issue closed; no action remains."));
        assert!(!written.contains("Merge the PR"));
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let (_dir, path) = temp_bundle();
        let before = fs::read_to_string(&path).unwrap();
        let code = run(&[
            "--execution-state".into(),
            path.to_string_lossy().into_owned(),
            "--issue-url".into(),
            "https://github.com/o/r/issues/9".into(),
            "--dry-run".into(),
        ]);
        assert_eq!(code, 0);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            before,
            "dry-run must not write"
        );
    }

    #[test]
    fn no_fields_is_usage_error() {
        let (dir, _path) = temp_bundle();
        let code = run(&["--bundle".into(), dir.path().to_string_lossy().into_owned()]);
        assert_eq!(code, 2);
    }

    #[test]
    fn missing_target_is_usage_error() {
        let code = run(&[
            "--issue-url".into(),
            "https://github.com/o/r/issues/9".into(),
        ]);
        assert_eq!(code, 2);
    }
}
