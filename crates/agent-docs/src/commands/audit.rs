//! `audit` — repo health: install-symlink wiring, declared-doc presence and
//! content validity, and catalog validity. It reports problems and prints a
//! suggested fix command; it never repairs anything.

use std::fs;

use crate::config::load_catalog_from_roots;
use crate::env::{self, ResolvedRoots, SymlinkWiring};
use crate::model::{
    AuditReport, AuditTarget, ConfigLoadError, DocumentStatus, FallbackMode, LoadedCatalog,
    Product, ResolvedDocument, SkillCheck, SkillPolicy, WiringCheck,
};
use crate::resolver;

pub fn run_audit(
    target: AuditTarget,
    roots: &ResolvedRoots,
    product: Option<Product>,
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

    let documents = resolver::resolve_documents_for_target_for_product(
        roots,
        target,
        product,
        fallback_mode,
        &catalog,
    );
    let skill_policy = effective_skill_policy(target, &catalog);
    let skills = skill_checks(roots, skill_policy);

    let doc_problems = documents
        .iter()
        .filter(|doc| doc.required && !doc.satisfied())
        .count();
    let wiring_problems = wiring.iter().filter(|check| !check.ok).count();
    let skill_problems = skills.iter().filter(|check| !check.ok).count();
    let problems = doc_problems + wiring_problems + skill_problems;

    let suggested_actions = suggested_actions(&documents, &wiring, &skills, skill_policy);

    Ok(AuditReport {
        schema_version: AuditReport::SCHEMA_VERSION,
        target,
        product,
        strict,
        docs_home: roots.docs_home.clone(),
        project_path: roots.project_path.clone(),
        wiring,
        skills,
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

/// Resolve the skill-name policy that applies to this audit.
///
/// Skill naming is a project-scope concern, so only the *project* catalog can
/// opt in (a home/global policy must never silently force enforcement on every
/// consumer repo). The scan therefore runs only for `Project`/`All` targets and
/// only when the project catalog declares `[skills]`.
fn effective_skill_policy(target: AuditTarget, catalog: &LoadedCatalog) -> Option<&SkillPolicy> {
    if !matches!(target, AuditTarget::Project | AuditTarget::All) {
        return None;
    }
    catalog
        .project
        .as_ref()
        .and_then(|scope| scope.skill_policy.as_ref())
        .filter(|policy| policy.enforce_name_prefix)
}

/// Check every immediate subdirectory of the configured skills directory
/// against the policy. Missing/unreadable directories yield no checks (a repo
/// that opted in but has no skills yet is not a failure).
fn skill_checks(roots: &ResolvedRoots, policy: Option<&SkillPolicy>) -> Vec<SkillCheck> {
    let Some(policy) = policy else {
        return Vec::new();
    };

    let skills_dir = roots.project_path.join(&policy.dir);
    let Ok(entries) = fs::read_dir(&skills_dir) else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();

    names
        .into_iter()
        .map(|name| {
            let ok = policy.name_is_valid(&name);
            let detail = if ok {
                "matches the required project-local skill-name prefix".to_string()
            } else {
                format!(
                    "name must be lowercase kebab-case starting with one of: {}",
                    policy.prefix_hint()
                )
            };
            SkillCheck { name, ok, detail }
        })
        .collect()
}

fn suggested_actions(
    documents: &[ResolvedDocument],
    wiring: &[WiringCheck],
    skills: &[SkillCheck],
    skill_policy: Option<&SkillPolicy>,
) -> Vec<String> {
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

    let prefix_hint = skill_policy
        .map(SkillPolicy::prefix_hint)
        .unwrap_or_default();
    for check in skills.iter().filter(|check| !check.ok) {
        actions.push(format!(
            "rename skill '{}' to start with one of: {prefix_hint}",
            check.name
        ));
    }

    actions
}
