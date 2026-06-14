//! Validator for `agent-evidence-archive`'s `config/hosts.yaml`.
//!
//! Reuses the same `HostsConfig` shape as `agent-plan-archive` (host →
//! class/employer/retention/primary_identity classification) so the two
//! archives share host-classification semantics.
//!
//! Schema (v1):
//!
//! ```yaml
//! schema: agent-evidence-archive.hosts.v1  # optional but recommended
//! version: 1
//! hosts:
//!   github.com:
//!     class: personal              # `personal` or `employer`
//!     primary_identity: graysurf   # optional, free-form
//!   gitlab.example.com:
//!     class: employer
//!     employer: ExampleCorp        # required when class == employer
//!     retention: delete-on-termination
//! ```

use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ValidationWarning;

const SUPPORTED_VERSION: u32 = 1;
const VALID_RETENTION_VALUES: &[&str] = &["delete-on-termination"];

/// Parsed and validated `config/hosts.yaml` document.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostsConfig {
    pub version: u32,
    /// Ordered map from host FQDN to its classification entry. Order matches
    /// the input file for stable JSON output.
    pub hosts: IndexMap<String, HostEntry>,
}

/// Single host classification entry.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostEntry {
    pub class: HostClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub employer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_identity: Option<String>,
}

/// Whether a host is personal or employer-operated.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HostClass {
    Personal,
    Employer,
}

/// Output payload of a successful `validate-hosts` run.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostsValidationData {
    pub config: HostsConfig,
    pub summary: HostsSummary,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostsSummary {
    pub host_count: usize,
    pub personal_count: usize,
    pub employer_count: usize,
}

/// Successful validation result.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostsValidation {
    pub data: HostsValidationData,
    pub warnings: Vec<ValidationWarning>,
}

/// Validation failure modes for `config/hosts.yaml`.
#[derive(Debug, Error)]
pub enum HostsValidationError {
    #[error("hosts.yaml is empty")]
    Empty,
    #[error("hosts.yaml could not be parsed as YAML: {0}")]
    Parse(String),
    #[error("hosts.yaml version {found} is not supported (expected {SUPPORTED_VERSION})")]
    UnsupportedVersion { found: u32 },
    #[error("hosts.yaml top-level `hosts` is missing or empty")]
    MissingHosts,
    #[error(
        "host `{host}`: class `{found}` is not a recognised value (expected `personal` or `employer`)"
    )]
    UnknownClass { host: String, found: String },
    #[error("host `{host}`: class `employer` requires a non-empty `employer` field")]
    EmployerMissingName { host: String },
    #[error(
        "host `{host}`: retention value `{found}` is not recognised (expected one of {expected:?})"
    )]
    UnknownRetention {
        host: String,
        found: String,
        expected: &'static [&'static str],
    },
    #[error("host `{host}`: failed to parse entry: {message}")]
    EntryParse { host: String, message: String },
}

impl HostsValidationError {
    /// Stable error code emitted in the JSON envelope.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty => "hosts-empty",
            Self::Parse(_) => "hosts-parse-error",
            Self::UnsupportedVersion { .. } => "hosts-unsupported-version",
            Self::MissingHosts => "hosts-missing-hosts",
            Self::UnknownClass { .. } => "hosts-unknown-class",
            Self::EmployerMissingName { .. } => "hosts-employer-missing-name",
            Self::UnknownRetention { .. } => "hosts-unknown-retention",
            Self::EntryParse { .. } => "hosts-entry-parse-error",
        }
    }
}

/// Validate the raw `hosts.yaml` content.
pub fn validate_hosts_yaml(input: &str) -> Result<HostsValidation, HostsValidationError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(HostsValidationError::Empty);
    }

    let raw: RawHostsFile = serde_yaml_ng::from_str(input)
        .map_err(|err| HostsValidationError::Parse(err.to_string()))?;

    if raw.version != SUPPORTED_VERSION {
        return Err(HostsValidationError::UnsupportedVersion { found: raw.version });
    }

    let raw_hosts = raw.hosts.unwrap_or_default();
    if raw_hosts.is_empty() {
        return Err(HostsValidationError::MissingHosts);
    }

    let mut hosts: IndexMap<String, HostEntry> = IndexMap::with_capacity(raw_hosts.len());
    let mut personal_count = 0;
    let mut employer_count = 0;

    for (host, raw_entry) in raw_hosts.into_iter() {
        let entry = build_entry(&host, raw_entry)?;
        match entry.class {
            HostClass::Personal => personal_count += 1,
            HostClass::Employer => employer_count += 1,
        }
        hosts.insert(host, entry);
    }

    let host_count = hosts.len();
    let summary = HostsSummary {
        host_count,
        personal_count,
        employer_count,
    };

    let config = HostsConfig {
        version: raw.version,
        hosts,
    };

    Ok(HostsValidation {
        data: HostsValidationData { config, summary },
        warnings: Vec::new(),
    })
}

