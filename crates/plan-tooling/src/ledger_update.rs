//! `plan-tooling ledger-update` subcommand: patch one row in an execution-state
//! `## Task Ledger` table.

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

use crate::ledger::{self, LedgerError};

const SCHEMA_VERSION: &str = "plan-tooling.ledger-update.v1";

const USAGE: &str = r#"Usage:
  plan-tooling ledger-update \
    --execution-state <path> \
    --task <id> \
    --status <status> \
    --evidence <text> \
    [--notes <text>] [--dry-run] [--format json|text]

Purpose:
  Patch one row in an execution-state `## Task Ledger` table. Locates the row
  by exact `ID` column match, replaces the `Status` cell, appends to
  `Evidence` (with `; ` separator when a value already exists), and updates
  `Notes` only when `--notes` is passed. Writes are atomic (temp + rename);
  every other byte of the file is preserved.

Options:
  --execution-state <path>  Path to the `<slug>-execution-state.md` file
  --task <id>               Ledger row ID to patch (exact match)
  --status <status>         New status: pending | in-progress | done | deferred |
                            blocked | waived
  --evidence <text>         Evidence text to merge into the Evidence cell
                            (empty string preserves existing value)
  --notes <text>            Optional notes value; omit to leave Notes untouched
  --dry-run                 Compute the patch but do not write the file
  --format <fmt>            text (default) or json
  -h, --help                Show help

Exit:
  0: row patched (or dry-run succeeded)
  1: row not found / ambiguous / malformed table / write failure
  2: usage error
"#;

#[derive(Debug, Serialize)]
struct LedgerUpdateOutput {
    schema_version: &'static str,
    ok: bool,
    operation: &'static str,
    dry_run: bool,
    file: String,
    task_id: String,
    status: StatusDelta,
    evidence: EvidenceDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<NotesDelta>,
    file_changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
struct StatusDelta {
    previous: String,
    new: String,
}

#[derive(Debug, Serialize)]
struct EvidenceDelta {
    previous: String,
    new: String,
    appended: bool,
}

#[derive(Debug, Serialize)]
struct NotesDelta {
    previous: String,
    new: String,
    changed: bool,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Default)]
struct Config {
    execution_state: Option<PathBuf>,
    task: Option<String>,
    status: Option<String>,
    evidence: Option<String>,
    notes: Option<String>,
    dry_run: bool,
    format: String,
}

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die(msg: &str) -> i32 {
    eprintln!("plan-tooling ledger-update: {msg}");
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
            "--execution-state" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--execution-state requires a path");
                };
                cfg.execution_state = Some(PathBuf::from(value));
                i += 2;
            }
            "--task" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--task requires an ID");
                };
                cfg.task = Some(value.to_string());
                i += 2;
            }
            "--status" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--status requires a value");
                };
                cfg.status = Some(value.to_string());
                i += 2;
            }
            "--evidence" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--evidence requires a value");
                };
                cfg.evidence = Some(value.to_string());
                i += 2;
            }
            "--notes" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--notes requires a value");
                };
                cfg.notes = Some(value.to_string());
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
    let Some(path) = cfg.execution_state.clone() else {
        return die("--execution-state is required");
    };
    let Some(task_id) = cfg.task.clone() else {
        return die("--task is required");
    };
    let Some(status) = cfg.status.clone() else {
        return die("--status is required");
    };
    let Some(evidence) = cfg.evidence.clone() else {
        return die("--evidence is required (pass an empty string to keep the existing value)");
    };

    match ledger::update_row(
        &path,
        &task_id,
        &status,
        &evidence,
        cfg.notes.as_deref(),
        cfg.dry_run,
    ) {
        Ok(outcome) => {
            let file_changed = outcome.previous_status != outcome.new_status
                || outcome.previous_evidence != outcome.new_evidence
                || outcome.notes_changed;
            let notes = cfg.notes.as_ref().map(|_| NotesDelta {
                previous: outcome.previous_notes.clone(),
                new: outcome.new_notes.clone(),
                changed: outcome.notes_changed,
            });
            let output = LedgerUpdateOutput {
                schema_version: SCHEMA_VERSION,
                ok: true,
                operation: "ledger-update",
                dry_run: cfg.dry_run,
                file: path.to_string_lossy().to_string(),
                task_id: outcome.task_id.clone(),
                status: StatusDelta {
                    previous: outcome.previous_status.clone(),
                    new: outcome.new_status.clone(),
                },
                evidence: EvidenceDelta {
                    previous: outcome.previous_evidence.clone(),
                    new: outcome.new_evidence.clone(),
                    appended: outcome.evidence_appended,
                },
                notes,
                file_changed,
                error: None,
            };
            print_output(&output, &cfg.format, file_changed, &outcome.task_id);
            0
        }
        Err(err) => {
            let code = err.code().to_string();
            let message = err.to_string();
            let output = LedgerUpdateOutput {
                schema_version: SCHEMA_VERSION,
                ok: false,
                operation: "ledger-update",
                dry_run: cfg.dry_run,
                file: path.to_string_lossy().to_string(),
                task_id: task_id.clone(),
                status: StatusDelta {
                    previous: String::new(),
                    new: status.clone(),
                },
                evidence: EvidenceDelta {
                    previous: String::new(),
                    new: evidence.clone(),
                    appended: false,
                },
                notes: None,
                file_changed: false,
                error: Some(ErrorBody {
                    code: code.clone(),
                    message: message.clone(),
                }),
            };
            print_output(&output, &cfg.format, false, &task_id);
            match err {
                LedgerError::InvalidStatus { .. } => 2,
                _ => 1,
            }
        }
    }
}

fn print_output(output: &LedgerUpdateOutput, format: &str, file_changed: bool, task_id: &str) {
    if format == "json" {
        match serde_json::to_string_pretty(output) {
            Ok(s) => println!("{s}"),
            Err(err) => eprintln!("plan-tooling ledger-update: failed to serialize JSON: {err}"),
        }
        return;
    }
    if let Some(err) = &output.error {
        eprintln!("plan-tooling ledger-update: {} ({})", err.message, err.code);
        return;
    }
    let verb = if output.dry_run {
        "would patch"
    } else if file_changed {
        "patched"
    } else {
        "no-op"
    };
    println!(
        "{verb} {task} in {file}: status {old_status} -> {new_status}",
        verb = verb,
        task = task_id,
        file = output.file,
        old_status = output.status.previous,
        new_status = output.status.new,
    );
    if output.evidence.previous != output.evidence.new {
        println!(
            "  evidence: {} -> {}",
            display_value(&output.evidence.previous),
            display_value(&output.evidence.new),
        );
    }
    if let Some(notes) = &output.notes
        && notes.changed
    {
        println!(
            "  notes:    {} -> {}",
            display_value(&notes.previous),
            display_value(&notes.new),
        );
    }
}

fn display_value(value: &str) -> String {
    if value.is_empty() {
        "<empty>".to_string()
    } else {
        value.to_string()
    }
}
