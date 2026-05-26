//! Parse issue / PR / MR URLs into a structured refresh target and
//! derive the append-only `_index/` snapshot directory.

use std::path::PathBuf;

use serde::Serialize;

/// The kind of provider reference being refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Issue,
    Pull,
    MergeRequest,
}

impl RefKind {
    /// `_index/` sub-directory name for this kind.
    pub fn index_segment(self) -> &'static str {
        match self {
            RefKind::Issue => "issues",
            RefKind::Pull => "pulls",
            RefKind::MergeRequest => "merge_requests",
        }
    }
}

/// A fully-resolved refresh target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefTarget {
    pub host: String,
    pub org_or_group_path: String,
    pub repo: String,
    pub kind: RefKind,
    pub number: u64,
}

impl RefTarget {
    /// Canonical provider URL for this target (used in reports and
    /// for the failed-ref list).
    pub fn canonical_url(&self) -> String {
        let segment = match self.kind {
            RefKind::Issue => "issues",
            RefKind::Pull => "pull",
            RefKind::MergeRequest => "-/merge_requests",
        };
        format!(
            "https://{}/{}/{}/{}/{}",
            self.host, self.org_or_group_path, self.repo, segment, self.number
        )
    }

    /// Append-only directory under `_index/` for this target's
    /// snapshots.
    pub fn index_dir(&self) -> PathBuf {
        let mut p = PathBuf::from("_index");
        p.push(&self.host);
        for segment in self.org_or_group_path.split('/').filter(|s| !s.is_empty()) {
            p.push(segment);
        }
        p.push(&self.repo);
        p.push(self.kind.index_segment());
        p.push(self.number.to_string());
        p
    }
}

/// Parse a provider issue / PR / MR URL into a [`RefTarget`].
///
/// Supports:
/// - `https://github.com/org/repo/issues/123`
/// - `https://github.com/org/repo/pull/123`
/// - `https://gitlab.example.com/group/sub/repo/-/issues/123`
/// - `https://gitlab.example.com/group/sub/repo/-/merge_requests/123`
pub fn parse_ref_url(url: &str) -> Option<RefTarget> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 4 {
        return None;
    }

    // Locate the kind keyword and number from the tail.
    let number: u64 = segments.last()?.parse().ok()?;
    let kind_kw = segments.get(segments.len() - 2)?;
    let kind = match *kind_kw {
        "issues" => RefKind::Issue,
        "pull" | "pulls" => RefKind::Pull,
        "merge_requests" => RefKind::MergeRequest,
        _ => return None,
    };

    // Everything before the kind keyword (minus an optional GitLab
    // `-` separator) is org/group-path + repo.
    let mut head = &segments[..segments.len() - 2];
    if head.last() == Some(&"-") {
        head = &head[..head.len() - 1];
    }
    if head.len() < 2 {
        return None;
    }
    let repo = head[head.len() - 1].to_string();
    let org_or_group_path = head[..head.len() - 1].join("/");
    if org_or_group_path.is_empty() {
        return None;
    }

    Some(RefTarget {
        host: host.to_string(),
        org_or_group_path,
        repo,
        kind,
        number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_issue() {
        let t = parse_ref_url("https://github.com/graysurf/agent-runtime-kit/issues/126").unwrap();
        assert_eq!(t.host, "github.com");
        assert_eq!(t.org_or_group_path, "graysurf");
        assert_eq!(t.repo, "agent-runtime-kit");
        assert_eq!(t.kind, RefKind::Issue);
        assert_eq!(t.number, 126);
        assert_eq!(
            t.index_dir(),
            PathBuf::from("_index/github.com/graysurf/agent-runtime-kit/issues/126")
        );
    }

    #[test]
    fn parses_github_pull() {
        let t = parse_ref_url("https://github.com/sympoies/nils-cli/pull/574").unwrap();
        assert_eq!(t.kind, RefKind::Pull);
        assert_eq!(t.number, 574);
        assert_eq!(
            t.index_dir(),
            PathBuf::from("_index/github.com/sympoies/nils-cli/pulls/574")
        );
    }

    #[test]
    fn parses_gitlab_merge_request_with_dash_and_nested_group() {
        let t =
            parse_ref_url("https://gitlab.example.com/acme/platform/ingest/-/merge_requests/42")
                .unwrap();
        assert_eq!(t.host, "gitlab.example.com");
        assert_eq!(t.org_or_group_path, "acme/platform");
        assert_eq!(t.repo, "ingest");
        assert_eq!(t.kind, RefKind::MergeRequest);
        assert_eq!(t.number, 42);
        assert_eq!(
            t.index_dir(),
            PathBuf::from("_index/gitlab.example.com/acme/platform/ingest/merge_requests/42")
        );
    }

    #[test]
    fn parses_gitlab_issue_with_dash() {
        let t = parse_ref_url("https://gitlab.example.com/acme/ingest/-/issues/7").unwrap();
        assert_eq!(t.org_or_group_path, "acme");
        assert_eq!(t.repo, "ingest");
        assert_eq!(t.kind, RefKind::Issue);
    }

    #[test]
    fn rejects_non_numeric_id() {
        assert!(parse_ref_url("https://github.com/org/repo/issues/abc").is_none());
    }

    #[test]
    fn rejects_unknown_kind() {
        assert!(parse_ref_url("https://github.com/org/repo/wiki/123").is_none());
    }

    #[test]
    fn rejects_missing_repo() {
        assert!(parse_ref_url("https://github.com/org/issues/1").is_none());
    }

    #[test]
    fn canonical_url_round_trips_kind() {
        let t = parse_ref_url("https://github.com/org/repo/pull/9").unwrap();
        assert_eq!(t.canonical_url(), "https://github.com/org/repo/pull/9");
    }
}
