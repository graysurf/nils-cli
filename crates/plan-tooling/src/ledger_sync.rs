//! `plan-tooling ledger-sync --from-issue` subcommand.
//!
//! Compares a bundle's `## Task Ledger` Evidence column against the URLs that
//! the tracking issue's `state`-role lifecycle comments cite per task. Emits a
//! drift report; `--write` patches the Evidence cell only when it is empty
//! (the empty-cell preference rule from the plan).

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::ledger::{self, LedgerError};

const SCHEMA_VERSION: &str = "plan-tooling.ledger-sync.v1";

const USAGE: &str = r#"Usage:
  plan-tooling ledger-sync \
    --bundle <dir> \
    [--body-file <path> --comments-json <path> | --fixture <dir>] \
    [--write] [--format json|text]

Purpose:
  Reconcile an execution-state `## Task Ledger` Evidence column against the
  URLs cited by the tracking issue's `state`-role lifecycle comments. Without
  --write, emits a drift report. With --write, fills empty Evidence cells.

Options:
  --bundle <dir>           Plan bundle directory containing `*-execution-state.md`
  --body-file <path>       Issue body markdown file
  --comments-json <path>   Issue comments JSON file (from `gh issue view --json comments`)
  --fixture <dir>          Fixture directory containing `body.md` + `comments.json`
                           (alternative to --body-file/--comments-json)
  --write                  Patch the ledger file (empty-cell preference rule)
  --format <fmt>           text (default) or json
  -h, --help               Show help

Exit:
  0: drift report emitted
  1: bundle / ledger / fixture read error
  2: usage error
"#;

#[derive(Debug, Default)]
struct Config {
    bundle: Option<PathBuf>,
    body_file: Option<PathBuf>,
    comments_json: Option<PathBuf>,
    fixture: Option<PathBuf>,
    write: bool,
    format: String,
}

#[derive(Debug, Serialize)]
struct SyncOutput {
    schema_version: &'static str,
    ok: bool,
    operation: &'static str,
    write: bool,
    bundle: String,
    ledger_file: String,
    entries: Vec<DriftEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    patched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
struct DriftEntry {
    task_id: String,
    action: &'static str,
    ledger_evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch_applied: Option<bool>,
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
    eprintln!("plan-tooling ledger-sync: {msg}");
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
            "--body-file" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--body-file requires a path");
                };
                cfg.body_file = Some(PathBuf::from(value));
                i += 2;
            }
            "--comments-json" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--comments-json requires a path");
                };
                cfg.comments_json = Some(PathBuf::from(value));
                i += 2;
            }
            "--fixture" => {
                let Some(value) = args.get(i + 1) else {
                    return die("--fixture requires a directory");
                };
                cfg.fixture = Some(PathBuf::from(value));
                i += 2;
            }
            "--write" => {
                cfg.write = true;
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
    let Some(bundle) = cfg.bundle.clone() else {
        return die("--bundle is required");
    };

    let (body_path, comments_path) = match resolve_issue_inputs(&cfg) {
        Ok(paths) => paths,
        Err(code) => return code,
    };

    let ledger_path = match find_ledger(&bundle) {
        Ok(path) => path,
        Err(err) => return emit_error(&cfg, &bundle, "<unknown>", err),
    };

    match run_sync(&bundle, &ledger_path, &body_path, &comments_path, cfg.write) {
        Ok((entries, patched)) => {
            let output = SyncOutput {
                schema_version: SCHEMA_VERSION,
                ok: true,
                operation: "ledger-sync",
                write: cfg.write,
                bundle: bundle.to_string_lossy().to_string(),
                ledger_file: ledger_path.to_string_lossy().to_string(),
                entries,
                patched,
                error: None,
            };
            print_output(&output, &cfg.format);
            0
        }
        Err(SyncError::Ledger(err)) => {
            emit_error(&cfg, &bundle, &ledger_path.to_string_lossy(), err)
        }
        Err(SyncError::Io { code, message }) => {
            let output = SyncOutput {
                schema_version: SCHEMA_VERSION,
                ok: false,
                operation: "ledger-sync",
                write: cfg.write,
                bundle: bundle.to_string_lossy().to_string(),
                ledger_file: ledger_path.to_string_lossy().to_string(),
                entries: Vec::new(),
                patched: Vec::new(),
                error: Some(ErrorBody {
                    code,
                    message: message.clone(),
                }),
            };
            print_output(&output, &cfg.format);
            eprintln!("plan-tooling ledger-sync: {message}");
            1
        }
    }
}

