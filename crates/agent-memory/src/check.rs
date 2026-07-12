//! Structural integrity checks for a memory scope.
//!
//! `agent-memory check` validates the deterministic invariants of a memory
//! store: index/file parity, broken index links, dangling `[[wikilinks]]`, and
//! note frontmatter schema. It intentionally does not judge staleness,
//! storage-worthiness, or prose formatting.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use nils_common::cli_contract::{OutputFormat, schema_version_for};
use nils_common::fs::display_path;

use crate::cli::CheckArgs;
use crate::frontmatter::{self, VALID_TYPES};
use crate::{CliError, EXIT_OK, EXIT_RUNTIME, Layout, markdown_files};

const SCHEMA_VERSION_COMMAND: &str = "check";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    Warn,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warn => "warn",
        }
    }
}

struct Finding {
    scope: String,
    kind: &'static str,
    file: String,
    detail: String,
    severity: Severity,
}

impl Finding {
    fn error(
        scope: &str,
        kind: &'static str,
        file: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            scope: scope.to_string(),
            kind,
            file: file.into(),
            detail: detail.into(),
            severity: Severity::Error,
        }
    }

    fn warn(
        scope: &str,
        kind: &'static str,
        file: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            scope: scope.to_string(),
            kind,
            file: file.into(),
            detail: detail.into(),
            severity: Severity::Warn,
        }
    }
}

struct ScopeReport {
    scope: String,
    findings: Vec<Finding>,
}

pub(crate) fn run(layout: &Layout, args: &CheckArgs) -> Result<i32, CliError> {
    let forbidden_terms = args
        .forbid_terms_file
        .as_deref()
        .map(read_forbidden_terms)
        .transpose()?
        .unwrap_or_default();
    let targets = if args.all {
        crate::memory_scopes(layout)?
    } else {
        let scope = args.scope.as_deref().unwrap_or("global");
        vec![resolve_target(layout, scope)?]
    };

    let mut reports = Vec::new();
    for (label, dir) in targets {
        let findings = check_scope(&label, &dir, args.max_index_bytes, &forbidden_terms)?;
        reports.push(ScopeReport {
            scope: label,
            findings,
        });
    }

    let error_count = count_severity(&reports, Severity::Error);
    let warn_count = count_severity(&reports, Severity::Warn);
    let failed = error_count > 0 || (args.strict && warn_count > 0);

    let format = if args.json {
        OutputFormat::Json
    } else {
        args.format
    };
    if format.is_json() {
        print_json(&reports, error_count, warn_count, args.strict);
    } else {
        print_human(&reports, error_count, warn_count, args.strict);
    }

    Ok(if failed { EXIT_RUNTIME } else { EXIT_OK })
}

fn count_severity(reports: &[ScopeReport], severity: Severity) -> usize {
    reports
        .iter()
        .flat_map(|report| &report.findings)
        .filter(|finding| finding.severity == severity)
        .count()
}

/// Resolve a single scope to the directory that holds its `MEMORY.md`.
fn resolve_target(layout: &Layout, scope: &str) -> Result<(String, PathBuf), CliError> {
    let dir = layout.resolve_scope(Some(scope))?;
    if !dir.is_dir() {
        return Err(CliError::runtime(format!(
            "not found: {}",
            display_path(&dir)
        )));
    }
    // Persona launchpads keep their notes under a `memory/` subdirectory.
    if !dir.join("MEMORY.md").is_file() && dir.join("memory").join("MEMORY.md").is_file() {
        return Ok((scope.to_string(), dir.join("memory")));
    }
    Ok((scope.to_string(), dir))
}

