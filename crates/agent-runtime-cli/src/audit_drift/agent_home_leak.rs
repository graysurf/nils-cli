//! `$AGENT_HOME` leak class (block-tier, exit 2).
//!
//! Per Resolved Decision #5 in
//! `agent-runtime-kit/docs/source/inventory-target-architecture.md`,
//! `$AGENT_HOME` is the removed pre-Phase-1 environment variable. Any
//! occurrence of the literal substring in rendered output, source
//! templates, or manifest content is a `block`-tier finding — Phase 2
//! must not ship a build that re-introduces it.
//!
//! Scope (Phase 1.5):
//! - `build/<product>/**` per product
//! - `core/**`
//! - `targets/**`
//! - `manifests/**`
//!
//! `docs/` is intentionally outside scope here — the source doc
//! `docs/source/inventory-target-architecture.md` legitimately names
//! the removed variable. Plan 04 brings the proper
//! `drift-audit.allow.yaml` mechanism when scope expands to docs/.
//!
//! Symlinks: every walk goes through `audit_drift::walk::collect_files_under`,
//! which uses `render::writer::canonicalize_under` to drop entries
//! that resolve outside the source root. A hostile
//! `build/<product>/symlink -> /etc/passwd` cannot make audit-drift
//! slurp host files.

use crate::audit_drift::walk;
use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::manifest::SourceRoot;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub const CLASS: &str = "agent-home-leak";
pub const NEEDLE: &str = "$AGENT_HOME";

/// Walk `build/<product>/` and report every leak as a block-tier finding.
pub fn check_product_build(
    root: &SourceRoot,
    product: &str,
    report: &mut DriftReport,
) -> Result<()> {
    let build_dir = root.path().join("build").join(product);
    scan_tree(root, &build_dir, Some(product.to_string()), report)
}

/// Walk `core/`, `targets/`, `manifests/` and report leaks.
pub fn check_source_tree(root: &SourceRoot, report: &mut DriftReport) -> Result<()> {
    for sub in ["core", "targets", "manifests"] {
        let dir = root.path().join(sub);
        scan_tree(root, &dir, None, report)?;
    }
    Ok(())
}

fn scan_tree(
    root: &SourceRoot,
    dir: &Path,
    product: Option<String>,
    report: &mut DriftReport,
) -> Result<()> {
    for path in walk::collect_files_under(dir, root.path()) {
        scan_file(root, &path, product.as_deref(), report)?;
    }
    Ok(())
}

fn scan_file(
    root: &SourceRoot,
    path: &Path,
    product: Option<&str>,
    report: &mut DriftReport,
) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("audit-drift read {}", path.display()))?;
    // Treat anything non-UTF8 as opaque (we only care about literal
    // ASCII matches in render output / source text).
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };
    let rel = path.strip_prefix(root.path()).unwrap_or(path).to_path_buf();
    let mut byte_offset = 0usize;
    let mut found = false;
    for (line_idx, line) in text.split_inclusive('\n').enumerate() {
        if let Some(col) = line.find(NEEDLE) {
            report.push(Finding {
                class: CLASS,
                severity: Severity::Block,
                product: product.map(|p| p.to_string()),
                path: rel.clone(),
                message: format!(
                    "literal `{NEEDLE}` at line {line}, col {col} (byte {byte}); \
                     Resolved Decision #5 removed this variable",
                    line = line_idx + 1,
                    col = col + 1,
                    byte = byte_offset + col,
                ),
            });
            found = true;
        }
        byte_offset += line.len();
        if found {
            // Only the first hit per file becomes a finding — the
            // reporting POC consumer wants one row per file, not one
            // per byte. Subsequent occurrences are implied.
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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
    fn agent_home_in_build_codex_is_block_finding() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/skills/sample/SKILL.md"),
            "before $AGENT_HOME after\n",
        );
        let mut report = DriftReport::default();
        check_product_build(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.class, CLASS);
        assert_eq!(f.severity, Severity::Block);
        assert_eq!(f.product.as_deref(), Some("codex"));
        assert_eq!(f.path, PathBuf::from("build/codex/skills/sample/SKILL.md"));
    }

    #[test]
    fn agent_home_in_core_is_block_finding() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("core/skills/sample/SKILL.md.tera"),
            "{{ \"$AGENT_HOME\" }}\n",
        );
        let mut report = DriftReport::default();
        check_source_tree(&root, &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.severity, Severity::Block);
        assert!(f.product.is_none(), "source-tree finding has no product");
    }

    #[test]
    fn docs_tree_is_outside_phase_1_5_scope() {
        // Phase 1.5 explicitly skips `docs/`; the source doc names
        // `$AGENT_HOME` legitimately and must not fire a finding.
        // Plan 04 expands the scope with proper allowlist handling.
        let (tmp, root) = make_root();
        write(
            &tmp.path()
                .join("docs/source/inventory-target-architecture.md"),
            "Resolved Decision #5 removed $AGENT_HOME from the runtime.\n",
        );
        let mut report = DriftReport::default();
        check_source_tree(&root, &mut report).unwrap();
        assert_eq!(report.findings, Vec::new());
    }

    #[test]
    fn clean_tree_has_no_findings() {
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("core/skills/sample/SKILL.md.tera"),
            "no leak here\n",
        );
        write(
            &tmp.path().join("build/codex/skills/sample/SKILL.md"),
            "rendered clean output\n",
        );
        let mut report = DriftReport::default();
        check_product_build(&root, "codex", &mut report).unwrap();
        check_source_tree(&root, &mut report).unwrap();
        assert_eq!(report.findings, Vec::new());
    }

    #[test]
    fn only_first_hit_per_file_is_reported() {
        // The class records one finding per file even when multiple
        // occurrences exist on different lines. The reporting POC
        // counts file-level hits, not byte-level, so this is the
        // contract — but it deserves an explicit test so a future
        // refactor doesn't silently change it.
        let (tmp, root) = make_root();
        write(
            &tmp.path().join("build/codex/skills/sample/SKILL.md"),
            "leak one: $AGENT_HOME\nleak two: $AGENT_HOME also\nleak three: $AGENT_HOME end\n",
        );
        let mut report = DriftReport::default();
        check_product_build(&root, "codex", &mut report).unwrap();
        assert_eq!(report.findings.len(), 1);
        // First hit is on line 1; later hits are implied.
        assert!(report.findings[0].message.contains("line 1"));
    }
}
