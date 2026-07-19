//! `agent-runtime doctor --class version-alignment` — version-policy gate.
//!
//! Schema v1 preserves exact `pinned_tag` alignment. Schema v2 separates the
//! minimum supported release from the exact validated release: hosts below the
//! minimum block, hosts above the validated release warn, and required CLI
//! floors remain independently blocking.
//!
//! It is a version-number gate only. It does not diff surfaces between two
//! tags, nor query a registry for newer releases.

use super::version::Version;
use super::{DoctorFinding, DoctorSeverity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const CLASS: &str = "version-alignment";
pub const PIN_SCHEMA_VERSION: u32 = 1;
pub const VERSION_POLICY_SCHEMA_VERSION: u32 = 2;
pub const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[PIN_SCHEMA_VERSION, VERSION_POLICY_SCHEMA_VERSION];
pub const HOST_CHECK: &str = "version-alignment.host";
pub const MINIMUM_CHECK: &str = "version-alignment.minimum";
pub const VALIDATED_CHECK: &str = "version-alignment.validated";
pub const REQUIRED_CLI_CHECK: &str = "version-alignment.required-cli";
pub const ACCEPTANCE_BOUNDARY: &str = "version-number gate only; does not diff surfaces between tags or query a registry for newer releases";

/// Schema-v1 pin manifest retained as the stable public evaluation input.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PinManifest {
    pub schema_version: u32,
    pub nils_cli: NilsCliPin,
    #[serde(default)]
    pub required_clis: Vec<RequiredCli>,
}

/// Schema-v1 nils-cli pin retained for source compatibility with published
/// `nils-agent-runtime` consumers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NilsCliPin {
    pub pinned_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NilsCliPolicy {
    Exact {
        pinned_tag: String,
    },
    Compatibility {
        minimum_supported_tag: String,
        validated_tag: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedPinManifest {
    schema_version: u32,
    nils_cli: NilsCliPolicy,
    required_clis: Vec<RequiredCli>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RequiredCli {
    pub bin: String,
    pub min: String,
}

#[derive(Debug, Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVersionPolicyManifest {
    schema_version: u32,
    nils_cli: RawNilsCliPolicy,
    #[serde(default)]
    required_clis: Vec<RequiredCli>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNilsCliPolicy {
    pinned_tag: Option<String>,
    minimum_supported_tag: Option<String>,
    validated_tag: Option<String>,
    release_sha256: Option<BTreeMap<String, String>>,
}

/// Pure inputs to [`evaluate`]; the I/O layer gathers `--version` outputs.
pub struct AlignmentInputs<'a> {
    pub manifest: &'a PinManifest,
    /// Raw host version string (e.g. compiled `CARGO_PKG_VERSION`).
    pub host_raw: &'a str,
    /// `bin` -> raw `<bin> --version` output (or an error string if missing).
    pub required_raw: &'a BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionAlignmentReport {
    /// Exact schema-v1 pin. Empty for schema v2; use [`Self::validated_tag`]
    /// and [`Self::minimum_supported_tag`] for compatibility policies.
    pub pinned_tag: String,
    pub host_observed: Option<String>,
    pub items: Vec<AlignmentItem>,
    pub findings: Vec<DoctorFinding>,
    pub acceptance_boundary: Option<String>,
}

impl VersionAlignmentReport {
    pub fn policy_schema_version(&self) -> u32 {
        if self
            .items
            .iter()
            .any(|item| matches!(item.check, MINIMUM_CHECK | VALIDATED_CHECK))
        {
            VERSION_POLICY_SCHEMA_VERSION
        } else {
            PIN_SCHEMA_VERSION
        }
    }

    pub fn schema_pinned_tag(&self) -> Option<&str> {
        (self.policy_schema_version() == PIN_SCHEMA_VERSION).then_some(self.pinned_tag.as_str())
    }

    pub fn minimum_supported_tag(&self) -> Option<&str> {
        self.items
            .iter()
            .find(|item| item.check == MINIMUM_CHECK)
            .and_then(|item| item.expected.strip_prefix(">= "))
    }

    pub fn validated_tag(&self) -> Option<&str> {
        self.items
            .iter()
            .find(|item| item.check == VALIDATED_CHECK)
            .map(|item| item.expected.as_str())
    }
}

#[derive(Serialize)]
struct VersionAlignmentReportWire<'a> {
    policy_schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pinned_tag: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum_supported_tag: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validated_tag: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_observed: Option<&'a str>,
    items: &'a [AlignmentItem],
    findings: &'a [DoctorFinding],
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_boundary: Option<&'a str>,
}

impl Serialize for VersionAlignmentReport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        VersionAlignmentReportWire {
            policy_schema_version: self.policy_schema_version(),
            pinned_tag: self.schema_pinned_tag(),
            minimum_supported_tag: self.minimum_supported_tag(),
            validated_tag: self.validated_tag(),
            host_observed: self.host_observed.as_deref(),
            items: &self.items,
            findings: &self.findings,
            acceptance_boundary: self.acceptance_boundary.as_deref(),
        }
        .serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlignmentItem {
    pub check: &'static str,
    pub target: String,
    pub expected: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    pub severity: DoctorSeverity,
}

/// Classify host + required-CLI alignment. Pure; no process spawning or I/O.
pub fn evaluate(inputs: &AlignmentInputs) -> VersionAlignmentReport {
    let manifest = NormalizedPinManifest {
        schema_version: inputs.manifest.schema_version,
        nils_cli: NilsCliPolicy::Exact {
            pinned_tag: inputs.manifest.nils_cli.pinned_tag.clone(),
        },
        required_clis: inputs.manifest.required_clis.clone(),
    };
    evaluate_normalized(&NormalizedAlignmentInputs {
        manifest: &manifest,
        host_raw: inputs.host_raw,
        required_raw: inputs.required_raw,
    })
}

struct NormalizedAlignmentInputs<'a> {
    manifest: &'a NormalizedPinManifest,
    host_raw: &'a str,
    required_raw: &'a BTreeMap<String, String>,
}

