use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::load_configs_from_roots;
use crate::env::ResolvedRoots;
use crate::model::{
    BaselineCheckItem, BaselineCheckReport, BaselineTarget, ConfigDocumentEntry, ConfigLoadError,
    ConfigScopeFile, Context, DocumentSource, DocumentStatus, FallbackMode, Scope,
};
use crate::paths::normalize_path;
use crate::scope_rules;

pub fn check_builtin_baseline(
    target: BaselineTarget,
    roots: &ResolvedRoots,
    strict: bool,
) -> Result<BaselineCheckReport, ConfigLoadError> {
    check_builtin_baseline_with_mode(target, roots, strict, FallbackMode::Auto)
}

pub fn check_builtin_baseline_with_mode(
    target: BaselineTarget,
    roots: &ResolvedRoots,
    strict: bool,
    fallback_mode: FallbackMode,
) -> Result<BaselineCheckReport, ConfigLoadError> {
    let mut items = builtin_items_for_target(target, roots, fallback_mode);
    let builtin_keys: HashSet<BaselineKey> = items.iter().map(BaselineKey::from_item).collect();

    let configs = load_configs_from_roots(roots)?;
    let configs_in_load_order = configs.in_load_order();
    let home_project_scope_applies = scope_rules::home_catalog_project_scope_applies(roots);

    // A project may opt out of a builtin requirement by declaring a
    // `required = false` entry for the builtin's own key. Downgrade the
    // matching builtin item in place so the opt-out stays visible (and so it no
    // longer counts toward `missing_required`).
    let opt_outs = collect_builtin_opt_outs(
        target,
        roots,
        fallback_mode,
        &configs_in_load_order,
        home_project_scope_applies,
        &builtin_keys,
    );
    if !opt_outs.is_empty() {
        for item in items.iter_mut() {
            if let Some(opt_out) = opt_outs.get(&BaselineKey::from_item(item)) {
                *item = opt_out.clone();
            }
        }
    }

    items.extend(required_extension_items(
        target,
        roots,
        fallback_mode,
        &configs_in_load_order,
        home_project_scope_applies,
        &builtin_keys,
    ));

    let suggested_actions = suggested_actions(&items);

    Ok(BaselineCheckReport::from_items(
        target,
        strict,
        roots.docs_home.clone(),
        roots.project_path.clone(),
        items,
        suggested_actions,
    ))
}

fn builtin_items_for_target(
    target: BaselineTarget,
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
) -> Vec<BaselineCheckItem> {
    let mut items = Vec::new();
    match target {
        BaselineTarget::Home => items.extend(home_items(roots)),
        BaselineTarget::Project => items.extend(project_items(roots, fallback_mode)),
        BaselineTarget::All => {
            items.extend(home_items(roots));
            items.extend(project_items(roots, fallback_mode));
        }
    }
    items
}

fn home_items(roots: &ResolvedRoots) -> Vec<BaselineCheckItem> {
    vec![
        startup_policy_item(Scope::Home, &roots.docs_home, None),
        required_item(
            Scope::Home,
            Context::SkillDev,
            "skill-dev",
            &roots.docs_home,
            "DEVELOPMENT.md",
            "skill development guidance from AGENT_DOCS_HOME/DEVELOPMENT.md",
            DocumentSource::Builtin,
        ),
        required_item(
            Scope::Home,
            Context::TaskTools,
            "task-tools",
            &roots.docs_home,
            "core/policies/cli-tools.md",
            "tool-selection guidance from AGENT_DOCS_HOME/core/policies/cli-tools.md",
            DocumentSource::Builtin,
        ),
    ]
}

fn project_items(roots: &ResolvedRoots, fallback_mode: FallbackMode) -> Vec<BaselineCheckItem> {
    let project_fallback_root = project_fallback_root(roots, fallback_mode);
    vec![
        startup_policy_item(Scope::Project, &roots.project_path, project_fallback_root),
        project_dev_item(&roots.project_path, project_fallback_root),
    ]
}

