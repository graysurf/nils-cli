//! `audit` — repo health: install-symlink wiring, declared-doc presence and
//! content validity, and catalog validity. It reports problems and prints a
//! suggested fix command; it never repairs anything.

use crate::config::load_catalog_from_roots;
use crate::env::{self, ResolvedRoots, SymlinkWiring};
use crate::model::{
    AuditReport, AuditTarget, ConfigLoadError, DocumentStatus, FallbackMode, ResolvedDocument,
    WiringCheck,
};
use crate::resolver;

pub fn run_audit(
    target: AuditTarget,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
) -> Result<AuditReport, ConfigLoadError> {
    // Loading the catalog also validates it; a parse/validation error surfaces
    // to the caller as a config error.
    let catalog = load_catalog_from_roots(roots)?;

    let mut wiring = Vec::new();
    if matches!(target, AuditTarget::Home | AuditTarget::All) {
        wiring.push(symlink_wiring_check(roots));
    }

    let documents = resolver::resolve_documents_for_target(roots, target, fallback_mode, &catalog);

    let doc_problems = documents
        .iter()
        .filter(|doc| doc.required && !doc.satisfied())
        .count();
    let wiring_problems = wiring.iter().filter(|check| !check.ok).count();
    let problems = doc_problems + wiring_problems;

    let suggested_actions = suggested_actions(&documents, &wiring);

    Ok(AuditReport {
        schema_version: AuditReport::SCHEMA_VERSION,
        target,
        strict,
        docs_home: roots.docs_home.clone(),
        project_path: roots.project_path.clone(),
        wiring,
        documents,
        problems,
        suggested_actions,
    })
}

fn symlink_wiring_check(roots: &ResolvedRoots) -> WiringCheck {
    let (wiring, detail) =
        env::inspect_symlink_wiring(env::home_dir().as_deref(), &roots.docs_home);
    WiringCheck {
        name: "install-symlink".to_string(),
        ok: wiring == SymlinkWiring::Intact,
        detail,
    }
}

fn suggested_actions(documents: &[ResolvedDocument], wiring: &[WiringCheck]) -> Vec<String> {
    let mut actions = Vec::new();

    let missing: Vec<&ResolvedDocument> = documents
        .iter()
        .filter(|doc| doc.required && doc.status == DocumentStatus::Missing)
        .collect();
    for doc in &missing {
        actions.push(format!(
            "create the required document: {}",
            doc.path.display()
        ));
    }

    let invalid: Vec<&ResolvedDocument> = documents
        .iter()
        .filter(|doc| {
            doc.required && doc.status == DocumentStatus::Present && !doc.validation.valid
        })
        .collect();
    for doc in &invalid {
        actions.push(format!(
            "fix invalid content (non-empty + marker + freshness): {}",
            doc.path.display()
        ));
    }

    if wiring.iter().any(|check| !check.ok) {
        actions.push(
            "restore the install symlink by re-running the kit install/sync (\
             ~/.claude/CLAUDE.md and ~/.codex/AGENTS.md must point at AGENT_HOME.md)"
                .to_string(),
        );
    }

    actions
}