fn resolve_issue_inputs(cfg: &Config) -> Result<(PathBuf, PathBuf), i32> {
    if let Some(fixture) = &cfg.fixture {
        if cfg.body_file.is_some() || cfg.comments_json.is_some() {
            return Err(die(
                "--fixture is mutually exclusive with --body-file / --comments-json",
            ));
        }
        return Ok((fixture.join("body.md"), fixture.join("comments.json")));
    }
    match (&cfg.body_file, &cfg.comments_json) {
        (Some(body), Some(comments)) => Ok((body.clone(), comments.clone())),
        (None, None) => Err(die(
            "either --fixture <dir> OR both --body-file <path> and --comments-json <path> are required",
        )),
        _ => Err(die(
            "--body-file and --comments-json must be passed together",
        )),
    }
}

fn find_ledger(bundle: &Path) -> Result<PathBuf, LedgerError> {
    let read = fs::read_dir(bundle).map_err(|source| LedgerError::ReadFailed {
        path: bundle.to_path_buf(),
        source,
    })?;
    for entry in read.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with("-execution-state.md")
        {
            return Ok(path);
        }
    }
    Err(LedgerError::ReadFailed {
        path: bundle.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no `*-execution-state.md` in bundle",
        ),
    })
}

enum SyncError {
    Ledger(LedgerError),
    Io { code: String, message: String },
}

impl From<LedgerError> for SyncError {
    fn from(value: LedgerError) -> Self {
        SyncError::Ledger(value)
    }
}

fn run_sync(
    _bundle: &Path,
    ledger_path: &Path,
    _body_path: &Path,
    comments_path: &Path,
    write: bool,
) -> Result<(Vec<DriftEntry>, Vec<String>), SyncError> {
    let raw = fs::read_to_string(ledger_path).map_err(|source| SyncError::Io {
        code: "ledger-file-read-failed".to_string(),
        message: format!("{}: {source}", ledger_path.display()),
    })?;
    let ledger_rows = ledger::read_rows(&raw, ledger_path)?;

    let comments_raw = fs::read_to_string(comments_path).map_err(|source| SyncError::Io {
        code: "comments-json-read-failed".to_string(),
        message: format!("{}: {source}", comments_path.display()),
    })?;
    let issue_map = parse_issue_evidence(&comments_raw).map_err(|message| SyncError::Io {
        code: "comments-json-parse-failed".to_string(),
        message,
    })?;

    let mut entries: Vec<DriftEntry> = Vec::new();
    let mut patched: Vec<String> = Vec::new();
    for row in &ledger_rows {
        let ledger_evidence = row.evidence.clone();
        let issue_url = issue_map.get(&row.id).cloned();
        let action = classify(&ledger_evidence, &issue_url);
        let mut patch_applied = None;
        if write && action != "match" && ledger_evidence.is_empty() && issue_url.is_some() {
            let url = issue_url.clone().unwrap();
            match ledger::update_row(ledger_path, &row.id, &row.status, &url, None, false) {
                Ok(_) => {
                    patched.push(row.id.clone());
                    patch_applied = Some(true);
                }
                Err(err) => return Err(SyncError::Ledger(err)),
            }
        } else if write {
            patch_applied = Some(false);
        }
        entries.push(DriftEntry {
            task_id: row.id.clone(),
            action,
            ledger_evidence,
            issue_evidence: issue_url,
            patch_applied,
        });
    }

    Ok((entries, patched))
}