fn evaluate_normalized(inputs: &NormalizedAlignmentInputs) -> VersionAlignmentReport {
    let manifest = inputs.manifest;
    let mut items = Vec::new();
    let mut findings = Vec::new();
    let host = Version::parse(inputs.host_raw);
    let host_observed = host.map(|v| v.to_string());

    let pinned_tag = match &manifest.nils_cli {
        NilsCliPolicy::Exact { pinned_tag } => {
            let pinned = Version::parse(pinned_tag);
            let (severity, message) = match (pinned, host) {
                (None, _) => (
                    DoctorSeverity::Block,
                    format!("pinned_tag `{pinned_tag}` is not a parseable version"),
                ),
                (Some(_), None) => (
                    DoctorSeverity::Block,
                    format!(
                        "host version `{}` is not parseable; cannot verify alignment",
                        inputs.host_raw
                    ),
                ),
                (Some(pin), Some(found)) if found == pin => (
                    DoctorSeverity::Ok,
                    format!("host is aligned with pinned {pinned_tag}"),
                ),
                (Some(_), Some(found)) => (
                    DoctorSeverity::Block,
                    format!("host {found} drifted from pinned {pinned_tag}"),
                ),
            };
            items.push(AlignmentItem {
                check: HOST_CHECK,
                target: "agent-runtime".to_string(),
                expected: pinned_tag.clone(),
                observed: host_observed.clone(),
                severity,
            });
            if severity != DoctorSeverity::Ok {
                findings.push(DoctorFinding::block(
                    "host", HOST_CHECK, None, None, message,
                ));
            }
            Some(pinned_tag.clone())
        }
        NilsCliPolicy::Compatibility {
            minimum_supported_tag,
            validated_tag,
        } => {
            let minimum = Version::parse(minimum_supported_tag);
            let validated = Version::parse(validated_tag);

            let (minimum_severity, minimum_message) = match (minimum, host) {
                (None, _) => (
                    DoctorSeverity::Block,
                    format!(
                        "minimum_supported_tag `{minimum_supported_tag}` is not a parseable version"
                    ),
                ),
                (Some(_), None) => (
                    DoctorSeverity::Block,
                    format!(
                        "host version `{}` is not parseable; cannot verify compatibility",
                        inputs.host_raw
                    ),
                ),
                (Some(floor), Some(found)) if found >= floor => (
                    DoctorSeverity::Ok,
                    format!("host {found} meets minimum supported {minimum_supported_tag}"),
                ),
                (Some(_), Some(found)) => (
                    DoctorSeverity::Block,
                    format!("host {found} is below minimum supported {minimum_supported_tag}"),
                ),
            };
            items.push(AlignmentItem {
                check: MINIMUM_CHECK,
                target: "agent-runtime".to_string(),
                expected: format!(">= {minimum_supported_tag}"),
                observed: host_observed.clone(),
                severity: minimum_severity,
            });
            if minimum_severity != DoctorSeverity::Ok {
                findings.push(DoctorFinding::block(
                    "host",
                    MINIMUM_CHECK,
                    None,
                    None,
                    minimum_message,
                ));
            }

            let (validated_severity, validated_message) = match (validated, host) {
                (None, _) => (
                    DoctorSeverity::Block,
                    format!("validated_tag `{validated_tag}` is not a parseable version"),
                ),
                (Some(_), None) => (
                    DoctorSeverity::Block,
                    format!(
                        "host version `{}` is not parseable; cannot compare with validated release",
                        inputs.host_raw
                    ),
                ),
                (Some(validated), Some(found)) if found > validated => (
                    DoctorSeverity::Warn,
                    format!(
                        "host {found} is admitted by the compatibility floor but is not formally validated beyond {validated_tag}"
                    ),
                ),
                (Some(validated), Some(found)) if found == validated => (
                    DoctorSeverity::Ok,
                    format!("host {found} is the exact validated release {validated_tag}"),
                ),
                (Some(_), Some(found)) if minimum_severity == DoctorSeverity::Block => (
                    DoctorSeverity::Ok,
                    format!(
                        "host {found} is below validated {validated_tag}; compatibility admission is blocked by the minimum check"
                    ),
                ),
                (Some(_), Some(found)) => (
                    DoctorSeverity::Ok,
                    format!(
                        "host {found} is within the supported range; formal snapshots use {validated_tag}"
                    ),
                ),
            };
            items.push(AlignmentItem {
                check: VALIDATED_CHECK,
                target: "agent-runtime".to_string(),
                expected: validated_tag.clone(),
                observed: host_observed.clone(),
                severity: validated_severity,
            });
            match validated_severity {
                DoctorSeverity::Ok => {}
                DoctorSeverity::Warn => findings.push(DoctorFinding::warn(
                    "host",
                    VALIDATED_CHECK,
                    None,
                    None,
                    validated_message,
                )),
                DoctorSeverity::Block => findings.push(DoctorFinding::block(
                    "host",
                    VALIDATED_CHECK,
                    None,
                    None,
                    validated_message,
                )),
            }

            None
        }
    };

    // Required CLIs: each must meet its `min` floor (existing >= semantics).
    for cli in &manifest.required_clis {
        let raw = inputs
            .required_raw
            .get(&cli.bin)
            .map(String::as_str)
            .unwrap_or("");
        let min = Version::parse(&cli.min);
        let observed = Version::parse(raw);
        let observed_str = observed.map(|v| v.to_string());
        let (severity, message) = match (min, observed) {
            (None, _) => (
                DoctorSeverity::Block,
                format!(
                    "required_clis[{}].min `{}` is not a parseable version",
                    cli.bin, cli.min
                ),
            ),
            (Some(_), None) => (
                DoctorSeverity::Block,
                format!(
                    "{} not found on PATH or `--version` unparseable: {:?}",
                    cli.bin, raw
                ),
            ),
            (Some(floor), Some(found)) if found >= floor => (
                DoctorSeverity::Ok,
                format!("{} {} meets floor {}", cli.bin, found, cli.min),
            ),
            (Some(_), Some(found)) => (
                DoctorSeverity::Block,
                format!("{} {} is below required floor {}", cli.bin, found, cli.min),
            ),
        };
        items.push(AlignmentItem {
            check: REQUIRED_CLI_CHECK,
            target: cli.bin.clone(),
            expected: cli.min.clone(),
            observed: observed_str,
            severity,
        });
        if severity != DoctorSeverity::Ok {
            findings.push(DoctorFinding::block(
                &cli.bin,
                REQUIRED_CLI_CHECK,
                None,
                None,
                message,
            ));
        }
    }

    VersionAlignmentReport {
        pinned_tag: pinned_tag.unwrap_or_default(),
        host_observed,
        items,
        findings,
        acceptance_boundary: Some(ACCEPTANCE_BOUNDARY.to_string()),
    }
}

