use std::path::{Path, PathBuf};
use std::process::Command;

/// Release every crate in this workspace is bumped to.
///
/// `nils-test-support` is versioned in lockstep with the rest of the workspace,
/// which `scripts/ci/workspace-version-lockstep.sh --strict` enforces as a
/// required check, so this crate's own package version is the release a
/// workspace binary must report to belong to this build.
const WORKSPACE_RELEASE: &str = env!("CARGO_PKG_VERSION");

/// Set this to make an absent sibling fatal instead of a skip.
///
/// A skip reaches captured output, which both test runners discard for a passing
/// test, so a lane that is supposed to build every binary would report
/// green-but-empty if resolution ever regressed. CI sets this for the same reason
/// `AGENT_SESSION_TEST_REQUIRE_CGROUP` exists.
const REQUIRE_SIBLING_ENV: &str = "NILS_TEST_REQUIRE_SIBLING_BINS";

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

/// Resolve a workspace binary path for tests, or `None` when nothing selects one.
///
/// This reports selection only. A path it returns can be an artifact from an
/// earlier release, and an env-var value is returned verbatim without an
/// existence check, so prefer [`sibling_or_skip`] when the binary belongs to
/// another package.
pub fn resolve_optional(bin_name: &str) -> Option<PathBuf> {
    select(bin_name).map(|(path, _)| path)
}

/// What selected an artifact, so a reason can name the input to fix rather than a
/// machine-local path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// A `CARGO_BIN_EXE_*` value, returned verbatim and never existence-checked.
    CargoEnv(String),
    /// An artifact found in `target/<profile>`, which was checked to exist.
    ProfileDir,
}

fn select(bin_name: &str) -> Option<(PathBuf, Origin)> {
    for candidate in env_names(bin_name) {
        if let Ok(bin) = std::env::var(&candidate) {
            return Some((PathBuf::from(bin), Origin::CargoEnv(candidate)));
        }
    }

    let exe = std::env::current_exe().expect("current exe");
    let target_dir = exe.parent().and_then(|p| p.parent()).expect("target dir");
    let bin_file = format!("{bin_name}{}", std::env::consts::EXE_SUFFIX);
    let bin = target_dir.join(bin_file);
    bin.exists().then_some((bin, Origin::ProfileDir))
}

/// Whether a workspace binary another package owns can be used by this test run.
///
/// Cargo sets `CARGO_BIN_EXE_<name>` only for binaries of the package being
/// tested. A `dev-dependencies` entry does not extend that: it neither exports
/// the variable nor builds the dependency's binary. So a cross-crate sibling is
/// selected from `target/<profile>/<name>`, where a package-scoped run has no
/// reason to have built it and an earlier build may have left an artifact from
/// another release. See `sympoies/nils-cli#1413`.
///
/// Exactly one of these outcomes is a property of the run; the rest are operator
/// errors, which is why [`sibling_or_skip`] skips one and fails the others.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sibling {
    /// Selected and reporting [`WORKSPACE_RELEASE`].
    Ready(PathBuf),
    /// Nothing selected an artifact: no `CARGO_BIN_EXE_<name>`, and nothing in
    /// the profile directory. The only legitimate reason a run cannot use the
    /// sibling.
    Absent,
    /// An artifact was selected but reports a different release.
    WrongRelease { reported: String, origin: Origin },
    /// An artifact was selected but could not be identified at all: it did not
    /// run, or its `--version` carried nothing parseable. A dangling
    /// `CARGO_BIN_EXE_*` value lands here rather than in
    /// [`Sibling::Absent`], because something did select it.
    Unidentifiable { origin: Origin },
}

impl Sibling {
    /// Why this run cannot use the sibling, and whether that is a legitimate
    /// skip. `None` when the sibling is [`Sibling::Ready`].
    ///
    /// The messages deliberately omit the artifact path: it is machine-local, and
    /// the input to fix is the actionable part.
    fn unusable(&self, bin_name: &str, cargo_package: &str) -> Option<Unusable> {
        let build = format!("cargo build -p {cargo_package} --bins");
        match self {
            Self::Ready(_) => None,
            Self::Absent => Some(Unusable {
                skippable: true,
                message: format!(
                    "the `{bin_name}` binary is not built for this run. A package-scoped run \
                     does not build a binary another package owns. Run `{build}`, or any \
                     full-workspace build, first."
                ),
            }),
            Self::WrongRelease { reported, origin } => Some(Unusable {
                skippable: false,
                message: format!(
                    "the `{bin_name}` binary reports release {reported} but this workspace \
                     builds {WORKSPACE_RELEASE}, so the artifact is from an earlier release. \
                     {}",
                    origin.remedy(&build)
                ),
            }),
            Self::Unidentifiable { origin } => Some(Unusable {
                skippable: false,
                message: format!(
                    "the `{bin_name}` binary did not answer `--version` with a release, so it \
                     cannot be identified. {}",
                    origin.remedy(&build)
                ),
            }),
        }
    }
}

