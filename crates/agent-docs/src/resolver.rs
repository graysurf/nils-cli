//! Catalog-driven resolution.
//!
//! There are no hardcoded builtins: every required document and validation
//! contract comes from the loaded catalog (`AGENT_DOCS.toml` at the docs-home
//! and the project). Resolution evaluates each entry's `when` predicate,
//! validates content, de-duplicates by resolved path, and resolves the
//! per-intent validation contract.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use nils_common::git as shared_git;

use crate::config::load_catalog_from_roots;
use crate::content;
use crate::env::ResolvedRoots;
use crate::model::{
    AuditTarget, ConfigLoadError, Context, DocumentEntry, DocumentSource, DocumentStatus,
    DocumentValidation, FallbackMode, LoadedCatalog, PreflightReport, ResolveSummary,
    ResolvedDocument, Scope, ScopeCatalog, ValidationContract,
};
use crate::paths::{normalize_path, normalize_root_path};
use crate::predicate;

/// Resolve the document set and validation contract for a single intent.
pub fn resolve_intent(
    intent: &Context,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
) -> Result<PreflightReport, ConfigLoadError> {
    let catalog = load_catalog_from_roots(roots)?;
    Ok(resolve_intent_with_catalog(
        intent,
        roots,
        strict,
        fallback_mode,
        emit_content,
        &catalog,
    ))
}

pub fn resolve_intent_with_catalog(
    intent: &Context,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
    catalog: &LoadedCatalog,
) -> PreflightReport {
    let documents = resolve_documents(roots, fallback_mode, emit_content, catalog, &mut |entry| {
        entry.context == *intent
    });
    let validation = resolve_validation_contract(intent, roots, catalog);
    let summary = ResolveSummary::from_documents(&documents);

    PreflightReport {
        schema_version: PreflightReport::SCHEMA_VERSION,
        intent: intent.clone(),
        strict,
        docs_home: roots.docs_home.clone(),
        project_path: roots.project_path.clone(),
        is_linked_worktree: roots.is_linked_worktree,
        documents,
        validation,
        summary,
    }
}

/// Resolve every declared document whose scope falls within `target`, for
/// `audit`.
pub fn resolve_documents_for_target(
    roots: &ResolvedRoots,
    target: AuditTarget,
    fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
) -> Vec<ResolvedDocument> {
    resolve_documents(roots, fallback_mode, false, catalog, &mut |entry| {
        target.includes_scope(entry.scope)
    })
}

/// Resolve every declared document (for `list`).
pub fn resolve_all_documents(
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
) -> Vec<ResolvedDocument> {
    resolve_documents(roots, fallback_mode, false, catalog, &mut |_| true)
}

fn resolve_documents(
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    emit_content: bool,
    catalog: &LoadedCatalog,
    accept: &mut dyn FnMut(&DocumentEntry) -> bool,
) -> Vec<ResolvedDocument> {
    let mut documents: Vec<ResolvedDocument> = Vec::new();
    let mut index_by_path: HashMap<PathBuf, usize> = HashMap::new();
    let home_project_scope_applies = home_project_scope_applies(roots);

    for scope_catalog in catalog.in_load_order() {
        for entry in &scope_catalog.documents {
            if !accept(entry) {
                continue;
            }
            // A `scope = "project"` entry declared in the *home* (docs-home)
            // catalog is a repo-only requirement of the docs-home repository.
            // It applies only when the docs-home and the project being resolved
            // are the same repository; otherwise the docs-home's project-local
            // requirements would leak into every unrelated project. Home- and
            // global-scope entries still inherit everywhere.
            if scope_catalog.source_scope == Scope::Home
                && entry.scope == Scope::Project
                && !home_project_scope_applies
            {
                continue;
            }
            let resolved = resolve_entry(entry, scope_catalog, roots, fallback_mode, emit_content);
            // De-duplicate by resolved path; a later (project) catalog entry
            // overrides an earlier (home) one at the same position.
            if let Some(existing) = index_by_path.get(&resolved.path).copied() {
                documents[existing] = resolved;
            } else {
                index_by_path.insert(resolved.path.clone(), documents.len());
                documents.push(resolved);
            }
        }
    }

    documents
}

fn resolve_entry(
    entry: &DocumentEntry,
    scope_catalog: &ScopeCatalog,
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    emit_content: bool,
) -> ResolvedDocument {
    let path = resolve_entry_path(entry, roots, fallback_mode);
    let when_satisfied = predicate::evaluate(&entry.when, &roots.project_path);
    let required = entry.required && when_satisfied;

    let status = if path.exists() {
        DocumentStatus::Present
    } else {
        DocumentStatus::Missing
    };

    let validation = if status == DocumentStatus::Present {
        content::validate(&path, entry)
    } else {
        DocumentValidation::missing()
    };

    let content = if emit_content && status == DocumentStatus::Present {
        std::fs::read_to_string(&path).ok()
    } else {
        None
    };

    ResolvedDocument {
        context: entry.context.clone(),
        scope: entry.scope,
        path,
        declared_required: entry.required,
        required,
        when: entry.when_raw.clone(),
        when_satisfied,
        status,
        validation,
        source: DocumentSource::from_scope(scope_catalog.source_scope),
        why: describe_why(entry, scope_catalog, when_satisfied),
        content,
    }
}