#[derive(Debug, Error)]
pub enum VersionAlignmentError {
    #[error("--class version-alignment requires --pin <manifest>")]
    MissingPin,
    #[error("missing pin manifest: {path}")]
    Missing { path: PathBuf },
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("schema_version mismatch in {path}: supported schema versions 1 and 2, got {found}")]
    SchemaVersion {
        path: PathBuf,
        /// Historical schema-v1 scalar retained for source compatibility.
        /// Use [`VersionAlignmentError::supported_schema_versions`] for the
        /// complete accepted set.
        expected: u32,
        found: u32,
    },
}

impl VersionAlignmentError {
    /// Returns the complete accepted schema set for schema-version errors.
    pub fn supported_schema_versions(&self) -> Option<&'static [u32]> {
        matches!(self, Self::SchemaVersion { .. }).then_some(SUPPORTED_SCHEMA_VERSIONS)
    }
}

/// Read + parse the pin manifest (YAML or JSON), gather `<bin> --version`
/// for each `required_clis[]` entry from PATH, then classify alignment of
/// `host_version` and those binaries. Schema v1 routes through the stable
/// public [`evaluate`] contract; schema v2 uses a private normalized policy.
pub fn check(
    pin_path: &Path,
    host_version: &str,
) -> Result<VersionAlignmentReport, VersionAlignmentError> {
    if !pin_path.exists() {
        return Err(VersionAlignmentError::Missing {
            path: pin_path.to_path_buf(),
        });
    }
    let raw = std::fs::read_to_string(pin_path).map_err(|source| VersionAlignmentError::Io {
        path: pin_path.to_path_buf(),
        source,
    })?;
    let header: SchemaHeader =
        serde_yaml_ng::from_str(&raw).map_err(|source| VersionAlignmentError::Parse {
            path: pin_path.to_path_buf(),
            source,
        })?;
    match header.schema_version {
        PIN_SCHEMA_VERSION => {
            let manifest: PinManifest =
                serde_yaml_ng::from_str(&raw).map_err(|source| VersionAlignmentError::Parse {
                    path: pin_path.to_path_buf(),
                    source,
                })?;
            validate_required_cli_names(&manifest.required_clis)
                .map_err(|message| invalid_policy_error(pin_path, message))?;
            let required_raw = probe_required_clis(&manifest.required_clis);
            Ok(evaluate(&AlignmentInputs {
                manifest: &manifest,
                host_raw: host_version,
                required_raw: &required_raw,
            }))
        }
        VERSION_POLICY_SCHEMA_VERSION => {
            let raw_manifest: RawVersionPolicyManifest =
                serde_yaml_ng::from_str(&raw).map_err(|source| VersionAlignmentError::Parse {
                    path: pin_path.to_path_buf(),
                    source,
                })?;
            let manifest = normalize_version_policy(raw_manifest, pin_path)?;
            let required_raw = probe_required_clis(&manifest.required_clis);
            Ok(evaluate_normalized(&NormalizedAlignmentInputs {
                manifest: &manifest,
                host_raw: host_version,
                required_raw: &required_raw,
            }))
        }
        found => Err(VersionAlignmentError::SchemaVersion {
            path: pin_path.to_path_buf(),
            expected: PIN_SCHEMA_VERSION,
            found,
        }),
    }
}

