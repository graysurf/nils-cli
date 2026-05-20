//! Docs-home per product class (block-tier, exit 2).
//!
//! Each product has a single sanctioned `--docs-home` value:
//!
//! - codex → `"$CODEX_HOME"`
//! - claude → `"$HOME/.claude"`
//!
//! A rendered policy line that names a `--docs-home` flag with any
//! other variable, missing quoting, or the wrong product's home is a
//! `block`-tier finding (exit 2). Lines without `--docs-home` are not
//! findings.
//!
//! The class is intentionally table-driven on the product → expected
//! value mapping so Plan 04's third-product support is additive. The
//! mapping mirrors `runtime-roots.yaml` but is hard-coded here because
//! Phase 1.5 ships before the runtime-roots `docs_home` field is
//! marketed as the canonical source.

use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::manifest::SourceRoot;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const CLASS: &str = "docs-home";

pub fn expected_docs_home(product: &str) -> Option<&'static str> {
    match product {
        "codex" => Some("\"$CODEX_HOME\""),
        "claude" => Some("\"$HOME/.claude\""),
        _ => None,
    }
}

pub fn check(root: &SourceRoot, product: &str, report: &mut DriftReport) -> Result<()> {
    let Some(expected) = expected_docs_home(product) else {
        return Ok(());
    };
    let build_dir = root.path().join("build").join(product);
    if !build_dir.exists() {
        return Ok(());
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_files(&build_dir, &mut paths)?;
    paths.sort();
    for path in paths {
        scan_file(root, &path, product, expected, report)?;
    }
    Ok(())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("audit-drift docs-home read_dir {}", dir.display()))?;
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn scan_file(
    root: &SourceRoot,
    path: &Path,
    product: &str,
    expected: &'static str,
    report: &mut DriftReport,
) -> Result<()> {
    let bytes =
        fs::read(path).with_context(|| format!("audit-drift docs-home read {}", path.display()))?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };
    let rel = path.strip_prefix(root.path()).unwrap_or(path).to_path_buf();
    for (line_idx, line) in text.lines().enumerate() {
        let Some(actual) = find_docs_home_arg(line) else {
            continue;
        };
        if actual != expected {
            report.push(Finding {
                class: CLASS,
                severity: Severity::Block,
                product: Some(product.to_string()),
                path: rel.clone(),
                message: format!(
                    "--docs-home {actual} at line {line}; expected {expected} for product `{product}`",
                    line = line_idx + 1,
                ),
            });
        }
    }
    Ok(())
}

/// Return the literal token following `--docs-home` on this line.
/// Returns `None` when the flag is absent. The token is whatever
/// whitespace-delimited slice follows the flag — including the quotes
/// when the renderer wrote `"$CODEX_HOME"` rather than the bare
/// `$CODEX_HOME`. Missing-quote variants therefore mismatch the
/// expected string and surface as findings.
fn find_docs_home_arg(line: &str) -> Option<&str> {
    let idx = line.find("--docs-home")?;
    let rest = &line[idx + "--docs-home".len()..];
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    // Use the first whitespace-delimited token. Quote characters are
    // part of the token, so `"$CODEX_HOME"` and `$CODEX_HOME` differ
    // — the contract requires the quoted form.
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn make_root() -> (TempDir, SourceRoot) {
        let tmp = TempDir::new().unwrap();
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        (tmp, root)
    }

    #[test]
    fn codex_with_claude_home_is_block_finding() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/policy/docs-home.md"),
            "tool --docs-home \"$HOME/.claude\" --foo bar\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.class, CLASS);
        assert_eq!(f.severity, Severity::Block);
        assert!(f.message.contains("\"$HOME/.claude\""));
        assert!(f.message.contains("\"$CODEX_HOME\""));
    }

    #[test]
    fn claude_with_codex_home_is_block_finding() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/claude/policy/docs-home.md"),
            "tool --docs-home \"$CODEX_HOME\"\n",
        );
        let mut report = DriftReport::default();
        check(&root, "claude", &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Block);
    }

    #[test]
    fn unquoted_codex_home_is_block_finding() {
        // Missing quotes shouldn't pass — the contract demands the
        // quoted form so shell substitution behaves identically across
        // hosts.
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/policy/docs-home.md"),
            "tool --docs-home $CODEX_HOME\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn correct_codex_home_passes() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/policy/docs-home.md"),
            "tool --docs-home \"$CODEX_HOME\" --next other\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings, Vec::new());
    }

    #[test]
    fn lines_without_docs_home_are_ignored() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/skills/sample/SKILL.md"),
            "no flags here\nsome content\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings, Vec::new());
    }
}
