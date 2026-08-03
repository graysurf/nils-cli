use std::path::{Path, PathBuf};
use std::process::Command;

/// Release every crate in this workspace is bumped to.
///
/// `nils-test-support` is versioned in lockstep with the rest of the workspace,
/// so its own package version is the release a workspace binary must report to
/// belong to this build.
pub const WORKSPACE_RELEASE: &str = env!("CARGO_PKG_VERSION");

/// Resolve a workspace binary path for tests.
///
/// Cargo exposes test binaries via `CARGO_BIN_EXE_<name>`. This helper checks the
/// name as-is and also tries swapping `-` and `_` to match how Cargo exports
/// environment variables for hyphenated crate names.
///
/// When no env var is present, it falls back to `target/<profile>/<name>` based
/// on the current test executable location.
///
/// Panics when neither source yields a path. Use [`sibling_or_skip`] for a
/// binary another package owns, where absence is a property of the run rather
/// than a defect.
pub fn resolve(bin_name: &str) -> PathBuf {
    resolve_optional(bin_name).unwrap_or_else(|| panic!("{bin_name} binary path: NotPresent"))
}

/// Resolve a workspace binary path for tests, or `None` when nothing is there.
///
/// This reports presence only. A path it returns can still be an artifact from
/// an earlier build, so prefer [`sibling`] when the binary belongs to another
/// package.
pub fn resolve_optional(bin_name: &str) -> Option<PathBuf> {
    for candidate in env_names(bin_name) {
        if let Ok(bin) = std::env::var(&candidate) {
            return Some(PathBuf::from(bin));
        }
    }

    let exe = std::env::current_exe().expect("current exe");
    let target_dir = exe.parent().and_then(|p| p.parent()).expect("target dir");
    let bin_file = format!("{bin_name}{}", std::env::consts::EXE_SUFFIX);
    let bin = target_dir.join(bin_file);
    bin.exists().then_some(bin)
}

/// Whether a workspace binary another package owns can be used by this test run.
///
/// Cargo sets `CARGO_BIN_EXE_<name>` only for binaries of the package being
/// tested. A `dev-dependencies` entry does not extend that: it neither exports
/// the variable nor builds the dependency's binary. So a cross-crate sibling is
/// resolved from `target/<profile>/<name>`, where a package-scoped run has no
/// reason to have built it and an earlier build may have left an artifact from
/// another release. See `sympoies/nils-cli#1413`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sibling {
    /// Present and reporting [`WORKSPACE_RELEASE`].
    Ready(PathBuf),
    /// Neither `CARGO_BIN_EXE_<name>` nor an artifact in the profile directory.
    Absent,
    /// An artifact exists but does not belong to this build: it reports another
    /// release, or its `--version` could not be read at all.
    ReleaseMismatch { reported: Option<String> },
}

impl Sibling {
    /// The usable path, or `None` when this run cannot use the sibling.
    pub fn ready(&self) -> Option<&Path> {
        match self {
            Self::Ready(path) => Some(path),
            _ => None,
        }
    }

    /// Why this run cannot use the sibling, naming the build command that fixes
    /// it. `None` when the sibling is [`Sibling::Ready`].
    ///
    /// The message deliberately omits the artifact path: it is machine-local and
    /// the build command is the actionable part.
    pub fn skip_reason(&self, bin_name: &str, cargo_package: &str) -> Option<String> {
        let build = format!("cargo build -p {cargo_package} --bins");
        match self {
            Self::Ready(_) => None,
            Self::Absent => Some(format!(
                "skipping: the `{bin_name}` binary is not built for this run. \
                 A package-scoped run does not build a binary another package owns. \
                 Run `{build}`, or any full-workspace build, first."
            )),
            Self::ReleaseMismatch {
                reported: Some(reported),
            } => Some(format!(
                "skipping: the `{bin_name}` binary reports release {reported} but this \
                 workspace builds {WORKSPACE_RELEASE}, so the artifact is from an earlier \
                 build. Run `{build}` to refresh it."
            )),
            Self::ReleaseMismatch { reported: None } => Some(format!(
                "skipping: the `{bin_name}` binary did not report a usable release from \
                 `--version`. Run `{build}` to rebuild it."
            )),
        }
    }
}

/// Classify a workspace binary another package owns.
///
/// The release is read from the artifact's own `--version` rather than inferred
/// from its mtime, because a stale artifact is indistinguishable from a current
/// one by timestamp alone once the sibling stops being rebuilt.
pub fn sibling(bin_name: &str) -> Sibling {
    let Some(path) = resolve_optional(bin_name) else {
        return Sibling::Absent;
    };

    match reported_release(&path) {
        Some(release) if release == WORKSPACE_RELEASE => Sibling::Ready(path),
        reported => Sibling::ReleaseMismatch { reported },
    }
}

/// Resolve a sibling binary, or explain on stderr why this test cannot run.
///
/// Returns `None` so the caller can return early instead of asserting against an
/// absent or stale artifact. A full-workspace run always builds the sibling at
/// the current release, so this never skips on CI; the reason is visible locally
/// under `cargo nextest run --no-capture`.
pub fn sibling_or_skip(bin_name: &str, cargo_package: &str) -> Option<PathBuf> {
    let sibling = sibling(bin_name);
    if let Some(reason) = sibling.skip_reason(bin_name, cargo_package) {
        eprintln!("{reason}");
        return None;
    }

    sibling.ready().map(Path::to_path_buf)
}

/// Read the release an artifact reports from its own `--version`.
///
/// Every user-facing CLI in this workspace exposes root `--version` and prints
/// the package version as its second whitespace-separated token, so a failure to
/// run or parse means the artifact is not usable rather than merely old.
fn reported_release(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
}

fn env_names(bin_name: &str) -> Vec<String> {
    let mut names = Vec::new();
    names.push(format!("CARGO_BIN_EXE_{bin_name}"));

    if bin_name.contains('-') {
        names.push(format!("CARGO_BIN_EXE_{}", bin_name.replace('-', "_")));
    }
    if bin_name.contains('_') {
        names.push(format!("CARGO_BIN_EXE_{}", bin_name.replace('_', "-")));
    }

    names
}

#[cfg(test)]
mod tests {
    use super::env_names;

    #[test]
    fn env_names_includes_variants() {
        let names = env_names("api-test");
        assert_eq!(
            names,
            vec![
                "CARGO_BIN_EXE_api-test".to_string(),
                "CARGO_BIN_EXE_api_test".to_string(),
            ]
        );

        let names = env_names("api_test");
        assert_eq!(
            names,
            vec![
                "CARGO_BIN_EXE_api_test".to_string(),
                "CARGO_BIN_EXE_api-test".to_string(),
            ]
        );
    }
}