fn startup_policy_item(
    scope: Scope,
    root: &Path,
    fallback_root: Option<&Path>,
) -> BaselineCheckItem {
    let override_path = normalize_path(&root.join("AGENTS.override.md"));
    if override_path.exists() {
        return BaselineCheckItem {
            scope,
            context: Context::Startup,
            label: "startup policy".to_string(),
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

    let local_agents = normalize_path(&root.join("AGENTS.md"));
    if local_agents.exists() {
        return BaselineCheckItem {
            scope,
            context: Context::Startup,
            label: "startup policy".to_string(),
            path: local_agents,
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
            return BaselineCheckItem {
                scope,
                context: Context::Startup,
                label: "startup policy".to_string(),
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
            return BaselineCheckItem {
                scope,
                context: Context::Startup,
                label: "startup policy".to_string(),
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

    BaselineCheckItem {
        scope,
        context: Context::Startup,
        label: "startup policy".to_string(),
        path: local_agents,
        required: true,
        status: DocumentStatus::Missing,
        source: DocumentSource::BuiltinFallback,
        why: format!(
            "startup {} policy (AGENTS.override.md missing, fallback AGENTS.md)",
            scope
        ),
    }
}

fn required_item(
    scope: Scope,
    context: Context,
    label: &str,
    root: &Path,
    file_name: &str,
    why: &str,
    source: DocumentSource,
) -> BaselineCheckItem {
    let path = normalize_path(&root.join(file_name));
    let status = if path.exists() {
        DocumentStatus::Present
    } else {
        DocumentStatus::Missing
    };

    BaselineCheckItem {
        scope,
        context,
        label: label.to_string(),
        path,
        required: true,
        status,
        source,
        why: why.to_string(),
    }
}

fn project_dev_item(root: &Path, fallback_root: Option<&Path>) -> BaselineCheckItem {
    let local_path = normalize_path(&root.join("DEVELOPMENT.md"));
    if local_path.exists() {
        return BaselineCheckItem {
            scope: Scope::Project,
            context: Context::ProjectDev,
            label: "project-dev".to_string(),
            path: local_path,
            required: true,
            status: DocumentStatus::Present,
            source: DocumentSource::Builtin,
            why: "project development guidance from PROJECT_PATH/DEVELOPMENT.md".to_string(),
        };
    }

    if let Some(fallback_root) = fallback_root {
        let fallback_path = normalize_path(&fallback_root.join("DEVELOPMENT.md"));
        if fallback_path.exists() {
            return BaselineCheckItem {
                scope: Scope::Project,
                context: Context::ProjectDev,
                label: "project-dev".to_string(),
                path: fallback_path,
                required: true,
                status: DocumentStatus::Present,
                source: DocumentSource::Builtin,
                why: "project development guidance from PROJECT_PATH/DEVELOPMENT.md (fallback to primary worktree)".to_string(),
            };
        }
    }

    BaselineCheckItem {
        scope: Scope::Project,
        context: Context::ProjectDev,
        label: "project-dev".to_string(),
        path: local_path,
        required: true,
        status: DocumentStatus::Missing,
        source: DocumentSource::Builtin,
        why: "project development guidance from PROJECT_PATH/DEVELOPMENT.md".to_string(),
    }
}

fn required_extension_items(
    target: BaselineTarget,
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    configs_in_load_order: &[&ConfigScopeFile],
    home_project_scope_applies: bool,
    builtin_keys: &HashSet<BaselineKey>,
) -> Vec<BaselineCheckItem> {
    let mut extension_items = Vec::new();
    let mut extension_indices: HashMap<BaselineKey, usize> = HashMap::new();

    for config in configs_in_load_order {
        let merge_context = RequiredExtensionMergeContext {
            target,
            roots,
            fallback_mode,
            config,
            home_project_scope_applies,
            builtin_keys,
        };
        merge_required_extension_items(
            &merge_context,
            &mut extension_items,
            &mut extension_indices,
        );
    }

    extension_items
}

struct RequiredExtensionMergeContext<'a> {
    target: BaselineTarget,
    roots: &'a ResolvedRoots,
    fallback_mode: FallbackMode,
    config: &'a ConfigScopeFile,
    home_project_scope_applies: bool,
    builtin_keys: &'a HashSet<BaselineKey>,
}

fn merge_required_extension_items(
    merge_context: &RequiredExtensionMergeContext<'_>,
    extension_items: &mut Vec<BaselineCheckItem>,
    extension_indices: &mut HashMap<BaselineKey, usize>,
) {
    let config = merge_context.config;
    for (index, entry) in config.documents.iter().enumerate() {
        if !entry.required || !target_includes_scope(merge_context.target, entry.scope) {
            continue;
        }
        if !scope_rules::should_include_extension_entry(
            config.source_scope,
            entry.scope,
            merge_context.home_project_scope_applies,
        ) {
            continue;
        }

        let path = resolve_extension_path_with_project_fallback(
            entry,
            merge_context.roots,
            merge_context.fallback_mode,
        );
        let key = BaselineKey::new(entry.context, entry.scope, path.clone());
        if merge_context.builtin_keys.contains(&key) {
            continue;
        }

        let item = BaselineCheckItem {
            scope: entry.scope,
            context: entry.context,
            label: entry.context.as_str().to_string(),
            path: path.clone(),
            required: true,
            status: if path.exists() {
                DocumentStatus::Present
            } else {
                DocumentStatus::Missing
            },
            source: extension_source(config.source_scope),
            why: extension_why(config, index, entry),
        };

        if let Some(existing_index) = extension_indices.get(&key).copied() {
            extension_items[existing_index] = item;
        } else {
            let next_index = extension_items.len();
            extension_items.push(item);
            extension_indices.insert(key, next_index);
        }
    }
}

fn collect_builtin_opt_outs(
    target: BaselineTarget,
    roots: &ResolvedRoots,
    fallback_mode: FallbackMode,
    configs_in_load_order: &[&ConfigScopeFile],
    home_project_scope_applies: bool,
    builtin_keys: &HashSet<BaselineKey>,
) -> HashMap<BaselineKey, BaselineCheckItem> {
    let mut opt_outs: HashMap<BaselineKey, BaselineCheckItem> = HashMap::new();

    for config in configs_in_load_order {
        for (index, entry) in config.documents.iter().enumerate() {
            // Only an explicit `required = false` entry opts a builtin out, and
            // the foundational `startup` policy can never be opted out.
            if entry.required || entry.context == Context::Startup {
                continue;
            }
            if !target_includes_scope(target, entry.scope) {
                continue;
            }
            if !scope_rules::should_include_extension_entry(
                config.source_scope,
                entry.scope,
                home_project_scope_applies,
            ) {
                continue;
            }

            let path = resolve_extension_path_with_project_fallback(entry, roots, fallback_mode);
            let key = BaselineKey::new(entry.context, entry.scope, path.clone());
            if !builtin_keys.contains(&key) {
                continue;
            }

            opt_outs.insert(
                key,
                BaselineCheckItem {
                    scope: entry.scope,
                    context: entry.context,
                    label: entry.context.as_str().to_string(),
                    path: path.clone(),
                    required: false,
                    status: if path.exists() {
                        DocumentStatus::Present
                    } else {
                        DocumentStatus::Missing
                    },
                    source: DocumentSource::BuiltinOptOut,
                    why: opt_out_why(config, index, entry),
                },
            );
        }
    }

    opt_outs
}

fn opt_out_why(config: &ConfigScopeFile, index: usize, entry: &ConfigDocumentEntry) -> String {
    match entry
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|notes| !notes.is_empty())
    {
        Some(notes) => format!(
            "builtin requirement opted out by {} document[{index}] (required=false) notes={notes}",
            config.file_path.display()
        ),
        None => format!(
            "builtin requirement opted out by {} document[{index}] (required=false)",
            config.file_path.display()
        ),
    }
}

fn target_includes_scope(target: BaselineTarget, scope: Scope) -> bool {
    match target {
        BaselineTarget::Home => scope == Scope::Home || scope == Scope::Global,
        BaselineTarget::Project => scope == Scope::Project,
        BaselineTarget::All => true,
    }
}

fn extension_source(source_scope: Scope) -> DocumentSource {
    match source_scope {
        Scope::Home | Scope::Global => DocumentSource::ExtensionHome,
        Scope::Project => DocumentSource::ExtensionProject,
    }
}

fn resolve_extension_path(entry: &ConfigDocumentEntry, roots: &ResolvedRoots) -> PathBuf {
    let root = scope_rules::root_for_entry_scope(entry.scope, roots);
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
struct BaselineKey {
    context: &'static str,
    scope: &'static str,
    path: PathBuf,
}

impl BaselineKey {
    fn new(context: Context, scope: Scope, path: PathBuf) -> Self {
        Self {
            context: context.as_str(),
            scope: scope.as_str(),
            path,
        }
    }

    fn from_item(item: &BaselineCheckItem) -> Self {
        Self::new(item.context, item.scope, item.path.clone())
    }
}

fn suggested_actions(items: &[BaselineCheckItem]) -> Vec<String> {
    let has_home_missing_required = items.iter().any(|item| {
        (item.scope == Scope::Home || item.scope == Scope::Global)
            && item.required
            && matches!(item.status, DocumentStatus::Missing)
    });
    let has_project_missing_required = items.iter().any(|item| {
        item.scope == Scope::Project
            && item.required
            && matches!(item.status, DocumentStatus::Missing)
    });

    let mut actions = Vec::new();
    if has_home_missing_required {
        actions.push("agent-docs scaffold-baseline --missing-only --target home".to_string());
    }
    if has_project_missing_required {
        actions.push("agent-docs scaffold-baseline --missing-only --target project".to_string());
    }

    actions
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
