//! Heuristic Inbox primitive: manage curated heuristic-system case folders.
//!
//! Behaviour parity with the original `heuristic-error-inbox` Python
//! implementation is the contract; see HEURISTIC_SYSTEM.md.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum, ValueHint};
use nils_common::fs::home_dir;
use nils_common::process;
use nils_markdown::Engine;
use nils_term::prompt::{self, PromptError, PromptOptions};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::common::{
    CliError, OutputFormat, absolute_path, display_path, render_error, render_success,
};
use crate::completion::{self, CompletionShell};

const ARCHIVE_TEMPLATE: &str = include_str!("../templates/heuristic_inbox/archive.md.tera");
const ARCHIVE_TEMPLATE_NAME: &str = "heuristic_inbox_archive";

const NEXT_ACTION_TEMPLATE: &str = include_str!("../templates/heuristic_inbox/next_action.md.tera");
const NEXT_ACTION_TEMPLATE_NAME: &str = "heuristic_inbox_next_action";

#[derive(Debug, Serialize)]
struct ArchiveSectionView<'a> {
    archive_date: &'a str,
    reason: &'a str,
    link: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct NextActionSectionView<'a> {
    body: &'a str,
}

const LIST_SCHEMA_VERSION: &str = "cli.heuristic-inbox.list.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.heuristic-inbox.verify.v1";
const NEW_SCHEMA_VERSION: &str = "cli.heuristic-inbox.new.v1";
const SET_STATUS_SCHEMA_VERSION: &str = "cli.heuristic-inbox.set-status.v1";
const ARCHIVE_SCHEMA_VERSION: &str = "cli.heuristic-inbox.archive.v1";
const INGEST_SCHEMA_VERSION: &str = "cli.heuristic-inbox.ingest-evidence.v1";
const DELIVER_SCHEMA_VERSION: &str = "cli.heuristic-inbox.deliver.v1";

const LIST_COMMAND: &str = "heuristic-inbox list";
const VERIFY_COMMAND: &str = "heuristic-inbox verify";
const NEW_COMMAND: &str = "heuristic-inbox new";
const SET_STATUS_COMMAND: &str = "heuristic-inbox set-status";
const ARCHIVE_COMMAND: &str = "heuristic-inbox archive";
const INGEST_COMMAND: &str = "heuristic-inbox ingest-evidence";
const DELIVER_COMMAND: &str = "heuristic-inbox deliver";

const RAW_RECORD_FILENAME: &str = "skill-usage.record.json";
const RAW_RECORD_SCHEMA: &str = "skill-usage.record.v1";
const DEFAULT_EVIDENCE_MAX_BYTES: u64 = 64 * 1024;

const REQUIRED_INBOX_SECTIONS: &[&str] = &[
    "Status",
    "Signal",
    "Evidence",
    "Impact",
    "Current Workaround",
    "Promotion Criteria",
    "Next Action",
];

const REQUIRED_RECORD_SECTIONS: &[&str] = &[
    "Status",
    "Signal",
    "Evidence",
    "Diagnosis",
    "Promotion Decision",
    "Durable Fix",
    "Validation",
];

// Inbox-entry status fields plus the operation-record-only fields
// (Cluster / Superseded-by / Enforced-by). Inbox entries simply lack the
// record-only lines, so parsing them here does not change inbox behavior.
const STATUS_FIELDS: &[&str] = &[
    "Status",
    "First observed",
    "Area",
    "Severity",
    "Cluster",
    "Superseded-by",
    "Enforced-by",
];

const VALID_STATUSES: &[&str] = &["open", "promoted", "wontfix"];
const RETIRED_STATUSES: &[&str] = &["triaged", "planned"];
const ARCHIVE_READY_STATUSES: &[&str] = &["promoted", "wontfix"];
const VALID_SEVERITIES: &[&str] = &["low", "medium", "high"];

// Operation-record lifecycle vocabulary (defined in the consuming repo's
// Heuristic System policy; the CLI side enforces these values).
const VALID_RECORD_STATUSES: &[&str] = &["active", "superseded", "retired"];
const RECORD_ARCHIVE_READY_STATUSES: &[&str] = &["superseded", "retired"];

fn readable_statuses() -> &'static [&'static str] {
    &["open", "promoted", "wontfix", "triaged", "planned"]
}

fn slug_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").expect("slug regex"))
}

fn home_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/(?:Users|home)/[A-Za-z0-9._-]+").expect("home path regex"))
}

fn token_regexes() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        // Patterns that begin with a short literal prefix (`sk-`, `Bearer`) need
        // a leading boundary so they only match a real token, not the same
        // substring inside an ordinary hyphenated identifier (e.g. the `sk-`
        // inside `task-ledger-durability`). The `regex` crate has no lookbehind,
        // so we require the preceding char to be the start of input or a
        // non-identifier byte. These patterns are only used via `is_match`, so
        // consuming that leading byte is harmless.
        let patterns = [
            r"(?:^|[^A-Za-z0-9_-])sk-[A-Za-z0-9_-]{16,}",
            r"(?:^|[^A-Za-z0-9_-])Bearer\s+[A-Za-z0-9._~+/=-]{16,}",
            r"(?i)\btoken\s*[:=]\s*[\x22']?[A-Za-z0-9._~+/=-]{16,}",
            r"(?i)\bapi[_-]?key\s*[:=]\s*[\x22']?[A-Za-z0-9._~+/=-]{16,}",
            r"-----BEGIN [A-Z ]+-----",
            r"(?i)(?:password|credential)\s*[:=]\s*[\x22']?[A-Za-z0-9._~+/=-]{8,}",
        ];
        patterns
            .iter()
            .map(|p| Regex::new(p).expect("token regex"))
            .collect()
    })
}

fn status_line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^-\s+Status:\s*\S+\s*$").expect("status line regex"))
}

/// Matches the whole `- Status:` line regardless of how many tokens the value
/// has. Inbox statuses are always single tokens, but operation records may
/// still carry a multi-word free-text status (e.g. `- Status:
/// implemented and validated`) that the record lifecycle migration must be able
/// to rewrite.
fn record_status_line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^-\s+Status:.*$").expect("record status line regex"))
}

fn date_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("date regex"))
}

fn label_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9._-]+$").expect("label regex"))
}

fn suffix_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\.[A-Za-z0-9]+$").expect("suffix regex"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CaseKind {
    Inbox,
    Record,
}

impl CaseKind {
    fn as_str(self) -> &'static str {
        match self {
            CaseKind::Inbox => "inbox",
            CaseKind::Record => "record",
        }
    }

    fn required_sections(self) -> &'static [&'static str] {
        match self {
            CaseKind::Inbox => REQUIRED_INBOX_SECTIONS,
            CaseKind::Record => REQUIRED_RECORD_SECTIONS,
        }
    }
}

#[derive(Debug, Clone)]
struct Case {
    folder: PathBuf,
    doc_path: PathBuf,
    kind: CaseKind,
}

impl Case {
    fn evidence_dir(&self) -> PathBuf {
        self.folder.join("evidence")
    }
}

#[derive(Debug, Clone, Serialize)]
struct Violation {
    kind: String,
    message: String,
}