impl Origin {
    /// What to fix. A dangling or misdirected override is not repaired by any
    /// rebuild, so naming the variable matters more than naming the path.
    fn remedy(&self, build: &str) -> String {
        match self {
            Self::CargoEnv(variable) => format!(
                "`{variable}` selected it, so that value is what needs fixing - rebuilding will \
                 not change it."
            ),
            Self::ProfileDir => format!("Run `{build}` to refresh it."),
        }
    }
}

/// Why a run cannot use a sibling, and whether skipping is legitimate.
struct Unusable {
    message: String,
    skippable: bool,
}

/// Classify a workspace binary another package owns.
///
/// The release is read from the artifact's own `--version` rather than inferred
/// from its mtime, because a stale artifact is indistinguishable from a current
/// one by timestamp alone once the sibling stops being rebuilt.
///
/// Two boundaries are worth knowing:
///
/// - The gate detects **cross-release** artifacts only. An artifact built from an
///   older commit within the same release reports the same release and is
///   [`Sibling::Ready`]; intra-release drift remains the caller's problem.
/// - The probe runs the artifact and waits for it, so this is only for a binary
///   whose root `--version` is answered before any real work. Every user-facing
///   CLI in this workspace satisfies that because clap handles it during parse.
fn sibling(bin_name: &str) -> Sibling {
    let Some((path, origin)) = select(bin_name) else {
        return Sibling::Absent;
    };

    match reported_release(&path) {
        Some(release) if release == WORKSPACE_RELEASE => Sibling::Ready(path),
        Some(reported) => Sibling::WrongRelease { reported, origin },
        None => Sibling::Unidentifiable { origin },
    }
}

/// Resolve a sibling binary, or explain on stderr why this test cannot run.
///
/// Returns `None` only when nothing selected an artifact — the package-scoped
/// case, where skipping is honest. Panics when an artifact *was* selected but
/// does not belong to this build: a stale `target/<profile>` artifact or a
/// misdirected `CARGO_BIN_EXE_*` is an operator error, and skipping it would
/// hide that the test never ran against the binary it names.
///
/// Set `NILS_TEST_REQUIRE_SIBLING_BINS=1` to make the absent case fatal too. CI
/// sets it, so a lane that should have built every binary cannot report
/// green-but-empty if resolution regresses.
pub fn sibling_or_skip(bin_name: &str, cargo_package: &str) -> Option<PathBuf> {
    match sibling(bin_name) {
        Sibling::Ready(path) => Some(path),
        other => {
            let unusable = other
                .unusable(bin_name, cargo_package)
                .expect("every non-Ready sibling has a reason");
            if unusable.skippable && !require_sibling() {
                eprintln!("skipping: {}", unusable.message);
                return None;
            }

            panic!("{}", unusable.message);
        }
    }
}

fn require_sibling() -> bool {
    std::env::var(REQUIRE_SIBLING_ENV).is_ok_and(|value| value == "1")
}

