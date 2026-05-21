//! Heuristic Inbox primitive: manage curated heuristic-system case folders.
//!
//! Ported from agent-kit
//! `skills/workflows/heuristic-system/heuristic-error-inbox/bin/heuristic_error_inbox.py`.
//! Behaviour parity is the contract; see HEURISTIC_SYSTEM.md and the agent-kit
//! plan `docs/plans/heuristic-inbox-cli-graduation/`.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use nils_term::prompt::{self, PromptError, PromptOptions};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::common::{CliError, OutputFormat, display_path, render_error, render_success};
use crate::completion::{self, CompletionShell};

const LIST_SCHEMA_VERSION: &str = "cli.heuristic-inbox.list.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.heuristic-inbox.verify.v1";
const NEW_SCHEMA_VERSION: &str = "cli.heuristic-inbox.new.v1";
const SET_STATUS_SCHEMA_VERSION: &str = "cli.heuristic-inbox.set-status.v1";
const ARCHIVE_SCHEMA_VERSION: &str = "cli.heuristic-inbox.archive.v1";
const INGEST_SCHEMA_VERSION: &str = "cli.heuristic-inbox.ingest-evidence.v1";

const LIST_COMMAND: &str = "heuristic-inbox list";
const VERIFY_COMMAND: &str = "heuristic-inbox verify";
const NEW_COMMAND: &str = "heuristic-inbox new";
const SET_STATUS_COMMAND: &str = "heuristic-inbox set-status";
const ARCHIVE_COMMAND: &str = "heuristic-inbox archive";
const INGEST_COMMAND: &str = "heuristic-inbox ingest-evidence";

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

const STATUS_FIELDS: &[&str] = &["Status", "First observed", "Area", "Severity"];

