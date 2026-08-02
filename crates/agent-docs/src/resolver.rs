//! Catalog-driven resolution.
//!
//! There are no hardcoded builtins: every required document and validation
//! contract comes from the loaded catalog (`AGENT_DOCS.toml` at the docs-home
//! and the project). Resolution evaluates each entry's `when` predicate,
//! validates content, de-duplicates by resolved path, and resolves the
//! per-intent validation contract.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::load_catalog_from_roots;
use crate::content;
use crate::env::ResolvedRoots;
use crate::integration::EffectiveCatalog;
use crate::model::{
    AuditTarget, ConfigLoadError, Context, DocumentEntry, DocumentSource, DocumentStatus,
    DocumentValidation, FallbackMode, LoadedCatalog, Phase, PreflightReport, Product,
    ResolveSummary, ResolvedDocument, Scope, ScopeCatalog, ValidationContract,
};
use crate::paths::normalize_path;
use crate::predicate;

/// Resolve the document set and validation contract for a single intent.
pub fn resolve_intent(
    intent: &Context,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
) -> Result<PreflightReport, ConfigLoadError> {
    resolve_intent_for_product(intent, roots, None, strict, fallback_mode, emit_content)
}

pub fn resolve_intent_for_product(
    intent: &Context,
    roots: &ResolvedRoots,
    product: Option<Product>,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
) -> Result<PreflightReport, ConfigLoadError> {
    let catalog = load_catalog_from_roots(roots)?;
    Ok(resolve_intent_with_catalog_for_product(
        intent,
        roots,
        product,
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
    resolve_intent_with_catalog_for_product(
        intent,
        roots,
        None,
        strict,
        fallback_mode,
        emit_content,
        catalog,
    )
}

struct CatalogPolicy<'a> {
    catalog: &'a LoadedCatalog,
    private_allowed_roots: &'a [PathBuf],
}

struct PrivateReadBudget {
    remaining_documents: usize,
    remaining_bytes: usize,
}

impl PrivateReadBudget {
    fn new() -> Self {
        Self {
            remaining_documents: crate::integration::MAX_PRIVATE_DOCUMENTS,
            remaining_bytes: crate::integration::MAX_PRIVATE_AGGREGATE_BYTES,
        }
    }
}

pub fn resolve_intent_with_catalog_for_product(
    intent: &Context,
    roots: &ResolvedRoots,
    product: Option<Product>,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
    catalog: &LoadedCatalog,
) -> PreflightReport {
    resolve_intent_with_catalog_for_scope(
        intent,
        roots,
        product,
        None,
        strict,
        fallback_mode,
        emit_content,
        catalog,
    )
}

/// Resolve an intent scoped to an optional product AND an optional phase.
///
/// With `phase = Some(p)`, documents are filtered to those whose declared
/// phases include `p` plus every no-phase document (which applies to all
/// phases). `phase = None` applies no phase filter.
#[allow(clippy::too_many_arguments)]
pub fn resolve_intent_with_catalog_for_scope(
    intent: &Context,
    roots: &ResolvedRoots,
    product: Option<Product>,
    phase: Option<Phase>,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
    catalog: &LoadedCatalog,
) -> PreflightReport {
    resolve_intent_with_catalog_for_product_policy(
        intent,
        roots,
        product,
        phase,
        strict,
        fallback_mode,
        emit_content,
        CatalogPolicy {
            catalog,
            private_allowed_roots: &[],
        },
    )
}

pub(crate) fn resolve_intent_with_effective_catalog_for_product(
    intent: &Context,
    roots: &ResolvedRoots,
    product: Option<Product>,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
    effective: &EffectiveCatalog,
) -> PreflightReport {
    resolve_intent_with_effective_catalog_for_scope(
        intent,
        roots,
        product,
        None,
        strict,
        fallback_mode,
        emit_content,
        effective,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_intent_with_effective_catalog_for_scope(
    intent: &Context,
    roots: &ResolvedRoots,
    product: Option<Product>,
    phase: Option<Phase>,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
    effective: &EffectiveCatalog,
) -> PreflightReport {
    resolve_intent_with_catalog_for_product_policy(
        intent,
        roots,
        product,
        phase,
        strict,
        fallback_mode,
        emit_content,
        CatalogPolicy {
            catalog: &effective.catalog,
            private_allowed_roots: &effective.private_allowed_roots,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_intent_with_catalog_for_product_policy(
    intent: &Context,
    roots: &ResolvedRoots,
    product: Option<Product>,
    phase: Option<Phase>,
    strict: bool,
    fallback_mode: FallbackMode,
    emit_content: bool,
    policy: CatalogPolicy<'_>,
) -> PreflightReport {
    let documents = resolve_documents(
        roots,
        product,
        phase.as_ref(),
        fallback_mode,
        emit_content,
        policy.catalog,
        policy.private_allowed_roots,
        &mut |entry| entry.context == *intent,
    );
    let validation =
        resolve_validation_contract_for_product(intent, roots, product, policy.catalog);
    let summary = ResolveSummary::from_documents(&documents);

    PreflightReport {
        schema_version: PreflightReport::SCHEMA_VERSION,
        intent: intent.clone(),
        product,
        phase,
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
    resolve_documents_for_target_for_product(roots, target, None, fallback_mode, catalog)
}

pub fn resolve_documents_for_target_for_product(
    roots: &ResolvedRoots,
    target: AuditTarget,
    product: Option<Product>,
    fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
) -> Vec<ResolvedDocument> {
    resolve_documents(
        roots,
        product,
        None,
        fallback_mode,
        false,
        catalog,
        &[],
        &mut |entry| target.includes_scope(entry.scope),
    )
}

/// Resolve every declared document (for `list`).
pub fn resolve_all_documents(
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
) -> Vec<ResolvedDocument> {
    resolve_all_documents_for_product(roots, None, fallback_mode, catalog)
}

pub fn resolve_all_documents_for_product(
    roots: &ResolvedRoots,
    product: Option<Product>,
    fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
) -> Vec<ResolvedDocument> {
    resolve_all_documents_for_product_policy(roots, product, fallback_mode, catalog, &[])
}

pub(crate) fn resolve_all_documents_for_product_policy(
    roots: &ResolvedRoots,
    product: Option<Product>,
    fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
    private_allowed_roots: &[PathBuf],
) -> Vec<ResolvedDocument> {
    resolve_documents(
        roots,
        product,
        None,
        fallback_mode,
        false,
        catalog,
        private_allowed_roots,
        &mut |_| true,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_documents(
    roots: &ResolvedRoots,
    product: Option<Product>,
    phase: Option<&Phase>,
    fallback_mode: FallbackMode,
    emit_content: bool,
    catalog: &LoadedCatalog,
    private_allowed_roots: &[PathBuf],
    accept: &mut dyn FnMut(&DocumentEntry) -> bool,
) -> Vec<ResolvedDocument> {
    let mut documents: Vec<ResolvedDocument> = Vec::new();
    let mut index_by_path: HashMap<PathBuf, usize> = HashMap::new();
    let home_project_scope_applies = home_project_scope_applies(roots);
    let mut private_budget = PrivateReadBudget::new();

    for scope_catalog in catalog.in_load_order() {
        for entry in &scope_catalog.documents {
            if !accept(entry) {
                continue;
            }
            if !matches_product(&entry.products, product) {
                continue;
            }
            if !matches_phase(&entry.phases, phase) {
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
            let private_project_catalog =
                !private_allowed_roots.is_empty() && scope_catalog.source_scope == Scope::Project;
            let resolved = resolve_entry(
                entry,
                scope_catalog,
                roots,
                fallback_mode,
                emit_content,
                if private_project_catalog {
                    Some((private_allowed_roots, &mut private_budget))
                } else {
                    None
                },
            );
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
    private_policy: Option<(&[PathBuf], &mut PrivateReadBudget)>,
) -> ResolvedDocument {
    let path = resolve_entry_path(entry, roots, fallback_mode);
    let when_satisfied = predicate::evaluate(&entry.when, &roots.project_path);
    let required = entry.required && when_satisfied;
    let private_project_catalog = private_policy.is_some();

    let raw = read_entry_content(&path, private_policy);
    let status = if raw.is_some() {
        DocumentStatus::Present
    } else {
        DocumentStatus::Missing
    };
    let validation = raw
        .as_deref()
        .map(|raw| content::validate_content(raw, entry))
        .unwrap_or_else(DocumentValidation::missing);
    let content = if emit_content { raw } else { None };

    ResolvedDocument {
        context: entry.context.clone(),
        scope: entry.scope,
        path,
        products: entry.products.clone(),
        phases: entry.phases.clone(),
        declared_required: entry.required,
        required,
        when: entry.when_raw.clone(),
        when_satisfied,
        status,
        validation,
        source: DocumentSource::from_scope(scope_catalog.source_scope),
        why: describe_why(
            entry,
            scope_catalog,
            when_satisfied,
            private_project_catalog,
        ),
        content,
    }
}

fn read_entry_content(
    path: &Path,
    private_policy: Option<(&[PathBuf], &mut PrivateReadBudget)>,
) -> Option<String> {
    let Some((allowed_roots, budget)) = private_policy else {
        return fs::read_to_string(path).ok();
    };
    read_private_project_document(path, allowed_roots, budget).ok()
}

fn read_private_project_document(
    path: &Path,
    allowed_roots: &[PathBuf],
    budget: &mut PrivateReadBudget,
) -> std::io::Result<String> {
    if budget.remaining_documents == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private project document count limit exceeded",
        ));
    }
    budget.remaining_documents -= 1;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private project document must be a regular file and not a symlink",
        ));
    }

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private project document must be a regular file",
        ));
    }

    if metadata.len() > crate::integration::MAX_PRIVATE_DOCUMENT_BYTES as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private project document exceeds the per-file size limit",
        ));
    }

    let canonical_path = fs::canonicalize(path)?;
    if !allowed_roots
        .iter()
        .any(|root| canonical_path.starts_with(root))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private project document resolves outside the target worktree",
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let path_metadata = fs::metadata(&canonical_path)?;
        if opened_metadata.dev() != path_metadata.dev()
            || opened_metadata.ino() != path_metadata.ino()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "private project document changed while it was opened",
            ));
        }
    }

    let per_file_limit = crate::integration::MAX_PRIVATE_DOCUMENT_BYTES;
    let read_limit = per_file_limit.min(budget.remaining_bytes);
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take((read_limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > read_limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private project document exceeds the aggregate size limit",
        ));
    }
    budget.remaining_bytes -= bytes.len();
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private project document is not valid UTF-8",
        )
    })
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
        && let Some(primary) = roots.primary_worktree_fallback()
    {
        let fallback = normalize_path(&primary.path.join(&entry.path));
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
/// They apply only when the canonical roots are equal or both roots have a
/// positively resolved, matching Git common-dir identity.
fn home_project_scope_applies(roots: &ResolvedRoots) -> bool {
    roots.docs_home_matches_project()
}

fn describe_why(
    entry: &DocumentEntry,
    scope_catalog: &ScopeCatalog,
    when_satisfied: bool,
    private_project_catalog: bool,
) -> String {
    let catalog_source = if private_project_catalog {
        "user"
    } else {
        DocumentSource::from_scope(scope_catalog.source_scope).as_str()
    };
    let mut why = format!(
        "{} catalog {} document, scope={}",
        catalog_source,
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
    resolve_validation_contract_for_product(intent, roots, None, catalog)
}

pub fn resolve_validation_contract_for_product(
    intent: &Context,
    roots: &ResolvedRoots,
    product: Option<Product>,
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
            if !matches_product(&validation.products, product) {
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

fn matches_product(products: &[Product], requested: Option<Product>) -> bool {
    products.is_empty() || requested.is_none_or(|product| products.contains(&product))
}

/// A document matches a phase filter when it declares no phases (applies to all
/// phases) or when the requested phase is one of its declared phases. A `None`
/// request (no `--phase`) matches every document.
fn matches_phase(phases: &[Phase], requested: Option<&Phase>) -> bool {
    phases.is_empty() || requested.is_none_or(|phase| phases.contains(phase))
}

/// The distinct intents declared anywhere in the catalog, sorted.
pub fn available_intents(catalog: &LoadedCatalog) -> Vec<String> {
    available_intents_for_product(None, catalog)
}

pub fn available_intents_for_product(
    product: Option<Product>,
    catalog: &LoadedCatalog,
) -> Vec<String> {
    let mut intents: Vec<String> = Vec::new();
    for scope_catalog in catalog.in_load_order() {
        for entry in &scope_catalog.documents {
            if !matches_product(&entry.products, product) {
                continue;
            }
            let name = entry.context.as_str().to_string();
            if !intents.contains(&name) {
                intents.push(name);
            }
        }
        for validation in &scope_catalog.validations {
            if !matches_product(&validation.products, product) {
                continue;
            }
            let name = validation.context.as_str().to_string();
            if !intents.contains(&name) {
                intents.push(name);
            }
        }
    }
    intents.sort();
    intents
}

pub fn available_intents_for_product_in_roots(
    roots: &ResolvedRoots,
    product: Option<Product>,
    catalog: &LoadedCatalog,
) -> Vec<String> {
    let mut intents = Vec::new();
    let home_project_scope_applies = home_project_scope_applies(roots);
    for scope_catalog in catalog.in_load_order() {
        for entry in &scope_catalog.documents {
            if !matches_product(&entry.products, product)
                || (scope_catalog.source_scope == Scope::Home
                    && entry.scope == Scope::Project
                    && !home_project_scope_applies)
            {
                continue;
            }
            push_unique_intent(&mut intents, entry.context.as_str());
        }
        if scope_catalog.source_scope == Scope::Home && !home_project_scope_applies {
            continue;
        }
        for validation in &scope_catalog.validations {
            if matches_product(&validation.products, product) {
                push_unique_intent(&mut intents, validation.context.as_str());
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
    all_validation_contracts_for_product(roots, None, catalog)
}

pub fn all_validation_contracts_for_product(
    roots: &ResolvedRoots,
    product: Option<Product>,
    catalog: &LoadedCatalog,
) -> Vec<ValidationContract> {
    let mut seen: Vec<String> = Vec::new();
    let mut contracts = Vec::new();
    let home_project_scope_applies = home_project_scope_applies(roots);
    for scope_catalog in catalog.in_load_order() {
        if scope_catalog.source_scope == Scope::Home && !home_project_scope_applies {
            continue;
        }
        for validation in &scope_catalog.validations {
            if !matches_product(&validation.products, product) {
                continue;
            }
            let name = validation.context.as_str().to_string();
            if seen.contains(&name) {
                continue;
            }
            seen.push(name);
            contracts.push(resolve_validation_contract_for_product(
                &validation.context,
                roots,
                product,
                catalog,
            ));
        }
    }
    contracts
}

/// The distinct intents that apply to the current project after scope
/// filtering, sorted.
pub fn declared_intents(
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
) -> Vec<String> {
    declared_intents_for_product(roots, None, fallback_mode, catalog)
}

pub fn declared_intents_for_product(
    roots: &ResolvedRoots,
    product: Option<Product>,
    _fallback_mode: FallbackMode,
    catalog: &LoadedCatalog,
) -> Vec<String> {
    let mut intents: Vec<String> = Vec::new();
    let home_project_scope_applies = home_project_scope_applies(roots);

    for scope_catalog in catalog.in_load_order() {
        for document in &scope_catalog.documents {
            if !matches_product(&document.products, product) {
                continue;
            }
            if scope_catalog.source_scope == Scope::Home
                && document.scope == Scope::Project
                && !home_project_scope_applies
            {
                continue;
            }
            push_unique_intent(&mut intents, document.context.as_str());
        }
        if scope_catalog.source_scope == Scope::Home && !home_project_scope_applies {
            continue;
        }
        for validation in &scope_catalog.validations {
            if matches_product(&validation.products, product) {
                push_unique_intent(&mut intents, validation.context.as_str());
            }
        }
    }
    intents.sort();
    intents
}

fn push_unique_intent(intents: &mut Vec<String>, name: &str) {
    if !intents.iter().any(|intent| intent == name) {
        intents.push(name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        roots: ResolvedRoots,
        project: PathBuf,
    }

    /// A docs-home and a separate project root, each with its own catalog.
    fn fixture(home_catalog: &str, project_catalog: &str) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("docs-home");
        let project = tmp.path().join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&project).unwrap();
        if !home_catalog.is_empty() {
            fs::write(home.join("AGENT_DOCS.toml"), home_catalog).unwrap();
        }
        if !project_catalog.is_empty() {
            fs::write(project.join("AGENT_DOCS.toml"), project_catalog).unwrap();
        }
        Fixture {
            roots: ResolvedRoots::for_paths(home, project.clone()),
            project,
            _tmp: tmp,
        }
    }

    fn intent(name: &str) -> Context {
        Context::parse(name).expect("intent")
    }

    const PROJECT_CATALOG: &str = r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DEVELOPMENT.md"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "OPTIONAL.md"

[[validation]]
context = "project-dev"
commands = ["cargo test"]
"#;

    #[test]
    fn a_present_required_document_is_reported_as_satisfied() {
        let fixture = fixture("", PROJECT_CATALOG);
        fs::write(fixture.project.join("DEVELOPMENT.md"), "# dev\n").unwrap();
        fs::write(fixture.project.join("OPTIONAL.md"), "# optional\n").unwrap();

        let report = resolve_intent(
            &intent("project-dev"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            false,
        )
        .expect("resolve");

        assert_eq!(report.schema_version, PreflightReport::SCHEMA_VERSION);
        assert_eq!(report.intent.as_str(), "project-dev");
        assert_eq!(report.documents.len(), 2);
        assert_eq!(report.summary.required_total, 1);
        assert_eq!(report.summary.satisfied_required, 1);
        assert_eq!(report.summary.missing_required, 0);
        assert!(!report.has_unsatisfied_required());
        assert_eq!(report.validation.commands, vec!["cargo test"]);
        assert!(
            report.documents.iter().all(|doc| doc.content.is_none()),
            "content is emitted only on request"
        );
    }

    #[test]
    fn a_missing_required_document_leaves_the_intent_unsatisfied() {
        let fixture = fixture("", PROJECT_CATALOG);

        let report = resolve_intent(
            &intent("project-dev"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            false,
        )
        .expect("resolve");

        let required = report
            .documents
            .iter()
            .find(|doc| doc.required)
            .expect("required document");
        assert_eq!(required.status, DocumentStatus::Missing);
        assert_eq!(report.summary.missing_required, 1);
        assert!(report.has_unsatisfied_required());
    }

    #[test]
    fn content_emission_is_opt_in_and_reads_the_resolved_file() {
        let fixture = fixture("", PROJECT_CATALOG);
        fs::write(fixture.project.join("DEVELOPMENT.md"), "# dev\nbody\n").unwrap();

        let report = resolve_intent(
            &intent("project-dev"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            true,
        )
        .expect("resolve");

        let required = report
            .documents
            .iter()
            .find(|doc| doc.required)
            .expect("required document");
        assert_eq!(required.content.as_deref(), Some("# dev\nbody\n"));
        // A document that does not exist has no content to emit.
        let optional = report
            .documents
            .iter()
            .find(|doc| !doc.required)
            .expect("optional document");
        assert_eq!(optional.status, DocumentStatus::Missing);
        assert_eq!(optional.content, None);
    }

    #[test]
    fn an_unknown_intent_resolves_to_an_empty_document_set() {
        let fixture = fixture("", PROJECT_CATALOG);

        let report = resolve_intent(
            &intent("no-such-intent"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            false,
        )
        .expect("resolve");

        assert!(report.documents.is_empty());
        assert_eq!(report.summary.required_total, 0);
        assert!(!report.has_unsatisfied_required());
        assert!(report.validation.commands.is_empty());
    }

    #[test]
    fn a_product_filter_selects_only_matching_documents() {
        let catalog = r#"
[[document]]
context = "project-dev"
scope = "project"
path = "CLAUDE.md"
product = "claude"

[[document]]
context = "project-dev"
scope = "project"
path = "CODEX.md"
product = "codex"

[[document]]
context = "project-dev"
scope = "project"
path = "SHARED.md"
"#;
        let fixture = fixture("", catalog);

        let claude = resolve_intent_for_product(
            &intent("project-dev"),
            &fixture.roots,
            Some(Product::Claude),
            false,
            FallbackMode::Auto,
            false,
        )
        .expect("resolve");
        let paths = |report: &PreflightReport| {
            report
                .documents
                .iter()
                .map(|doc| doc.path.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        // A product-neutral document applies to every product.
        assert_eq!(paths(&claude), vec!["CLAUDE.md", "SHARED.md"]);
        assert_eq!(claude.product, Some(Product::Claude));

        let codex = resolve_intent_for_product(
            &intent("project-dev"),
            &fixture.roots,
            Some(Product::Codex),
            false,
            FallbackMode::Auto,
            false,
        )
        .expect("resolve");
        assert_eq!(paths(&codex), vec!["CODEX.md", "SHARED.md"]);

        // With no product filter, every declared document is resolved.
        let all = resolve_intent(
            &intent("project-dev"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            false,
        )
        .expect("resolve");
        assert_eq!(all.documents.len(), 3);
        assert_eq!(all.product, None);
    }

    #[test]
    fn a_pre_loaded_catalog_resolves_without_touching_the_filesystem_catalog() {
        let fixture = fixture("", PROJECT_CATALOG);
        fs::write(fixture.project.join("DEVELOPMENT.md"), "# dev\n").unwrap();
        let catalog = load_catalog_from_roots(&fixture.roots).expect("catalog");

        // Deleting the on-disk catalog proves the pre-loaded one is used.
        fs::remove_file(fixture.project.join("AGENT_DOCS.toml")).unwrap();

        let report = resolve_intent_with_catalog(
            &intent("project-dev"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            false,
            &catalog,
        );

        assert_eq!(report.documents.len(), 2);
        assert_eq!(report.summary.satisfied_required, 1);
    }

    #[test]
    fn a_broken_catalog_surfaces_the_config_error_instead_of_an_empty_report() {
        let fixture = fixture("", "[[document]\n");

        let err = resolve_intent(
            &intent("project-dev"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            false,
        )
        .expect_err("broken catalog");

        assert!(err.file_path.ends_with("AGENT_DOCS.toml"), "{err:?}");
    }

    #[test]
    fn a_home_catalog_contributes_its_own_scoped_documents() {
        let home_catalog = r#"
[[document]]
context = "project-dev"
scope = "home"
path = "HOME_POLICY.md"
required = true
"#;
        let fixture = fixture(home_catalog, PROJECT_CATALOG);

        let report = resolve_intent(
            &intent("project-dev"),
            &fixture.roots,
            false,
            FallbackMode::Auto,
            false,
        )
        .expect("resolve");

        assert_eq!(
            report.documents.len(),
            3,
            "home and project catalogs both contribute: {:?}",
            report.documents.iter().map(|d| &d.path).collect::<Vec<_>>()
        );
        assert!(
            report
                .documents
                .iter()
                .any(|doc| doc.scope == Scope::Home && doc.required),
            "the home-scoped requirement must survive resolution"
        );
    }

    #[test]
    fn strict_mode_still_returns_a_report_for_a_satisfied_intent() {
        let fixture = fixture("", PROJECT_CATALOG);
        fs::write(fixture.project.join("DEVELOPMENT.md"), "# dev\n").unwrap();

        let report = resolve_intent(
            &intent("project-dev"),
            &fixture.roots,
            true,
            FallbackMode::LocalOnly,
            false,
        )
        .expect("resolve");

        assert!(report.strict);
        assert_eq!(report.docs_home, fixture.roots.docs_home);
        assert_eq!(report.project_path, fixture.roots.project_path);
        assert!(!report.is_linked_worktree);
    }
}
