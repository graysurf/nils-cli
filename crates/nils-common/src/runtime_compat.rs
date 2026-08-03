//! Release-compatibility and live-executable primitives for the control plane.
//!
//! A fleet upgrade can replace an installed binary while an older session and
//! its coordination broker are still live (`sympoies/nils-cli#1409`). The newer
//! consumer then reads state that an older producer wrote. That is *version
//! drift*: a recoverable condition with a bounded repair path. Reporting it as
//! corruption, or as a generic capability failure, sends the operator down a
//! dead end — so drift needs its own classification.
//!
//! This module owns the two primitives that classification needs and nothing
//! else. Deciding what to do about a skew stays with the consumer, because the
//! safe response differs per lane.

use std::path::Path;

/// Schema-version family prefix for the coordination registry.
///
/// A registry whose `schema_version` starts with this prefix but whose version
/// suffix is unrecognized was written by a different release generation. That is
/// drift, not corruption — unlike a body that does not parse at all.
pub const COORDINATION_REGISTRY_FAMILY: &str = "agent-session.coordination-registry.";

/// How far apart two releases are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseSkew {
    /// Identical releases.
    Same,
    /// Same generation, different patch level.
    Patch,
    /// Same major, different minor generation.
    Minor,
    /// Different major generation.
    Major,
    /// At least one side could not be parsed.
    Unknown,
}

impl ReleaseSkew {
    /// Stable kebab-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this difference crosses a protocol generation.
    ///
    /// A patch difference within one generation shares the same protocol shape
    /// and must stay silent, otherwise ordinary rolling upgrades would report
    /// drift on every dispatch. An unparsable release is not treated as drift
    /// either: it carries no evidence that a boundary was crossed.
    pub fn crosses_generation(self) -> bool {
        matches!(self, Self::Minor | Self::Major)
    }
}

/// Classify the distance between the local release and a peer's release.
pub fn classify_release(local: &str, peer: &str) -> ReleaseSkew {
    let (Some(local), Some(peer)) = (semantic_triple(local), semantic_triple(peer)) else {
        return ReleaseSkew::Unknown;
    };
    if local == peer {
        return ReleaseSkew::Same;
    }
    if local.0 != peer.0 {
        return ReleaseSkew::Major;
    }
    if local.1 != peer.1 {
        return ReleaseSkew::Minor;
    }
    ReleaseSkew::Patch
}

/// Whether a coordination registry schema version belongs to a different
/// release generation of the same family.
pub fn registry_generation_drift(schema_version: &str, supported: &[&str]) -> bool {
    schema_version.starts_with(COORDINATION_REGISTRY_FAMILY) && !supported.contains(&schema_version)
}

/// Whether a live process still has the executable it was started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableState {
    /// The executable is present and unmodified in place.
    Live,
    /// The executable was deleted or replaced underneath the live process.
    Replaced,
    /// The state could not be determined on this platform or for this process.
    Unknown,
}

impl ExecutableState {
    /// Stable kebab-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Replaced => "replaced",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify the live executable behind a process id.
///
/// Checking an installation symlink is not sufficient: a fleet upgrade can leave
/// the symlink correct while a live process still references a deleted inode.
/// Only the kernel's own view of the process answers that, so this reads
/// `/proc/<pid>/exe` where it exists and reports [`ExecutableState::Unknown`]
/// elsewhere rather than guessing.
pub fn executable_state(pid: u32) -> ExecutableState {
    if !Path::new("/proc").is_dir() {
        return ExecutableState::Unknown;
    }
    let Ok(target) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return ExecutableState::Unknown;
    };
    classify_exe_target(&target.to_string_lossy())
}

/// Classify a resolved `/proc/<pid>/exe` target string.
fn classify_exe_target(target: &str) -> ExecutableState {
    // Linux appends this marker when the referenced inode is unlinked.
    if target.ends_with(" (deleted)") {
        return ExecutableState::Replaced;
    }
    if target.is_empty() {
        return ExecutableState::Unknown;
    }
    ExecutableState::Live
}

/// Parse the leading `major.minor.patch` of a release string.
///
/// Trailing build metadata such as `1.2.3 (v1.2.3, rustc ...)` or `1.2.3+build`
/// is ignored so a long form and a short form of the same release compare equal.
fn semantic_triple(value: &str) -> Option<(u64, u64, u64)> {
    let head = value
        .trim()
        .split([' ', '+', '-'])
        .next()
        .filter(|head| !head.is_empty())?;
    let mut parts = head.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn release_classification_separates_generation_drift_from_patch_drift() {
        assert_eq!(classify_release("1.25.13", "1.25.13"), ReleaseSkew::Same);
        assert_eq!(classify_release("1.25.13", "1.25.14"), ReleaseSkew::Patch);
        assert_eq!(classify_release("1.25.13", "1.26.0"), ReleaseSkew::Minor);
        assert_eq!(classify_release("1.25.13", "2.0.0"), ReleaseSkew::Major);

        assert!(!ReleaseSkew::Same.crosses_generation());
        assert!(!ReleaseSkew::Patch.crosses_generation());
        assert!(ReleaseSkew::Minor.crosses_generation());
        assert!(ReleaseSkew::Major.crosses_generation());
    }

    #[test]
    fn long_version_forms_compare_equal_to_their_short_release() {
        assert_eq!(
            classify_release("1.25.13", "1.25.13 (v1.25.13, rustc 1.97.1 (8bab26f4f))"),
            ReleaseSkew::Same
        );
        assert_eq!(
            classify_release("1.25.13+local", "1.25.13"),
            ReleaseSkew::Same
        );
    }

    #[test]
    fn an_unparsable_release_is_unknown_and_never_reported_as_drift() {
        for peer in ["", "unknown", "1.25", "1.25.13.1", "x.y.z"] {
            let skew = classify_release("1.25.13", peer);
            assert_eq!(skew, ReleaseSkew::Unknown, "peer={peer:?}");
            assert!(
                !skew.crosses_generation(),
                "an unknown release must not claim a crossed boundary: peer={peer:?}"
            );
        }
    }

    #[test]
    fn registry_generation_drift_excludes_supported_versions_and_foreign_families() {
        let supported = [
            "agent-session.coordination-registry.v1",
            "agent-session.coordination-registry.v2",
        ];
        assert!(!registry_generation_drift(
            "agent-session.coordination-registry.v1",
            &supported
        ));
        assert!(registry_generation_drift(
            "agent-session.coordination-registry.v9",
            &supported
        ));
        assert!(
            !registry_generation_drift("agent-session.work-context.v1", &supported),
            "another schema family is not registry drift"
        );
        assert!(
            !registry_generation_drift("", &supported),
            "an absent schema version is not registry drift"
        );
    }

    #[test]
    fn a_deleted_executable_target_is_classified_as_replaced() {
        assert_eq!(
            classify_exe_target("/opt/nils/bin/agent-session (deleted)"),
            ExecutableState::Replaced
        );
        assert_eq!(
            classify_exe_target("/opt/nils/bin/agent-session"),
            ExecutableState::Live
        );
        assert_eq!(classify_exe_target(""), ExecutableState::Unknown);
    }

    #[test]
    fn the_current_process_reports_a_determinate_executable_state() {
        let state = executable_state(std::process::id());
        if Path::new("/proc/self/exe").exists() {
            assert_eq!(
                state,
                ExecutableState::Live,
                "the running test binary is not deleted"
            );
        } else {
            assert_eq!(
                state,
                ExecutableState::Unknown,
                "platforms without procfs report unknown rather than guessing"
            );
        }
    }
}