fn probe_required_clis(required_clis: &[RequiredCli]) -> BTreeMap<String, String> {
    required_clis
        .iter()
        .map(|cli| {
            let raw = super::version::run_probe_command(&format!("{} --version", cli.bin));
            (cli.bin.clone(), raw)
        })
        .collect()
}

fn normalize_version_policy(
    raw: RawVersionPolicyManifest,
    path: &Path,
) -> Result<NormalizedPinManifest, VersionAlignmentError> {
    let invalid = |message: String| invalid_policy_error(path, message);
    validate_required_clis(&raw.required_clis).map_err(&invalid)?;

    debug_assert_eq!(raw.schema_version, VERSION_POLICY_SCHEMA_VERSION);
    if raw.nils_cli.pinned_tag.is_some() {
        return Err(invalid(
            "schema v2 replaces nils_cli.pinned_tag with minimum_supported_tag and validated_tag"
                .to_string(),
        ));
    }
    let minimum_supported_tag = raw
        .nils_cli
        .minimum_supported_tag
        .ok_or_else(|| invalid("schema v2 requires nils_cli.minimum_supported_tag".to_string()))?;
    let validated_tag = raw
        .nils_cli
        .validated_tag
        .ok_or_else(|| invalid("schema v2 requires nils_cli.validated_tag".to_string()))?;
    let minimum =
        parse_stable_tag("minimum_supported_tag", &minimum_supported_tag).map_err(&invalid)?;
    let validated = parse_stable_tag("validated_tag", &validated_tag).map_err(&invalid)?;
    if minimum > validated {
        return Err(invalid(format!(
            "minimum_supported_tag {minimum_supported_tag} must not exceed validated_tag {validated_tag}"
        )));
    }
    validate_release_digests(raw.nils_cli.release_sha256.as_ref()).map_err(&invalid)?;

    Ok(NormalizedPinManifest {
        schema_version: raw.schema_version,
        nils_cli: NilsCliPolicy::Compatibility {
            minimum_supported_tag,
            validated_tag,
        },
        required_clis: raw.required_clis,
    })
}

