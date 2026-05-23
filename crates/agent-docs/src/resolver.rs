use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::load_configs_from_roots;
use crate::env::ResolvedRoots;
use crate::model::{
    ConfigDocumentEntry, ConfigLoadError, ConfigScopeFile, Context, DocumentSource, DocumentStatus,
    FallbackMode, LoadedConfigs, ResolveReport, ResolveSummary, ResolvedDocument,
    SUPPORTED_CONTEXTS, Scope,
};
use crate::paths::normalize_path;

pub fn supported_contexts() -> &'static [Context] {
    &SUPPORTED_CONTEXTS
}

pub fn resolve(
    context: Context,
    roots: &ResolvedRoots,
    strict: bool,
) -> Result<ResolveReport, ConfigLoadError> {
    resolve_with_mode(context, roots, strict, FallbackMode::Auto)
}

pub fn resolve_with_mode(
    context: Context,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
) -> Result<ResolveReport, ConfigLoadError> {
    let configs = load_configs_from_roots(roots)?;
    Ok(resolve_with_configs_with_mode(
        context,
        roots,
        strict,
        fallback_mode,
        &configs,
    ))
}

pub fn resolve_builtin(context: Context, roots: &ResolvedRoots, strict: bool) -> ResolveReport {
    resolve_builtin_with_mode(context, roots, strict, FallbackMode::Auto)
}

pub fn resolve_builtin_with_mode(
    context: Context,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
) -> ResolveReport {
    match resolve_with_mode(context, roots, strict, fallback_mode) {
        Ok(report) => report,
        Err(_) => resolve_builtin_only_with_mode(context, roots, strict, fallback_mode),
    }
}

pub fn resolve_builtin_only(
    context: Context,
    roots: &ResolvedRoots,
    strict: bool,
) -> ResolveReport {
    resolve_builtin_only_with_mode(context, roots, strict, FallbackMode::Auto)
}

pub fn resolve_builtin_only_with_mode(
    context: Context,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
) -> ResolveReport {
    let project_fallback_root = project_fallback_root(roots, fallback_mode);
    let documents = match context {
        Context::Startup => resolve_startup(roots, fallback_mode),
        Context::SkillDev => vec![resolve_required_doc(
            Context::SkillDev,
            Scope::Home,
            &roots.docs_home,
            "DEVELOPMENT.md",
            "skill development guidance from AGENT_DOCS_HOME/DEVELOPMENT.md",
            DocumentSource::Builtin,
        )],
        Context::TaskTools => vec![resolve_required_doc(
            Context::TaskTools,
            Scope::Home,
            &roots.docs_home,
            "core/policies/cli-tools.md",
            "tool-selection guidance from AGENT_DOCS_HOME/core/policies/cli-tools.md",
            DocumentSource::Builtin,
        )],
        Context::ProjectDev => vec![resolve_required_doc_with_project_fallback(
            Context::ProjectDev,
            Scope::Project,
            &roots.project_path,
            "DEVELOPMENT.md",
            "project development guidance from PROJECT_PATH/DEVELOPMENT.md",
            DocumentSource::Builtin,
            project_fallback_root,
        )],
    };

    let summary = ResolveSummary::from_documents(&documents);

    ResolveReport {
        context,
        strict,
        docs_home: roots.docs_home.clone(),
        project_path: roots.project_path.clone(),
        is_linked_worktree: roots.is_linked_worktree,
        git_common_dir: roots.git_common_dir.clone(),
        primary_worktree_path: roots.primary_worktree_path.clone(),
        documents,
        summary,
    }
}

pub fn resolve_with_configs(
    context: Context,
    roots: &ResolvedRoots,
    strict: bool,
    configs: &LoadedConfigs,
) -> ResolveReport {
    resolve_with_configs_with_mode(context, roots, strict, FallbackMode::Auto, configs)
}

