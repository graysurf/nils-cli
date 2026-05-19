//! Shared PR/MR state normalization for `pr view`, `pr list`, `pr close`,
//! and the `pr deliver` macro.
//!
//! Spec: `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"pr view" — the
//! canonical envelope state enum is `open | closed | merged`. GitHub
//! already uses these literals (uppercased). GitLab uses `opened` /
//! `closed` / `merged` / `locked`. The mapping below is the only place
//! state strings cross the wire.

use nils_common::cli_contract::schema_version_for;

use crate::cli::BINARY;
use crate::error::ForgeError;
use crate::provider::Provider;

/// Convert a raw backend state into the canonical `open | closed | merged`
/// literal. Unknown values produce a `SOFTWARE 70` error so consumers see a
/// hard failure rather than a silent fallthrough.
pub fn normalize_state(raw: &str, provider: Provider) -> Result<&'static str, ForgeError> {
    let lower = raw.trim().to_ascii_lowercase();
    let mapped = match provider {
        Provider::GitHub => match lower.as_str() {
            "open" => "open",
            "closed" => "closed",
            "merged" => "merged",
            _ => return unknown(raw, provider),
        },
        Provider::GitLab => match lower.as_str() {
            "opened" | "open" => "open",
            "closed" | "locked" => "closed",
            "merged" => "merged",
            _ => return unknown(raw, provider),
        },
    };
    Ok(mapped)
}

fn unknown(raw: &str, provider: Provider) -> Result<&'static str, ForgeError> {
    Err(ForgeError::software(
        schema_version_for(BINARY, "error", 1),
        format!(
            "{provider} returned an unknown state literal '{raw}'",
            provider = provider.as_str()
        ),
        None,
    ))
}

/// Convert GitHub's mergeable trio (`MERGEABLE`/`CONFLICTING`/`UNKNOWN`) or
/// GitLab's `merge_status` shape into the canonical `yes | no | unknown`
/// literal.
pub fn normalize_mergeable_github(value: Option<&str>) -> &'static str {
    match value.unwrap_or("").to_ascii_uppercase().as_str() {
        "MERGEABLE" => "yes",
        "CONFLICTING" => "no",
        _ => "unknown",
    }
}

pub fn normalize_mergeable_gitlab(value: Option<&str>) -> &'static str {
    match value.unwrap_or("") {
        "can_be_merged" | "mergeable" => "yes",
        "cannot_be_merged" | "cannot_be_merged_recheck" => "no",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn github_state_passthrough() {
        assert_eq!(normalize_state("OPEN", Provider::GitHub).unwrap(), "open");
        assert_eq!(
            normalize_state("merged", Provider::GitHub).unwrap(),
            "merged"
        );
    }

    #[test]
    fn gitlab_opened_becomes_open() {
        assert_eq!(normalize_state("opened", Provider::GitLab).unwrap(), "open");
    }

    #[test]
    fn gitlab_locked_becomes_closed() {
        assert_eq!(
            normalize_state("locked", Provider::GitLab).unwrap(),
            "closed"
        );
    }

    #[test]
    fn unknown_state_errors_software() {
        let err = normalize_state("invented", Provider::GitHub).expect_err("unknown");
        assert_eq!(err.kind(), "software_error");
    }

    #[test]
    fn mergeable_mapping_github() {
        assert_eq!(normalize_mergeable_github(Some("MERGEABLE")), "yes");
        assert_eq!(normalize_mergeable_github(Some("CONFLICTING")), "no");
        assert_eq!(normalize_mergeable_github(Some("UNKNOWN")), "unknown");
        assert_eq!(normalize_mergeable_github(None), "unknown");
    }

    #[test]
    fn mergeable_mapping_gitlab() {
        assert_eq!(normalize_mergeable_gitlab(Some("can_be_merged")), "yes");
        assert_eq!(normalize_mergeable_gitlab(Some("cannot_be_merged")), "no");
        assert_eq!(normalize_mergeable_gitlab(None), "unknown");
    }
}