const VALID_STATUSES: &[&str] = &["open", "promoted", "wontfix"];
const RETIRED_STATUSES: &[&str] = &["triaged", "planned"];
const ARCHIVE_READY_STATUSES: &[&str] = &["promoted", "wontfix"];
const VALID_SEVERITIES: &[&str] = &["low", "medium", "high"];

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
        let patterns = [
            r"sk-[A-Za-z0-9_-]{16,}",
            r"Bearer\s+[A-Za-z0-9._~+/=-]{16,}",
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

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
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

fn render_archive_section(archive_date: &str, reason: &str, link: &str) -> String {
    let mut lines = vec![
        "## Archive".to_string(),
        String::new(),
        format!("- Archived: {archive_date}"),
        format!("- Reason: {reason}"),
    ];
    if !link.is_empty() {
        lines.push(format!("- Durable link: `{link}`"));
    }
    let mut joined = lines.join("\n");
    joined.push('\n');
    joined
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

    let (record_file, record) = load_skill_usage_record(&args.from_skill_usage)?;

    let title = if args.title.is_empty() {
        title_from_slug(slug)
    } else {
        args.title.clone()
    };
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
    let area = if args.area.is_empty() {
        skill.clone()
    } else {
        args.area.clone()
    };
    let first_observed = today_from_record(&record);
    let next_action = if args.next_action.is_empty() {
        "Triage this gap and route any implementation work to a focused plan or domain workflow."
            .to_string()
    } else {
        args.next_action.clone()
    };
    let raw_record_pointer = normalize_home_paths(&display_path(&record_file));

    let text = format!(
        "# {title}\n\n## Status\n\n- Status: {status}\n- First observed: {first_observed}\n- Area: {area}\n- Severity: {severity}\n\n## Signal\n\nSkill `{skill}` ended with `{outcome_status}`. Summary: {outcome_summary}\n\n## Evidence\n\n- Raw record: `{raw_record_pointer}`\n- Summary: linked `skill-usage.record.v1` envelope; raw runtime details remain in the evidence location.\n\n## Impact\n\nFuture agents may repeat this workflow gap unless the retained entry is triaged,\nrouted, and later promoted into a durable fix, runbook, test, script, or skill\npolicy.\n\n## Current Workaround\n\nUse the linked raw record for details, apply the safest manual workaround for\nthe affected workflow, and avoid copying raw logs or secrets into this entry.\n\n## Promotion Criteria\n\nPromote after the durable fix or accepted-risk decision is implemented,\nvalidated, and linked from this entry.\n\n## Next Action\n\n{next_action}\n",
        title = title,
        status = args.status.as_str(),
        first_observed = first_observed,
        area = area,
        severity = args.severity.as_str(),
        skill = skill,
        outcome_status = outcome_status,
        outcome_summary = outcome_summary,
        raw_record_pointer = raw_record_pointer,
        next_action = next_action,
    );

    write_text(&target, &text)?;
    fs::create_dir_all(&evidence_dir).map_err(|err| {
        CliError::runtime(
            "create-dir-failed",
            format!("failed to create {}: {err}", evidence_dir.display()),
            Some(json!({ "path": display_path(&evidence_dir) })),
        )
    })?;
    Ok(NewResult {
        ok: true,
        path: display_path(&target),
        folder: display_path(&case_folder),
        status: args.status.as_str().to_string(),
        severity: args.severity.as_str().to_string(),
    })
}

fn replace_next_action(text: &str, link: &str, next_action: &str) -> String {
    let Some((start, end)) = find_section_span(text, "## Next Action") else {
        if !next_action.is_empty() {
            let mut s = text.trim_end().to_string();
            s.push_str(&format!("\n\n## Next Action\n\n{next_action}\n"));
            return s;
        }
        if !link.is_empty() {
            let mut s = text.trim_end().to_string();
            s.push_str(&format!("\n\n## Next Action\n\nLifecycle link: `{link}`\n"));
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
    let replacement = format!("## Next Action\n\n{}\n", body.trim());
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&replacement);
    out.push_str(&text[end..]);
    out
}

fn run_set_status(args: &SetStatusArgs) -> Result<SetStatusResult, CliError> {
    let status = args.status.trim();
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
    let case = resolve_case(&args.entry)?;
    if case.kind != CaseKind::Inbox {
        return Err(CliError::usage(
            "set-status-inbox-only",
            format!(
                "set-status is only supported for inbox cases (ENTRY.md); got kind={}",
                case.kind.as_str()
            ),
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

fn run_archive(args: &ArchiveArgs) -> Result<ArchiveResult, CliError> {
    let archive_date = normalize_archive_date(&args.date)?;
    let archive_root = args
        .archive_root
        .clone()
        .unwrap_or_else(|| args.inbox_dir.join("archive"));
    let case = resolve_case(&args.entry)?;
    if case.kind != CaseKind::Inbox {
        return Err(CliError::usage(
            "archive-inbox-only",
            format!(
                "archive is only supported for inbox cases (ENTRY.md); got kind={}",
                case.kind.as_str()
            ),
            None,
        ));
    }
    let destination_folder = archive_destination(&case.folder, &archive_root, &archive_date);
    let destination_doc = destination_folder.join("ENTRY.md");
    let reason = if args.reason.is_empty() {
        "Completed entry archived out of the active error inbox.".to_string()
    } else {
        args.reason.clone()
    };
    let verification = verify_case(
        &case,
        Some(&args.inbox_dir),
        DEFAULT_EVIDENCE_MAX_BYTES,
        false,
    )?;
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
        &upsert_archive_section(&text, &archive_date, &reason, &args.link),
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
        let archive_root = args
            .archive_root
            .clone()
            .unwrap_or_else(|| args.inbox_dir.join("archive"));
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

#[derive(Debug, Parser)]
#[command(
    name = "heuristic-inbox",
    version,
    about = "Manage curated HEURISTIC_SYSTEM error-inbox and operation-record case folders.",
    long_about = "Manage curated heuristic-system inbox cases, operation records, evidence ingestion, and archival transitions.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  heuristic-inbox list --format json\n  heuristic-inbox verify heuristic-system/error-inbox/<slug>/ --format json\n  heuristic-inbox new --from-skill-usage out/.../skill-usage.record.json --slug pipeline-gap\n  heuristic-inbox set-status heuristic-system/error-inbox/<slug>/ --status promoted --link docs/plans/foo.md\n  heuristic-inbox archive heuristic-system/error-inbox/<slug>/ --date 2026-05-18\n  heuristic-inbox ingest-evidence heuristic-system/error-inbox/<slug>/ --from validation.md\n  heuristic-inbox completion zsh\n\nENVIRONMENT:\n  HOME  Fallback base path when expanding home-relative paths.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
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
    /// Create a curated entry from a skill usage record.
    New(NewArgs),
    /// Update an inbox entry lifecycle status.
    SetStatus(SetStatusArgs),
    /// Move a completed inbox case folder out of the active inbox.
    Archive(ArchiveArgs),
    /// Redact and write an evidence file inside a case folder.
    IngestEvidence(IngestArgs),
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
struct NewArgs {
    /// Path to a skill-usage record file or its containing directory.
    #[arg(long = "from-skill-usage", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    from_skill_usage: PathBuf,

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

    /// Inbox directory used for duplicate detection.
    #[arg(long = "inbox-dir", value_name = "DIR", value_hint = ValueHint::DirPath, default_value_os_t = default_inbox_dir())]
    inbox_dir: PathBuf,

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
    fn home_path_regex_rewrites_to_workspace() {
        let text = "Run from /Users/example/project and /home/example/build.log";
        let rewritten = normalize_home_paths(text);
        assert!(rewritten.contains("<workspace>/project"));
        assert!(rewritten.contains("<workspace>/build.log"));
    }
}