fn invalid_policy_error(path: &Path, message: String) -> VersionAlignmentError {
    VersionAlignmentError::Parse {
        path: path.to_path_buf(),
        source: <serde_yaml_ng::Error as serde::de::Error>::custom(format!(
            "invalid version policy: {message}"
        )),
    }
}

fn parse_stable_tag(field: &str, raw: &str) -> Result<Version, String> {
    let Some(version) = Version::parse(raw) else {
        return Err(format!(
            "{field} `{raw}` is not a stable tag of the form vMAJOR.MINOR.PATCH"
        ));
    };
    if format!("v{version}") != raw {
        return Err(format!(
            "{field} `{raw}` is not a stable tag of the form vMAJOR.MINOR.PATCH"
        ));
    }
    Ok(version)
}

fn parse_required_floor(bin: &str, raw: &str) -> Result<Version, String> {
    let Some(version) = Version::parse(raw) else {
        return Err(format!(
            "required_clis[{bin}].min `{raw}` is not a stable MAJOR.MINOR.PATCH version"
        ));
    };
    if version.to_string() != raw {
        return Err(format!(
            "required_clis[{bin}].min `{raw}` is not a stable MAJOR.MINOR.PATCH version"
        ));
    }
    Ok(version)
}

fn validate_release_digests(digests: Option<&BTreeMap<String, String>>) -> Result<(), String> {
    let Some(digests) = digests else {
        return Err(
            "schema v2 requires nils_cli.release_sha256 digests for validated_tag".to_string(),
        );
    };
    for target in ["linux_amd64", "linux_arm64"] {
        let Some(digest) = digests.get(target) else {
            return Err(format!(
                "schema v2 requires nils_cli.release_sha256.{target} for validated_tag"
            ));
        };
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "nils_cli.release_sha256.{target} must be a 64-character SHA256 digest"
            ));
        }
    }
    Ok(())
}