impl Violation {
    fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// File / text helpers
// ---------------------------------------------------------------------------

fn default_inbox_dir() -> PathBuf {
    PathBuf::from("heuristic-system").join("error-inbox")
}

fn read_text(path: &Path) -> Result<String, CliError> {
    fs::read_to_string(path).map_err(|err| {
        CliError::runtime(
            "read-failed",
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

fn write_text(path: &Path, body: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "create-dir-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }
    fs::write(path, body).map_err(|err| {
        CliError::runtime(
            "write-failed",
            format!("failed to write {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

fn normalize_text(value: &str) -> String {
    let trimmed = value.trim().to_lowercase();
    let mut out = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out
}

fn title_from_slug(slug: &str) -> String {
    slug.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let upper = first.to_uppercase().collect::<String>();
                    upper + chars.as_str()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn today_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days since epoch -> civil date (Howard Hinnant algorithm).
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn normalize_home_paths(text: &str) -> String {
    let mut result = text.to_string();
    if let Some(home) = home_dir().and_then(|p| p.to_str().map(|s| s.to_string()))
        && !home.is_empty()
        && result.contains(&home)
    {
        result = result.replace(&home, "<workspace>");
    }
    home_path_regex()
        .replace_all(&result, "<workspace>")
        .into_owned()
}

fn redact_summary(value: &str) -> String {
    static SK_RE: OnceLock<Regex> = OnceLock::new();
    static CRED_RE: OnceLock<Regex> = OnceLock::new();
    let sk = SK_RE.get_or_init(|| Regex::new(r"sk-[A-Za-z0-9_-]+").expect("sk regex"));
    let cred = CRED_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(secret|token|password|credential)[A-Za-z0-9_:=/-]*")
            .expect("cred regex")
    });
    let first = sk.replace_all(value, "[redacted]");
    let second = cred.replace_all(&first, "[redacted]");
    second.trim().to_string()
}

fn strip_inline_code(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('`') && trimmed.ends_with('`') {
        trimmed[1..trimmed.len() - 1].trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_title(text: &str) -> String {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

fn extract_sections(text: &str) -> BTreeMap<String, String> {
    let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            let key = rest.trim().to_string();
            sections.entry(key.clone()).or_default();
            current = Some(key);
            continue;
        }
        if let Some(name) = &current {
            sections.get_mut(name).unwrap().push(line.to_string());
        }
    }
    sections
        .into_iter()
        .map(|(k, v)| (k, v.join("\n").trim().to_string()))
        .collect()
}

fn extract_status_fields(status_section: &str) -> BTreeMap<String, String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^-\s+([^:]+):\s*(.*)$").expect("status field regex"));
    let mut fields = BTreeMap::new();
    for line in status_section.lines() {
        let trimmed = line.trim();
        if let Some(caps) = re.captures(trimmed) {
            let key = caps.get(1).unwrap().as_str().trim();
            if STATUS_FIELDS.contains(&key) {
                let normalized_key = key.to_lowercase().replace(' ', "_");
                let value = strip_inline_code(caps.get(2).unwrap().as_str());
                fields.insert(normalized_key, value);
            }
        }
    }
    fields
}

fn extract_raw_records(evidence_section: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"^-\s+Raw record:\s*(.+?)\s*$").expect("raw record regex"));
    let mut records = Vec::new();
    for line in evidence_section.lines() {
        if let Some(caps) = re.captures(line.trim()) {
            let value = strip_inline_code(caps.get(1).unwrap().as_str());
            if !value.is_empty() {
                records.push(value);
            }
        }
    }
    records
}

#[derive(Debug, Clone)]
struct ParsedEntry {
    path: PathBuf,
    title: String,
    sections: BTreeMap<String, String>,
    fields: BTreeMap<String, String>,
    raw_records: Vec<String>,
}

fn parse_entry(path: &Path) -> Result<ParsedEntry, CliError> {
    let text = read_text(path)?;
    Ok(parse_entry_from_text(path, &text))
}

fn parse_entry_from_text(path: &Path, text: &str) -> ParsedEntry {
    let sections = extract_sections(text);
    let fields = extract_status_fields(sections.get("Status").map(String::as_str).unwrap_or(""));
    let raw_records =
        extract_raw_records(sections.get("Evidence").map(String::as_str).unwrap_or(""));
    ParsedEntry {
        path: path.to_path_buf(),
        title: extract_title(text),
        sections,
        fields,
        raw_records,
    }
}

// ---------------------------------------------------------------------------
// Case resolution
// ---------------------------------------------------------------------------

fn resolve_case(input_path: &Path) -> Result<Case, CliError> {
    if input_path.is_dir() {
        for (name, kind) in [
            ("ENTRY.md", CaseKind::Inbox),
            ("RECORD.md", CaseKind::Record),
        ] {
            let doc = input_path.join(name);
            if doc.is_file() {
                return Ok(Case {
                    folder: input_path.to_path_buf(),
                    doc_path: doc,
                    kind,
                });
            }
        }
        return Err(CliError::usage(
            "case-folder-missing-doc",
            format!(
                "case folder missing ENTRY.md or RECORD.md: {}",
                input_path.display()
            ),
            None,
        ));
    }
    if let Some(name) = input_path.file_name().and_then(|s| s.to_str()) {
        if name == "ENTRY.md" && input_path.is_file() {
            return Ok(Case {
                folder: input_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from(".")),
                doc_path: input_path.to_path_buf(),
                kind: CaseKind::Inbox,
            });
        }
        if name == "RECORD.md" && input_path.is_file() {
            return Ok(Case {
                folder: input_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from(".")),
                doc_path: input_path.to_path_buf(),
                kind: CaseKind::Record,
            });
        }
    }
    Err(CliError::usage(
        "unrecognized-case-path",
        format!(
            "unrecognized case path: {}; expected folder or ENTRY.md/RECORD.md",
            input_path.display()
        ),
        None,
    ))
}

fn iter_cases(root: &Path, doc_name: &str) -> Vec<Case> {
    if !root.exists() {
        return Vec::new();
    }
    let mut entries: Vec<_> = match fs::read_dir(root) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort_by_key(|e| e.file_name());
    let kind = if doc_name == "RECORD.md" {
        CaseKind::Record
    } else {
        CaseKind::Inbox
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().and_then(|s| s.to_str()) == Some("archive") {
            continue;
        }
        let doc = path.join(doc_name);
        if doc.is_file() {
            out.push(Case {
                folder: path,
                doc_path: doc,
                kind,
            });
        }
    }
    out
}

fn iter_archived_inbox_cases(inbox_dir: &Path) -> Vec<Case> {
    let archive_dir = inbox_dir.join("archive");
    if !archive_dir.exists() {
        return Vec::new();
    }
    let mut years: Vec<_> = match fs::read_dir(&archive_dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return Vec::new(),
    };
    years.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for year in years {
        let year_path = year.path();
        if !year_path.is_dir() {
            continue;
        }
        let mut cases: Vec<_> = match fs::read_dir(&year_path) {
            Ok(rd) => rd.flatten().collect(),
            Err(_) => continue,
        };
        cases.sort_by_key(|e| e.file_name());
        for case in cases {
            let folder = case.path();
            if !folder.is_dir() {
                continue;
            }
            let doc = folder.join("ENTRY.md");
            if doc.is_file() {
                out.push(Case {
                    folder,
                    doc_path: doc,
                    kind: CaseKind::Inbox,
                });
            }
        }
    }
    out
}

fn iter_evidence_files(case: &Case) -> Vec<PathBuf> {
    let dir = case.evidence_dir();
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![dir];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(rd) => rd.flatten().collect::<Vec<_>>(),
            Err(_) => continue,
        };
        for entry in entries {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with('.'))
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Redaction guardrail
// ---------------------------------------------------------------------------

fn evidence_violations(name: &str, text: &str, max_bytes: u64) -> Vec<Violation> {
    let mut violations = Vec::new();
    let byte_size = text.len() as u64;
    if byte_size > max_bytes {
        violations.push(Violation::new(
            "evidence_too_large",
            format!("{name}: file is {byte_size} bytes; limit is {max_bytes}"),
        ));
    }
    for pattern in token_regexes() {
        if pattern.is_match(text) {
            violations.push(Violation::new(
                "evidence_token_pattern",
                format!(
                    "{name}: matches redacted-secret pattern '{}'",
                    pattern.as_str()
                ),
            ));
            break;
        }
    }
    if home_path_regex().is_match(text) {
        violations.push(Violation::new(
            "evidence_absolute_home_path",
            format!("{name}: contains absolute home path; rewrite to <workspace>"),
        ));
    }
    violations
}

fn raw_skill_usage_json_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#""schema"\s*:\s*"skill-usage\.record\.v1""#)
            .expect("raw skill usage json regex")
    })
}

fn body_violations(text: &str, max_bytes: u64) -> Vec<Violation> {
    let mut violations = Vec::new();
    let byte_size = text.len() as u64;
    if byte_size > max_bytes {
        violations.push(Violation::new(
            "body_too_large",
            format!("body is {byte_size} bytes; limit is {max_bytes}"),
        ));
    }
    for pattern in token_regexes() {
        if pattern.is_match(text) {
            violations.push(Violation::new(
                "body_token_pattern",
                format!(
                    "body matches redacted-secret pattern '{}'",
                    pattern.as_str()
                ),
            ));
            break;
        }
    }
    if home_path_regex().is_match(text) {
        violations.push(Violation::new(
            "body_absolute_home_path",
            "body contains absolute home path; rewrite to <workspace>",
        ));
    }
    if raw_skill_usage_json_regex().is_match(text) {
        violations.push(Violation::new(
            "body_raw_skill_usage",
            "body contains raw skill-usage record JSON shape; extract curated fields instead",
        ));
    }
    violations
}

fn is_raw_skill_usage_record(src: &Path, sniff: bool) -> bool {
    if src.file_name().and_then(|s| s.to_str()) == Some(RAW_RECORD_FILENAME) {
        return true;
    }
    if src
        .extension()
        .and_then(|s| s.to_str())
        .map(|ext| ext.to_lowercase())
        != Some("json".to_string())
    {
        return false;
    }
    if !sniff {
        return false;
    }
    let Ok(bytes) = fs::read(src) else {
        return false;
    };
    let limit = bytes.len().min(512);
    let head = String::from_utf8_lossy(&bytes[..limit]);
    head.contains(RAW_RECORD_SCHEMA)
}

fn redact_ingest_source(src: &Path, max_bytes: u64) -> Result<(String, Vec<Violation>), CliError> {
    if is_raw_skill_usage_record(src, true) {
        return Ok((
            String::new(),
            vec![Violation::new(
                "evidence_raw_skill_usage",
                format!(
                    "{}: raw skill-usage record cannot be ingested; extract curated fields first",
                    src.file_name().and_then(|s| s.to_str()).unwrap_or("source")
                ),
            )],
        ));
    }
    let bytes = fs::read(src).map_err(|err| {
        CliError::runtime(
            "read-evidence-failed",
            format!("failed to read evidence source: {err}"),
            Some(json!({ "path": display_path(src) })),
        )
    })?;
    let name = src
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("source")
        .to_string();
    if (bytes.len() as u64) > max_bytes {
        return Ok((
            String::new(),
            vec![Violation::new(
                "evidence_too_large",
                format!(
                    "{name}: source is {} bytes; limit is {}",
                    bytes.len(),
                    max_bytes
                ),
            )],
        ));
    }
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => {
            return Ok((
                String::new(),
                vec![Violation::new(
                    "evidence_binary",
                    format!("{name}: only utf-8 text evidence is supported"),
                )],
            ));
        }
    };
    let redacted = normalize_home_paths(&text);
    let violations = evidence_violations(&name, &redacted, max_bytes);
    if !violations.is_empty() {
        return Ok((String::new(), violations));
    }
    Ok((redacted, Vec::new()))
}

// ---------------------------------------------------------------------------
// Per-entry payload builders
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct EntryItem {
    path: String,
    title: String,
    status: String,
    first_observed: String,
    area: String,
    severity: String,
    raw_records: Vec<String>,
    archived: bool,
}

fn entry_item(parsed: &ParsedEntry, archived: bool) -> EntryItem {
    EntryItem {
        path: display_path(&parsed.path),
        title: parsed.title.clone(),
        status: parsed.fields.get("status").cloned().unwrap_or_default(),
        first_observed: parsed
            .fields
            .get("first_observed")
            .cloned()
            .unwrap_or_default(),
        area: parsed.fields.get("area").cloned().unwrap_or_default(),
        severity: parsed.fields.get("severity").cloned().unwrap_or_default(),
        raw_records: parsed.raw_records.clone(),
        archived,
    }
}

#[derive(Debug, Serialize)]
struct DuplicateRef {
    path: String,
    reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EvidenceFile {
    path: String,
    ok: bool,
}

#[derive(Debug, Serialize)]
struct VerifyResult {
    ok: bool,
    strict: bool,
    path: String,
    folder: String,
    kind: &'static str,
    title: String,
    fields: BTreeMap<String, String>,
    raw_records: Vec<String>,
    duplicates: Vec<DuplicateRef>,
    evidence: Vec<EvidenceFile>,
    violations: Vec<Violation>,
    body_violations: Vec<Violation>,
    warnings: Vec<String>,
}

fn detect_duplicates(
    case: &Case,
    parsed: &ParsedEntry,
    inbox_dir: &Path,
) -> Result<Vec<DuplicateRef>, CliError> {
    let title = normalize_text(&parsed.title);
    let area = normalize_text(parsed.fields.get("area").map(String::as_str).unwrap_or(""));
    let raw_records: BTreeSet<String> = parsed
        .raw_records
        .iter()
        .map(|s| normalize_text(s))
        .collect();
    let self_folder = case
        .folder
        .canonicalize()
        .unwrap_or_else(|_| case.folder.clone());
    let mut out = Vec::new();
    for other in iter_cases(inbox_dir, "ENTRY.md") {
        let other_folder = other
            .folder
            .canonicalize()
            .unwrap_or_else(|_| other.folder.clone());
        if other_folder == self_folder {
            continue;
        }
        let other_parsed = parse_entry(&other.doc_path)?;
        let mut reasons = BTreeSet::new();
        if other
            .folder
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            == case
                .folder
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        {
            reasons.insert("slug".to_string());
        }
        let other_title = normalize_text(&other_parsed.title);
        let other_area = normalize_text(
            other_parsed
                .fields
                .get("area")
                .map(String::as_str)
                .unwrap_or(""),
        );
        if !title.is_empty() && other_title == title && (area.is_empty() || other_area == area) {
            reasons.insert("title".to_string());
        }
        let other_raw: BTreeSet<String> = other_parsed
            .raw_records
            .iter()
            .map(|s| normalize_text(s))
            .collect();
        if !raw_records.is_empty() && raw_records.intersection(&other_raw).next().is_some() {
            reasons.insert("raw_record".to_string());
        }
        if !reasons.is_empty() {
            out.push(DuplicateRef {
                path: display_path(&other.folder),
                reasons: reasons.into_iter().collect(),
            });
        }
    }
    Ok(out)
}

fn verify_case(
    case: &Case,
    inbox_dir: Option<&Path>,
    max_bytes: u64,
    strict: bool,
) -> Result<VerifyResult, CliError> {
    let body_text = read_text(&case.doc_path)?;
    let parsed = parse_entry_from_text(&case.doc_path, &body_text);
    let body_findings = body_violations(&body_text, max_bytes);
    let mut violations: Vec<Violation> = Vec::new();

    if parsed.title.is_empty() {
        violations.push(Violation::new("missing_title", "missing H1 title"));
    }

    let required = case.kind.required_sections();
    for section in required {
        if !parsed.sections.contains_key(*section) {
            violations.push(Violation::new(
                "missing_section",
                format!("missing required section: {section}"),
            ));
        }
    }

    if case.kind == CaseKind::Inbox {
        for field in ["status", "first_observed", "area", "severity"] {
            if parsed
                .fields
                .get(field)
                .map(String::as_str)
                .unwrap_or("")
                .is_empty()
            {
                violations.push(Violation::new(
                    "missing_status_field",
                    format!("missing status field: {}", field.replace('_', " ")),
                ));
            }
        }
        let status = parsed
            .fields
            .get("status")
            .map(String::as_str)
            .unwrap_or("");
        if !status.is_empty() && !readable_statuses().contains(&status) {
            violations.push(Violation::new(
                "invalid_status",
                format!("invalid status: {status}"),
            ));
        }
        let severity = parsed
            .fields
            .get("severity")
            .map(String::as_str)
            .unwrap_or("");
        if !severity.is_empty() && !VALID_SEVERITIES.contains(&severity) {
            violations.push(Violation::new(
                "invalid_severity",
                format!("invalid severity: {severity}"),
            ));
        }
        if parsed.raw_records.is_empty() {
            violations.push(Violation::new(
                "missing_evidence",
                "missing raw evidence pointer",
            ));
        }
    }

    let mut duplicates: Vec<DuplicateRef> = Vec::new();
    if case.kind == CaseKind::Inbox {
        let scan_dir = inbox_dir.map(Path::to_path_buf).unwrap_or_else(|| {
            case.folder
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        });
        duplicates = detect_duplicates(case, &parsed, &scan_dir)?;
        if !duplicates.is_empty() {
            violations.push(Violation::new(
                "duplicate_entry",
                "duplicate inbox entry detected",
            ));
        }
    }

    let mut evidence_files: Vec<EvidenceFile> = Vec::new();
    for evidence_path in iter_evidence_files(case) {
        let rel = evidence_path
            .strip_prefix(&case.folder)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| evidence_path.clone());
        let rel_str = display_path(&rel);
        if is_raw_skill_usage_record(&evidence_path, true) {
            violations.push(Violation::new(
                "evidence_raw_skill_usage",
                format!(
                    "evidence/{}: raw skill-usage record committed; extract curated fields instead",
                    rel_str.trim_start_matches("evidence/")
                ),
            ));
            evidence_files.push(EvidenceFile {
                path: rel_str,
                ok: false,
            });
            continue;
        }
        let bytes = fs::read(&evidence_path).map_err(|err| {
            CliError::runtime(
                "read-evidence-failed",
                format!(
                    "failed to read evidence file {}: {err}",
                    evidence_path.display()
                ),
                Some(json!({ "path": display_path(&evidence_path) })),
            )
        })?;
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                violations.push(Violation::new(
                    "evidence_binary",
                    format!(
                        "evidence/{}: only utf-8 text evidence is supported",
                        rel_str.trim_start_matches("evidence/")
                    ),
                ));
                evidence_files.push(EvidenceFile {
                    path: rel_str,
                    ok: false,
                });
                continue;
            }
        };
        let name = format!("evidence/{}", rel_str.trim_start_matches("evidence/"));
        let file_violations = evidence_violations(&name, &text, max_bytes);
        if !file_violations.is_empty() {
            violations.extend(file_violations);
            evidence_files.push(EvidenceFile {
                path: rel_str,
                ok: false,
            });
        } else {
            evidence_files.push(EvidenceFile {
                path: rel_str,
                ok: true,
            });
        }
    }

    if strict {
        violations.extend(body_findings.clone());
    }
    let mut warnings: Vec<String> = Vec::new();
    if !strict {
        for finding in &body_findings {
            warnings.push(format!("body warning: {}", finding.message));
        }
    }
    Ok(VerifyResult {
        ok: violations.is_empty(),
        strict,
        path: display_path(&case.doc_path),
        folder: display_path(&case.folder),
        kind: case.kind.as_str(),
        title: parsed.title,
        fields: parsed.fields,
        raw_records: parsed.raw_records,
        duplicates,
        evidence: evidence_files,
        violations,
        body_violations: body_findings,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// Archive helpers
// ---------------------------------------------------------------------------

fn next_action_is_closed(value: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?is)^none\b[.:]?").expect("none regex"));
    re.is_match(value.trim())
}

fn evidence_bullets(evidence_section: &str) -> Vec<String> {
    let mut bullets: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in evidence_section.lines() {
        if line
            .trim()
            .to_lowercase()
            .starts_with("relevant evidence summary")
        {
            break;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            if !current.is_empty() {
                bullets.push(
                    current
                        .iter()
                        .map(|s| s.trim())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            current = vec![rest.to_string()];
            continue;
        }
        if !current.is_empty() && line.starts_with("  ") {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        bullets.push(
            current
                .iter()
                .map(|s| s.trim())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    bullets
}

fn durable_evidence_links(evidence_section: &str, explicit_link: &str) -> Vec<String> {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    static BT_RE: OnceLock<Regex> = OnceLock::new();
    let url_re = URL_RE.get_or_init(|| Regex::new(r"https?://[^\s`)]+").expect("url regex"));
    let bt_re = BT_RE.get_or_init(|| Regex::new(r"`([^`]+)`").expect("backtick regex"));
    let mut links: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let add = |value: &str, links: &mut Vec<String>, seen: &mut BTreeSet<String>| {
        if !value.is_empty() && !seen.contains(value) {
            seen.insert(value.to_string());
            links.push(value.to_string());
        }
    };
    if !explicit_link.is_empty() {
        add(explicit_link, &mut links, &mut seen);
    }
    for bullet in evidence_bullets(evidence_section) {
        let (label, body) = match bullet.split_once(':') {
            Some((l, b)) => (l.to_string(), b.to_string()),
            None => (bullet.clone(), String::new()),
        };
        let normalized_label = normalize_text(&label);
        if matches!(normalized_label.as_str(), "raw record" | "summary") {
            continue;
        }
        let candidate = if body.trim().is_empty() {
            bullet.clone()
        } else {
            body.trim().to_string()
        };
        if candidate.contains("skill-usage.record.json") {
            continue;
        }
        for url in url_re.find_iter(&candidate) {
            add(url.as_str(), &mut links, &mut seen);
        }
        for caps in bt_re.captures_iter(&candidate) {
            let token = caps.get(1).unwrap().as_str();
            if token.contains("skill-usage.record.json") || token.chars().any(char::is_whitespace) {
                continue;
            }
            if token.contains('/') && !token.starts_with("http") {
                add(token, &mut links, &mut seen);
            }
        }
    }
    links
}

fn archive_destination(folder: &Path, archive_root: &Path, archive_date: &str) -> PathBuf {
    let year: String = archive_date.chars().take(4).collect();
    let name = folder
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| OsString::from("case"));
    archive_root.join(year).join(name)
}

/// Inbox directory used to resolve an archive destination. When `--inbox-dir`
/// is given explicitly it wins; otherwise derive it from the case folder's own
/// parent so the archive lands in the same inbox tree as the case, regardless of
/// the shell's current working directory (issue #739). The cwd-relative
/// [`default_inbox_dir`] is only a last resort for a parentless case path.
fn resolve_archive_inbox_dir(explicit: Option<&Path>, case_folder: &Path) -> PathBuf {
    explicit.map(Path::to_path_buf).unwrap_or_else(|| {
        case_folder
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(default_inbox_dir)
    })
}

/// Archive root for a case. `--archive-root` wins when given; otherwise it is
/// the `archive/` subtree of the resolved inbox directory.
fn resolve_archive_root(explicit_root: Option<&Path>, inbox_dir: &Path) -> PathBuf {
    explicit_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| inbox_dir.join("archive"))
}

fn render_archive_section(archive_date: &str, reason: &str, link: &str) -> String {
    let view = ArchiveSectionView {
        archive_date,
        reason,
        link: Some(link).filter(|value| !value.is_empty()),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(ARCHIVE_TEMPLATE_NAME, ARCHIVE_TEMPLATE)
        .expect("archive template registers");
    engine
        .render(ARCHIVE_TEMPLATE_NAME, &view)
        .expect("archive template renders")
}

fn render_next_action_section(body: &str) -> String {
    let view = NextActionSectionView { body };
    let mut engine = Engine::builder().build();
    engine
        .register_template(NEXT_ACTION_TEMPLATE_NAME, NEXT_ACTION_TEMPLATE)
        .expect("next_action template registers");
    engine
        .render(NEXT_ACTION_TEMPLATE_NAME, &view)
        .expect("next_action template renders")
}

/// Find a section in `text` by its header line (e.g. "## Archive").
/// Returns (start, end) byte offsets of the section's full extent: from the
/// header line's start through the line before the next `## ` header (or EOF).
fn find_section_span(text: &str, header: &str) -> Option<(usize, usize)> {
    let mut start: Option<usize> = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n');
        if let Some(begin) = start {
            if trimmed.starts_with("## ") {
                return Some((begin, offset));
            }
        } else if trimmed == header {
            start = Some(offset);
        }
        offset += line.len();
    }
    start.map(|s| (s, text.len()))
}

fn upsert_archive_section(text: &str, archive_date: &str, reason: &str, link: &str) -> String {
    let section = render_archive_section(archive_date, reason, link);
    if let Some((start, end)) = find_section_span(text, "## Archive") {
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..start]);
        out.push_str(section.trim_end());
        out.push_str("\n\n");
        out.push_str(&text[end..]);
        let mut s = out.trim_end().to_string();
        s.push('\n');
        s
    } else {
        let mut s = text.trim_end().to_string();
        s.push_str("\n\n");
        s.push_str(&section);
        s
    }
}

/// Insert or replace a `- {key}: {value}` field within the `## Status` section.
/// If a line for `key` already exists it is replaced in place; otherwise a new
/// line is inserted immediately after the `- Status:` line. When there is no
/// `## Status` section the text is returned unchanged.
fn upsert_status_field(text: &str, key: &str, value: &str) -> String {
    let Some((start, end)) = find_section_span(text, "## Status") else {
        return text.to_string();
    };
    let section = &text[start..end];
    let new_line = format!("- {key}: {value}");
    let key_prefix = format!("- {key}:");
    // Replace in place when the field already exists; only insert after the
    // `- Status:` line when it does not. Doing both would leave two field lines
    // whenever an existing supersession link is updated.
    let exists = section
        .lines()
        .any(|line| line.trim_start().starts_with(&key_prefix));

    let mut rebuilt = String::with_capacity(section.len() + new_line.len() + 1);
    let mut done = false;
    for line in section.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if exists {
            if !done && body.trim_start().starts_with(&key_prefix) {
                // Replace the existing field line, preserving the original newline.
                rebuilt.push_str(&new_line);
                if line.ends_with('\n') {
                    rebuilt.push('\n');
                }
                done = true;
                continue;
            }
            rebuilt.push_str(line);
        } else {
            rebuilt.push_str(line);
            if !done && body.trim_start().starts_with("- Status:") {
                // Insert the new field on the line after `- Status:`.
                rebuilt.push_str(&new_line);
                rebuilt.push('\n');
                done = true;
            }
        }
    }

    let mut out = String::with_capacity(text.len() + new_line.len() + 1);
    out.push_str(&text[..start]);
    out.push_str(&rebuilt);
    out.push_str(&text[end..]);
    out
}

fn normalize_archive_date(value: &str) -> Result<String, CliError> {
    if value.is_empty() {
        return Ok(today_utc());
    }
    if !date_regex().is_match(value) {
        return Err(CliError::usage(
            "invalid-archive-date",
            format!("invalid archive date: {value}"),
            None,
        ));
    }
    Ok(value.to_string())
}

// ---------------------------------------------------------------------------
// Subcommand impls
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ListPayload {
    ok: bool,
    entries: Vec<EntryItem>,
}

fn run_list(args: &ListArgs) -> Result<ListPayload, CliError> {
    let mut statuses: BTreeSet<String> = BTreeSet::new();
    for part in args.status.split(',') {
        let value = part.trim();
        if !value.is_empty() {
            statuses.insert(value.to_string());
        }
    }
    let unknown: Vec<&String> = statuses
        .iter()
        .filter(|s| !readable_statuses().contains(&s.as_str()))
        .collect();
    if !unknown.is_empty() {
        return Err(CliError::usage(
            "invalid-status-filter",
            format!(
                "invalid status filter: {}",
                unknown
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
        ));
    }
    let mut entries: Vec<EntryItem> = Vec::new();
    for case in iter_cases(&args.inbox_dir, "ENTRY.md") {
        let parsed = parse_entry(&case.doc_path)?;
        let item = entry_item(&parsed, false);
        if !statuses.is_empty() && !statuses.contains(&item.status) {
            continue;
        }
        entries.push(item);
    }
    if args.include_archived {
        for case in iter_archived_inbox_cases(&args.inbox_dir) {
            let parsed = parse_entry(&case.doc_path)?;
            let item = entry_item(&parsed, true);
            if !statuses.is_empty() && !statuses.contains(&item.status) {
                continue;
            }
            entries.push(item);
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ListPayload { ok: true, entries })
}

#[derive(Debug, Serialize)]
struct NewResult {
    ok: bool,
    path: String,
    folder: String,
    status: String,
    severity: String,
}

#[derive(Debug, Serialize)]
struct SetStatusResult {
    ok: bool,
    path: String,
    folder: String,
    status: String,
    link: String,
}

#[derive(Debug, Serialize)]
struct ArchiveResult {
    ok: bool,
    archive_ready: bool,
    dry_run: bool,
    path: String,
    source: String,
    destination: String,
    folder: String,
    status: String,
    durable_links: Vec<String>,
    violations: Vec<Violation>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct IngestResult {
    ok: bool,
    path: String,
    source: String,
    case: String,
    label: String,
}

#[derive(Debug, Serialize)]
struct IngestFailure {
    ok: bool,
    path: String,
    source: String,
    violations: Vec<Violation>,
    warnings: Vec<String>,
}

fn load_skill_usage_record(input: &Path) -> Result<(PathBuf, Value), CliError> {
    let record_file = if input.is_dir() {
        input.join(RAW_RECORD_FILENAME)
    } else {
        input.to_path_buf()
    };
    if !record_file.is_file() {
        return Err(CliError::runtime(
            "missing-skill-usage-record",
            format!("missing skill usage record: {}", record_file.display()),
            Some(json!({ "path": display_path(&record_file) })),
        ));
    }
    let text = read_text(&record_file)?;
    let value: Value = serde_json::from_str(&text).map_err(|err| {
        CliError::runtime(
            "invalid-record-json",
            format!("failed to read skill usage record: {err}"),
            Some(json!({ "path": display_path(&record_file) })),
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(RAW_RECORD_SCHEMA) {
        return Err(CliError::runtime(
            "invalid-record-schema",
            "record schema is not skill-usage.record.v1",
            Some(json!({ "path": display_path(&record_file) })),
        ));
    }
    Ok((record_file, value))
}

fn today_from_record(record: &Value) -> String {
    let started = record
        .get("started_at")
        .and_then(Value::as_str)
        .unwrap_or("");
    if started.len() >= 10
        && date_regex().is_match(&started[..10.min(started.len())])
        && started
            .get(..10)
            .map(|s| date_regex().is_match(s))
            .unwrap_or(false)
    {
        return started[..10].to_string();
    }
    today_utc()
}

fn run_new(args: &NewArgs) -> Result<NewResult, CliError> {
    let slug = args.slug.trim();
    if !slug_regex().is_match(slug) {
        return Err(CliError::usage(
            "invalid-slug",
            format!("invalid slug: {slug}"),
            None,
        ));
    }
    if !VALID_STATUSES.contains(&args.status.as_str()) {
        return Err(CliError::usage(
            "invalid-status",
            format!("invalid status: {}", args.status.as_str()),
            None,
        ));
    }
    if !VALID_SEVERITIES.contains(&args.severity.as_str()) {
        return Err(CliError::usage(
            "invalid-severity",
            format!("invalid severity: {}", args.severity.as_str()),
            None,
        ));
    }

    let out_dir = args.out_dir.clone().unwrap_or_else(default_inbox_dir);
    let case_folder = out_dir.join(slug);
    let target = case_folder.join("ENTRY.md");
    let evidence_dir = case_folder.join("evidence");
    if case_folder.exists() {
        return Err(CliError::runtime(
            "inbox-case-exists",
            format!(
                "inbox case folder already exists: {}",
                case_folder.display()
            ),
            Some(json!({ "path": display_path(&case_folder) })),
        ));
    }

    // Exactly one source is guaranteed by the `new_source` ArgGroup.
    let resolved = if let Some(path) = &args.from_skill_usage {
        resolve_skill_usage_source(path)?
    } else if let Some(path) = &args.from_evidence {
        resolve_evidence_source(path)?
    } else {
        resolve_manual_source()
    };

    let title = if args.title.is_empty() {
        title_from_slug(slug)
    } else {
        args.title.clone()
    };
    let area = if !args.area.is_empty() {
        args.area.clone()
    } else {
        resolved.area_default
    };
    let next_action = if args.next_action.is_empty() {
        "Triage this gap and route any implementation work to a focused plan or domain workflow."
            .to_string()
    } else {
        args.next_action.clone()
    };

    let text = compose_entry(EntryParts {
        title: &title,
        status: args.status.as_str(),
        first_observed: &resolved.first_observed,
        area: &area,
        severity: args.severity.as_str(),
        signal: &resolved.signal,
        raw_record_display: &resolved.raw_record_display,
        evidence_summary: &resolved.evidence_summary,
        next_action: &next_action,
    });

    write_text(&target, &text)?;
    fs::create_dir_all(&evidence_dir).map_err(|err| {
        CliError::runtime(
            "create-dir-failed",
            format!("failed to create {}: {err}", evidence_dir.display()),
            Some(json!({ "path": display_path(&evidence_dir) })),
        )
    })?;
    for (filename, body) in &resolved.evidence_files {
        write_text(&evidence_dir.join(filename), body)?;
    }
    Ok(NewResult {
        ok: true,
        path: display_path(&target),
        folder: display_path(&case_folder),
        status: args.status.as_str().to_string(),
        severity: args.severity.as_str().to_string(),
    })
}

/// Resolved per-source content used to compose an inbox `ENTRY.md`.
struct ResolvedSource {
    /// Fallback `Area` value when `--area` is not supplied.
    area_default: String,
    first_observed: String,
    /// Body of the `## Signal` section.
    signal: String,
    /// Rendered value after `- Raw record: ` (already escaped/quoted as needed).
    raw_record_display: String,
    /// Trailing summary line under `## Evidence`.
    evidence_summary: String,
    /// Files to write under the case `evidence/` directory: `(filename, body)`.
    evidence_files: Vec<(String, String)>,
}

fn resolve_skill_usage_source(path: &Path) -> Result<ResolvedSource, CliError> {
    let (record_file, record) = load_skill_usage_record(path)?;
    let skill = record
        .get("skill")
        .and_then(Value::as_str)
        .unwrap_or("unknown skill")
        .to_string();
    let outcome = record.get("outcome").and_then(Value::as_object);
    let outcome_status = outcome
        .and_then(|o| o.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let outcome_summary_raw = outcome
        .and_then(|o| o.get("summary"))
        .and_then(Value::as_str)
        .unwrap_or("See linked skill usage record.");
    let outcome_summary = redact_summary(outcome_summary_raw);
    let raw_record_pointer = normalize_home_paths(&display_path(&record_file));
    Ok(ResolvedSource {
        area_default: skill.clone(),
        first_observed: today_from_record(&record),
        signal: format!("Skill `{skill}` ended with `{outcome_status}`. Summary: {outcome_summary}"),
        raw_record_display: format!("`{raw_record_pointer}`"),
        evidence_summary:
            "linked `skill-usage.record.v1` envelope; raw runtime details remain in the evidence location."
                .to_string(),
        evidence_files: Vec::new(),
    })
}

fn resolve_evidence_source(path: &Path) -> Result<ResolvedSource, CliError> {
    let (redacted, violations) = redact_ingest_source(path, DEFAULT_EVIDENCE_MAX_BYTES)?;
    if !violations.is_empty() {
        let messages: Vec<String> = violations.iter().map(|v| v.message.clone()).collect();
        return Err(CliError::usage(
            "evidence-not-redactable",
            format!(
                "evidence source cannot be ingested safely: {}",
                messages.join("; ")
            ),
            Some(json!({ "violations": violations })),
        ));
    }
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("evidence.md")
        .to_string();
    Ok(ResolvedSource {
        area_default: "uncategorized".to_string(),
        first_observed: today_utc(),
        signal:
            "Workflow gap captured from an ingested evidence file. See the Evidence section for the redacted source."
                .to_string(),
        raw_record_display: format!("`evidence/{filename}`"),
        evidence_summary:
            "redacted evidence ingested at creation time; raw logs and secrets were stripped before commit."
                .to_string(),
        evidence_files: vec![(filename, redacted)],
    })
}

fn resolve_manual_source() -> ResolvedSource {
    let today = today_utc();
    ResolvedSource {
        area_default: "uncategorized".to_string(),
        first_observed: today.clone(),
        signal:
            "Workflow gap diagnosed manually during a live session; no skill-usage record was captured. See Next Action for the triage and routing plan."
                .to_string(),
        raw_record_display: format!("not captured (manual diagnosis, {today})"),
        evidence_summary:
            "manual diagnosis with no captured raw record; attach redacted evidence later via `heuristic-inbox ingest-evidence`."
                .to_string(),
        evidence_files: Vec::new(),
    }
}

/// Named arguments for [`compose_entry`].
struct EntryParts<'a> {
    title: &'a str,
    status: &'a str,
    first_observed: &'a str,
    area: &'a str,
    severity: &'a str,
    signal: &'a str,
    raw_record_display: &'a str,
    evidence_summary: &'a str,
    next_action: &'a str,
}

fn compose_entry(parts: EntryParts<'_>) -> String {
    format!(
        "# {title}\n\n## Status\n\n- Status: {status}\n- First observed: {first_observed}\n- Area: {area}\n- Severity: {severity}\n\n## Signal\n\n{signal}\n\n## Evidence\n\n- Raw record: {raw_record_display}\n- Summary: {evidence_summary}\n\n## Impact\n\nFuture agents may repeat this workflow gap unless the retained entry is triaged,\nrouted, and later promoted into a durable fix, runbook, test, script, or skill\npolicy.\n\n## Current Workaround\n\nApply the safest manual workaround for the affected workflow until the durable\nfix lands, and avoid copying raw logs or secrets into this entry.\n\n## Promotion Criteria\n\nPromote after the durable fix or accepted-risk decision is implemented,\nvalidated, and linked from this entry.\n\n## Next Action\n\n{next_action}\n",
        title = parts.title,
        status = parts.status,
        first_observed = parts.first_observed,
        area = parts.area,
        severity = parts.severity,
        signal = parts.signal,
        raw_record_display = parts.raw_record_display,
        evidence_summary = parts.evidence_summary,
        next_action = parts.next_action,
    )
}

fn replace_next_action(text: &str, link: &str, next_action: &str) -> String {
    let Some((start, end)) = find_section_span(text, "## Next Action") else {
        if !next_action.is_empty() {
            let mut s = text.trim_end().to_string();
            s.push_str("\n\n");
            s.push_str(&render_next_action_section(next_action));
            return s;
        }
        if !link.is_empty() {
            let mut s = text.trim_end().to_string();
            s.push_str("\n\n");
            s.push_str(&render_next_action_section(&format!(
                "Lifecycle link: `{link}`"
            )));
            return s;
        }
        return text.to_string();
    };
    // Strip "## Next Action\n\n" prefix from the matched span to extract body.
    let section_text = &text[start..end];
    let body_text = section_text
        .strip_prefix("## Next Action\n")
        .unwrap_or(section_text)
        .trim_start_matches('\n');
    let mut body = if next_action.is_empty() {
        body_text.trim().to_string()
    } else {
        next_action.trim().to_string()
    };
    if !link.is_empty() && !body.contains(link) {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!("Lifecycle link: `{link}`"));
    }
    let replacement = render_next_action_section(body.trim());
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&replacement);
    out.push_str(&text[end..]);
    out
}

fn run_set_status(args: &SetStatusArgs) -> Result<SetStatusResult, CliError> {
    let status = args.status.trim();
    let case = resolve_case(&args.entry)?;
    match case.kind {
        CaseKind::Inbox => run_set_status_inbox(args, &case, status),
        CaseKind::Record => run_set_status_record(args, &case, status),
    }
}

fn run_set_status_inbox(
    args: &SetStatusArgs,
    case: &Case,
    status: &str,
) -> Result<SetStatusResult, CliError> {
    if RETIRED_STATUSES.contains(&status) {
        return Err(CliError::runtime(
            "retired-lifecycle-status",
            format!(
                "retired lifecycle status '{status}' cannot be re-set; the active lifecycle is open|promoted|wontfix (see HEURISTIC_SYSTEM.md #error-inbox)"
            ),
            Some(json!({ "status": status })),
        ));
    }
    if !VALID_STATUSES.contains(&status) {
        return Err(CliError::usage(
            "invalid-status",
            format!("invalid status: {status}"),
            None,
        ));
    }
    let text = read_text(&case.doc_path)?;
    let re = status_line_regex();
    if !re.is_match(&text) {
        return Err(CliError::runtime(
            "missing-status-line",
            "entry has no status line",
            None,
        ));
    }
    let replaced = re
        .replace(&text, format!("- Status: {status}").as_str())
        .into_owned();
    let new_text = replace_next_action(&replaced, &args.link, &args.next_action);
    write_text(&case.doc_path, &new_text)?;
    Ok(SetStatusResult {
        ok: true,
        path: display_path(&case.doc_path),
        folder: display_path(&case.folder),
        status: status.to_string(),
        link: args.link.clone(),
    })
}

fn run_set_status_record(
    args: &SetStatusArgs,
    case: &Case,
    status: &str,
) -> Result<SetStatusResult, CliError> {
    if !VALID_RECORD_STATUSES.contains(&status) {
        return Err(CliError::usage(
            "invalid-record-status",
            format!(
                "invalid record status: {status}; the record lifecycle is active|superseded|retired"
            ),
            None,
        ));
    }
    let text = read_text(&case.doc_path)?;
    // Records may carry a multi-word free-text status, so match the whole line
    // rather than the single-token inbox status regex — otherwise the records
    // that most need migrating into the lifecycle cannot be re-set.
    let re = record_status_line_regex();
    if !re.is_match(&text) {
        return Err(CliError::runtime(
            "missing-status-line",
            "record has no status line",
            None,
        ));
    }
    let mut new_text = re
        .replace(&text, format!("- Status: {status}").as_str())
        .into_owned();
    if !args.link.is_empty() {
        new_text = upsert_status_field(&new_text, "Superseded-by", &args.link);
    }
    write_text(&case.doc_path, &new_text)?;
    Ok(SetStatusResult {
        ok: true,
        path: display_path(&case.doc_path),
        folder: display_path(&case.folder),
        status: status.to_string(),
        link: args.link.clone(),
    })
}

fn run_archive(args: &ArchiveArgs) -> Result<ArchiveResult, CliError> {
    let archive_date = normalize_archive_date(&args.date)?;
    let case = resolve_case(&args.entry)?;
    match case.kind {
        CaseKind::Inbox => run_archive_inbox(args, &case, &archive_date),
        CaseKind::Record => run_archive_record(args, &case, &archive_date),
    }
}

fn run_archive_inbox(
    args: &ArchiveArgs,
    case: &Case,
    archive_date: &str,
) -> Result<ArchiveResult, CliError> {
    let inbox_dir = resolve_archive_inbox_dir(args.inbox_dir.as_deref(), &case.folder);
    let archive_root = resolve_archive_root(args.archive_root.as_deref(), &inbox_dir);
    let destination_folder = archive_destination(&case.folder, &archive_root, archive_date);
    let destination_doc = destination_folder.join("ENTRY.md");
    let reason = if args.reason.is_empty() {
        "Completed entry archived out of the active error inbox.".to_string()
    } else {
        args.reason.clone()
    };
    let verification = verify_case(case, Some(&inbox_dir), DEFAULT_EVIDENCE_MAX_BYTES, false)?;
    let parsed = parse_entry(&case.doc_path)?;
    let sections = parsed.sections.clone();
    let fields = parsed.fields.clone();
    let mut violations = verification.violations.clone();

    let status = fields.get("status").cloned().unwrap_or_default();
    if !ARCHIVE_READY_STATUSES.contains(&status.as_str()) {
        violations.push(Violation::new(
            "archive_status",
            "archive requires a closed status: promoted or wontfix",
        ));
    }

    let next_action = sections.get("Next Action").cloned().unwrap_or_default();
    if !next_action_is_closed(&next_action) {
        violations.push(Violation::new(
            "archive_next_action",
            "archive requires Next Action to start with None.",
        ));
    }

    let durable_links = durable_evidence_links(
        sections.get("Evidence").map(String::as_str).unwrap_or(""),
        &args.link,
    );
    if durable_links.is_empty() {
        violations.push(Violation::new(
            "archive_missing_durable_link",
            "archive requires a durable outcome link outside the raw record",
        ));
    }

    if destination_folder.exists() {
        violations.push(Violation::new(
            "archive_target_exists",
            format!(
                "archive target already exists: {}",
                destination_folder.display()
            ),
        ));
    }

    let payload = ArchiveResult {
        ok: violations.is_empty(),
        archive_ready: violations.is_empty(),
        dry_run: args.dry_run,
        path: display_path(&destination_doc),
        source: display_path(&case.folder),
        destination: display_path(&destination_doc),
        folder: display_path(&destination_folder),
        status: status.clone(),
        durable_links: durable_links.clone(),
        violations: violations.clone(),
        warnings: Vec::new(),
    };
    if !violations.is_empty() {
        return Ok(payload);
    }
    if args.dry_run {
        return Ok(payload);
    }
    if !args.yes {
        let question = format!(
            "Archive {} to {}? [y/N] ",
            display_path(&case.folder),
            display_path(&destination_folder)
        );
        match prompt::confirm(&question, true, PromptOptions::new()) {
            Ok(true) => {}
            Ok(false) => {
                return Err(CliError::runtime(
                    "archive-cancelled",
                    "archive cancelled",
                    None,
                ));
            }
            Err(PromptError::NonInteractive) => {
                return Err(CliError::usage(
                    "archive-confirmation-required",
                    "archive requires --yes when stdin or stderr is not a TTY",
                    Some(json!({
                        "source": display_path(&case.folder),
                        "destination": display_path(&destination_folder),
                    })),
                ));
            }
            Err(PromptError::Io(err)) => {
                return Err(CliError::runtime(
                    "archive-confirmation-failed",
                    format!("failed to read archive confirmation: {err}"),
                    None,
                ));
            }
        }
    }

    if let Some(parent) = destination_folder.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "create-dir-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }
    fs::rename(&case.folder, &destination_folder).map_err(|err| {
        CliError::runtime(
            "archive-move-failed",
            format!(
                "failed to move case folder {} -> {}: {err}",
                case.folder.display(),
                destination_folder.display()
            ),
            Some(json!({
                "source": display_path(&case.folder),
                "destination": display_path(&destination_folder),
            })),
        )
    })?;
    let text = read_text(&destination_doc)?;
    write_text(
        &destination_doc,
        &upsert_archive_section(&text, archive_date, &reason, &args.link),
    )?;
    Ok(payload)
}

fn run_archive_record(
    args: &ArchiveArgs,
    case: &Case,
    archive_date: &str,
) -> Result<ArchiveResult, CliError> {
    let inbox_dir = resolve_archive_inbox_dir(args.inbox_dir.as_deref(), &case.folder);
    let archive_root = resolve_archive_root(args.archive_root.as_deref(), &inbox_dir);
    let destination_folder = archive_destination(&case.folder, &archive_root, archive_date);
    let destination_doc = destination_folder.join("RECORD.md");

    let verification = verify_case(case, Some(&inbox_dir), DEFAULT_EVIDENCE_MAX_BYTES, false)?;
    let parsed = parse_entry(&case.doc_path)?;
    let fields = parsed.fields.clone();
    let mut violations = verification.violations.clone();

    let status = fields.get("status").cloned().unwrap_or_default();
    if !RECORD_ARCHIVE_READY_STATUSES.contains(&status.as_str()) {
        violations.push(Violation::new(
            "archive_record_status",
            "archive requires a retired record status: superseded or retired",
        ));
    }

    // The supersession link is satisfied either by a `Superseded-by` status
    // field (non-empty and not "none") or by an explicit `--link`.
    let superseded_by = fields.get("superseded-by").cloned().unwrap_or_default();
    let superseded_by_present =
        !superseded_by.trim().is_empty() && !superseded_by.trim().eq_ignore_ascii_case("none");
    let link_present = superseded_by_present || !args.link.is_empty();
    if !link_present {
        violations.push(Violation::new(
            "archive_record_supersede_link",
            "archive requires a Superseded-by link or --link",
        ));
    }

    if destination_folder.exists() {
        violations.push(Violation::new(
            "archive_target_exists",
            format!(
                "archive target already exists: {}",
                destination_folder.display()
            ),
        ));
    }

    let mut durable_links: Vec<String> = Vec::new();
    if superseded_by_present {
        durable_links.push(superseded_by.trim().to_string());
    }
    if !args.link.is_empty() && !durable_links.iter().any(|l| l == &args.link) {
        durable_links.push(args.link.clone());
    }

    let payload = ArchiveResult {
        ok: violations.is_empty(),
        archive_ready: violations.is_empty(),
        dry_run: args.dry_run,
        path: display_path(&destination_doc),
        source: display_path(&case.folder),
        destination: display_path(&destination_doc),
        folder: display_path(&destination_folder),
        status: status.clone(),
        durable_links: durable_links.clone(),
        violations: violations.clone(),
        warnings: Vec::new(),
    };
    if !violations.is_empty() {
        return Ok(payload);
    }
    if args.dry_run {
        return Ok(payload);
    }
    if !args.yes {
        let question = format!(
            "Archive {} to {}? [y/N] ",
            display_path(&case.folder),
            display_path(&destination_folder)
        );
        match prompt::confirm(&question, true, PromptOptions::new()) {
            Ok(true) => {}
            Ok(false) => {
                return Err(CliError::runtime(
                    "archive-cancelled",
                    "archive cancelled",
                    None,
                ));
            }
            Err(PromptError::NonInteractive) => {
                return Err(CliError::usage(
                    "archive-confirmation-required",
                    "archive requires --yes when stdin or stderr is not a TTY",
                    Some(json!({
                        "source": display_path(&case.folder),
                        "destination": display_path(&destination_folder),
                    })),
                ));
            }
            Err(PromptError::Io(err)) => {
                return Err(CliError::runtime(
                    "archive-confirmation-failed",
                    format!("failed to read archive confirmation: {err}"),
                    None,
                ));
            }
        }
    }

    if let Some(parent) = destination_folder.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "create-dir-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }
    fs::rename(&case.folder, &destination_folder).map_err(|err| {
        CliError::runtime(
            "archive-move-failed",
            format!(
                "failed to move case folder {} -> {}: {err}",
                case.folder.display(),
                destination_folder.display()
            ),
            Some(json!({
                "source": display_path(&case.folder),
                "destination": display_path(&destination_folder),
            })),
        )
    })?;
    let reason = if args.reason.is_empty() {
        "Retired operation record archived out of the active operation-records lane.".to_string()
    } else {
        args.reason.clone()
    };
    let link = if args.link.is_empty() {
        superseded_by.trim().to_string()
    } else {
        args.link.clone()
    };
    let mut text = read_text(&destination_doc)?;
    // When `--link` is the supersession source (the record had no usable
    // `Superseded-by` field, or it still said `none`), persist it into the
    // Status block as well — otherwise field consumers like `verify` and the
    // lifecycle tooling lose the supersession metadata that only the rendered
    // `## Archive` section would otherwise carry.
    if !args.link.is_empty() && !superseded_by_present {
        text = upsert_status_field(&text, "Superseded-by", &args.link);
    }
    write_text(
        &destination_doc,
        &upsert_archive_section(&text, archive_date, &reason, &link),
    )?;
    Ok(payload)
}

fn run_ingest(args: &IngestArgs) -> Result<Result<IngestResult, IngestFailure>, CliError> {
    let case = resolve_case(&args.case)?;
    if !args.source.is_file() {
        return Err(CliError::usage(
            "evidence-source-missing",
            format!("evidence source not found: {}", args.source.display()),
            None,
        ));
    }
    let (redacted, violations) = redact_ingest_source(&args.source, args.max_bytes)?;
    if !violations.is_empty() {
        return Ok(Err(IngestFailure {
            ok: false,
            path: String::new(),
            source: display_path(&args.source),
            violations,
            warnings: Vec::new(),
        }));
    }
    let label = if args.label.is_empty() {
        args.source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("evidence")
            .to_string()
    } else {
        args.label.clone()
    };
    if !label_regex().is_match(&label) {
        return Err(CliError::usage(
            "invalid-evidence-label",
            format!("invalid evidence label: {label}"),
            None,
        ));
    }
    let suffix = if args.suffix.is_empty() {
        ".md".to_string()
    } else if args.suffix.starts_with('.') {
        args.suffix.clone()
    } else {
        format!(".{}", args.suffix)
    };
    if !suffix_regex().is_match(&suffix) {
        return Err(CliError::usage(
            "invalid-evidence-suffix",
            format!("invalid evidence suffix: {suffix}"),
            None,
        ));
    }
    let target = case.evidence_dir().join(format!("{label}{suffix}"));
    if target.exists() && !args.force {
        return Err(CliError::runtime(
            "evidence-already-exists",
            format!(
                "evidence file already exists: {} (use --force to overwrite)",
                target.display()
            ),
            Some(json!({ "path": display_path(&target) })),
        ));
    }
    fs::create_dir_all(case.evidence_dir()).map_err(|err| {
        CliError::runtime(
            "create-dir-failed",
            format!("failed to create {}: {err}", case.evidence_dir().display()),
            Some(json!({ "path": display_path(&case.evidence_dir()) })),
        )
    })?;
    let body = if redacted.ends_with('\n') {
        redacted
    } else {
        format!("{redacted}\n")
    };
    write_text(&target, &body)?;
    Ok(Ok(IngestResult {
        ok: true,
        path: display_path(&target),
        source: display_path(&args.source),
        case: display_path(&case.folder),
        label,
    }))
}

// ---------------------------------------------------------------------------
// Invocation logging
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct SnapshotTarget {
    label: &'static str,
    path: PathBuf,
}

fn redacted_path(path: &Path) -> String {
    normalize_home_paths(&display_path(path))
}

fn redacted_fields(fields: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    fields
        .iter()
        .map(|(key, value)| (key.clone(), normalize_home_paths(value)))
        .collect()
}

fn evidence_metadata(case: &Case) -> Vec<Value> {
    iter_evidence_files(case)
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(&case.folder)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| path.clone());
            json!({
                "path": redacted_path(&rel),
                "bytes": fs::metadata(&path).ok().map(|metadata| metadata.len()),
            })
        })
        .collect()
}

fn case_snapshot(case: &Case) -> Value {
    let mut obj = Map::new();
    obj.insert("kind".to_string(), json!(case.kind.as_str()));
    obj.insert("folder".to_string(), json!(redacted_path(&case.folder)));
    obj.insert("doc_path".to_string(), json!(redacted_path(&case.doc_path)));
    obj.insert("doc_exists".to_string(), json!(case.doc_path.is_file()));
    obj.insert(
        "evidence".to_string(),
        Value::Array(evidence_metadata(case)),
    );

    match parse_entry(&case.doc_path) {
        Ok(parsed) => {
            obj.insert(
                "title".to_string(),
                json!(normalize_home_paths(&parsed.title)),
            );
            obj.insert("fields".to_string(), json!(redacted_fields(&parsed.fields)));
            obj.insert(
                "raw_records".to_string(),
                json!(
                    parsed
                        .raw_records
                        .iter()
                        .map(|value| normalize_home_paths(value))
                        .collect::<Vec<_>>()
                ),
            );
        }
        Err(err) => {
            obj.insert(
                "parse_error".to_string(),
                json!(normalize_home_paths(&format!("{err:?}"))),
            );
        }
    }

    Value::Object(obj)
}

fn path_snapshot(label: &str, path: &Path) -> Value {
    let mut obj = Map::new();
    obj.insert("label".to_string(), json!(label));
    obj.insert("path".to_string(), json!(redacted_path(path)));
    obj.insert("exists".to_string(), json!(path.exists()));

    if let Ok(metadata) = fs::metadata(path) {
        let kind = if metadata.is_dir() {
            "dir"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        obj.insert("path_kind".to_string(), json!(kind));
        obj.insert("bytes".to_string(), json!(metadata.len()));
    } else {
        obj.insert("path_kind".to_string(), json!("missing"));
    }

    if let Ok(case) = resolve_case(path) {
        obj.insert("case".to_string(), case_snapshot(&case));
    }

    Value::Object(obj)
}

fn snapshot_targets(targets: &[SnapshotTarget]) -> Value {
    json!({
        "schema_version": "cli.heuristic-inbox.state-snapshot.v1",
        "captured_at": now_rfc3339(),
        "targets": targets
            .iter()
            .map(|target| path_snapshot(target.label, &target.path))
            .collect::<Vec<_>>(),
    })
}

fn new_log_targets(args: &NewArgs) -> Vec<SnapshotTarget> {
    let slug = args.slug.trim();
    if !slug_regex().is_match(slug) {
        return Vec::new();
    }
    let out_dir = args.out_dir.clone().unwrap_or_else(default_inbox_dir);
    vec![SnapshotTarget {
        label: "case",
        path: out_dir.join(slug),
    }]
}

fn set_status_log_targets(args: &SetStatusArgs) -> Vec<SnapshotTarget> {
    vec![SnapshotTarget {
        label: "case",
        path: args.entry.clone(),
    }]
}

fn archive_log_targets(args: &ArchiveArgs) -> Vec<SnapshotTarget> {
    let mut targets = vec![SnapshotTarget {
        label: "source",
        path: args.entry.clone(),
    }];
    if let Ok(case) = resolve_case(&args.entry)
        && let Ok(archive_date) = normalize_archive_date(&args.date)
    {
        let inbox_dir = resolve_archive_inbox_dir(args.inbox_dir.as_deref(), &case.folder);
        let archive_root = resolve_archive_root(args.archive_root.as_deref(), &inbox_dir);
        targets.push(SnapshotTarget {
            label: "destination",
            path: archive_destination(&case.folder, &archive_root, &archive_date),
        });
    }
    targets
}

fn normalized_evidence_suffix(raw: &str) -> Option<String> {
    let suffix = if raw.is_empty() {
        ".md".to_string()
    } else if raw.starts_with('.') {
        raw.to_string()
    } else {
        format!(".{raw}")
    };
    suffix_regex().is_match(&suffix).then_some(suffix)
}

fn ingest_log_targets(args: &IngestArgs) -> Vec<SnapshotTarget> {
    let mut targets = vec![SnapshotTarget {
        label: "case",
        path: args.case.clone(),
    }];
    let Ok(case) = resolve_case(&args.case) else {
        return targets;
    };
    let label = if args.label.is_empty() {
        args.source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("evidence")
            .to_string()
    } else {
        args.label.clone()
    };
    let Some(suffix) = normalized_evidence_suffix(&args.suffix) else {
        return targets;
    };
    if label_regex().is_match(&label) {
        targets.push(SnapshotTarget {
            label: "evidence",
            path: case.evidence_dir().join(format!("{label}{suffix}")),
        });
    }
    targets
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Reuse civil date math; for ISO timestamp append UTC time.
    let date = today_utc();
    let s_of_day = secs % 86_400;
    let h = s_of_day / 3600;
    let m = (s_of_day % 3600) / 60;
    let s = s_of_day % 60;
    format!("{date}T{h:02}:{m:02}:{s:02}Z")
}

fn execution_log_dir(out_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = out_dir {
        return Some(dir.to_path_buf());
    }
    agent_out::project_dir_for_current_repo("heuristic-inbox", true)
        .ok()
        .map(|result| PathBuf::from(result.path))
}

fn write_json_best_effort(path: &Path, value: &Value) {
    let body = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    let _ = fs::write(path, format!("{body}\n"));
}

fn write_execution_log(
    out_dir: Option<&Path>,
    command: &str,
    argv: &[String],
    exit_code: i32,
    started_at: &str,
    before: Option<Value>,
    after: Option<Value>,
) -> Option<PathBuf> {
    let dir = execution_log_dir(out_dir)?;
    if fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let redacted_argv: Vec<String> = argv.iter().map(|s| normalize_home_paths(s)).collect();
    let cwd = env::current_dir()
        .ok()
        .map(|p| normalize_home_paths(&display_path(&p)))
        .unwrap_or_default();
    let payload = json!({
        "schema_version": "cli.heuristic-inbox.invocation.v1",
        "command": command,
        "argv": redacted_argv,
        "exit_code": exit_code,
        "started_at": started_at,
        "ended_at": now_rfc3339(),
        "cwd": cwd,
    });
    let path = dir.join("invocation.json");
    write_json_best_effort(&path, &payload);
    if let Some(snapshot) = before {
        write_json_best_effort(&dir.join("before.json"), &snapshot);
    }
    if let Some(snapshot) = after {
        write_json_best_effort(&dir.join("after.json"), &snapshot);
    }
    Some(path)
}

// ---------------------------------------------------------------------------
// deliver: records-branch PR for uncommitted heuristic-system changes
// ---------------------------------------------------------------------------

/// PR kind for `deliver`. Mirrors the `forge-cli pr create --kind` enum and
/// carries the branch-prefix rule it pairs with, so a `--kind docs` records
/// branch is always created as `docs/<slug>` (footgun: a mismatched prefix
/// makes `forge-cli pr create` refuse with `branch_kind_mismatch`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum DeliverKind {
    Feature,
    Bug,
    Chore,
    Docs,
    Ci,
    Refactor,
}

impl DeliverKind {
    /// Value forwarded to `forge-cli pr create --kind`.
    fn forge_kind(self) -> &'static str {
        match self {
            DeliverKind::Feature => "feature",
            DeliverKind::Bug => "bug",
            DeliverKind::Chore => "chore",
            DeliverKind::Docs => "docs",
            DeliverKind::Ci => "ci",
            DeliverKind::Refactor => "refactor",
        }
    }

    /// Branch prefix the records branch must use for this kind.
    fn branch_prefix(self) -> &'static str {
        match self {
            DeliverKind::Feature => "feat",
            DeliverKind::Bug => "fix",
            DeliverKind::Chore => "chore",
            DeliverKind::Docs => "docs",
            DeliverKind::Ci => "ci",
            DeliverKind::Refactor => "refactor",
        }
    }

    /// Conventional-commit type passed to `semantic-commit commit --type`.
    fn commit_type(self) -> &'static str {
        match self {
            DeliverKind::Feature => "feat",
            DeliverKind::Bug => "fix",
            DeliverKind::Chore => "chore",
            DeliverKind::Docs => "docs",
            DeliverKind::Ci => "ci",
            DeliverKind::Refactor => "refactor",
        }
    }
}