fn resolve_entry_path(
    entry: &DocumentEntry,
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
) -> PathBuf {
    let root = root_for_scope(entry.scope, roots);
    let local = normalize_path(&root.join(&entry.path));
    if local.exists() {
        return local;
    }

    // Linked-worktree fallback: a project-scope doc missing locally may live in
    // the primary worktree.
    if entry.scope == Scope::Project
        && fallback_mode == FallbackMode::Auto
        && roots.is_linked_worktree
        && let Some(primary) = roots.primary_worktree_path.as_deref()
    {
        let fallback = normalize_path(&primary.join(&entry.path));
        if fallback.exists() {
            return fallback;
        }
    }

    local
}

fn root_for_scope(scope: Scope, roots: &ResolvedRoots) -> &Path {
    match scope {
        Scope::Home | Scope::Global => &roots.docs_home,
        Scope::Project => &roots.project_path,
    }
}

/// Whether the docs-home catalog's `scope = "project"` entries apply to the
/// project being resolved.
///
/// They apply only when the docs-home and the project are the same repository.
/// When both roots are git repositories we compare their common dirs; if we
/// cannot positively establish that they are *distinct* repositories (e.g. a
/// non-git docs-home or a scratch directory), we stay permissive and apply the
/// entries, so the only behavior change versus an ungated resolver is to stop
/// repo-only requirements leaking into an unrelated git project.
fn home_project_scope_applies(roots: &ResolvedRoots) -> bool {
    match (
        git_common_dir(&roots.docs_home),
        git_common_dir(&roots.project_path),
    ) {
        (Some(docs_home_repo), Some(project_repo)) => docs_home_repo == project_repo,
        _ => true,
    }
}

fn git_common_dir(root: &Path) -> Option<PathBuf> {
    let raw = shared_git::rev_parse_in(root, &["--git-common-dir"])
        .ok()
        .flatten()?;
    Some(canonical_root(&normalize_root_path(Path::new(&raw), root)))
}

fn canonical_root(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

fn describe_why(
    entry: &DocumentEntry,
    scope_catalog: &ScopeCatalog,
    when_satisfied: bool,
) -> String {
    let mut why = format!(
        "{} catalog {} document, scope={}",
        scope_catalog.source_scope,
        scope_catalog.file_path.display(),
        entry.scope
    );
    if entry.when_raw != "always" {
        why.push_str(&format!(
            " when=\"{}\" ({})",
            entry.when_raw,
            if when_satisfied { "matched" } else { "skipped" }
        ));
    }
    if let Some(notes) = entry
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|notes| !notes.is_empty())
    {
        why.push_str(&format!(" notes={notes}"));
    }
    why
}

pub fn resolve_validation_contract(
    intent: &Context,
    roots: &ResolvedRoots,
    catalog: &LoadedCatalog,
) -> ValidationContract {
    let mut commands: Vec<String> = Vec::new();
    let mut marker: Option<String> = None;
    let mut description: Option<String> = None;
    let mut declared = false;

    let home_project_scope_applies = home_project_scope_applies(roots);

    for scope_catalog in catalog.in_load_order() {
        // A validation entry carries no scope and is a repo-local contract: the
        // commands it names (e.g. `bash scripts/ci/all.sh`) only make sense in
        // the repository that declares them. A docs-home catalog's validation
        // therefore applies only when the docs-home and the project are the
        // same repository, so it never leaks into an unrelated project.
        if scope_catalog.source_scope == Scope::Home && !home_project_scope_applies {
            continue;
        }
        for validation in &scope_catalog.validations {
            if validation.context != *intent {
                continue;
            }
            declared = true;
            for command in &validation.commands {
                if !commands.contains(command) {
                    commands.push(command.clone());
                }
            }
            if validation.marker.is_some() {
                marker = validation.marker.clone();
            }
            if validation.description.is_some() {
                description = validation.description.clone();
            }
        }
    }

    ValidationContract {
        context: intent.clone(),
        declared,
        commands,
        marker,
        description,
    }
}

/// The distinct intents declared anywhere in the catalog, sorted.
pub fn available_intents(catalog: &LoadedCatalog) -> Vec<String> {
    let mut intents: Vec<String> = Vec::new();
    for scope_catalog in catalog.in_load_order() {
        for entry in &scope_catalog.documents {
            let name = entry.context.as_str().to_string();
            if !intents.contains(&name) {
                intents.push(name);
            }
        }
        for validation in &scope_catalog.validations {
            let name = validation.context.as_str().to_string();
            if !intents.contains(&name) {
                intents.push(name);
            }
        }
    }
    intents.sort();
    intents
}

/// All validation contracts declared in the catalog, one per intent.
pub fn all_validation_contracts(
    roots: &ResolvedRoots,
    catalog: &LoadedCatalog,
) -> Vec<ValidationContract> {
    let mut seen: Vec<String> = Vec::new();
    let mut contracts = Vec::new();
    for scope_catalog in catalog.in_load_order() {
        for validation in &scope_catalog.validations {
            let name = validation.context.as_str().to_string();
            if seen.contains(&name) {
                continue;
            }
            seen.push(name);
            contracts.push(resolve_validation_contract(
                &validation.context,
                roots,
                catalog,
            ));
        }
    }
    contracts
}
