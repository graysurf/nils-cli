//! Pure path / id helpers for the evidence migration pipeline.

use std::path::PathBuf;

/// Build the relative archive target dir for a rollup:
/// `evidence/<host>/<org_or_group_path>/<repo>/<id>/`.
///
/// Mirrors plan-archive's `archive_target_path` but rooted at `evidence/`
/// (not `plans/`). The host preserves dots; the `org_or_group_path`
/// preserves nested GitLab group separators.
pub fn archive_target_path(host: &str, org_or_group_path: &str, repo: &str, id: &str) -> PathBuf {
    // Sanitize each identity component to a single safe in-target segment. Repo
    // identity can derive from a crafted `origin` remote (cwd -> origin), so a
    // `..`/separator in host/org/repo must never form a parent-dir escape. The
    // group path is split first so legitimate nested GitLab groups are kept as
    // distinct (sanitized) segments. `id` is internally generated and safe.
    use super::sanitize_path_segment;
    let mut p = PathBuf::from("evidence");
    p.push(sanitize_path_segment(host, "unknown-host"));
    for segment in org_or_group_path.split('/').filter(|s| !s.is_empty()) {
        p.push(sanitize_path_segment(segment, "unknown-org"));
    }
    p.push(sanitize_path_segment(repo, "unknown-repo"));
    p.push(id);
    p
}