const DELIVER_SCOPE: &str = "heuristic-system";
const DELIVER_COMMIT_SUBJECT: &str = "deliver retained records";
const DEFAULT_HEURISTIC_REL: &str = "core/policies/heuristic-system";

/// Result of one external command, abstracted so tests inject scripted output
/// instead of spawning real `git` / `semantic-commit` / `forge-cli` binaries.
#[derive(Debug, Clone)]
struct RunResult {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Subprocess seam for `deliver`. The production impl shells out; tests inject
/// a scripted runner (see the `deliver_*` unit tests).
trait CommandRunner {
    fn run(&self, program: &str, args: &[&str], cwd: Option<&Path>) -> Result<RunResult, CliError>;
}

/// Default runner: spawns the real binary (cwd-scoped) via `nils_common::process`.
struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, program: &str, args: &[&str], cwd: Option<&Path>) -> Result<RunResult, CliError> {
        let output = match cwd {
            Some(dir) => process::run_output_in(program, args, dir),
            None => process::run_output(program, args),
        }
        .map_err(|err| {
            CliError::runtime(
                "spawn-failed",
                format!("failed to execute {program}: {err}"),
                Some(json!({ "program": program })),
            )
        })?;
        Ok(RunResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

fn git_bin() -> String {
    env_non_empty("HEURISTIC_INBOX_GIT_BIN").unwrap_or_else(|| "git".to_string())
}

fn semantic_commit_bin() -> String {
    env_non_empty("HEURISTIC_INBOX_SEMANTIC_COMMIT_BIN")
        .unwrap_or_else(|| "semantic-commit".to_string())
}

fn forge_bin() -> String {
    // Reuse the same override knob `plan-issue` uses so a single env var
    // redirects every forge-cli caller in this workspace.
    env_non_empty("FORGE_CLI_BIN").unwrap_or_else(|| "forge-cli".to_string())
}

fn env_non_empty(key: &str) -> Option<String> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().to_string())
}

