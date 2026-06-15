//! Project-slug normalization shared across the workspace.
//!
//! The agent-out `<owner__repo>` project slug is the canonical identity key for
//! a repo's evidence/output: `agent-out` derives it when allocating project
//! directories, and `nils-evidence` reuses the same normalization to match a
//! record's slug back to a local checkout. Keeping the rule here ensures both
//! sides agree on what "the same repo" means.

/// Derive the `<owner>__<repo>` project slug from an `owner/repo` path,
/// preserving only the last owner segment (so nested provider groups such as
/// `acme/platform/backend/svc` collapse to `backend__svc`). Each segment is
/// sanitized with [`sanitize_path_label`]. Returns `None` when no usable slug
/// can be formed.
pub fn project_slug_from_owner_repo(value: &str) -> Option<String> {
    let mut parts: Vec<&str> = value
        .trim()
        .trim_end_matches(".git")
        .split('/')
        .filter(|part| !part.trim().is_empty())
        .collect();

    if parts.len() >= 2 {
        let repo = parts.pop().expect("repo segment");
        let owner = parts.pop().expect("owner segment");
        let owner = sanitize_path_label(owner, "");
        let repo = sanitize_path_label(repo, "");
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        return Some(format!("{owner}__{repo}"));
    }

    let slug = sanitize_path_label(value, "");
    if slug.is_empty() { None } else { Some(slug) }
}

/// Derive the `<owner>__<repo>` project slug from a git remote URL by parsing
/// the URL and normalizing its `owner/repo` path.
pub fn project_slug_from_remote_url(remote: &str) -> Option<String> {
    let parsed = crate::git::parse_git_remote_url(remote)?;
    project_slug_from_owner_repo(&parsed.path)
}

/// Normalize a single path label: lowercase ASCII alphanumerics, collapse every
/// other run of characters to a single `-`, trim leading/trailing dashes, and
/// cap at 80 characters. Returns `fallback` when nothing survives.
pub fn sanitize_path_label(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }

    let trimmed = out.trim_matches('-');
    let mut sanitized: String = trimmed.chars().take(80).collect();
    sanitized = sanitized.trim_matches('-').to_string();

    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_repo_uses_double_underscore_and_sanitizes() {
        assert_eq!(
            project_slug_from_owner_repo("Sympoies/Nils CLI"),
            Some("sympoies__nils-cli".to_string())
        );
    }

    #[test]
    fn nested_groups_keep_only_last_owner_segment() {
        assert_eq!(
            project_slug_from_owner_repo("acme/platform/backend/svc"),
            Some("backend__svc".to_string())
        );
    }

    #[test]
    fn remote_url_forms_match() {
        assert_eq!(
            project_slug_from_remote_url("git@github.com:sympoies/nils-cli.git"),
            Some("sympoies__nils-cli".to_string())
        );
        assert_eq!(
            project_slug_from_remote_url("https://github.com/sympoies/nils-cli.git"),
            Some("sympoies__nils-cli".to_string())
        );
    }

    #[test]
    fn single_segment_and_empty() {
        assert_eq!(
            project_slug_from_owner_repo("solo"),
            Some("solo".to_string())
        );
        assert_eq!(project_slug_from_owner_repo("   "), None);
    }

    #[test]
    fn sanitize_collapses_and_trims() {
        assert_eq!(sanitize_path_label("  My.Repo!! ", "fb"), "my-repo");
        assert_eq!(sanitize_path_label("", "fb"), "fb");
        assert_eq!(sanitize_path_label("***", "untitled"), "untitled");
    }
}