fn classify(ledger: &str, issue: &Option<String>) -> &'static str {
    match issue {
        None => "missing",
        Some(url) => {
            if ledger.contains(url.as_str()) {
                "match"
            } else {
                "drift"
            }
        }
    }
}

fn parse_issue_evidence(comments_json: &str) -> Result<HashMap<String, String>, String> {
    let root: Value = serde_json::from_str(comments_json)
        .map_err(|err| format!("comments JSON parse error: {err}"))?;
    let comments = root
        .get("comments")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "comments JSON missing `comments` array".to_string())?;
    let mut map: HashMap<String, String> = HashMap::new();
    for comment in comments {
        let body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");
        let url = comment.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if body.is_empty() || url.is_empty() {
            continue;
        }
        if let Some(payload) = extract_payload(body)
            && let Ok(tasks) = decode_state_tasks(&payload)
        {
            for task_id in tasks {
                // Later comments overwrite earlier mappings (most recent wins).
                map.insert(task_id, url.to_string());
            }
        }
    }
    Ok(map)
}

fn extract_payload(body: &str) -> Option<String> {
    const PREFIX: &str = "<!-- plan-issue-record-payload:hex:";
    let start = body.find(PREFIX)?;
    let after = &body[start + PREFIX.len()..];
    let end = after.find(" -->")?;
    Some(after[..end].to_string())
}

fn decode_state_tasks(hex: &str) -> Result<Vec<String>, String> {
    let bytes = decode_hex(hex)?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("payload JSON decode: {err}"))?;
    if value.get("role").and_then(|r| r.as_str()) != Some("state") {
        return Ok(Vec::new());
    }
    let tasks = value
        .get("data")
        .and_then(|d| d.get("tasks"))
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ids = Vec::new();
    for task in tasks {
        if let Some(id) = task.get("id").and_then(|i| i.as_str()) {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    if !input.len().is_multiple_of(2) {
        return Err("hex payload has odd length".to_string());
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let bytes = input.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = hex_value(pair[0])
            .ok_or_else(|| format!("invalid hex digit `{}`", char::from(pair[0])))?;
        let lo = hex_value(pair[1])
            .ok_or_else(|| format!("invalid hex digit `{}`", char::from(pair[1])))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn emit_error(cfg: &Config, bundle: &Path, ledger_file: &str, err: LedgerError) -> i32 {
    let output = SyncOutput {
        schema_version: SCHEMA_VERSION,
        ok: false,
        operation: "ledger-sync",
        write: cfg.write,
        bundle: bundle.to_string_lossy().to_string(),
        ledger_file: ledger_file.to_string(),
        entries: Vec::new(),
        patched: Vec::new(),
        error: Some(ErrorBody {
            code: err.code().to_string(),
            message: err.to_string(),
        }),
    };
    print_output(&output, &cfg.format);
    eprintln!("plan-tooling ledger-sync: {err}");
    1
}

fn print_output(output: &SyncOutput, format: &str) {
    if format == "json" {
        match serde_json::to_string_pretty(output) {
            Ok(s) => println!("{s}"),
            Err(err) => eprintln!("plan-tooling ledger-sync: failed to serialize JSON: {err}"),
        }
        return;
    }
    if let Some(err) = &output.error {
        eprintln!("plan-tooling ledger-sync: {} ({})", err.message, err.code);
        return;
    }
    let mode = if output.write { "write" } else { "report" };
    println!(
        "ledger-sync ({mode}) for {}: {} entries",
        output.ledger_file,
        output.entries.len()
    );
    for entry in &output.entries {
        let issue = entry.issue_evidence.as_deref().unwrap_or("<none>");
        let patched = match entry.patch_applied {
            Some(true) => " [patched]",
            Some(false) => " [unchanged]",
            None => "",
        };
        let ledger_repr = if entry.ledger_evidence.is_empty() {
            "<empty>".to_string()
        } else {
            entry.ledger_evidence.clone()
        };
        println!(
            "  {:<8} {}: ledger={} issue={}{}",
            format!("[{}]", entry.action),
            entry.task_id,
            ledger_repr,
            issue,
            patched,
        );
    }
}