fn dir_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn check_scope(
    scope: &str,
    dir: &Path,
    max_index_bytes: Option<usize>,
    forbidden_terms: &[String],
) -> Result<Vec<Finding>, CliError> {
    let mut findings = Vec::new();

    let index_path = dir.join("MEMORY.md");
    let index_metadata = fs::symlink_metadata(&index_path).ok();
    let index_links: BTreeSet<String> = if index_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        let contents = read_file(&index_path)?;
        if let Some(maximum) = max_index_bytes {
            let actual = contents.len();
            if actual > maximum {
                findings.push(Finding::error(
                    scope,
                    "index-byte-budget-exceeded",
                    "MEMORY.md",
                    format!("index is {actual} bytes; maximum is {maximum} bytes"),
                ));
            }
        }
        check_forbidden_terms(
            scope,
            "MEMORY.md",
            &contents,
            forbidden_terms,
            &mut findings,
        );
        extract_index_links(&contents)
    } else if index_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        findings.push(Finding::error(
            scope,
            "index-symlink",
            "MEMORY.md",
            format!("MEMORY.md must not be a symlink in {}", display_path(dir)),
        ));
        BTreeSet::new()
    } else {
        findings.push(Finding::error(
            scope,
            "index-missing",
            "MEMORY.md",
            format!("no MEMORY.md in {}", display_path(dir)),
        ));
        BTreeSet::new()
    };

    let notes: Vec<PathBuf> = markdown_files(dir)?
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name != "MEMORY.md"))
        .collect();
    let note_names: BTreeSet<String> = notes.iter().filter_map(|path| dir_name(path)).collect();

    // Index <-> file parity.
    for name in &note_names {
        if !index_links.contains(name) {
            findings.push(Finding::error(
                scope,
                "orphan-note",
                name.clone(),
                "note has no MEMORY.md index entry",
            ));
        }
    }
    for link in &index_links {
        let linked_path = dir.join(link);
        let linked_metadata = fs::symlink_metadata(&linked_path).ok();
        if linked_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
        {
            findings.push(Finding::error(
                scope,
                "index-unsafe-link",
                link.clone(),
                "MEMORY.md link resolves to a symlink",
            ));
        } else if !linked_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.is_file())
        {
            findings.push(Finding::error(
                scope,
                "index-broken-link",
                link.clone(),
                "MEMORY.md links to a file that does not exist",
            ));
        }
    }

    // Per-note frontmatter schema and dangling wikilinks.
    for note_path in &notes {
        let name = dir_name(note_path).unwrap_or_default();
        let contents = read_file(note_path)?;
        check_frontmatter(scope, &name, &contents, &mut findings);
        check_forbidden_terms(scope, &name, &contents, forbidden_terms, &mut findings);
        for target in extract_wikilinks(&contents) {
            if !dir.join(format!("{target}.md")).is_file() {
                findings.push(Finding::warn(
                    scope,
                    "dangling-wikilink",
                    name.clone(),
                    format!("[[{target}]] does not resolve to a file in scope"),
                ));
            }
        }
    }

    Ok(findings)
}

fn read_forbidden_terms(path: &str) -> Result<Vec<String>, CliError> {
    let path = Path::new(path);
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        CliError::runtime(format!(
            "failed to inspect forbidden terms file {}: {err}",
            display_path(path)
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CliError::runtime(format!(
            "forbidden terms file must be a regular, non-symlink file: {}",
            display_path(path)
        )));
    }
    let contents = fs::read_to_string(path).map_err(|err| {
        CliError::runtime(format!(
            "failed to read forbidden terms file {}: {err}",
            display_path(path)
        ))
    })?;
    let terms: Vec<String> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect();
    if terms.is_empty() {
        return Err(CliError::usage(format!(
            "forbidden terms file contains no terms: {}",
            display_path(path)
        )));
    }
    Ok(terms)
}

fn check_forbidden_terms(
    scope: &str,
    file: &str,
    contents: &str,
    terms: &[String],
    findings: &mut Vec<Finding>,
) {
    for (line_index, line) in contents.lines().enumerate() {
        for term in terms {
            if line.contains(term) {
                findings.push(Finding::error(
                    scope,
                    "forbidden-term",
                    file,
                    format!("line {} contains forbidden term '{term}'", line_index + 1),
                ));
            }
        }
    }
}

fn read_file(path: &Path) -> Result<String, CliError> {
    fs::read_to_string(path)
        .map_err(|err| CliError::runtime(format!("failed to read {}: {err}", display_path(path))))
}

/// Collect markdown link targets (`](target.md)`) that point at local files.
fn extract_index_links(contents: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let mut rest = contents;
    while let Some(open) = rest.find("](") {
        let after = &rest[open + 2..];
        if let Some(close) = after.find(')') {
            let target = &after[..close];
            if target.ends_with(".md") && !target.contains("://") {
                set.insert(target.to_string());
            }
            rest = &after[close + 1..];
        } else {
            break;
        }
    }
    set
}