pub fn resolve_with_configs_with_mode(
    context: Context,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
    configs: &LoadedConfigs,
) -> ResolveReport {
    let mut documents =
        resolve_builtin_only_with_mode(context, roots, strict, fallback_mode).documents;
    let builtin_keys: HashSet<ResolveKey> =
        documents.iter().map(ResolveKey::from_document).collect();

    let mut extension_documents: Vec<ResolvedDocument> = Vec::new();
    let mut extension_indices: HashMap<ResolveKey, usize> = HashMap::new();

    for config in configs.in_load_order() {
        merge_extension_documents(
            context,
            roots,
            fallback_mode,
            config,
            &builtin_keys,
            &mut extension_documents,
            &mut extension_indices,
        );
    }

    documents.extend(extension_documents);
    let summary = ResolveSummary::from_documents(&documents);

    ResolveReport {
        context,
        strict,
        docs_home: roots.docs_home.clone(),
        project_path: roots.project_path.clone(),
        is_linked_worktree: roots.is_linked_worktree,
        git_common_dir: roots.git_common_dir.clone(),
        primary_worktree_path: roots.primary_worktree_path.clone(),
        documents,
        summary,
    }
}

fn merge_extension_documents(
    context: Context,
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    config: &ConfigScopeFile,
    builtin_keys: &HashSet<ResolveKey>,
    extension_documents: &mut Vec<ResolvedDocument>,
    extension_indices: &mut HashMap<ResolveKey, usize>,
) {
    for (index, entry) in config.documents.iter().enumerate() {
        if entry.context != context {
            continue;
        }

        let resolved_path =
            resolve_extension_path_with_project_fallback(entry, roots, fallback_mode);
        let key = ResolveKey::new(context, entry.scope, resolved_path.clone());
        if builtin_keys.contains(&key) {
            continue;
        }

        let document = ResolvedDocument {
            context,
            scope: entry.scope,
            path: resolved_path.clone(),
            required: entry.required,
            status: if resolved_path.exists() {
                DocumentStatus::Present
            } else {
                DocumentStatus::Missing
            },
            source: extension_source(config.source_scope),
            why: extension_why(config, index, entry),
        };

        if let Some(existing_index) = extension_indices.get(&key).copied() {
            extension_documents[existing_index] = document;
        } else {
            let next_index = extension_documents.len();
            extension_documents.push(document);
            extension_indices.insert(key, next_index);
        }
    }
}

fn extension_source(source_scope: Scope) -> DocumentSource {
    match source_scope {
        Scope::Home => DocumentSource::ExtensionHome,
        Scope::Project => DocumentSource::ExtensionProject,
    }
}

fn resolve_extension_path(entry: &ConfigDocumentEntry, roots: &ResolvedRoots) -> PathBuf {
    let root = match entry.scope {
        Scope::Home => &roots.docs_home,
        Scope::Project => &roots.project_path,
    };
    normalize_path(&root.join(&entry.path))
}

fn resolve_extension_path_with_project_fallback(
    entry: &ConfigDocumentEntry,
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
) -> PathBuf {
    let local_path = resolve_extension_path(entry, roots);
    if local_path.exists()
        || !should_use_project_fallback(entry.scope, entry.required, fallback_mode)
    {
        return local_path;
    }

    let Some(primary_root) = project_fallback_root(roots, fallback_mode) else {
        return local_path;
    };

    let fallback_path = normalize_path(&primary_root.join(&entry.path));
    if fallback_path.exists() {
        fallback_path
    } else {
        local_path
    }
}