fn validate_required_clis(required: &[RequiredCli]) -> Result<(), String> {
    validate_required_cli_names(required)?;
    let mut seen = BTreeSet::new();
    for cli in required {
        if !seen.insert(cli.bin.as_str()) {
            return Err(format!("duplicate required_clis entry for `{}`", cli.bin));
        }
        parse_required_floor(&cli.bin, &cli.min)?;
    }
    Ok(())
}

fn validate_required_cli_names(required: &[RequiredCli]) -> Result<(), String> {
    for cli in required {
        if cli.bin.is_empty()
            || !cli
                .bin
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(format!(
                "required_clis bin `{}` must be a non-empty executable name",
                cli.bin
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(pinned: &str, required: &[(&str, &str)]) -> PinManifest {
        PinManifest {
            schema_version: PIN_SCHEMA_VERSION,
            nils_cli: NilsCliPin {
                pinned_tag: pinned.to_string(),
            },
            required_clis: required
                .iter()
                .map(|(bin, min)| RequiredCli {
                    bin: bin.to_string(),
                    min: min.to_string(),
                })
                .collect(),
        }
    }

    fn compatibility_manifest(
        minimum_supported_tag: &str,
        validated_tag: &str,
        required: &[(&str, &str)],
    ) -> NormalizedPinManifest {
        NormalizedPinManifest {
            schema_version: VERSION_POLICY_SCHEMA_VERSION,
            nils_cli: NilsCliPolicy::Compatibility {
                minimum_supported_tag: minimum_supported_tag.to_string(),
                validated_tag: validated_tag.to_string(),
            },
            required_clis: required
                .iter()
                .map(|(bin, min)| RequiredCli {
                    bin: bin.to_string(),
                    min: min.to_string(),
                })
                .collect(),
        }
    }

    fn eval(m: &PinManifest, host_raw: &str, reqs: &[(&str, &str)]) -> VersionAlignmentReport {
        let required_raw: BTreeMap<String, String> = reqs
            .iter()
            .map(|(bin, raw)| (bin.to_string(), raw.to_string()))
            .collect();
        evaluate(&AlignmentInputs {
            manifest: m,
            host_raw,
            required_raw: &required_raw,
        })
    }

    fn eval_compatibility(
        m: &NormalizedPinManifest,
        host_raw: &str,
        reqs: &[(&str, &str)],
    ) -> VersionAlignmentReport {
        let required_raw: BTreeMap<String, String> = reqs
            .iter()
            .map(|(bin, raw)| (bin.to_string(), raw.to_string()))
            .collect();
        evaluate_normalized(&NormalizedAlignmentInputs {
            manifest: m,
            host_raw,
            required_raw: &required_raw,
        })
    }

    #[test]
    fn aligned_host_is_ok() {
        let m = manifest("v0.17.7", &[]);
        let report = eval(&m, "agent-runtime 0.17.7 (v0.17.7, rustc 1.96.0)", &[]);
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].check, HOST_CHECK);
        assert_eq!(report.items[0].severity, DoctorSeverity::Ok);
        assert!(
            report.findings.is_empty(),
            "findings: {:?}",
            report.findings
        );
    }

    #[test]
    fn host_ahead_of_pin_blocks() {
        // The core new capability: the existing probe treats this as OK.
        let m = manifest("v0.17.6", &[]);
        let report = eval(&m, "agent-runtime 0.17.7 (v0.17.7)", &[]);
        assert_eq!(report.items[0].severity, DoctorSeverity::Block);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, DoctorSeverity::Block);
        assert_eq!(report.findings[0].check, HOST_CHECK);
    }

    #[test]
    fn host_behind_pin_blocks() {
        let m = manifest("v0.18.0", &[]);
        let report = eval(&m, "agent-runtime 0.17.7", &[]);
        assert_eq!(report.items[0].severity, DoctorSeverity::Block);
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn host_unparseable_blocks_fail_closed() {
        let m = manifest("v0.17.7", &[]);
        let report = eval(&m, "agent-runtime (no version available)", &[]);
        assert_eq!(report.items[0].severity, DoctorSeverity::Block);
    }

    #[test]
    fn compatibility_host_ahead_warns_without_blocking() {
        let m = compatibility_manifest("v1.20.0", "v1.24.3", &[]);
        let report = eval_compatibility(&m, "agent-runtime 1.24.4 (v1.24.4)", &[]);
        assert_eq!(report.items.len(), 2);
        assert_eq!(report.items[0].check, MINIMUM_CHECK);
        assert_eq!(report.items[0].severity, DoctorSeverity::Ok);
        assert_eq!(report.items[1].check, VALIDATED_CHECK);
        assert_eq!(report.items[1].severity, DoctorSeverity::Warn);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, DoctorSeverity::Warn);
    }

    #[test]
    fn compatibility_host_between_minimum_and_validated_is_ok() {
        let m = compatibility_manifest("v1.20.0", "v1.24.3", &[]);
        let report = eval_compatibility(&m, "agent-runtime 1.22.0", &[]);
        assert!(
            report
                .items
                .iter()
                .all(|item| item.severity == DoctorSeverity::Ok),
            "items: {:?}",
            report.items
        );
        assert!(report.findings.is_empty());
    }

    #[test]
    fn compatibility_host_below_minimum_blocks() {
        let m = compatibility_manifest("v1.20.0", "v1.24.3", &[]);
        let report = eval_compatibility(&m, "agent-runtime 1.19.9", &[]);
        assert_eq!(report.items[0].check, MINIMUM_CHECK);
        assert_eq!(report.items[0].severity, DoctorSeverity::Block);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, DoctorSeverity::Block);
    }

    #[test]
    fn required_cli_meets_min_is_ok() {
        let m = manifest("v0.17.7", &[("plan-issue", "0.17.4")]);
        let report = eval(
            &m,
            "0.17.7",
            &[("plan-issue", "plan-issue 0.17.7 (v0.17.7)")],
        );
        assert_eq!(report.items.len(), 2);
        assert!(
            report
                .items
                .iter()
                .all(|i| i.severity == DoctorSeverity::Ok),
            "items: {:?}",
            report.items
        );
        assert!(report.findings.is_empty());
    }

    #[test]
    fn required_cli_below_min_blocks() {
        let m = manifest("v0.17.7", &[("forge-cli", "0.16.0")]);
        let report = eval(&m, "0.17.7", &[("forge-cli", "forge-cli 0.15.0 (v0.15.0)")]);
        let item = report
            .items
            .iter()
            .find(|i| i.target == "forge-cli")
            .expect("forge-cli item present");
        assert_eq!(item.check, REQUIRED_CLI_CHECK);
        assert_eq!(item.severity, DoctorSeverity::Block);
        assert!(report.findings.iter().any(|f| f.product == "forge-cli"));
    }

    #[test]
    fn required_cli_missing_blocks() {
        let m = manifest("v0.17.7", &[("ghost-cli", "0.1.0")]);
        let report = eval(
            &m,
            "0.17.7",
            &[(
                "ghost-cli",
                "failed to run `ghost-cli --version`: No such file or directory (os error 2)",
            )],
        );
        let item = report
            .items
            .iter()
            .find(|i| i.target == "ghost-cli")
            .expect("ghost-cli item present");
        assert_eq!(item.severity, DoctorSeverity::Block);
    }
}