fn build_entry(host: &str, raw: serde_yaml_ng::Value) -> Result<HostEntry, HostsValidationError> {
    let parsed: RawHostEntry =
        serde_yaml_ng::from_value(raw).map_err(|err| HostsValidationError::EntryParse {
            host: host.to_string(),
            message: err.to_string(),
        })?;

    let class = match parsed.class.as_str() {
        "personal" => HostClass::Personal,
        "employer" => HostClass::Employer,
        other => {
            return Err(HostsValidationError::UnknownClass {
                host: host.to_string(),
                found: other.to_string(),
            });
        }
    };

    if matches!(class, HostClass::Employer) && parsed.employer.as_deref().is_none_or(str::is_empty)
    {
        return Err(HostsValidationError::EmployerMissingName {
            host: host.to_string(),
        });
    }

    if let Some(retention) = parsed.retention.as_deref()
        && !VALID_RETENTION_VALUES.contains(&retention)
    {
        return Err(HostsValidationError::UnknownRetention {
            host: host.to_string(),
            found: retention.to_string(),
            expected: VALID_RETENTION_VALUES,
        });
    }

    Ok(HostEntry {
        class,
        employer: parsed.employer,
        retention: parsed.retention,
        primary_identity: parsed.primary_identity,
    })
}

#[derive(Debug, Deserialize)]
struct RawHostsFile {
    #[serde(default = "default_version")]
    version: u32,
    hosts: Option<BTreeMap<String, serde_yaml_ng::Value>>,
    // Optional schema-tag — kept for forward compatibility and ignored.
    #[allow(dead_code)]
    #[serde(default)]
    schema: Option<String>,
}

fn default_version() -> u32 {
    SUPPORTED_VERSION
}

#[derive(Debug, Deserialize)]
struct RawHostEntry {
    class: String,
    #[serde(default)]
    employer: Option<String>,
    #[serde(default)]
    retention: Option<String>,
    #[serde(default)]
    primary_identity: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_only_validates() {
        let input = r"
schema: agent-evidence-archive.hosts.v1
version: 1
hosts:
  github.com:
    class: personal
    primary_identity: graysurf
  gitlab.com:
    class: personal
";
        let v = validate_hosts_yaml(input).expect("validation");
        assert_eq!(v.data.summary.host_count, 2);
        assert_eq!(v.data.summary.personal_count, 2);
        assert_eq!(v.data.summary.employer_count, 0);
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn employer_requires_name() {
        let input = r"
version: 1
hosts:
  gitlab.example.com:
    class: employer
";
        let err = validate_hosts_yaml(input).expect_err("missing employer name");
        assert_eq!(err.code(), "hosts-employer-missing-name");
    }

    #[test]
    fn unknown_class_rejected() {
        let input = r"
version: 1
hosts:
  github.com:
    class: visitor
";
        let err = validate_hosts_yaml(input).expect_err("unknown class");
        assert_eq!(err.code(), "hosts-unknown-class");
    }

    #[test]
    fn unknown_retention_rejected() {
        let input = r"
version: 1
hosts:
  gitlab.example.com:
    class: employer
    employer: ExampleCorp
    retention: maybe-keep
";
        let err = validate_hosts_yaml(input).expect_err("unknown retention");
        assert_eq!(err.code(), "hosts-unknown-retention");
    }

    #[test]
    fn unsupported_version_rejected() {
        let input = r"
version: 99
hosts:
  github.com:
    class: personal
";
        let err = validate_hosts_yaml(input).expect_err("unsupported version");
        assert_eq!(err.code(), "hosts-unsupported-version");
    }

    #[test]
    fn empty_input_rejected() {
        let err = validate_hosts_yaml("   \n").expect_err("empty");
        assert_eq!(err.code(), "hosts-empty");
    }

    #[test]
    fn missing_hosts_rejected() {
        let err = validate_hosts_yaml("version: 1\n").expect_err("missing hosts");
        assert_eq!(err.code(), "hosts-missing-hosts");
    }

    #[test]
    fn employer_mixed_validates() {
        let input = r"
version: 1
hosts:
  github.com:
    class: personal
  gitlab.example.com:
    class: employer
    employer: ExampleCorp
    retention: delete-on-termination
";
        let v = validate_hosts_yaml(input).expect("validation");
        assert_eq!(v.data.summary.host_count, 2);
        assert_eq!(v.data.summary.personal_count, 1);
        assert_eq!(v.data.summary.employer_count, 1);
    }
}