/// Collect `[[wikilink]]` targets (filename slugs without the `.md` suffix).
fn extract_wikilinks(contents: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = contents;
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        if let Some(close) = after.find("]]") {
            let inner = &after[..close];
            if !inner.is_empty()
                && inner
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
            {
                links.push(inner.to_string());
            }
            rest = &after[close + 2..];
        } else {
            break;
        }
    }
    links
}

fn check_frontmatter(scope: &str, file: &str, contents: &str, findings: &mut Vec<Finding>) {
    let Some(frontmatter) = frontmatter::parse(contents) else {
        findings.push(Finding::error(
            scope,
            "frontmatter-missing",
            file.to_string(),
            "note has no YAML frontmatter block",
        ));
        return;
    };

    if frontmatter.name.is_none() {
        findings.push(Finding::error(
            scope,
            "frontmatter-field-missing",
            file.to_string(),
            "missing required field: name",
        ));
    }
    if frontmatter.description.is_none() {
        findings.push(Finding::error(
            scope,
            "frontmatter-field-missing",
            file.to_string(),
            "missing required field: description",
        ));
    }
    match frontmatter.typ.as_deref() {
        None => findings.push(Finding::error(
            scope,
            "frontmatter-field-missing",
            file.to_string(),
            "missing required field: metadata.type",
        )),
        Some(value) if !VALID_TYPES.contains(&value) => findings.push(Finding::error(
            scope,
            "frontmatter-type-invalid",
            file.to_string(),
            format!("type '{value}' is not one of user|feedback|project|reference"),
        )),
        Some(_) => {}
    }
    if frontmatter.node_type.is_none() {
        findings.push(Finding::warn(
            scope,
            "expected-field-missing",
            file.to_string(),
            "missing expected field: metadata.node_type",
        ));
    }
    if frontmatter.origin_session_id.is_none() {
        findings.push(Finding::warn(
            scope,
            "expected-field-missing",
            file.to_string(),
            "missing expected field: metadata.originSessionId",
        ));
    }
}

fn print_human(reports: &[ScopeReport], error_count: usize, warn_count: usize, strict: bool) {
    for report in reports {
        println!("scope: {}", report.scope);
        if report.findings.is_empty() {
            println!("  [ok]    no issues");
            continue;
        }
        for finding in &report.findings {
            println!(
                "  [{:<5}] {}  {}  {}",
                finding.severity.as_str(),
                finding.kind,
                finding.file,
                finding.detail
            );
        }
    }

    let scope_count = reports.len();
    if error_count == 0 && warn_count == 0 {
        println!("checked {scope_count} scope(s): clean");
    } else {
        let note = if strict && warn_count > 0 {
            " (--strict: warnings fail)"
        } else {
            ""
        };
        println!(
            "checked {scope_count} scope(s): {error_count} error(s), {warn_count} warning(s){note}"
        );
    }
}

fn print_json(reports: &[ScopeReport], error_count: usize, warn_count: usize, strict: bool) {
    let overall_ok = error_count == 0 && !(strict && warn_count > 0);
    let scopes: Vec<serde_json::Value> = reports
        .iter()
        .map(|report| {
            let scope_errors = report
                .findings
                .iter()
                .filter(|finding| finding.severity == Severity::Error)
                .count();
            let scope_warns = report.findings.len() - scope_errors;
            let scope_ok = scope_errors == 0 && !(strict && scope_warns > 0);
            let findings: Vec<serde_json::Value> = report
                .findings
                .iter()
                .map(|finding| {
                    json!({
                        "scope": finding.scope,
                        "kind": finding.kind,
                        "file": finding.file,
                        "detail": finding.detail,
                        "severity": finding.severity.as_str(),
                    })
                })
                .collect();
            json!({"scope": report.scope, "ok": scope_ok, "findings": findings})
        })
        .collect();

    let doc = json!({
        "schema_version": schema_version_for("agent-memory", SCHEMA_VERSION_COMMAND, 1),
        "ok": overall_ok,
        "counts": {"error": error_count, "warn": warn_count},
        "scopes": scopes,
    });
    println!(
        "{}",
        serde_json::to_string(&doc).expect("check report should serialize")
    );
}