fn extension_why(config: &ConfigScopeFile, index: usize, entry: &ConfigDocumentEntry) -> String {
    match entry
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|notes| !notes.is_empty())
    {
        Some(notes) => format!(
            "extension document from {} document[{index}] notes={notes}",
            config.file_path.display()
        ),
        None => format!(
            "extension document from {} document[{index}]",
            config.file_path.display()
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResolveKey {
    context: &'static str,
    scope: &'static str,
    path: PathBuf,
}

impl ResolveKey {
    fn new(context: Context, scope: Scope, path: PathBuf) -> Self {
        Self {
            context: context.as_str(),
            scope: scope.as_str(),
            path,
        }
    }

    fn from_document(document: &ResolvedDocument) -> Self {
        Self::new(document.context, document.scope, document.path.clone())
    }
}

fn resolve_startup(roots: &ResolvedRoots, fallback_mode: FallbackMode) -> Vec<ResolvedDocument> {
    vec![
        resolve_startup_scope(Scope::Home, &roots.docs_home, None),
        resolve_startup_scope(
            Scope::Project,
            &roots.project_path,
            project_fallback_root(roots, fallback_mode),
        ),
    ]
}

fn resolve_startup_scope(
    scope: Scope,
    root: &Path,
    fallback_root: Option<&Path>,
) -> ResolvedDocument {
    let override_path = normalize_path(&root.join("AGENTS.override.md"));
    if override_path.exists() {
        return ResolvedDocument {
            context: Context::Startup,
            scope,
            path: override_path,
            required: true,
            status: DocumentStatus::Present,
            source: DocumentSource::Builtin,
            why: format!(
                "startup {} policy (AGENTS.override.md preferred over AGENTS.md)",
                scope
            ),
        };
    }

    let local_agents_path = normalize_path(&root.join("AGENTS.md"));
    if local_agents_path.exists() {
        return ResolvedDocument {
            context: Context::Startup,
            scope,
            path: local_agents_path,
            required: true,
            status: DocumentStatus::Present,
            source: DocumentSource::BuiltinFallback,
            why: format!(
                "startup {} policy (AGENTS.override.md missing, fallback AGENTS.md)",
                scope
            ),
        };
    }

    if let Some(fallback_root) = fallback_root {
        let fallback_override = normalize_path(&fallback_root.join("AGENTS.override.md"));
        if fallback_override.exists() {
            return ResolvedDocument {
                context: Context::Startup,
                scope,
                path: fallback_override,
                required: true,
                status: DocumentStatus::Present,
                source: DocumentSource::Builtin,
                why: format!(
                    "startup {} policy (local missing, fallback to primary AGENTS.override.md)",
                    scope
                ),
            };
        }

        let fallback_agents = normalize_path(&fallback_root.join("AGENTS.md"));
        if fallback_agents.exists() {
            return ResolvedDocument {
                context: Context::Startup,
                scope,
                path: fallback_agents,
                required: true,
                status: DocumentStatus::Present,
                source: DocumentSource::BuiltinFallback,
                why: format!(
                    "startup {} policy (local missing, fallback to primary AGENTS.md)",
                    scope
                ),
            };
        }
    }

    resolve_required_doc(
        Context::Startup,
        scope,
        root,
        "AGENTS.md",
        &format!(
            "startup {} policy (AGENTS.override.md missing, fallback AGENTS.md)",
            scope
        ),
        DocumentSource::BuiltinFallback,
    )
}

fn resolve_required_doc_with_project_fallback(
    context: Context,
    scope: Scope,
    root: &Path,
    file_name: &str,
    why: &str,
    source: DocumentSource,
    fallback_root: Option<&Path>,
) -> ResolvedDocument {
    let local_path = normalize_path(&root.join(file_name));
    if local_path.exists() {
        return ResolvedDocument {
            context,
            scope,
            path: local_path,
            required: true,
            status: DocumentStatus::Present,
            source,
            why: why.to_string(),
        };
    }

    if scope == Scope::Project
        && let Some(fallback_root) = fallback_root
    {
        let fallback_path = normalize_path(&fallback_root.join(file_name));
        if fallback_path.exists() {
            return ResolvedDocument {
                context,
                scope,
                path: fallback_path,
                required: true,
                status: DocumentStatus::Present,
                source,
                why: format!("{why} (fallback to primary worktree)"),
            };
        }
    }

    ResolvedDocument {
        context,
        scope,
        path: local_path,
        required: true,
        status: DocumentStatus::Missing,
        source,
        why: why.to_string(),
    }
}

fn project_fallback_root(roots: &ResolvedRoots, fallback_mode: FallbackMode) -> Option<&Path> {
    if fallback_mode == FallbackMode::Auto && roots.is_linked_worktree {
        roots.primary_worktree_path.as_deref()
    } else {
        None
    }
}

fn should_use_project_fallback(scope: Scope, required: bool, fallback_mode: FallbackMode) -> bool {
    scope == Scope::Project && required && fallback_mode == FallbackMode::Auto
}

fn resolve_required_doc(
    context: Context,
    scope: Scope,
    root: &Path,
    file_name: &str,
    why: &str,
    source: DocumentSource,
) -> ResolvedDocument {
    let path = normalize_path(&root.join(file_name));
    let status = if path.exists() {
        DocumentStatus::Present
    } else {
        DocumentStatus::Missing
    };

    ResolvedDocument {
        context,
        scope,
        path,
        required: true,
        status,
        source,
        why: why.to_string(),
    }
}