/// One planned subprocess, surfaced verbatim by `--dry-run`.
#[derive(Debug, Clone, Serialize)]
struct PlanStep {
    program: String,
    argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

/// Envelope payload for `cli.heuristic-inbox.deliver.v1`.
#[derive(Debug, Serialize)]
struct DeliverResult {
    branch: String,
    base: String,
    kind: &'static str,
    dry_run: bool,
    pr_url: Option<String>,
    committed_paths: Vec<String>,
    repo_root: String,
    worktree_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    plan: Vec<PlanStep>,
}

fn render_deliver_text(result: &DeliverResult) -> String {
    let mut lines = Vec::new();
    if result.dry_run {
        lines.push(format!(
            "dry-run: would deliver {} record(s) on {} (off origin/{})",
            result.committed_paths.len(),
            result.branch,
            result.base
        ));
        for step in &result.plan {
            lines.push(format!("  $ {} {}", step.program, step.argv.join(" ")));
        }
    } else {
        lines.push(format!(
            "delivered {} record(s) on {}",
            result.committed_paths.len(),
            result.branch
        ));
        if let Some(url) = &result.pr_url {
            lines.push(format!("PR: {url}"));
        }
        lines.push(format!("worktree: {}", result.worktree_path));
    }
    lines.join("\n")
}

/// Records branch for the kind, e.g. `docs/heuristic-records-2026-06-01`.
fn records_branch(kind: DeliverKind, slug: &str) -> String {
    format!("{}/{}", kind.branch_prefix(), slug)
}

fn default_records_slug() -> String {
    format!("heuristic-records-{}", today_utc())
}

#[derive(Debug)]
struct RecordsTarget {
    branch: String,
    worktree_path: PathBuf,
}

#[derive(Debug)]
struct RecordsTargetCollision {
    local_branch_exists: bool,
    remote_branch_exists: bool,
    worktree_path_exists: bool,
    worktree_path_blocked: bool,
}

impl RecordsTargetCollision {
    fn any(&self) -> bool {
        self.local_branch_exists || self.remote_branch_exists || self.worktree_path_blocked
    }
}

fn resolve_records_target(
    runner: &dyn CommandRunner,
    git: &str,
    repo_root: &Path,
    kind: DeliverKind,
    requested_slug: Option<&str>,
    check_remote: bool,
) -> Result<RecordsTarget, CliError> {
    let base_slug = requested_slug
        .map(str::to_string)
        .unwrap_or_else(default_records_slug);
    let base_slug = sanitize_slug_segment(&base_slug);
    if base_slug.is_empty() {
        return Err(CliError::usage(
            "invalid-slug",
            "--slug must contain at least one ASCII letter or digit",
            None,
        ));
    }

    let auto_suffix = requested_slug.is_none();
    for attempt in 0..100 {
        let slug = if attempt == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{}", attempt + 1)
        };
        let branch = records_branch(kind, &slug);
        let worktree_path = managed_worktree_path(repo_root, &slug);
        let collision = records_target_collision(
            runner,
            git,
            repo_root,
            &branch,
            &worktree_path,
            check_remote,
        )?;
        if !collision.any() {
            return Ok(RecordsTarget {
                branch,
                worktree_path,
            });
        }
        if !auto_suffix {
            return Err(records_target_collision_error(
                slug,
                branch,
                worktree_path,
                collision,
            ));
        }
    }

