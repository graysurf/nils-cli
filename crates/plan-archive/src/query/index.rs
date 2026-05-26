//! Walk the `_index/` tree and reconstruct ref identities from
//! snapshot directory paths.

use std::path::{Path, PathBuf};

use crate::refresh::refparse::RefKind;

/// A ref folder discovered under `_index/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub host: String,
    pub org_or_group_path: String,
    pub repo: String,
    pub kind: RefKind,
    pub number: u64,
}

impl IndexEntry {
    /// Snapshot directory relative to the archive root.
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

    /// Canonical provider URL for the ref.
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
}

/// Parse an `_index/`-relative path
/// (`<host>/<org…>/<repo>/<kind>/<number>`) into an [`IndexEntry`].
/// The kind segment (`issues`/`pulls`/`merge_requests`) splits the
/// org/repo head from the number tail.
pub fn parse_index_path(rel: &Path) -> Option<IndexEntry> {
    let segments: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    // Need at least host, org, repo, kind, number.
    if segments.len() < 5 {
        return None;
    }
    // Locate the kind keyword from the tail (second-to-last).
    let number: u64 = segments.last()?.parse().ok()?;
    let kind = match segments[segments.len() - 2].as_str() {
        "issues" => RefKind::Issue,
        "pulls" => RefKind::Pull,
        "merge_requests" => RefKind::MergeRequest,
        _ => return None,
    };
    let head = &segments[..segments.len() - 2];
    if head.len() < 3 {
        return None;
    }
    let host = head[0].clone();
    let repo = head[head.len() - 1].clone();
    let org_or_group_path = head[1..head.len() - 1].join("/");
    if org_or_group_path.is_empty() {
        return None;
    }
    Some(IndexEntry {
        host,
        org_or_group_path,
        repo,
        kind,
        number,
    })
}

/// Walk an `_index/` root and return every ref folder found. A ref
/// folder is a directory whose path matches the
/// `<host>/<org…>/<repo>/<kind>/<number>` shape.
pub fn walk_index(index_root: &Path) -> std::io::Result<Vec<IndexEntry>> {
    let mut out = Vec::new();
    if !index_root.is_dir() {
        return Ok(out);
    }
    let mut stack = vec![index_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let rel = path.strip_prefix(index_root).unwrap_or(&path);
            if let Some(index_entry) = parse_index_path(rel) {
                out.push(index_entry);
                // A number folder is a leaf; do not descend further.
            } else {
                stack.push(path);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_issue_path() {
        let e = parse_index_path(Path::new(
            "github.com/graysurf/agent-runtime-kit/issues/126",
        ))
        .unwrap();
        assert_eq!(e.host, "github.com");
        assert_eq!(e.org_or_group_path, "graysurf");
        assert_eq!(e.repo, "agent-runtime-kit");
        assert_eq!(e.kind, RefKind::Issue);
        assert_eq!(e.number, 126);
    }

    #[test]
    fn parses_nested_gitlab_mr_path() {
        let e = parse_index_path(Path::new(
            "gitlab.example.com/acme/platform/ingest/merge_requests/42",
        ))
        .unwrap();
        assert_eq!(e.org_or_group_path, "acme/platform");
        assert_eq!(e.repo, "ingest");
        assert_eq!(e.kind, RefKind::MergeRequest);
        assert_eq!(e.number, 42);
    }

    #[test]
    fn round_trips_index_dir() {
        let e = IndexEntry {
            host: "github.com".into(),
            org_or_group_path: "sympoies".into(),
            repo: "nils-cli".into(),
            kind: RefKind::Pull,
            number: 574,
        };
        let dir = e.index_dir();
        let rel = dir.strip_prefix("_index").unwrap();
        assert_eq!(parse_index_path(rel).unwrap(), e);
    }

    #[test]
    fn rejects_non_numeric_tail() {
        assert!(parse_index_path(Path::new("github.com/org/repo/issues/x")).is_none());
    }

    #[test]
    fn rejects_unknown_kind() {
        assert!(parse_index_path(Path::new("github.com/org/repo/wikis/1")).is_none());
    }
}
