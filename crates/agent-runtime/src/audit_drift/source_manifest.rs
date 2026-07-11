//! Source-manifest validity class (warn-tier, exit 1).
//!
//! Re-validates every Phase 1 manifest against its typed schema, and
//! scans the raw manifest bytes for the `<TBD` placeholder string that
//! Plan 01 left in `runtime-roots.yaml` until pinning. Either signal is
//! a `warn`-tier finding — Phase 2 reporting surfaces these but does
//! not block on them. Resolved Decision #9's stricter contracts land in
//! their own classes (block-tier).

use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::manifest::{self, ManifestSet, SourceRoot};
use anyhow::Result;
use std::fs;
use std::path::Path;

pub const CLASS: &str = "source-manifest";

const TBD_PLACEHOLDER: &str = "<TBD";

/// The five Phase 1 manifest filenames, in canonical order. The diff
/// class re-uses the same list when bundling manifest bytes for hashing.
const MANIFEST_FILES: &[&str] = &[
    "skills.yaml",
    "plugins.yaml",
    "product-capabilities.yaml",
    "runtime-roots.yaml",
    "cli-tools.yaml",
];

/// Run the class. Returns the parsed manifest set when validation
/// succeeds so downstream classes (rendered-target diff) can reuse it
/// — `None` when validation fails. Either branch records its own
/// findings in `report`.
pub fn check(root: &SourceRoot, report: &mut DriftReport) -> Result<Option<Box<ManifestSet>>> {
    // Validity check — parse via the same typed loader the render
    // engine uses. A parse / schema failure becomes one finding per
    // failed manifest; we return early when none load.
    let parsed = match manifest::load_all(root) {
        Ok(set) => Some(Box::new(set)),
        Err(err) => {
            report.push(Finding {
                class: CLASS,
                severity: Severity::Warn,
                product: None,
                path: failing_manifest_path(&err, root),
                message: format!("manifest validity: {err}"),
            });
            None
        }
    };

    // Placeholder scan — runs regardless of parse outcome. The Phase 1
    // gate said `<TBD>` strings must not survive into the floor-pinned
    // manifest set; surfacing them as findings here lets the reporting
    // POC track the closure.
    for name in MANIFEST_FILES {
        let path = root.manifests_dir().join(name);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        scan_placeholders(root.path(), &path, &bytes, report)?;
    }

    Ok(parsed)
}

fn scan_placeholders(
    source_root: &Path,
    path: &Path,
    bytes: &[u8],
    report: &mut DriftReport,
) -> Result<()> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let rel = path.strip_prefix(source_root).unwrap_or(path).to_path_buf();
    let mut byte_offset: usize = 0;
    for (line_idx, line) in text.split_inclusive('\n').enumerate() {
        if let Some(col) = line.find(TBD_PLACEHOLDER) {
            report.push(Finding {
                class: CLASS,
                severity: Severity::Warn,
                product: None,
                path: rel.clone(),
                message: format!(
                    "placeholder `<TBD…>` at line {line}, col {col} (byte {byte}); pin before \
                     Phase 2 floor",
                    line = line_idx + 1,
                    col = col + 1,
                    byte = byte_offset + col,
                ),
            });
        }
        byte_offset += line.len();
    }
    Ok(())
}

/// Best-effort path extraction from a `ManifestError`. Walks the
/// `Display` for the source path on the variants that name one.
fn failing_manifest_path(err: &manifest::ManifestError, root: &SourceRoot) -> std::path::PathBuf {
    use manifest::ManifestError;
    let file = match err {
        ManifestError::Missing { path, .. } => Some(path.clone()),
        ManifestError::SchemaVersion { file, .. } => Some(file.clone()),
        ManifestError::SchemaVersions { file, .. } => Some(file.clone()),
        ManifestError::InvalidSkillContract { file, .. } => Some(file.clone()),
        ManifestError::Parse { file, .. } => Some(file.clone()),
        ManifestError::Io { file, .. } => Some(file.clone()),
        ManifestError::SourceRoot { path, .. } => Some(path.clone()),
    };
    let file = file.unwrap_or_else(|| root.path().to_path_buf());
    file.strip_prefix(root.path())
        .map(|p| p.to_path_buf())
        .unwrap_or(file)
}

#[cfg(test)]
mod tests {
    //! Per-class unit tests cover the `<TBD>` placeholder scan and the
    //! schema-version mismatch path against tiny inline fixtures. The
    //! full happy-path (valid manifest set yields no findings) lives
    //! in `tests/integration/audit_drift_classes.rs` against the
    //! shared fixture set so we don't duplicate the manifest body here.

    use super::*;
    use tempfile::TempDir;

    fn fixture_dir() -> (TempDir, SourceRoot) {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("manifests")).unwrap();
        let root = SourceRoot::from_arg_or_cwd(Some(tmp.path())).unwrap();
        (tmp, root)
    }

    #[test]
    fn tbd_placeholder_in_runtime_roots_emits_warn_finding() {
        let (tmp, root) = fixture_dir();
        // Only the placeholder scanner is exercised here — write a
        // manifest body that contains `<TBD…>`; the typed loader will
        // fail (incomplete schema) and we ignore that branch.
        fs::write(
            tmp.path().join("manifests/runtime-roots.yaml"),
            "schema_version: 1\nproducts:\n  codex:\n    min_version: \"<TBD: pin during Phase 1>\"\n",
        )
        .unwrap();
        let mut report = DriftReport::default();
        check(&root, &mut report).unwrap();
        let placeholder_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.message.contains("placeholder"))
            .collect();
        assert_eq!(placeholder_findings.len(), 1, "{:?}", report.findings);
        assert_eq!(placeholder_findings[0].severity, Severity::Warn);
        assert_eq!(
            placeholder_findings[0].path,
            std::path::PathBuf::from("manifests/runtime-roots.yaml")
        );
    }

    #[test]
    fn schema_version_mismatch_emits_warn_finding() {
        let (tmp, root) = fixture_dir();
        // A schema-version=9 manifest makes `manifest::load_all` fail
        // before it tries to deserialize fields — exactly what the
        // validity branch should surface.
        fs::write(
            tmp.path().join("manifests/skills.yaml"),
            "schema_version: 9\nskills: []\n",
        )
        .unwrap();
        let mut report = DriftReport::default();
        let parsed = check(&root, &mut report).unwrap();
        assert!(parsed.is_none(), "schema mismatch should drop manifests");
        let validity = report
            .findings
            .iter()
            .find(|f| f.message.starts_with("manifest validity"))
            .expect("expected one validity finding");
        assert_eq!(validity.severity, Severity::Warn);
    }
}
