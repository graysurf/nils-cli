//! `glab --version` parser and pinned-minor support range.
//!
//! Spec / plan §Sprint 3 Task 3.2: the branch-only `glab ci status` fallback
//! has no JSON output for pipeline status, so that text parser is pinned to one
//! specific minor. API-backed numeric MR checks/wait-checks do not call this
//! guard.
//!
//! The pin is `SUPPORTED_MAJOR.SUPPORTED_MINOR.x` — single minor only.
//! When `glab` ships a new minor that breaks the text parser, bumping these
//! constants is the one-line tracking change.

/// Pinned major version of `glab` whose text format we parse.
pub const SUPPORTED_MAJOR: u32 = 1;
/// Pinned minor version of `glab` whose text format we parse. Adjust this
/// together with parser changes when `glab` ships a breaking text format.
pub const SUPPORTED_MINOR: u32 = 99;

/// Result of parsing a `glab --version` first line. Tuple is
/// `(major, minor, patch)`.
pub type GlabVersion = (u32, u32, u32);

/// Parse the first line of `glab --version`. Accepts:
///
/// - `glab 1.45.0`
/// - `glab version 1.45.0 (some build)`
/// - `1.45.0` (bare semver — defensive fallback)
///
/// Returns `None` when the line does not contain a recognisable
/// `<major>.<minor>.<patch>` triple.
pub fn parse_version_line(line: &str) -> Option<GlabVersion> {
    for token in line.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        let cleaned = token.trim_start_matches('v').trim();
        if let Some(v) = parse_triple(cleaned) {
            return Some(v);
        }
    }
    None
}

fn parse_triple(s: &str) -> Option<GlabVersion> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = parts[1].parse::<u32>().ok()?;
    let patch = if let Some(p) = parts.get(2) {
        // Tolerate `-rc1` / `+build.7` suffixes.
        let p_main: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        p_main.parse::<u32>().unwrap_or(0)
    } else {
        0
    };
    Some((major, minor, patch))
}

/// Verify the parsed version is inside the pinned support range. Returns
/// `Ok(())` when supported; returns the human-readable hint string when not
/// (the caller wraps it in [`crate::error::ForgeError::unavailable`]).
pub fn ensure_supported(version: GlabVersion) -> Result<(), String> {
    let (major, minor, _) = version;
    if major == SUPPORTED_MAJOR && minor == SUPPORTED_MINOR {
        return Ok(());
    }
    Err(format!(
        "glab {major}.{minor}.x is not supported by this forge-cli build (pinned to {sup_major}.{sup_minor}.x). \
Please upgrade or downgrade glab so its minor matches; the text parser is intentionally pinned to a single minor.",
        sup_major = SUPPORTED_MAJOR,
        sup_minor = SUPPORTED_MINOR,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_version_line_handles_plain_form() {
        assert_eq!(parse_version_line("glab 1.45.0"), Some((1, 45, 0)));
    }

    #[test]
    fn parse_version_line_handles_version_keyword() {
        assert_eq!(
            parse_version_line("glab version 1.45.3 (linux/amd64)"),
            Some((1, 45, 3))
        );
    }

    #[test]
    fn parse_version_line_handles_v_prefix() {
        assert_eq!(parse_version_line("glab v1.45.0"), Some((1, 45, 0)));
    }

    #[test]
    fn parse_version_line_tolerates_pre_release_suffix() {
        assert_eq!(parse_version_line("glab 1.45.0-rc.1"), Some((1, 45, 0)));
    }

    #[test]
    fn parse_version_line_two_part_version_assumes_patch_zero() {
        assert_eq!(parse_version_line("glab 1.45"), Some((1, 45, 0)));
    }

    #[test]
    fn parse_version_line_rejects_garbage() {
        assert_eq!(parse_version_line("not a version"), None);
        assert_eq!(parse_version_line(""), None);
    }

    #[test]
    fn ensure_supported_accepts_pinned_minor() {
        assert!(ensure_supported((SUPPORTED_MAJOR, SUPPORTED_MINOR, 0)).is_ok());
        assert!(ensure_supported((SUPPORTED_MAJOR, SUPPORTED_MINOR, 7)).is_ok());
    }

    #[test]
    fn ensure_supported_rejects_other_minors() {
        let lower = ensure_supported((SUPPORTED_MAJOR, SUPPORTED_MINOR - 1, 9)).unwrap_err();
        assert!(lower.contains("upgrade"), "{lower}");
        let upper = ensure_supported((SUPPORTED_MAJOR, SUPPORTED_MINOR + 1, 0)).unwrap_err();
        assert!(upper.contains("downgrade"), "{upper}");
    }

    #[test]
    fn ensure_supported_rejects_other_majors() {
        let err = ensure_supported((SUPPORTED_MAJOR + 1, SUPPORTED_MINOR, 0)).unwrap_err();
        assert!(err.contains("pinned"));
    }
}
