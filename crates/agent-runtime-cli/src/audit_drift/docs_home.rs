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

use crate::audit_drift::walk;
use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::manifest::SourceRoot;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub const CLASS: &str = "docs-home";

const FLAG: &str = "--docs-home";

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
    for path in walk::collect_files_under(&build_dir, root.path()) {
        scan_file(root, &path, product, expected, report)?;
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
        // Every occurrence on the line is checked — multiple flags on
        // one rendered line shouldn't hide later ones behind the first.
        for actual in find_docs_home_args(line) {
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
    }
    Ok(())
}

/// Find every `--docs-home <value>` and `--docs-home=<value>` on the
/// line. Returns the literal value token (quotes included so
/// `"$CODEX_HOME"` differs from `$CODEX_HOME`). Rejects prefix
/// collisions like `--docs-home-extra` (different flag) by requiring
/// the trailing char to be either `=` or whitespace.
///
/// End-of-line case: a `--docs-home` flag with no following token is
/// emitted as an empty `""` value so the caller's expected-token
/// comparison fires a finding instead of silently passing. This makes
/// the contract robust against a renderer that accidentally drops the
/// value.
fn find_docs_home_args(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel_idx) = line[search_from..].find(FLAG) {
        let idx = search_from + rel_idx;
        let after = &line[idx + FLAG.len()..];
        // Defeat prefix collisions: `--docs-home-extra` must not match.
        match after.chars().next() {
            Some('=') => {
                // `--docs-home=<value>` form. Value ends at next
                // whitespace.
                let rest = &after[1..];
                let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
                out.push(&rest[..end]);
                search_from = idx + FLAG.len() + 1 + end;
            }
            Some(c) if c.is_whitespace() => {
                let rest = after.trim_start();
                if rest.is_empty() {
                    // `--docs-home` at end of line with no value —
                    // emit an empty token so the expected-value check
                    // surfaces it as a mismatch finding instead of a
                    // silent pass.
                    out.push("");
                    break;
                }
                let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
                out.push(&rest[..end]);
                let consumed = after.len() - rest.len() + end;
                search_from = idx + FLAG.len() + consumed;
            }
            None => {
                // `--docs-home` with nothing after — same end-of-line
                // case as above.
                out.push("");
                break;
            }
            Some(_) => {
                // Prefix collision (`--docs-home-extra`, etc). Skip
                // past this occurrence and keep looking.
                search_from = idx + FLAG.len();
            }
        }
    }
    out
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

    #[test]
    fn equals_form_is_parsed_and_validated() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/policy/docs-home.md"),
            "tool --docs-home=\"$HOME/.claude\" --foo\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].message.contains("\"$HOME/.claude\""));
    }

    #[test]
    fn prefix_collision_with_docs_home_extra_does_not_fire() {
        // `--docs-home-extra` is a different flag entirely; the
        // detector must distinguish it from `--docs-home`.
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/policy/extra.md"),
            "tool --docs-home-extra hello --docs-home \"$CODEX_HOME\"\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(
            report.findings,
            Vec::new(),
            "--docs-home-extra is a different flag and must not match"
        );
    }

    #[test]
    fn end_of_line_with_no_value_is_block_finding() {
        // A renderer that emits `--docs-home` with no value is broken;
        // the detector must surface it as a mismatch instead of
        // silently passing (the original implementation returned
        // `None` here and dropped the line on the floor).
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/policy/docs-home.md"),
            "tool --docs-home\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn multi_flag_on_same_line_checks_every_occurrence() {
        // Two `--docs-home` flags on the same line: one wrong, one
        // right. The detector must fire on the wrong one even though
        // it's preceded by a correct one.
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/policy/multi.md"),
            "tool --docs-home \"$CODEX_HOME\" then --docs-home \"$HOME/.claude\"\n",
        );
        let mut report = DriftReport::default();
        check(&root, "codex", &mut report).unwrap();
        assert_eq!(
            report.findings.len(),
            1,
            "exactly one mismatch out of the two flags on this line"
        );
        assert!(report.findings[0].message.contains("\"$HOME/.claude\""));
    }
}
