//! `drift-audit.allow.yaml` support for unsafe finding demotion.
//!
//! Task 4.2 only defines `unsafe_allow`; each match demotes an unsafe
//! finding by exactly one tier (`block` -> `warn`, `warn` ->
//! `suppressed`). Suppressed findings remain in the report so verbose
//! output can still show the evidence.

use crate::audit_drift::unsafe_score;
use crate::audit_drift::{DriftReport, Severity};
use crate::render::manifest::SourceRoot;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

const ALLOWLIST_FILE: &str = "drift-audit.allow.yaml";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum AllowlistError {
    #[error("unsupported private audit-drift allowlist: {path}; put {file} at the source root")]
    PrivateUnsupported { path: PathBuf, file: &'static str },
    #[error("io error reading {file}: {source}")]
    Io {
        file: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error in {file}: {source}")]
    Parse {
        file: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("schema_version mismatch in {file}: expected {expected}, got {found}")]
    SchemaVersion {
        file: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("{file}: unsafe_allow[{index}] path must be non-empty")]
    EmptyPath { file: PathBuf, index: usize },
    #[error("{file}: unsafe_allow[{index}] reason must be non-empty")]
    EmptyReason { file: PathBuf, index: usize },
}

#[derive(Debug, Default)]
pub struct Allowlist {
    unsafe_allow: Vec<UnsafeAllow>,
}

impl Allowlist {
    pub fn apply(&self, report: &mut DriftReport) {
        if self.unsafe_allow.is_empty() {
            return;
        }
        for finding in &mut report.findings {
            if finding.class != unsafe_score::CLASS {
                continue;
            }
            let path = finding.path.to_string_lossy();
            let Some(entry) = self.unsafe_allow.iter().find(|entry| entry.matches(&path)) else {
                continue;
            };
            let before = finding.severity;
            let after = demote(before);
            if after != before {
                finding.severity = after;
                finding.message.push_str(&format!(
                    "; allowlist demoted {before}->{after}: {reason}",
                    before = before.label(),
                    after = after.label(),
                    reason = entry.reason,
                ));
            }
        }
    }
}

#[derive(Debug)]
struct UnsafeAllow {
    path: String,
    reason: String,
}

impl UnsafeAllow {
    fn matches(&self, path: &str) -> bool {
        glob_matches(&self.path, path)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAllowlist {
    schema_version: u32,
    #[serde(default)]
    unsafe_allow: Vec<RawUnsafeAllow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUnsafeAllow {
    path: String,
    reason: String,
}

pub fn load(root: &SourceRoot) -> Result<Allowlist, AllowlistError> {
    if let Some(path) = find_private_allowlist(&root.path().join(".private")) {
        return Err(AllowlistError::PrivateUnsupported {
            path,
            file: ALLOWLIST_FILE,
        });
    }

    let file = root.path().join(ALLOWLIST_FILE);
    if !file.exists() {
        return Ok(Allowlist::default());
    }

    let raw = fs::read_to_string(&file).map_err(|source| AllowlistError::Io {
        file: file.clone(),
        source,
    })?;
    let parsed: RawAllowlist =
        serde_yaml_ng::from_str(&raw).map_err(|source| AllowlistError::Parse {
            file: file.clone(),
            source,
        })?;
    if parsed.schema_version != SCHEMA_VERSION {
        return Err(AllowlistError::SchemaVersion {
            file,
            expected: SCHEMA_VERSION,
            found: parsed.schema_version,
        });
    }

    let mut unsafe_allow = Vec::new();
    for (index, entry) in parsed.unsafe_allow.into_iter().enumerate() {
        if entry.path.trim().is_empty() {
            return Err(AllowlistError::EmptyPath { file, index });
        }
        if entry.reason.trim().is_empty() {
            return Err(AllowlistError::EmptyReason { file, index });
        }
        unsafe_allow.push(UnsafeAllow {
            path: entry.path,
            reason: entry.reason,
        });
    }
    Ok(Allowlist { unsafe_allow })
}

fn demote(severity: Severity) -> Severity {
    match severity {
        Severity::Block => Severity::Warn,
        Severity::Warn => Severity::Suppressed,
        Severity::Suppressed => Severity::Suppressed,
    }
}

fn find_private_allowlist(private_dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![private_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some(ALLOWLIST_FILE) {
                return Some(path);
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(path);
            }
        }
    }
    None
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = normalize_path(pattern);
    let path = normalize_path(path);
    let pattern_parts: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let path_parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match_components(&pattern_parts, &path_parts)
}

fn normalize_path(path: &str) -> String {
    path.trim().trim_matches('/').replace('\\', "/")
}

fn match_components(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        return (0..=path.len()).any(|idx| match_components(&pattern[1..], &path[idx..]));
    }
    if path.is_empty() {
        return false;
    }
    segment_matches(pattern[0], path[0]) && match_components(&pattern[1..], &path[1..])
}

fn segment_matches(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let mut remaining = text;
    if let Some(first) = parts.first().filter(|part| !part.is_empty()) {
        let Some(rest) = remaining.strip_prefix(first) else {
            return false;
        };
        remaining = rest;
    }
    for part in parts
        .iter()
        .skip(1)
        .take(parts.len().saturating_sub(2))
        .filter(|part| !part.is_empty())
    {
        let Some(idx) = remaining.find(part) else {
            return false;
        };
        remaining = &remaining[idx + part.len()..];
    }
    if let Some(last) = parts.last().filter(|part| !part.is_empty()) {
        remaining.ends_with(last)
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_drift::{Finding, Severity};

    #[test]
    fn glob_matches_double_star_and_segment_wildcards() {
        assert!(glob_matches(
            "tests/drift/fixtures/**",
            "tests/drift/fixtures/auth.json"
        ));
        assert!(glob_matches("core/*.json", "core/auth.json"));
        assert!(!glob_matches("core/*.json", "core/nested/auth.json"));
    }

    #[test]
    fn apply_demotes_unsafe_findings_only_one_tier() {
        let allowlist = Allowlist {
            unsafe_allow: vec![UnsafeAllow {
                path: "core/auth.json".to_string(),
                reason: "fixture".to_string(),
            }],
        };
        let mut report = DriftReport {
            findings: vec![Finding {
                class: unsafe_score::CLASS,
                severity: Severity::Block,
                product: None,
                path: PathBuf::from("core/auth.json"),
                message: "score=1.2".to_string(),
            }],
        };
        allowlist.apply(&mut report);
        assert_eq!(report.findings[0].severity, Severity::Warn);
        assert!(report.findings[0].message.contains("block->warn"));
    }
}
