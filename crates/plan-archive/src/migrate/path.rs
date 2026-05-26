//! Pure path helpers for the migration pipeline.

use std::path::PathBuf;

/// Build the relative archive target path:
/// `plans/<host>/<org_or_group_path>/<repo>/<folder_name>/`.
///
/// Paths are joined as-is; the host preserves dots, the
/// org_or_group_path preserves nested GitLab group separators.
pub fn archive_target_path(
    host: &str,
    org_or_group_path: &str,
    repo: &str,
    folder_name: &str,
) -> PathBuf {
    let mut p = PathBuf::from("plans");
    p.push(host);
    for segment in org_or_group_path.split('/').filter(|s| !s.is_empty()) {
        p.push(segment);
    }
    p.push(repo);
    p.push(folder_name);
    p
}

/// Split a plan folder name into its `<YYYY-MM-DD>` prefix (if any)
/// and the slug remainder. Returns `(None, full_name)` for pre-v1
/// folders that have no date prefix.
pub fn parse_plan_folder(folder_name: &str) -> (Option<String>, String) {
    let mut iter = folder_name.splitn(2, '-');
    let year = iter.next();
    let rest = iter.next();
    if let (Some(y), Some(rest)) = (year, rest)
        && y.len() == 4
        && y.chars().all(|c| c.is_ascii_digit())
    {
        let mut parts = rest.splitn(2, '-');
        let month = parts.next();
        let rest2 = parts.next();
        if let (Some(m), Some(rest2)) = (month, rest2)
            && m.len() == 2
            && m.chars().all(|c| c.is_ascii_digit())
        {
            let mut parts = rest2.splitn(2, '-');
            let day = parts.next();
            let slug = parts.next();
            if let (Some(d), Some(slug)) = (day, slug)
                && d.len() == 2
                && d.chars().all(|c| c.is_ascii_digit())
            {
                return (Some(format!("{y}-{m}-{d}")), slug.to_string());
            }
        }
    }
    (None, folder_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_target_for_github() {
        let p = archive_target_path(
            "github.com",
            "graysurf",
            "agent-runtime-kit",
            "2026-05-27-plan-archive-runtime-kit",
        );
        assert_eq!(
            p,
            PathBuf::from(
                "plans/github.com/graysurf/agent-runtime-kit/2026-05-27-plan-archive-runtime-kit"
            )
        );
    }

    #[test]
    fn archive_target_for_nested_gitlab_groups() {
        let p = archive_target_path(
            "gitlab.example.com",
            "acme/platform/backend",
            "ingest",
            "2026-04-10-cleanup-pipeline",
        );
        assert_eq!(
            p,
            PathBuf::from(
                "plans/gitlab.example.com/acme/platform/backend/ingest/2026-04-10-cleanup-pipeline"
            )
        );
    }

    #[test]
    fn parse_dated_folder() {
        let (date, slug) = parse_plan_folder("2026-05-27-plan-archive-runtime-kit");
        assert_eq!(date.as_deref(), Some("2026-05-27"));
        assert_eq!(slug, "plan-archive-runtime-kit");
    }

    #[test]
    fn parse_pre_v1_folder() {
        let (date, slug) = parse_plan_folder("skill-lifecycle-management");
        assert!(date.is_none());
        assert_eq!(slug, "skill-lifecycle-management");
    }

    #[test]
    fn parse_almost_dated_folder() {
        // Partial date prefix is not enough; treat as pre-v1 slug.
        let (date, slug) = parse_plan_folder("2026-something-else");
        assert!(date.is_none());
        assert_eq!(slug, "2026-something-else");
    }
}