    Err(CliError::runtime(
        "records-target-exhausted",
        format!("could not find an available records slug derived from {base_slug}"),
        Some(json!({ "base_slug": base_slug, "attempts": 100 })),
    ))
}

fn records_target_collision(
    runner: &dyn CommandRunner,
    git: &str,
    repo_root: &Path,
    branch: &str,
    worktree_path: &Path,
    check_remote: bool,
) -> Result<RecordsTargetCollision, CliError> {
    let local_ref = format!("refs/heads/{branch}");
    let local_branch_exists = git_ref_exists(runner, git, repo_root, &local_ref)?;
    let remote_branch_exists = if local_branch_exists || !check_remote {
        false
    } else {
        git_remote_head_exists(runner, git, repo_root, branch)?
    };
    let (worktree_path_exists, worktree_path_blocked) = worktree_path_collision(worktree_path)?;
    Ok(RecordsTargetCollision {
        local_branch_exists,
        remote_branch_exists,
        worktree_path_exists,
        worktree_path_blocked,
    })
}

fn git_ref_exists(
    runner: &dyn CommandRunner,
    git: &str,
    repo_root: &Path,
    ref_name: &str,
) -> Result<bool, CliError> {
    let result = runner.run(
        git,
        &["show-ref", "--verify", "--quiet", ref_name],
        Some(repo_root),
    )?;
    if result.success {
        return Ok(true);
    }
    if result.stderr.trim().is_empty() {
        return Ok(false);
    }
    Err(CliError::runtime(
        "git-show-ref-failed",
        format!("git show-ref failed while checking {ref_name}"),
        Some(json!({ "ref": ref_name, "stderr": result.stderr.trim() })),
    ))
}

fn git_remote_head_exists(
    runner: &dyn CommandRunner,
    git: &str,
    repo_root: &Path,
    branch: &str,
) -> Result<bool, CliError> {
    let result = runner.run(
        git,
        &["ls-remote", "--exit-code", "--heads", "origin", branch],
        Some(repo_root),
    )?;
    if result.success {
        return Ok(true);
    }
    if result.stderr.trim().is_empty() {
        return Ok(false);
    }
    Err(CliError::runtime(
        "git-ls-remote-failed",
        format!("git ls-remote failed while checking origin/{branch}"),
        Some(json!({ "branch": branch, "stderr": result.stderr.trim() })),
    ))
}

fn worktree_path_collision(worktree_path: &Path) -> Result<(bool, bool), CliError> {
    let metadata = match fs::metadata(worktree_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((false, false)),
        Err(err) => {
            return Err(CliError::runtime(
                "records-worktree-path-check-failed",
                format!(
                    "failed to inspect records worktree path {}",
                    worktree_path.display()
                ),
                Some(
                    json!({ "worktree_path": display_path(worktree_path), "error": err.to_string() }),
                ),
            ));
        }
    };

    if !metadata.is_dir() {
        return Ok((true, true));
    }

    let mut entries = fs::read_dir(worktree_path).map_err(|err| {
        CliError::runtime(
            "records-worktree-path-check-failed",
            format!(
                "failed to read records worktree path {}",
                worktree_path.display()
            ),
            Some(json!({ "worktree_path": display_path(worktree_path), "error": err.to_string() })),
        )
    })?;
    let has_entries = entries.next().transpose().map_err(|err| {
        CliError::runtime(
            "records-worktree-path-check-failed",
            format!(
                "failed to read records worktree path {}",
                worktree_path.display()
            ),
            Some(json!({ "worktree_path": display_path(worktree_path), "error": err.to_string() })),
        )
    })?;
    Ok((true, has_entries.is_some()))
}

fn records_target_collision_error(
    slug: String,
    branch: String,
    worktree_path: PathBuf,
    collision: RecordsTargetCollision,
) -> CliError {
    CliError::runtime(
        "records-target-exists",
        format!("records branch/worktree target already exists for {branch}; pass a unique --slug"),
        Some(json!({
            "slug": slug,
            "branch": branch,
            "worktree_path": display_path(&worktree_path),
            "local_branch_exists": collision.local_branch_exists,
            "remote_branch_exists": collision.remote_branch_exists,
            "worktree_path_exists": collision.worktree_path_exists,
            "worktree_path_blocked": collision.worktree_path_blocked,
        })),
    )
}

/// Parse `git status --porcelain` lines into repo-relative paths. Renames keep
/// the post-rename path; quoted paths are unquoted on a best-effort basis.
fn parse_porcelain_paths(stdout: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let rest = &line[3..];
        let raw = match rest.split_once(" -> ") {
            Some((_, new_path)) => new_path,
            None => rest,
        };
        let path = unquote_porcelain_path(raw.trim());
        if !path.is_empty() {
            paths.push(path);
        }
    }
    paths
}

fn unquote_porcelain_path(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].replace("\\\"", "\"")
    } else {
        value.to_string()
    }
}

