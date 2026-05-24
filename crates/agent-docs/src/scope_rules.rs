use std::fs;
use std::path::{Path, PathBuf};

use nils_common::git as shared_git;

use crate::env::ResolvedRoots;
use crate::model::Scope;
use crate::paths::{normalize_path, normalize_root_path};

pub fn should_include_extension_entry(
    source_scope: Scope,
    entry_scope: Scope,
    home_project_scope_applies: bool,
) -> bool {
    match source_scope {
        Scope::Home => match entry_scope {
            Scope::Project => home_project_scope_applies,
            Scope::Home | Scope::Global => true,
        },
        Scope::Project => true,
        Scope::Global => false,
    }
}

pub fn home_catalog_project_scope_applies(roots: &ResolvedRoots) -> bool {
    if let (Some(docs_home_repo), Some(project_repo)) = (
        git_common_dir(&roots.docs_home),
        git_common_dir(&roots.project_path),
    ) {
        return docs_home_repo == project_repo;
    }

    canonical_root(&roots.docs_home) == canonical_root(&roots.project_path)
}

pub fn root_for_entry_scope(scope: Scope, roots: &ResolvedRoots) -> &Path {
    match scope {
        Scope::Home | Scope::Global => &roots.docs_home,
        Scope::Project => &roots.project_path,
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