/// Read the release an artifact reports from its own `--version`.
///
/// Every user-facing CLI in this workspace exposes root `--version` and prints
/// the package version as its second whitespace-separated token.
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
    use super::{Origin, Sibling, WORKSPACE_RELEASE, env_names, resolve_optional, sibling};
    use crate::{EnvGuard, GlobalStateLock, write_exe};
    use tempfile::TempDir;

    /// Guard both spellings Cargo could have exported for a hyphenated name, so
    /// the absent cases cannot be satisfied by an inherited variable.
    fn without_bin_exe_env(lock: &GlobalStateLock, bin_name: &str) -> Vec<EnvGuard> {
        vec![
            EnvGuard::remove(lock, &format!("CARGO_BIN_EXE_{bin_name}")),
            EnvGuard::remove(
                lock,
                &format!("CARGO_BIN_EXE_{}", bin_name.replace('-', "_")),
            ),
        ]
    }

    /// Write a stub that answers `--version` the way a workspace CLI does.
    #[cfg(unix)]
    fn write_version_stub(
        dir: &std::path::Path,
        bin_name: &str,
        release: &str,
    ) -> std::path::PathBuf {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' '{bin_name} {release} (v{release}, rustc 1.0.0 (0000000 1970-01-01))'\n"
        );
        write_exe(dir, bin_name, &script);
        dir.join(bin_name)
    }

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

    #[test]
    fn resolve_optional_reports_absence_instead_of_panicking() {
        let lock = GlobalStateLock::new();
        let _guards = without_bin_exe_env(&lock, "nts-absent-sibling");

        assert_eq!(resolve_optional("nts-absent-sibling"), None);
    }

    #[test]
    fn sibling_is_absent_when_nothing_selects_an_artifact() {
        let lock = GlobalStateLock::new();
        let _guards = without_bin_exe_env(&lock, "nts-absent-sibling");

        assert_eq!(sibling("nts-absent-sibling"), Sibling::Absent);
    }

    #[cfg(unix)]
    #[test]
    fn sibling_is_ready_when_the_artifact_reports_this_workspace_release() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().expect("tempdir");
        let path = write_version_stub(temp.path(), "nts-current-sibling", WORKSPACE_RELEASE);
        let _guard = EnvGuard::set(
            &lock,
            "CARGO_BIN_EXE_nts-current-sibling",
            path.to_str().expect("path"),
        );

        assert_eq!(sibling("nts-current-sibling"), Sibling::Ready(path));
    }

    #[cfg(unix)]
    #[test]
    fn sibling_reports_the_wrong_release_for_an_artifact_from_an_earlier_one() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().expect("tempdir");
        let path = write_version_stub(temp.path(), "nts-stale-sibling", "1.0.0");
        let _guard = EnvGuard::set(
            &lock,
            "CARGO_BIN_EXE_nts-stale-sibling",
            path.to_str().expect("path"),
        );

        assert_eq!(
            sibling("nts-stale-sibling"),
            Sibling::WrongRelease {
                reported: "1.0.0".to_string(),
                origin: Origin::CargoEnv("CARGO_BIN_EXE_nts-stale-sibling".to_string()),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn sibling_is_unidentifiable_when_version_cannot_be_read() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().expect("tempdir");
        write_exe(temp.path(), "nts-broken-sibling", "#!/bin/sh\nexit 1\n");
        let path = temp.path().join("nts-broken-sibling");
        let _guard = EnvGuard::set(
            &lock,
            "CARGO_BIN_EXE_nts-broken-sibling",
            path.to_str().expect("path"),
        );

        assert_eq!(
            sibling("nts-broken-sibling"),
            Sibling::Unidentifiable {
                origin: Origin::CargoEnv("CARGO_BIN_EXE_nts-broken-sibling".to_string()),
            }
        );
    }

    /// A dangling override is selected by something, so it must not read as
    /// "nothing was built" — the remedy is the variable, not a rebuild.
    #[test]
    fn a_dangling_override_is_unidentifiable_rather_than_absent() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().expect("tempdir");
        let missing = temp.path().join("nts-dangling-sibling");
        let _guard = EnvGuard::set(
            &lock,
            "CARGO_BIN_EXE_nts-dangling-sibling",
            missing.to_str().expect("path"),
        );

        let classified = sibling("nts-dangling-sibling");
        assert_eq!(
            classified,
            Sibling::Unidentifiable {
                origin: Origin::CargoEnv("CARGO_BIN_EXE_nts-dangling-sibling".to_string()),
            }
        );

        let unusable = classified
            .unusable("nts-dangling-sibling", "nils-nts")
            .expect("a dangling override is unusable");
        assert!(!unusable.skippable, "message={}", unusable.message);
        assert!(
            unusable
                .message
                .contains("CARGO_BIN_EXE_nts-dangling-sibling"),
            "message={}",
            unusable.message
        );
        assert!(
            !unusable.message.contains("cargo build -p"),
            "a rebuild cannot fix an override; message={}",
            unusable.message
        );
    }

    #[test]
    fn only_an_absent_sibling_is_skippable() {
        let absent = Sibling::Absent
            .unusable("gemini-cli", "nils-gemini-cli")
            .expect("an absent sibling is unusable");
        assert!(absent.skippable);
        assert!(
            absent
                .message
                .contains("cargo build -p nils-gemini-cli --bins"),
            "message={}",
            absent.message
        );

        let stale = Sibling::WrongRelease {
            reported: "1.25.13".to_string(),
            origin: Origin::ProfileDir,
        }
        .unusable("agent-docs", "nils-agent-docs")
        .expect("a stale sibling is unusable");
        assert!(!stale.skippable, "message={}", stale.message);
        assert!(
            stale.message.contains("1.25.13"),
            "message={}",
            stale.message
        );
        assert!(
            stale.message.contains(WORKSPACE_RELEASE),
            "message={}",
            stale.message
        );
        assert!(
            stale
                .message
                .contains("cargo build -p nils-agent-docs --bins"),
            "message={}",
            stale.message
        );
    }

    #[test]
    fn a_ready_sibling_has_no_unusable_reason() {
        let ready = Sibling::Ready(std::path::PathBuf::from("agent-docs"));

        assert!(ready.unusable("agent-docs", "nils-agent-docs").is_none());
    }

    /// Pin the `--version` format against a real workspace artifact rather than
    /// against a stub that writes the shape the parser expects.
    ///
    /// Vacuous where no sibling is built, which is the package-scoped case. It
    /// carries its weight in the lanes that build every binary: if clap's
    /// rendering or `nils_build_info::long_version` ever stopped putting the
    /// release in the second token, every sibling would classify as
    /// `Unidentifiable` and this fails instead of the parity and read-only
    /// suites quietly losing their subject.
    #[test]
    fn a_real_workspace_artifact_is_never_unidentifiable() {
        let lock = GlobalStateLock::new();
        let _guards = without_bin_exe_env(&lock, "agent-docs");

        match sibling("agent-docs") {
            Sibling::Absent => {}
            Sibling::Ready(_) => {}
            Sibling::WrongRelease { reported, .. } => assert!(
                reported.split('.').count() >= 2,
                "a real artifact should report a release-shaped version, got {reported}"
            ),
            Sibling::Unidentifiable { origin } => panic!(
                "a built agent-docs must answer --version with a release in its second token; \
                 origin={origin:?}"
            ),
        }
    }
}