fn run_deliver(runner: &dyn CommandRunner, args: &DeliverArgs) -> Result<DeliverResult, CliError> {
    let git = git_bin();

    // 1. Resolve the canonical repo from --root (or cwd) — never the branch.
    let source_dir = match args.root.as_deref() {
        Some(root) => absolute_path(root)?,
        None => env::current_dir().map_err(|err| {
            CliError::runtime(
                "cwd-unavailable",
                format!("failed to read current directory: {err}"),
                None,
            )
        })?,
    };
    if !source_dir.exists() {
        return Err(CliError::usage(
            "root-not-found",
            format!("--root does not exist: {}", display_path(&source_dir)),
            Some(json!({ "root": display_path(&source_dir) })),
        ));
    }
    let toplevel = runner.run(&git, &["rev-parse", "--show-toplevel"], Some(&source_dir))?;
    if !toplevel.success {
        return Err(CliError::runtime(
            "not-a-repository",
            "could not resolve a git repository from --root / cwd",
            Some(json!({ "stderr": toplevel.stderr.trim() })),
        ));
    }
    let repo_root = PathBuf::from(toplevel.stdout.trim());

    // 2. Resolve the heuristic-system root and its repo-relative prefix.
    let hs_root = match args.root.as_deref() {
        Some(root) => absolute_path(root)?,
        None => repo_root.join(DEFAULT_HEURISTIC_REL),
    };
    let hs_rel = hs_root
        .strip_prefix(&repo_root)
        .map(display_path)
        .map_err(|_| {
            CliError::usage(
                "root-outside-repo",
                "--root must live inside the resolved git repository",
                Some(json!({
                    "root": display_path(&hs_root),
                    "repo_root": display_path(&repo_root),
                })),
            )
        })?;
    if hs_rel.is_empty() {
        return Err(CliError::usage(
            "root-is-repo-root",
            "--root must be a subdirectory, not the repository root",
            None,
        ));
    }

    // 3. Collect the uncommitted record changes under the heuristic-system root.
    let status = runner.run(
        &git,
        &["status", "--porcelain", "-uall", "--", hs_rel.as_str()],
        Some(&repo_root),
    )?;
    if !status.success {
        return Err(CliError::runtime(
            "git-status-failed",
            "git status failed in the source repository",
            Some(json!({ "stderr": status.stderr.trim() })),
        ));
    }
    let committed_paths = parse_porcelain_paths(&status.stdout);
    if committed_paths.is_empty() {
        return Err(CliError::runtime(
            "nothing-to-deliver",
            format!("no uncommitted changes under {hs_rel}"),
            Some(json!({ "root": hs_rel })),
        ));
    }

    // 4. Compute the records branch + managed worktree path (git-cli semantics).
    let target = resolve_records_target(
        runner,
        &git,
        &repo_root,
        args.kind,
        args.slug.as_deref(),
        !args.dry_run,
    )?;
    let branch = target.branch;
    let worktree_path = target.worktree_path;
    let worktree_display = display_path(&worktree_path);
    let base = args.base.clone();
    let origin_base = format!("origin/{base}");

    let title = deliver_title(args);
    let body = deliver_body(args, &base, &committed_paths)?;
    let subject = DELIVER_COMMIT_SUBJECT.to_string();

    // Labels forwarded verbatim to `forge-cli pr create --label` (repeatable).
    let label_args: Vec<String> = args
        .label
        .iter()
        .flat_map(|label| ["--label".to_string(), label.clone()])
        .collect();

    // Planned subprocesses, in order, for --dry-run rendering and execution.
    let plan = vec![
        PlanStep {
            program: git.clone(),
            argv: vec!["fetch".into(), "origin".into(), base.clone()],
            cwd: Some(display_path(&repo_root)),
        },
        PlanStep {
            program: git.clone(),
            argv: vec![
                "worktree".into(),
                "add".into(),
                "-b".into(),
                branch.clone(),
                worktree_display.clone(),
                origin_base.clone(),
            ],
            cwd: Some(display_path(&repo_root)),
        },
        PlanStep {
            program: git.clone(),
            argv: vec!["add".into(), "-A".into(), "--".into(), hs_rel.clone()],
            cwd: Some(worktree_display.clone()),
        },
        PlanStep {
            program: semantic_commit_bin(),
            argv: vec![
                "commit".into(),
                "--repo".into(),
                worktree_display.clone(),
                "--automation".into(),
                "--type".into(),
                args.kind.commit_type().into(),
                "--scope".into(),
                DELIVER_SCOPE.into(),
                "--subject".into(),
                subject.clone(),
                "--format".into(),
                "json".into(),
            ],
            cwd: None,
        },
        PlanStep {
            program: git.clone(),
            argv: vec!["push".into(), "-u".into(), "origin".into(), branch.clone()],
            cwd: Some(worktree_display.clone()),
        },
        PlanStep {
            program: forge_bin(),
            argv: {
                let mut argv = vec![
                    "pr".into(),
                    "create".into(),
                    "--head".into(),
                    branch.clone(),
                    "--base".into(),
                    base.clone(),
                    "--kind".into(),
                    args.kind.forge_kind().into(),
                    "--title".into(),
                    title.clone(),
                    "--body".into(),
                    "<rendered-body>".into(),
                    "--format".into(),
                    "json".into(),
                ];
                argv.extend(label_args.clone());
                argv
            },
            cwd: Some(worktree_display.clone()),
        },
    ];

    if args.dry_run {
        return Ok(DeliverResult {
            branch,
            base,
            kind: args.kind.forge_kind(),
            dry_run: true,
            pr_url: None,
            committed_paths,
            repo_root: display_path(&repo_root),
            worktree_path: worktree_display,
            plan,
        });
    }

    // 5. Fetch the base ref so the records branch forks from origin, not local.
    let fetch = runner.run(&git, &["fetch", "origin", base.as_str()], Some(&repo_root))?;
    if !fetch.success {
        return Err(CliError::runtime(
            "git-fetch-failed",
            format!("git fetch origin {base} failed"),
            Some(json!({ "stderr": fetch.stderr.trim() })),
        ));
    }

    // 6. Create the isolated records worktree off origin/<base>.
    let add = runner.run(
        &git,
        &[
            "worktree",
            "add",
            "-b",
            branch.as_str(),
            worktree_display.as_str(),
            origin_base.as_str(),
        ],
        Some(&repo_root),
    )?;
    if !add.success {
        return Err(CliError::runtime(
            "worktree-add-failed",
            format!("git worktree add for {branch} failed"),
            Some(json!({
                "branch": branch,
                "worktree_path": worktree_display,
                "stderr": add.stderr.trim(),
            })),
        ));
    }

    // 7. Transfer the working-tree record changes into the records worktree.
    transfer_records(&repo_root, &worktree_path, &committed_paths)?;

    // 8. Stage ONLY the heuristic-system path; refuse if anything else is dirty.
    let stage = runner.run(
        &git,
        &["add", "-A", "--", hs_rel.as_str()],
        Some(&worktree_path),
    )?;
    if !stage.success {
        return Err(CliError::runtime(
            "git-add-failed",
            "git add failed in the records worktree",
            Some(json!({ "stderr": stage.stderr.trim() })),
        ));
    }
    let wt_status = runner.run(&git, &["status", "--porcelain"], Some(&worktree_path))?;
    if !wt_status.success {
        return Err(CliError::runtime(
            "git-status-failed",
            "git status failed in the records worktree",
            Some(json!({ "stderr": wt_status.stderr.trim() })),
        ));
    }
    let prefix = format!("{hs_rel}/");
    let stray: Vec<String> = parse_porcelain_paths(&wt_status.stdout)
        .into_iter()
        .filter(|path| path != &hs_rel && !path.starts_with(&prefix))
        .collect();
    if !stray.is_empty() {
        return Err(CliError::runtime(
            "dirty-records-worktree",
            "records worktree has changes outside the heuristic-system root",
            Some(json!({ "stray_paths": stray, "worktree_path": worktree_display })),
        ));
    }

    // 9. Commit via semantic-commit (in the records worktree).
    let commit = runner.run(
        &semantic_commit_bin(),
        &[
            "commit",
            "--repo",
            worktree_display.as_str(),
            "--automation",
            "--type",
            args.kind.commit_type(),
            "--scope",
            DELIVER_SCOPE,
            "--subject",
            subject.as_str(),
            "--format",
            "json",
        ],
        None,
    )?;
    if !commit.success {
        return Err(CliError::runtime(
            "commit-failed",
            "semantic-commit failed in the records worktree",
            Some(json!({
                "stderr": commit.stderr.trim(),
                "stdout": commit.stdout.trim(),
            })),
        ));
    }

    // 10. Push the records branch.
    let push = runner.run(
        &git,
        &["push", "-u", "origin", branch.as_str()],
        Some(&worktree_path),
    )?;
    if !push.success {
        return Err(CliError::runtime(
            "push-failed",
            format!("git push for {branch} failed"),
            Some(json!({ "stderr": push.stderr.trim() })),
        ));
    }

    // 11. Open the PR via forge-cli and parse the URL from its JSON envelope.
    let mut create_argv: Vec<&str> = vec![
        "pr",
        "create",
        "--head",
        branch.as_str(),
        "--base",
        base.as_str(),
        "--kind",
        args.kind.forge_kind(),
        "--title",
        title.as_str(),
        "--body",
        body.as_str(),
        "--format",
        "json",
    ];
    create_argv.extend(label_args.iter().map(String::as_str));
    let create = runner.run(&forge_bin(), &create_argv, Some(&worktree_path))?;
    let pr_url = parse_forge_pr_url(&create)?;

    Ok(DeliverResult {
        branch,
        base,
        kind: args.kind.forge_kind(),
        dry_run: false,
        pr_url: Some(pr_url),
        committed_paths,
        repo_root: display_path(&repo_root),
        worktree_path: worktree_display,
        plan: Vec::new(),
    })
}

fn deliver_title(args: &DeliverArgs) -> String {
    if args.title.trim().is_empty() {
        format!(
            "{}({DELIVER_SCOPE}): {DELIVER_COMMIT_SUBJECT}",
            args.kind.commit_type()
        )
    } else {
        args.title.trim().to_string()
    }
}

fn deliver_body(
    args: &DeliverArgs,
    base: &str,
    committed_paths: &[String],
) -> Result<String, CliError> {
    if let Some(body) = &args.body {
        return Ok(body.clone());
    }
    if let Some(path) = &args.body_file {
        return read_text(path);
    }
    let files = committed_paths
        .iter()
        .map(|path| format!("- `{path}`"))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "## Summary\n\nDeliver {n} heuristic-system retained-record change(s) on a dedicated \
records branch off `origin/{base}`.\n\n{files}\n\n## Test plan\n\n- Records staged from the source \
working tree under `{rel}` and committed via `semantic-commit`.\n- Branch created off `origin/{base}` \
without touching the current branch.\n",
        n = committed_paths.len(),
        rel = DEFAULT_HEURISTIC_REL,
    ))
}

fn parse_forge_pr_url(result: &RunResult) -> Result<String, CliError> {
    let value: Value = serde_json::from_str(result.stdout.trim()).map_err(|err| {
        CliError::runtime(
            "forge-output-invalid",
            format!("could not parse forge-cli pr create output: {err}"),
            Some(json!({ "stdout": result.stdout.trim(), "stderr": result.stderr.trim() })),
        )
    })?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("forge-cli pr create did not succeed");
        return Err(CliError::runtime(
            "forge-create-failed",
            message.to_string(),
            Some(value.get("error").cloned().unwrap_or(Value::Null)),
        ));
    }
    value
        .pointer("/data/url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::runtime(
                "forge-url-missing",
                "forge-cli pr create succeeded but returned no PR URL",
                Some(json!({ "stdout": result.stdout.trim() })),
            )
        })
}

/// Copy each changed working-tree file into the records worktree; remove paths
/// that no longer exist in the source (deletions). Only paths under the
/// heuristic-system root are touched.
fn transfer_records(
    repo_root: &Path,
    worktree_path: &Path,
    paths: &[String],
) -> Result<(), CliError> {
    for rel in paths {
        let src = repo_root.join(rel);
        let dst = worktree_path.join(rel);
        if src.is_file() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    CliError::runtime(
                        "transfer-failed",
                        format!("failed to create {}: {err}", parent.display()),
                        Some(json!({ "path": rel })),
                    )
                })?;
            }
            fs::copy(&src, &dst).map_err(|err| {
                CliError::runtime(
                    "transfer-failed",
                    format!("failed to copy {rel}: {err}"),
                    Some(json!({ "path": rel })),
                )
            })?;
        } else if !src.exists() && dst.exists() {
            fs::remove_file(&dst).map_err(|err| {
                CliError::runtime(
                    "transfer-failed",
                    format!("failed to remove {rel}: {err}"),
                    Some(json!({ "path": rel })),
                )
            })?;
        }
    }
    Ok(())
}

// Managed-worktree path resolution, kept byte-compatible with `git-cli
// worktree` so a `deliver` worktree is discoverable / removable via
// `git-cli worktree list|remove <slug>`.

fn managed_worktree_path(repo_root: &Path, slug: &str) -> PathBuf {
    worktree_agent_home()
        .join("worktrees")
        .join(repo_key_for_path(repo_root))
        .join(slug)
}

fn worktree_agent_home() -> PathBuf {
    if let Some(value) = env_non_empty("AGENT_HOME") {
        return PathBuf::from(value);
    }
    if let Some(value) = env_non_empty("XDG_STATE_HOME") {
        return PathBuf::from(value).join("agent-runtime-kit");
    }
    if let Some(value) = env_non_empty("HOME") {
        return PathBuf::from(value).join(".local/state/agent-runtime-kit");
    }
    env::temp_dir().join("agent-runtime-kit")
}

fn repo_key_for_path(repo_root: &Path) -> String {
    let basename = repo_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repo");
    let slug = sanitize_slug_segment(basename);
    let slug = if slug.is_empty() {
        "repo".to_string()
    } else {
        slug
    };
    let hash = stable_short_hash(&repo_root.to_string_lossy());
    format!("{slug}-{hash}")
}

/// Sanitize a slug/segment the same way `git-cli worktree` does so branch and
/// path segments stay aligned across the two tools.
fn sanitize_slug_segment(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if matches!(ch, '-' | '_' | '.') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches(['-', '_', '.']);
    let mut sanitized: String = trimmed.chars().take(80).collect();
    sanitized = sanitized.trim_matches(['-', '_', '.']).to_string();
    sanitized
}

fn stable_short_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

// ---------------------------------------------------------------------------
// Clap definitions
// ---------------------------------------------------------------------------

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv: Vec<OsString> = args.into_iter().map(|a| a.into()).collect();
    let started_at = now_rfc3339();
    let argv_strings: Vec<String> = argv
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let cli = match Cli::try_parse_from(argv.iter()) {
        Ok(cli) => cli,
        Err(err) => return crate::common::handle_parse_error("heuristic-inbox", argv.clone(), err),
    };
    match cli.command {
        Command::List(args) => dispatch_list(args),
        Command::Verify(args) => dispatch_verify(args),
        Command::New(args) => dispatch_new(args, &argv_strings, &started_at),
        Command::SetStatus(args) => dispatch_set_status(args, &argv_strings, &started_at),
        Command::Archive(args) => dispatch_archive(args, &argv_strings, &started_at),
        Command::IngestEvidence(args) => dispatch_ingest(args, &argv_strings, &started_at),
        Command::Deliver(args) => dispatch_deliver(args, &argv_strings, &started_at),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "heuristic-inbox"),
    }
}

