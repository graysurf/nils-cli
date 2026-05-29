//! `agent-runtime doctor --class version-alignment` — surface-pin drift gate.
//!
//! Answers a single question: *is the host `agent-runtime` still on the
//! tag a downstream snapshot pinned, and do the downstream-consumed CLIs
//! still meet their version floors?* Unlike the existing version probe —
//! which treats "host ahead of floor" as OK — this class blocks on **any**
//! deviation from `pinned_tag` (ahead or behind), because a silent
//! `brew upgrade` past the pin is exactly the failure mode it guards.
//!
//! It is a version-number gate only. It does not diff surfaces between two
//! tags, nor query a registry for newer releases.

use super::version::Version;
use super::{DoctorFinding, DoctorSeverity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const CLASS: &str = "version-alignment";
pub const PIN_SCHEMA_VERSION: u32 = 1;
pub const HOST_CHECK: &str = "version-alignment.host";
pub const REQUIRED_CLI_CHECK: &str = "version-alignment.required-cli";
pub const ACCEPTANCE_BOUNDARY: &str = "version-number gate only; does not diff surfaces between tags or query a registry for newer releases";

/// Strawman pin manifest (`<pin-spec>`), parsed from YAML or JSON.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PinManifest {
    pub schema_version: u32,
    pub nils_cli: NilsCliPin,
    #[serde(default)]
    pub required_clis: Vec<RequiredCli>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NilsCliPin {
    /// Tag `agent-runtime --version` must report, e.g. `v0.17.7`.
    pub pinned_tag: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RequiredCli {
    pub bin: String,
    pub min: String,
}

/// Pure inputs to [`evaluate`]; the I/O layer gathers `--version` outputs.
pub struct AlignmentInputs<'a> {
    pub manifest: &'a PinManifest,
    /// Raw host version string (e.g. compiled `CARGO_PKG_VERSION`).
    pub host_raw: &'a str,
    /// `bin` -> raw `<bin> --version` output (or an error string if missing).
    pub required_raw: &'a BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionAlignmentReport {
    pub pinned_tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_observed: Option<String>,
    pub items: Vec<AlignmentItem>,
    pub findings: Vec<DoctorFinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance_boundary: Option<String>,
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
    let manifest = inputs.manifest;
    let mut items = Vec::new();
    let mut findings = Vec::new();

    // Host check: any deviation from `pinned_tag` blocks; unparseable
    // input (manifest or host) fails closed.
    let pinned = Version::parse(&manifest.nils_cli.pinned_tag);
    let host = Version::parse(inputs.host_raw);
    let host_observed = host.map(|v| v.to_string());
    let (host_severity, host_message) = match (pinned, host) {
        (None, _) => (
            DoctorSeverity::Block,
            format!(
                "pinned_tag `{}` is not a parseable version",
                manifest.nils_cli.pinned_tag
            ),
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
            format!(
                "host is aligned with pinned {}",
                manifest.nils_cli.pinned_tag
            ),
        ),
        (Some(_), Some(found)) => (
            DoctorSeverity::Block,
            format!(
                "host {} drifted from pinned {}",
                found, manifest.nils_cli.pinned_tag
            ),
        ),
    };
    items.push(AlignmentItem {
        check: HOST_CHECK,
        target: "agent-runtime".to_string(),
        expected: manifest.nils_cli.pinned_tag.clone(),
        observed: host_observed.clone(),
        severity: host_severity,
    });
    if host_severity != DoctorSeverity::Ok {
        findings.push(DoctorFinding::block(
            "host",
            HOST_CHECK,
            None,
            None,
            host_message,
        ));
    }

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
        pinned_tag: manifest.nils_cli.pinned_tag.clone(),
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
    #[error("schema_version mismatch in {path}: expected {expected}, got {found}")]
    SchemaVersion {
        path: PathBuf,
        expected: u32,
        found: u32,
    },
}

/// Read + parse the pin manifest (YAML or JSON), gather `<bin> --version`
/// for each `required_clis[]` entry from PATH, then classify alignment of
/// `host_version` and those binaries via [`evaluate`].
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
    let manifest: PinManifest =
        serde_yaml_ng::from_str(&raw).map_err(|source| VersionAlignmentError::Parse {
            path: pin_path.to_path_buf(),
            source,
        })?;
    if manifest.schema_version != PIN_SCHEMA_VERSION {
        return Err(VersionAlignmentError::SchemaVersion {
            path: pin_path.to_path_buf(),
            expected: PIN_SCHEMA_VERSION,
            found: manifest.schema_version,
        });
    }

    let mut required_raw = BTreeMap::new();
    for cli in &manifest.required_clis {
        let raw = super::version::run_probe_command(&format!("{} --version", cli.bin));
        required_raw.insert(cli.bin.clone(), raw);
    }

    Ok(evaluate(&AlignmentInputs {
        manifest: &manifest,
        host_raw: host_version,
        required_raw: &required_raw,
    }))
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