/// Encode an RFC3339 timestamp into a basic-format ISO8601 stamp
/// `YYYYMMDDThhmmssZ`.
///
/// NOTE: plan-archive only ships a `decode_basic_stamp` (decoder); there is
/// no upstream encoder, so this is a net-new function. It strips separators
/// and any sub-second fraction / offset from the input. If the input does not
/// look like an extended RFC3339 timestamp, it is sanitized to alphanumerics
/// so the result is always a safe path segment.
pub fn encode_basic_stamp(rfc3339: &str) -> String {
    // Expected shape: YYYY-MM-DDThh:mm:ss[.fff][Z|+hh:mm]
    let bytes = rfc3339.as_bytes();
    let looks_extended = bytes.len() >= 19
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && rfc3339[0..4].chars().all(|c| c.is_ascii_digit())
        && rfc3339[5..7].chars().all(|c| c.is_ascii_digit())
        && rfc3339[8..10].chars().all(|c| c.is_ascii_digit())
        && rfc3339[11..13].chars().all(|c| c.is_ascii_digit())
        && rfc3339[14..16].chars().all(|c| c.is_ascii_digit())
        && rfc3339[17..19].chars().all(|c| c.is_ascii_digit());
    if looks_extended {
        return format!(
            "{}{}{}T{}{}{}Z",
            &rfc3339[0..4],
            &rfc3339[5..7],
            &rfc3339[8..10],
            &rfc3339[11..13],
            &rfc3339[14..16],
            &rfc3339[17..19],
        );
    }
    // Fallback: keep only path-safe characters.
    rfc3339
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Derive a path-safe skill slug from a skill path/id: lowercase, `/` → `-`,
/// drop characters outside `[a-z0-9._-]`, collapse repeated `-`, and truncate
/// to 40 chars (hashing the tail when longer so distinct long skills stay
/// distinct). `[REDACTED]` collapses to `redacted`.
pub fn skill_slug(skill: &str) -> String {
    let lowered = skill.to_ascii_lowercase().replace("[redacted]", "redacted");
    let mut out = String::with_capacity(lowered.len());
    let mut last_dash = false;
    for ch in lowered.chars() {
        let mapped = match ch {
            '/' | '\\' | ' ' | ':' => '-',
            c if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' => c,
            _ => '-',
        };
        if mapped == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(mapped);
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        return "skill".to_string();
    }
    if trimmed.len() <= 40 {
        return trimmed;
    }
    // Stable, deterministic tail for long slugs: keep the first 31 chars and a
    // short FNV-1a hash so two long slugs that share a prefix do not collide.
    let head: String = trimmed.chars().take(31).collect();
    let hash = fnv1a(trimmed.as_bytes());
    format!("{head}-{hash:08x}")
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for b in bytes {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Build the rollup id: `<basic_stamp>-<skill_slug>-<digest8>`.
///
/// The digest tail is **8** hex chars (vs plan-archive's 4-char fetch stamp
/// tail) for lower collision risk across content-addressed rollups.
pub fn rollup_id(started_at: &str, skill: &str, source_digest: &str) -> String {
    let stamp = encode_basic_stamp(started_at);
    let slug = skill_slug(skill);
    let tail = digest_tail8(source_digest);
    format!("{stamp}-{slug}-{tail}")
}

/// Extract the 8-char hex tail from a `sha256:<hex>` (or bare hex) digest.
fn digest_tail8(source_digest: &str) -> String {
    let hex = source_digest
        .strip_prefix("sha256:")
        .unwrap_or(source_digest);
    hex.chars().take(8).collect()
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
            "20260614T100000Z-deliver-pr-deadbeef",
        );
        assert_eq!(
            p,
            PathBuf::from(
                "evidence/github.com/graysurf/agent-runtime-kit/20260614T100000Z-deliver-pr-deadbeef"
            )
        );
    }

    #[test]
    fn archive_target_for_nested_gitlab_groups() {
        let p = archive_target_path(
            "gitlab.example.com",
            "acme/platform/backend",
            "ingest",
            "20260410T010203Z-cleanup-1234abcd",
        );
        assert_eq!(
            p,
            PathBuf::from(
                "evidence/gitlab.example.com/acme/platform/backend/ingest/20260410T010203Z-cleanup-1234abcd"
            )
        );
    }

    #[test]
    fn archive_target_neutralizes_traversal_in_identity_segments() {
        // A `..` reaching an identity component (e.g. a crafted `origin` remote
        // resolved through cwd) must never form a parent-dir escape under the
        // archive target. Every component stays a plain in-target name.
        let p = archive_target_path("github.com", "../../../etc", "..", "id");
        for comp in p.components() {
            assert!(
                !matches!(comp, std::path::Component::ParentDir),
                "archive target escaped via `..`: {p:?}"
            );
        }
        // Separators inside a single identity segment cannot widen the path.
        let p2 = archive_target_path("evil/../host", "o", "r", "id");
        assert!(
            p2.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "identity segment introduced a non-normal component: {p2:?}"
        );
    }

    #[test]
    fn encode_basic_stamp_strips_separators_and_fraction() {
        assert_eq!(
            encode_basic_stamp("2026-06-14T10:00:00Z"),
            "20260614T100000Z"
        );
        assert_eq!(
            encode_basic_stamp("2026-05-28T19:53:44.392221Z"),
            "20260528T195344Z"
        );
    }

    #[test]
    fn encode_basic_stamp_handles_offset_form() {
        // Offset form still yields the local clock components in basic form.
        assert_eq!(
            encode_basic_stamp("2026-06-14T10:00:00+02:00"),
            "20260614T100000Z"
        );
    }

    #[test]
    fn encode_basic_stamp_garbage_is_path_safe() {
        let encoded = encode_basic_stamp("not a time!");
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "got {encoded}"
        );
    }

    #[test]
    fn skill_slug_path_like_lowercased_and_dashed() {
        // Short enough (<=40 chars) to stay un-hashed.
        assert_eq!(
            skill_slug("/skills/Deliver-PR/SKILL.md"),
            "skills-deliver-pr-skill.md"
        );
    }

    #[test]
    fn skill_slug_long_path_like_truncated_with_hash() {
        // A realistic long skill path exceeds 40 chars, so it is truncated and
        // a stable hash tail is appended.
        let slug = skill_slug("/abs/path/to/deliver-plan-tracking-issue/SKILL.md");
        assert!(slug.len() <= 40, "slug too long: {} ({slug})", slug.len());
        assert!(slug.starts_with("abs-path-to-deliver-plan-tracki"));
    }

    #[test]
    fn skill_slug_redacted_collapses() {
        assert_eq!(skill_slug("[REDACTED]"), "redacted");
    }

    #[test]
    fn skill_slug_long_is_truncated_with_hash() {
        let long = "a".repeat(120);
        let slug = skill_slug(&long);
        assert!(slug.len() <= 40, "slug too long: {} ({slug})", slug.len());
        // A different long slug yields a different hash tail.
        let other = "b".repeat(120);
        assert_ne!(skill_slug(&long), skill_slug(&other));
    }

    #[test]
    fn skill_slug_empty_fallback() {
        assert_eq!(skill_slug("///"), "skill");
    }

    #[test]
    fn rollup_id_format() {
        let id = rollup_id(
            "2026-06-14T10:00:00Z",
            "deliver-pr",
            "sha256:deadbeefcafef00d1234",
        );
        assert_eq!(id, "20260614T100000Z-deliver-pr-deadbeef");
    }
}