fn dispatch_list(args: ListArgs) -> i32 {
    let format = args.format;
    match run_list(&args) {
        Ok(result) => render_success(
            LIST_SCHEMA_VERSION,
            LIST_COMMAND,
            format,
            || {
                if result.entries.is_empty() {
                    "No heuristic error inbox entries found.".to_string()
                } else {
                    result
                        .entries
                        .iter()
                        .map(|e| {
                            format!(
                                "{}\t{}\t{}\t{}\t{}",
                                e.status, e.severity, e.area, e.path, e.title
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            },
            &result,
        ),
        Err(err) => render_error(LIST_SCHEMA_VERSION, LIST_COMMAND, format, err),
    }
}

fn dispatch_verify(args: VerifyArgs) -> i32 {
    let format = args.format;
    let case = match resolve_case(&args.entry) {
        Ok(c) => c,
        Err(err) => return render_error(VERIFY_SCHEMA_VERSION, VERIFY_COMMAND, format, err),
    };
    let inbox_dir = args.inbox_dir.as_deref();
    match verify_case(&case, inbox_dir, DEFAULT_EVIDENCE_MAX_BYTES, args.strict) {
        Ok(result) if result.ok => render_success(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            format,
            || format!("ok: {}", result.path),
            &result,
        ),
        Ok(result) => render_error(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            format,
            CliError::runtime(
                "verify-failed",
                "heuristic case did not pass verify",
                Some(serde_json::to_value(&result).unwrap_or(Value::Null)),
            ),
        ),
        Err(err) => render_error(VERIFY_SCHEMA_VERSION, VERIFY_COMMAND, format, err),
    }
}

fn dispatch_new(args: NewArgs, argv: &[String], started_at: &str) -> i32 {
    let format = args.format;
    let out_log = args.log_dir.clone();
    let log_targets = new_log_targets(&args);
    let before = snapshot_targets(&log_targets);
    let code = match run_new(&args) {
        Ok(result) => render_success(
            NEW_SCHEMA_VERSION,
            NEW_COMMAND,
            format,
            || format!("ok: {}", result.path),
            &result,
        ),
        Err(err) => render_error(NEW_SCHEMA_VERSION, NEW_COMMAND, format, err),
    };
    let after = snapshot_targets(&log_targets);
    write_execution_log(
        out_log.as_deref(),
        NEW_COMMAND,
        argv,
        code,
        started_at,
        Some(before),
        Some(after),
    );
    code
}

fn dispatch_set_status(args: SetStatusArgs, argv: &[String], started_at: &str) -> i32 {
    let format = args.format;
    let out_log = args.log_dir.clone();
    let log_targets = set_status_log_targets(&args);
    let before = snapshot_targets(&log_targets);
    let code = match run_set_status(&args) {
        Ok(result) => render_success(
            SET_STATUS_SCHEMA_VERSION,
            SET_STATUS_COMMAND,
            format,
            || format!("ok: {}", result.path),
            &result,
        ),
        Err(err) => render_error(SET_STATUS_SCHEMA_VERSION, SET_STATUS_COMMAND, format, err),
    };
    let after = snapshot_targets(&log_targets);
    write_execution_log(
        out_log.as_deref(),
        SET_STATUS_COMMAND,
        argv,
        code,
        started_at,
        Some(before),
        Some(after),
    );
    code
}

fn dispatch_archive(args: ArchiveArgs, argv: &[String], started_at: &str) -> i32 {
    let format = args.format;
    let out_log = args.log_dir.clone();
    let log_targets = archive_log_targets(&args);
    let before = snapshot_targets(&log_targets);
    let code = match run_archive(&args) {
        Ok(result) if result.ok => render_success(
            ARCHIVE_SCHEMA_VERSION,
            ARCHIVE_COMMAND,
            format,
            || format!("ok: {}", result.path),
            &result,
        ),
        Ok(result) => render_error(
            ARCHIVE_SCHEMA_VERSION,
            ARCHIVE_COMMAND,
            format,
            CliError::runtime(
                "archive-failed",
                "heuristic case is not archive-ready",
                Some(serde_json::to_value(&result).unwrap_or(Value::Null)),
            ),
        ),
        Err(err) => render_error(ARCHIVE_SCHEMA_VERSION, ARCHIVE_COMMAND, format, err),
    };
    let after = snapshot_targets(&log_targets);
    write_execution_log(
        out_log.as_deref(),
        ARCHIVE_COMMAND,
        argv,
        code,
        started_at,
        Some(before),
        Some(after),
    );
    code
}

fn dispatch_ingest(args: IngestArgs, argv: &[String], started_at: &str) -> i32 {
    let format = args.format;
    let out_log = args.log_dir.clone();
    let log_targets = ingest_log_targets(&args);
    let before = snapshot_targets(&log_targets);
    let code = match run_ingest(&args) {
        Ok(Ok(result)) => render_success(
            INGEST_SCHEMA_VERSION,
            INGEST_COMMAND,
            format,
            || format!("ok: {}", result.path),
            &result,
        ),
        Ok(Err(failure)) => render_error(
            INGEST_SCHEMA_VERSION,
            INGEST_COMMAND,
            format,
            CliError::runtime(
                "ingest-rejected",
                "evidence ingestion rejected by redaction guardrail",
                Some(serde_json::to_value(&failure).unwrap_or(Value::Null)),
            ),
        ),
        Err(err) => render_error(INGEST_SCHEMA_VERSION, INGEST_COMMAND, format, err),
    };
    let after = snapshot_targets(&log_targets);
    write_execution_log(
        out_log.as_deref(),
        INGEST_COMMAND,
        argv,
        code,
        started_at,
        Some(before),
        Some(after),
    );
    code
}

fn dispatch_deliver(args: DeliverArgs, argv: &[String], started_at: &str) -> i32 {
    let format = args.format;
    let out_log = args.log_dir.clone();
    let runner = ProcessCommandRunner;
    let code = match run_deliver(&runner, &args) {
        Ok(result) => render_success(
            DELIVER_SCHEMA_VERSION,
            DELIVER_COMMAND,
            format,
            || render_deliver_text(&result),
            &result,
        ),
        Err(err) => render_error(DELIVER_SCHEMA_VERSION, DELIVER_COMMAND, format, err),
    };
    write_execution_log(
        out_log.as_deref(),
        DELIVER_COMMAND,
        argv,
        code,
        started_at,
        None,
        None,
    );
    code
}

#[derive(Debug, Parser)]
#[command(
    name = "heuristic-inbox",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Manage curated HEURISTIC_SYSTEM error-inbox and operation-record case folders.",
    long_about = "Manage curated heuristic-system inbox cases, operation records, evidence ingestion, and archival transitions.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  heuristic-inbox list --format json\n  heuristic-inbox verify heuristic-system/error-inbox/<slug>/ --format json\n  heuristic-inbox new --from-skill-usage out/.../skill-usage.record.json --slug pipeline-gap\n  heuristic-inbox new --from-evidence out/.../diagnosis.md --slug worktree-signing-gap\n  heuristic-inbox new --manual --slug live-diagnosis-gap --area cli --severity high\n  heuristic-inbox set-status heuristic-system/error-inbox/<slug>/ --status promoted --link docs/plans/foo.md\n  heuristic-inbox archive heuristic-system/error-inbox/<slug>/ --date 2026-05-18\n  heuristic-inbox ingest-evidence heuristic-system/error-inbox/<slug>/ --from validation.md\n  heuristic-inbox deliver --root core/policies/heuristic-system --dry-run --format json\n  heuristic-inbox completion zsh\n\nENVIRONMENT:\n  HOME  Fallback base path when expanding home-relative paths.\n  AGENT_HOME  Managed worktree root for `deliver` (matches git-cli worktree).\n  FORGE_CLI_BIN  Override the forge-cli binary used by `deliver`.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// List inbox entries.
    List(ListArgs),
    /// Verify one inbox or operation-record case folder.
    Verify(VerifyArgs),
    /// Create a curated entry from a skill-usage record, redacted evidence, or manual diagnosis.
    New(NewArgs),
    /// Update an inbox entry lifecycle status.
    SetStatus(SetStatusArgs),
    /// Move a completed inbox case folder out of the active inbox.
    Archive(ArchiveArgs),
    /// Redact and write an evidence file inside a case folder.
    IngestEvidence(IngestArgs),
    /// Deliver uncommitted heuristic-system records as a records-branch PR.
    Deliver(DeliverArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Inbox directory.
    #[arg(long = "inbox-dir", value_name = "DIR", value_hint = ValueHint::DirPath, default_value_os_t = default_inbox_dir())]
    inbox_dir: PathBuf,

    /// Comma-separated lifecycle status filter.
    #[arg(long, default_value = "")]
    status: String,

    /// Include archived entries.
    #[arg(long = "include-archived")]
    include_archived: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    /// Case folder or ENTRY.md/RECORD.md path.
    #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
    entry: PathBuf,

    /// Inbox directory used for duplicate detection.
    #[arg(long = "inbox-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    inbox_dir: Option<PathBuf>,

    /// Escalate ENTRY.md/RECORD.md body redaction findings to ok=false.
    #[arg(long)]
    strict: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("new_source")
        .required(true)
        .args(["from_skill_usage", "from_evidence", "manual"]),
))]
struct NewArgs {
    /// Scaffold from a `skill-usage.record.v1` envelope (file or its directory).
    #[arg(long = "from-skill-usage", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    from_skill_usage: Option<PathBuf>,

    /// Scaffold from an already-redacted evidence file (reuses ingest-evidence redaction).
    #[arg(long = "from-evidence", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    from_evidence: Option<PathBuf>,

    /// Scaffold a manual-diagnosis skeleton with no captured raw evidence.
    #[arg(long = "manual")]
    manual: bool,

    /// Slug for the new inbox case.
    #[arg(long)]
    slug: String,

    /// Output inbox directory (defaults to heuristic-system/error-inbox).
    #[arg(long = "out-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    out_dir: Option<PathBuf>,

    /// Optional title override.
    #[arg(long, default_value = "")]
    title: String,

    /// Optional area override (defaults to the skill name).
    #[arg(long, default_value = "")]
    area: String,

    /// Lifecycle status for the new entry.
    #[arg(long, value_enum, default_value_t = NewStatus::Open)]
    status: NewStatus,

    /// Severity for the new entry.
    #[arg(long, value_enum, default_value_t = SeverityArg::Medium)]
    severity: SeverityArg,

    /// Optional Next Action override.
    #[arg(long = "next-action", default_value = "")]
    next_action: String,

    /// Execution log directory override (defaults to agent-out topic heuristic-inbox).
    #[arg(long = "log-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    log_dir: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct SetStatusArgs {
    /// Case folder or ENTRY.md path.
    #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
    entry: PathBuf,

    /// New lifecycle status: open|promoted|wontfix. Retired values
    /// triaged|planned cannot be re-set.
    #[arg(long)]
    status: String,

    /// Optional durable link to append to Next Action.
    #[arg(long, default_value = "")]
    link: String,

    /// Optional Next Action override.
    #[arg(long = "next-action", default_value = "")]
    next_action: String,

    /// Execution log directory override (defaults to agent-out topic heuristic-inbox).
    #[arg(long = "log-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    log_dir: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct ArchiveArgs {
    /// Case folder or ENTRY.md path.
    #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
    entry: PathBuf,

    /// Inbox directory for duplicate detection and archive destination
    /// (defaults to the case folder's own parent inbox).
    #[arg(long = "inbox-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    inbox_dir: Option<PathBuf>,

    /// Override archive root (defaults to <inbox-dir>/archive).
    #[arg(long = "archive-root", value_name = "DIR", value_hint = ValueHint::DirPath)]
    archive_root: Option<PathBuf>,

    /// Archive date (YYYY-MM-DD). Defaults to today.
    #[arg(long, default_value = "")]
    date: String,

    /// Optional durable outcome link.
    #[arg(long, default_value = "")]
    link: String,

    /// Optional archive reason.
    #[arg(long, default_value = "")]
    reason: String,

    /// Archive without prompting.
    #[arg(short = 'y', long = "yes")]
    yes: bool,

    /// Report destination without moving the case folder.
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// Execution log directory override (defaults to agent-out topic heuristic-inbox).
    #[arg(long = "log-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    log_dir: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct IngestArgs {
    /// Case folder or ENTRY.md/RECORD.md path.
    #[arg(value_name = "PATH", value_hint = ValueHint::AnyPath)]
    case: PathBuf,

    /// Source evidence file.
    #[arg(long = "from", value_name = "PATH", value_hint = ValueHint::FilePath)]
    source: PathBuf,

    /// Evidence label (default: source stem).
    #[arg(long, default_value = "")]
    label: String,

    /// Evidence file suffix (default: .md).
    #[arg(long, default_value = ".md")]
    suffix: String,

    /// Reject sources larger than this many bytes.
    #[arg(long = "max-bytes", default_value_t = DEFAULT_EVIDENCE_MAX_BYTES)]
    max_bytes: u64,

    /// Overwrite existing evidence with same label.
    #[arg(long)]
    force: bool,

    /// Execution log directory override (defaults to agent-out topic heuristic-inbox).
    #[arg(long = "log-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    log_dir: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct DeliverArgs {
    /// Heuristic System root holding the uncommitted records
    /// (defaults to <repo>/core/policies/heuristic-system).
    #[arg(long = "root", value_name = "DIR", value_hint = ValueHint::DirPath)]
    root: Option<PathBuf>,

    /// Records branch slug (default: heuristic-records-<UTC date>).
    #[arg(long)]
    slug: Option<String>,

    /// PR title (default: <type>(heuristic-system): deliver retained records).
    #[arg(long, default_value = "")]
    title: String,

    /// PR body text. Mutually exclusive with --body-file.
    #[arg(long, conflicts_with = "body_file")]
    body: Option<String>,

    /// Read PR body from a file.
    #[arg(long = "body-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
    body_file: Option<PathBuf>,

    /// PR kind (selects the records branch-prefix rule).
    #[arg(long, value_enum, default_value_t = DeliverKind::Docs)]
    kind: DeliverKind,

    /// Base branch the records branch forks from (origin/<base>).
    #[arg(long, default_value = "main")]
    base: String,

    /// Apply a label to the records PR (repeatable). Forwarded verbatim to
    /// `forge-cli pr create --label`.
    #[arg(long = "label", value_name = "LABEL")]
    label: Vec<String>,

    /// Render the plan / argv without fetching, creating a worktree, or pushing.
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// Execution log directory override (defaults to agent-out topic heuristic-inbox).
    #[arg(long = "log-dir", value_name = "DIR", value_hint = ValueHint::DirPath)]
    log_dir: Option<PathBuf>,

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum NewStatus {
    Open,
    Promoted,
    Wontfix,
}

impl NewStatus {
    fn as_str(self) -> &'static str {
        match self {
            NewStatus::Open => "open",
            NewStatus::Promoted => "promoted",
            NewStatus::Wontfix => "wontfix",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum SeverityArg {
    Low,
    Medium,
    High,
}

impl SeverityArg {
    fn as_str(self) -> &'static str {
        match self {
            SeverityArg::Low => "low",
            SeverityArg::Medium => "medium",
            SeverityArg::High => "high",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_text_collapses_whitespace() {
        assert_eq!(normalize_text("  Hello\n\tWorld "), "hello world");
    }

    #[test]
    fn title_from_slug_capitalizes_each_word() {
        assert_eq!(
            title_from_slug("pipeline-status-gap"),
            "Pipeline Status Gap"
        );
    }

    #[test]
    fn slug_regex_rejects_invalid() {
        assert!(slug_regex().is_match("foo-bar"));
        assert!(!slug_regex().is_match("Foo-Bar"));
        assert!(!slug_regex().is_match("foo--bar"));
        assert!(!slug_regex().is_match("foo bar"));
    }

    #[test]
    fn token_regexes_detect_bearer_and_keys() {
        let text = "Bearer abcdefghijklmnopqrstuvwxyz1234567890";
        assert!(token_regexes().iter().any(|re| re.is_match(text)));
        let text2 = "api_key=ABCDEFGHIJKLMNOP1234";
        assert!(token_regexes().iter().any(|re| re.is_match(text2)));
        let text3 = "-----BEGIN RSA PRIVATE KEY-----";
        assert!(token_regexes().iter().any(|re| re.is_match(text3)));
    }

    #[test]
    fn token_regexes_sk_pattern_requires_boundary() {
        // Real OpenAI-style key tokens still trip the gate, whether at the
        // start of input or after a non-identifier separator.
        for text in [
            "sk-proj-abcdefghijklmnop1234",
            "OPENAI_API_KEY=sk-proj-abcdefghijklmnop1234",
            "key: sk-abcdefghijklmnop1234",
        ] {
            assert!(
                token_regexes().iter().any(|re| re.is_match(text)),
                "expected a token match for {text:?}"
            );
        }
        // Ordinary hyphenated identifiers that merely contain the `sk-`
        // substring must not be flagged (regression for #740).
        for text in [
            "docs/plans/2026-05-28-plan-task-ledger-durability/",
            "task-ledger-durability",
            "a-risk-ledger-mitigation-summary",
        ] {
            assert!(
                !token_regexes().iter().any(|re| re.is_match(text)),
                "did not expect a token match for {text:?}"
            );
        }
    }

    #[test]
    fn next_action_is_closed_detects_none_prefix() {
        assert!(next_action_is_closed("None. Fixed."));
        assert!(next_action_is_closed("None: archived"));
        assert!(!next_action_is_closed("Create a follow-up plan."));
    }

    #[test]
    fn upsert_archive_section_replaces_existing() {
        let text = "# Title\n\n## Status\n\n- Status: promoted\n\n## Archive\n\n- Archived: 2025-01-01\n- Reason: old\n";
        let updated = upsert_archive_section(text, "2026-05-18", "new reason", "");
        assert!(updated.contains("- Archived: 2026-05-18"));
        assert!(updated.contains("- Reason: new reason"));
        assert!(!updated.contains("2025-01-01"));
    }

    #[test]
    fn upsert_archive_section_appends_when_missing() {
        let text = "# Title\n\n## Status\n\n- Status: promoted\n";
        let updated = upsert_archive_section(text, "2026-05-18", "new reason", "docs/x.md");
        assert!(updated.contains("## Archive"));
        assert!(updated.contains("- Durable link: `docs/x.md`"));
    }

    #[test]
    fn upsert_status_field_inserts_after_status_line() {
        let text = "# Title\n\n## Status\n\n- Date: 2026-06-15\n- Status: superseded\n- System area: x\n\n## Signal\n\nbody\n";
        let updated = upsert_status_field(text, "Superseded-by", "docs/next.md");
        assert!(updated.contains("- Status: superseded\n- Superseded-by: docs/next.md\n"));
        // The new field stays inside the Status section, ahead of the next header.
        let status_idx = updated.find("- Superseded-by:").unwrap();
        let signal_idx = updated.find("## Signal").unwrap();
        assert!(status_idx < signal_idx);
    }

    #[test]
    fn upsert_status_field_replaces_existing() {
        let text = "# Title\n\n## Status\n\n- Status: superseded\n- Superseded-by: docs/old.md\n\n## Signal\n\nbody\n";
        let updated = upsert_status_field(text, "Superseded-by", "docs/new.md");
        assert!(updated.contains("- Superseded-by: docs/new.md"));
        assert!(!updated.contains("docs/old.md"));
    }

    #[test]
    fn upsert_status_field_does_not_duplicate_existing_field() {
        // Regression: when the field already exists below the `- Status:` line,
        // the helper must replace it in place, never insert a second line.
        let text = "# Title\n\n## Status\n\n- Status: superseded\n- System area: x\n- Superseded-by: docs/old.md\n\n## Signal\n\nbody\n";
        let updated = upsert_status_field(text, "Superseded-by", "docs/new.md");
        assert_eq!(
            updated.matches("- Superseded-by:").count(),
            1,
            "expected a single Superseded-by line, got:\n{updated}"
        );
        assert!(updated.contains("- Superseded-by: docs/new.md"));
        assert!(!updated.contains("docs/old.md"));
    }

    #[test]
    fn home_path_regex_rewrites_to_workspace() {
        let text = "Run from /Users/example/project and /home/example/build.log";
        let rewritten = normalize_home_paths(text);
        assert!(rewritten.contains("<workspace>/project"));
        assert!(rewritten.contains("<workspace>/build.log"));
    }

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join("heuristic_inbox")
            .join(name)
    }

    fn assert_or_bless(name: &str, actual: &str) {
        let path = fixture_path(name);
        if std::env::var_os("BLESS_HEURISTIC_INBOX_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixture dir");
            std::fs::write(&path, actual).expect("write fixture");
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
        pretty_assertions::assert_eq!(expected, actual, "golden mismatch for {name}");
    }

    #[test]
    fn archive_section_without_link_matches_golden() {
        let out = render_archive_section("2026-05-26", "promoted to plan #541", "");
        assert_or_bless("archive_without_link.md", &out);
    }

    #[test]
    fn archive_section_with_link_matches_golden() {
        let out = render_archive_section(
            "2026-05-26",
            "promoted to plan #541",
            "docs/plans/markdown-render-template-layer/",
        );
        assert_or_bless("archive_with_link.md", &out);
    }

    #[test]
    fn next_action_section_matches_golden() {
        let out = render_next_action_section("Create a follow-up plan.");
        assert_or_bless("next_action_body.md", &out);
    }

    #[test]
    fn next_action_section_with_lifecycle_link_matches_golden() {
        let out = render_next_action_section(
            "Resolve via plan #541.\n\nLifecycle link: `docs/plans/markdown-render-template-layer/`",
        );
        assert_or_bless("next_action_with_link.md", &out);
    }

    // -----------------------------------------------------------------------
    // deliver
    // -----------------------------------------------------------------------

    use std::cell::RefCell;
    use std::path::PathBuf;

    use nils_test_support::{EnvGuard, GlobalStateLock};

    /// Scripted `CommandRunner` that answers git / semantic-commit / forge-cli
    /// calls from canned data and records every invocation. The `worktree add`
    /// handler creates the target directory so `transfer_records` can run on a
    /// real (temp) filesystem.
    struct ScriptedRunner {
        repo_root: PathBuf,
        source_status: String,
        worktree_status: String,
        forge_stdout: String,
        existing_refs: Vec<String>,
        remote_heads: Vec<String>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl ScriptedRunner {
        fn argv_log(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|call| call.join(" "))
                .collect()
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
            _cwd: Option<&Path>,
        ) -> Result<RunResult, CliError> {
            let mut record = vec![program.to_string()];
            record.extend(args.iter().map(|a| a.to_string()));
            self.calls.borrow_mut().push(record);

            let ok = |stdout: &str| {
                Ok(RunResult {
                    success: true,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                })
            };
            match args {
                ["rev-parse", "--show-toplevel"] => ok(&format!("{}\n", self.repo_root.display())),
                ["status", "--porcelain", "-uall", "--", _] => ok(&self.source_status),
                ["show-ref", "--verify", "--quiet", ref_name] => Ok(RunResult {
                    success: self.existing_refs.iter().any(|r| r == ref_name),
                    stdout: String::new(),
                    stderr: String::new(),
                }),
                ["ls-remote", "--exit-code", "--heads", "origin", branch] => {
                    let exists = self
                        .remote_heads
                        .iter()
                        .any(|head| head == branch || head == &format!("refs/heads/{branch}"));
                    Ok(RunResult {
                        success: exists,
                        stdout: if exists {
                            format!("abc123\trefs/heads/{branch}\n")
                        } else {
                            String::new()
                        },
                        stderr: String::new(),
                    })
                }
                ["fetch", "origin", _] => ok(""),
                ["worktree", "add", "-b", _branch, path, _base] => {
                    fs::create_dir_all(path).expect("stub creates worktree dir");
                    ok("")
                }
                ["add", "-A", "--", _] => ok(""),
                ["status", "--porcelain"] => ok(&self.worktree_status),
                ["commit", ..] => ok("{\"ok\":true}"),
                ["push", "-u", "origin", _] => ok(""),
                ["pr", "create", ..] => ok(&self.forge_stdout),
                _ => Ok(RunResult {
                    success: false,
                    stdout: String::new(),
                    stderr: format!("unexpected call: {program} {args:?}"),
                }),
            }
        }
    }

    const REC_REL: &str = "core/policies/heuristic-system/error-inbox/foo/ENTRY.md";

    fn forge_envelope(url: &str) -> String {
        format!(
            "{{\"ok\":true,\"schema_version\":\"cli.forge-cli.pr.create.v1\",\
\"data\":{{\"provider\":\"github\",\"number\":7,\"url\":\"{url}\",\"head\":\"docs/fixed-slug\",\
\"base\":\"main\",\"draft\":true,\"title\":\"t\",\"kind\":\"docs\"}}}}"
        )
    }

    fn deliver_args(repo: &Path, dry_run: bool) -> DeliverArgs {
        DeliverArgs {
            root: Some(repo.join("core/policies/heuristic-system")),
            slug: Some("fixed-slug".to_string()),
            title: String::new(),
            body: None,
            body_file: None,
            kind: DeliverKind::Docs,
            base: "main".to_string(),
            label: Vec::new(),
            dry_run,
            log_dir: None,
            format: OutputFormat::Json,
        }
    }

    fn seed_repo() -> tempfile::TempDir {
        let repo = tempfile::TempDir::new().expect("tempdir");
        let file = repo.path().join(REC_REL);
        fs::create_dir_all(file.parent().unwrap()).expect("mkdir record");
        fs::write(&file, "# record\n").expect("write record");
        repo
    }

    fn force_default_forge_cli(lock: &GlobalStateLock) -> EnvGuard {
        EnvGuard::set(lock, "FORGE_CLI_BIN", "forge-cli")
    }

    #[test]
    fn deliver_branch_prefix_tracks_kind() {
        assert_eq!(records_branch(DeliverKind::Docs, "x"), "docs/x");
        assert_eq!(records_branch(DeliverKind::Feature, "x"), "feat/x");
        assert_eq!(records_branch(DeliverKind::Bug, "x"), "fix/x");
        assert_eq!(records_branch(DeliverKind::Chore, "x"), "chore/x");
        assert_eq!(records_branch(DeliverKind::Ci, "x"), "ci/x");
        assert_eq!(records_branch(DeliverKind::Refactor, "x"), "refactor/x");
    }

    #[test]
    fn deliver_parse_porcelain_handles_rename_and_untracked() {
        let stdout = " M core/a.md\n?? core/b.md\nR  core/old.md -> core/new.md\n D core/gone.md\n";
        let paths = parse_porcelain_paths(stdout);
        assert_eq!(
            paths,
            vec!["core/a.md", "core/b.md", "core/new.md", "core/gone.md"]
        );
    }

    #[test]
    fn deliver_parse_forge_pr_url_extracts_url() {
        let result = RunResult {
            success: true,
            stdout: forge_envelope("https://github.com/o/r/pull/7"),
            stderr: String::new(),
        };
        assert_eq!(
            parse_forge_pr_url(&result).expect("url"),
            "https://github.com/o/r/pull/7"
        );
    }

    #[test]
    fn deliver_parse_forge_pr_url_surfaces_failure_envelope() {
        let result = RunResult {
            success: true,
            stdout:
                "{\"ok\":false,\"error\":{\"code\":\"dirty_worktree\",\"message\":\"unclean\"}}"
                    .to_string(),
            stderr: String::new(),
        };
        let err = parse_forge_pr_url(&result).expect_err("should fail");
        assert_eq!(err.code(), "forge-create-failed");
    }

    #[test]
    fn deliver_happy_path_opens_pr() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let _agent_home = EnvGuard::set(
            &lock,
            "AGENT_HOME",
            &repo.path().join(".agent-home").to_string_lossy(),
        );
        let _forge_cli_bin = force_default_forge_cli(&lock);
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: format!(" M {REC_REL}\n"),
            forge_stdout: forge_envelope("https://github.com/o/r/pull/7"),
            existing_refs: Vec::new(),
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };

        let result = run_deliver(&runner, &deliver_args(repo.path(), false)).expect("deliver ok");

        assert_eq!(result.branch, "docs/fixed-slug");
        assert!(!result.dry_run);
        assert_eq!(
            result.pr_url.as_deref(),
            Some("https://github.com/o/r/pull/7")
        );
        assert_eq!(result.committed_paths, vec![REC_REL.to_string()]);

        let log = runner.argv_log();
        assert!(log.iter().any(|c| c == "git fetch origin main"), "{log:?}");
        assert!(
            log.iter().any(
                |c| c.contains("worktree add -b docs/fixed-slug") && c.ends_with("origin/main")
            ),
            "{log:?}"
        );
        assert!(
            log.iter().any(|c| c.starts_with("forge-cli pr create")),
            "{log:?}"
        );
    }

    #[test]
    fn deliver_forwards_labels_to_forge() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let _agent_home = EnvGuard::set(
            &lock,
            "AGENT_HOME",
            &repo.path().join(".agent-home").to_string_lossy(),
        );
        let _forge_cli_bin = force_default_forge_cli(&lock);
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: format!(" M {REC_REL}\n"),
            forge_stdout: forge_envelope("https://github.com/o/r/pull/7"),
            existing_refs: Vec::new(),
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };
        let mut args = deliver_args(repo.path(), false);
        args.label = vec![
            "workflow::heuristic-records".to_string(),
            "area::skills".to_string(),
        ];

        run_deliver(&runner, &args).expect("deliver ok");

        let create = runner
            .argv_log()
            .into_iter()
            .find(|c| c.starts_with("forge-cli pr create"))
            .expect("forge pr create call recorded");
        assert!(
            create.contains("--label workflow::heuristic-records"),
            "{create}"
        );
        assert!(create.contains("--label area::skills"), "{create}");
    }

    #[test]
    fn deliver_default_slug_auto_uniquifies_existing_target() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let agent_home = repo.path().join(".agent-home");
        let _agent_home = EnvGuard::set(&lock, "AGENT_HOME", &agent_home.to_string_lossy());
        let _forge_cli_bin = force_default_forge_cli(&lock);
        let default_slug = default_records_slug();
        let default_branch = records_branch(DeliverKind::Docs, &default_slug);
        fs::create_dir_all(managed_worktree_path(repo.path(), &default_slug))
            .expect("existing worktree path");
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: format!(" M {REC_REL}\n"),
            forge_stdout: forge_envelope("https://github.com/o/r/pull/7"),
            existing_refs: vec![format!("refs/heads/{default_branch}")],
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };
        let mut args = deliver_args(repo.path(), false);
        args.slug = None;

        let result = run_deliver(&runner, &args).expect("deliver ok");

        let uniquified_slug = format!("{default_slug}-2");
        assert_eq!(
            result.branch,
            records_branch(DeliverKind::Docs, &uniquified_slug)
        );
        assert_eq!(
            result.worktree_path,
            display_path(&managed_worktree_path(repo.path(), &uniquified_slug))
        );
        let log = runner.argv_log();
        assert!(
            log.iter()
                .any(|c| c.contains(&format!("worktree add -b docs/{uniquified_slug}"))),
            "{log:?}"
        );
    }

    #[test]
    fn deliver_explicit_slug_collision_returns_error_before_mutation() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let agent_home = repo.path().join(".agent-home");
        let _agent_home = EnvGuard::set(&lock, "AGENT_HOME", &agent_home.to_string_lossy());
        let _forge_cli_bin = force_default_forge_cli(&lock);
        fs::create_dir_all(managed_worktree_path(repo.path(), "fixed-slug"))
            .expect("existing worktree path");
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: format!(" M {REC_REL}\n"),
            forge_stdout: forge_envelope("https://github.com/o/r/pull/7"),
            existing_refs: vec!["refs/heads/docs/fixed-slug".to_string()],
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };

        let err =
            run_deliver(&runner, &deliver_args(repo.path(), false)).expect_err("should refuse");

        assert_eq!(err.code(), "records-target-exists");
        let log = runner.argv_log();
        assert!(!log.iter().any(|c| c.starts_with("git fetch")), "{log:?}");
        assert!(!log.iter().any(|c| c.contains("worktree add")), "{log:?}");
        assert!(
            !log.iter().any(|c| c.starts_with("semantic-commit commit")),
            "{log:?}"
        );
        assert!(!log.iter().any(|c| c.starts_with("git push")), "{log:?}");
    }

    #[test]
    fn deliver_explicit_slug_remote_head_collision_returns_error_before_mutation() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let agent_home = repo.path().join(".agent-home");
        let _agent_home = EnvGuard::set(&lock, "AGENT_HOME", &agent_home.to_string_lossy());
        let _forge_cli_bin = force_default_forge_cli(&lock);
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: format!(" M {REC_REL}\n"),
            forge_stdout: forge_envelope("https://github.com/o/r/pull/7"),
            existing_refs: Vec::new(),
            remote_heads: vec!["docs/fixed-slug".to_string()],
            calls: RefCell::new(Vec::new()),
        };

        let err =
            run_deliver(&runner, &deliver_args(repo.path(), false)).expect_err("should refuse");

        assert_eq!(err.code(), "records-target-exists");
        let log = runner.argv_log();
        assert!(
            log.iter()
                .any(|c| c == "git ls-remote --exit-code --heads origin docs/fixed-slug"),
            "{log:?}"
        );
        assert!(!log.iter().any(|c| c.starts_with("git fetch")), "{log:?}");
        assert!(!log.iter().any(|c| c.contains("worktree add")), "{log:?}");
        assert!(
            !log.iter().any(|c| c.starts_with("semantic-commit commit")),
            "{log:?}"
        );
        assert!(!log.iter().any(|c| c.starts_with("git push")), "{log:?}");
    }

    #[test]
    fn deliver_explicit_slug_allows_existing_empty_managed_worktree_path() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let agent_home = repo.path().join(".agent-home");
        let _agent_home = EnvGuard::set(&lock, "AGENT_HOME", &agent_home.to_string_lossy());
        let _forge_cli_bin = force_default_forge_cli(&lock);
        fs::create_dir_all(managed_worktree_path(repo.path(), "fixed-slug"))
            .expect("existing empty worktree path");
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: format!(" M {REC_REL}\n"),
            forge_stdout: forge_envelope("https://github.com/o/r/pull/7"),
            existing_refs: Vec::new(),
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };

        let result = run_deliver(&runner, &deliver_args(repo.path(), false)).expect("deliver ok");

        assert_eq!(result.branch, "docs/fixed-slug");
        let log = runner.argv_log();
        assert!(
            log.iter()
                .any(|c| c.contains("worktree add -b docs/fixed-slug")),
            "{log:?}"
        );
    }

    #[test]
    fn deliver_dry_run_renders_labels_in_plan() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let _agent_home = EnvGuard::set(
            &lock,
            "AGENT_HOME",
            &repo.path().join(".agent-home").to_string_lossy(),
        );
        let _forge_cli_bin = force_default_forge_cli(&lock);
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: String::new(),
            forge_stdout: String::new(),
            existing_refs: Vec::new(),
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };
        let mut args = deliver_args(repo.path(), true);
        args.label = vec!["workflow::heuristic-records".to_string()];

        let result = run_deliver(&runner, &args).expect("dry-run ok");
        let forge_step = result
            .plan
            .iter()
            .find(|s| s.argv.first().map(String::as_str) == Some("pr"))
            .expect("forge plan step");
        let joined = forge_step.argv.join(" ");
        assert!(
            joined.contains("--label workflow::heuristic-records"),
            "{joined}"
        );
    }

    #[test]
    fn deliver_refuses_when_records_worktree_has_stray_paths() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let _agent_home = EnvGuard::set(
            &lock,
            "AGENT_HOME",
            &repo.path().join(".agent-home").to_string_lossy(),
        );
        let _forge_cli_bin = force_default_forge_cli(&lock);
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            // A stray non-heuristic-system path leaked into the records worktree.
            worktree_status: format!(" M {REC_REL}\n M src/unrelated.rs\n"),
            forge_stdout: forge_envelope("https://github.com/o/r/pull/7"),
            existing_refs: Vec::new(),
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };

        let err =
            run_deliver(&runner, &deliver_args(repo.path(), false)).expect_err("should refuse");
        assert_eq!(err.code(), "dirty-records-worktree");

        // It must refuse BEFORE committing or pushing.
        let log = runner.argv_log();
        assert!(
            !log.iter().any(|c| c.starts_with("semantic-commit commit")),
            "{log:?}"
        );
        assert!(!log.iter().any(|c| c.starts_with("git push")), "{log:?}");
    }

    #[test]
    fn deliver_dry_run_skips_side_effects() {
        let lock = GlobalStateLock::new();
        let repo = seed_repo();
        let _agent_home = EnvGuard::set(
            &lock,
            "AGENT_HOME",
            &repo.path().join(".agent-home").to_string_lossy(),
        );
        let _forge_cli_bin = force_default_forge_cli(&lock);
        let runner = ScriptedRunner {
            repo_root: repo.path().to_path_buf(),
            source_status: format!("?? {REC_REL}\n"),
            worktree_status: String::new(),
            forge_stdout: String::new(),
            existing_refs: Vec::new(),
            remote_heads: Vec::new(),
            calls: RefCell::new(Vec::new()),
        };

        let result = run_deliver(&runner, &deliver_args(repo.path(), true)).expect("dry-run ok");

        assert!(result.dry_run);
        assert!(result.pr_url.is_none());
        assert_eq!(result.plan.len(), 6);
        // Only read-only resolution ran: rev-parse + status. No fetch/worktree/push.
        let log = runner.argv_log();
        assert!(!log.iter().any(|c| c.contains("worktree add")), "{log:?}");
        assert!(!log.iter().any(|c| c.starts_with("git fetch")), "{log:?}");
    }
}
