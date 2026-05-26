//! Validator for the per-plan `metadata.yaml` written into each archived
//! plan folder at
//! `plans/<host>/<org-or-group-path>/<repo>/<YYYY-MM-DD>-<slug>/metadata.yaml`.
//!
//! Schema (v1):
//!
//! ```yaml
//! version: 1
//! source:
//!   host: github.com
//!   org_or_group_path: graysurf
//!   repo: agent-runtime-kit
//!   branch: main
//!   archive_commit: a2e8f227e...
//!   original_path: docs/plans/2026-05-27-plan-archive-runtime-kit/
//! captured_classification:
//!   class: personal
//!   primary_identity: graysurf
//!   employer: null
//!   retention: null
//! refs:
//!   issue: https://github.com/graysurf/agent-runtime-kit/issues/126
//!   pr: https://github.com/graysurf/agent-runtime-kit/pull/127
//! ```
//!
//! `captured_classification` may be omitted for pre-classification plans
//! archived before the captured-classification field was introduced.
//! Validators emit a `metadata-captured-classification-missing` warning
//! in that case but still accept the file.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{ValidationWarning, hosts::HostClass};

const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataConfig {
    pub version: u32,
    pub source: MetadataSource,
    pub captured_classification: Option<MetadataClassification>,
    pub refs: MetadataRefs,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataSource {
    pub host: String,
    pub org_or_group_path: String,
    pub repo: String,
    pub branch: String,
    pub archive_commit: String,
    pub original_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataClassification {
    pub class: HostClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub employer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mr: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataValidationData {
    pub config: MetadataConfig,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MetadataValidation {
    pub data: MetadataValidationData,
    pub warnings: Vec<ValidationWarning>,
}

#[derive(Debug, Error)]
pub enum MetadataValidationError {
    #[error("metadata.yaml is empty")]
    Empty,
    #[error("metadata.yaml could not be parsed as YAML: {0}")]
    Parse(String),
    #[error("metadata.yaml version {found} is not supported (expected {SUPPORTED_VERSION})")]
    UnsupportedVersion { found: u32 },
    #[error("metadata.yaml field `{field}` is missing or empty")]
    MissingRequiredField { field: String },
    #[error("metadata.yaml `refs`: at least one of `issue`, `pr`, `mr` must be present")]
    NoRefs,
    #[error(
        "metadata.yaml captured_classification.class `{found}` is not a recognised value (expected `personal` or `employer`)"
    )]
    UnknownClass { found: String },
    #[error(
        "metadata.yaml captured_classification.class `employer` requires a non-empty `employer` field"
    )]
    EmployerMissingName,
}

impl MetadataValidationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty => "metadata-empty",
            Self::Parse(_) => "metadata-parse-error",
            Self::UnsupportedVersion { .. } => "metadata-unsupported-version",
            Self::MissingRequiredField { .. } => "metadata-missing-required-field",
            Self::NoRefs => "metadata-no-refs",
            Self::UnknownClass { .. } => "metadata-unknown-class",
            Self::EmployerMissingName => "metadata-employer-missing-name",
        }
    }
}

pub fn validate_metadata_yaml(input: &str) -> Result<MetadataValidation, MetadataValidationError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(MetadataValidationError::Empty);
    }

    let raw: RawMetadataFile = serde_yaml_ng::from_str(input)
        .map_err(|err| MetadataValidationError::Parse(err.to_string()))?;

    let version = raw.version.unwrap_or(SUPPORTED_VERSION);
    if version != SUPPORTED_VERSION {
        return Err(MetadataValidationError::UnsupportedVersion { found: version });
    }

    let raw_source = raw
        .source
        .ok_or_else(|| MetadataValidationError::MissingRequiredField {
            field: "source".to_string(),
        })?;

    let source = MetadataSource {
        host: require_field("source.host", raw_source.host)?,
        org_or_group_path: require_field("source.org_or_group_path", raw_source.org_or_group_path)?,
        repo: require_field("source.repo", raw_source.repo)?,
        branch: require_field("source.branch", raw_source.branch)?,
        archive_commit: require_field("source.archive_commit", raw_source.archive_commit)?,
        original_path: require_field("source.original_path", raw_source.original_path)?,
    };

    let mut warnings = Vec::new();
    let captured_classification = match raw.captured_classification {
        None => {
            warnings.push(ValidationWarning::new(
                "metadata-captured-classification-missing",
                "metadata.yaml omits captured_classification (pre-classification plan); resolve against current config/hosts.yaml at query time",
            ));
            None
        }
        Some(raw_cls) => Some(build_classification(raw_cls)?),
    };

    let raw_refs = raw.refs.unwrap_or_default();
    if raw_refs.issue.is_none() && raw_refs.pr.is_none() && raw_refs.mr.is_none() {
        return Err(MetadataValidationError::NoRefs);
    }
    let refs = MetadataRefs {
        issue: raw_refs.issue,
        pr: raw_refs.pr,
        mr: raw_refs.mr,
    };

    Ok(MetadataValidation {
        data: MetadataValidationData {
            config: MetadataConfig {
                version,
                source,
                captured_classification,
                refs,
            },
        },
        warnings,
    })
}

fn require_field(name: &str, value: Option<String>) -> Result<String, MetadataValidationError> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(MetadataValidationError::MissingRequiredField {
            field: name.to_string(),
        }),
    }
}

fn build_classification(
    raw: RawClassification,
) -> Result<MetadataClassification, MetadataValidationError> {
    let class = match raw.class.as_str() {
        "personal" => HostClass::Personal,
        "employer" => HostClass::Employer,
        other => {
            return Err(MetadataValidationError::UnknownClass {
                found: other.to_string(),
            });
        }
    };
    if matches!(class, HostClass::Employer) && raw.employer.as_deref().is_none_or(str::is_empty) {
        return Err(MetadataValidationError::EmployerMissingName);
    }
    Ok(MetadataClassification {
        class,
        employer: raw.employer,
        retention: raw.retention,
        primary_identity: raw.primary_identity,
    })
}

#[derive(Debug, Deserialize)]
struct RawMetadataFile {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    source: Option<RawSource>,
    #[serde(default)]
    captured_classification: Option<RawClassification>,
    #[serde(default)]
    refs: Option<RawRefs>,
    #[allow(dead_code)]
    #[serde(default)]
    schema: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSource {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    org_or_group_path: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    archive_commit: Option<String>,
    #[serde(default)]
    original_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawClassification {
    class: String,
    #[serde(default)]
    employer: Option<String>,
    #[serde(default)]
    retention: Option<String>,
    #[serde(default)]
    primary_identity: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRefs {
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    pr: Option<String>,
    #[serde(default)]
    mr: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const GITHUB_PR: &str = r"
version: 1
source:
  host: github.com
  org_or_group_path: graysurf
  repo: agent-runtime-kit
  branch: main
  archive_commit: a2e8f227000000000000000000000000a2e8f227
  original_path: docs/plans/2026-05-27-plan-archive-runtime-kit/
captured_classification:
  class: personal
  primary_identity: graysurf
refs:
  issue: https://github.com/graysurf/agent-runtime-kit/issues/126
  pr: https://github.com/graysurf/agent-runtime-kit/pull/127
";

    const GITLAB_MR: &str = r"
version: 1
source:
  host: gitlab.example.com
  org_or_group_path: acme/platform/backend
  repo: services
  branch: main
  archive_commit: deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
  original_path: docs/plans/2026-04-10-some-plan/
captured_classification:
  class: employer
  employer: ExampleCorp
  retention: delete-on-termination
refs:
  issue: https://gitlab.example.com/acme/platform/backend/services/-/issues/42
  mr: https://gitlab.example.com/acme/platform/backend/services/-/merge_requests/99
";

    const ORPHAN_PLAN: &str = r"
version: 1
source:
  host: github.com
  org_or_group_path: graysurf
  repo: agent-runtime-kit
  branch: main
  archive_commit: c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00
  original_path: docs/plans/2026-01-15-orphan-experiment/
refs:
  issue: https://github.com/graysurf/agent-runtime-kit/issues/9999
";

    #[test]
    fn github_pr_validates() {
        let v = validate_metadata_yaml(GITHUB_PR).expect("validation");
        assert!(v.warnings.is_empty());
        assert_eq!(v.data.config.source.host, "github.com");
        assert!(v.data.config.captured_classification.is_some());
        assert_eq!(
            v.data.config.refs.pr.as_deref(),
            Some("https://github.com/graysurf/agent-runtime-kit/pull/127")
        );
    }

    #[test]
    fn gitlab_mr_validates() {
        let v = validate_metadata_yaml(GITLAB_MR).expect("validation");
        let cls = v.data.config.captured_classification.expect("captured");
        assert!(matches!(cls.class, HostClass::Employer));
        assert_eq!(cls.employer.as_deref(), Some("ExampleCorp"));
        assert!(v.data.config.refs.mr.is_some());
    }

    #[test]
    fn orphan_plan_warns_on_missing_classification() {
        let v = validate_metadata_yaml(ORPHAN_PLAN).expect("validation");
        assert!(v.data.config.captured_classification.is_none());
        assert_eq!(v.warnings.len(), 1);
        assert_eq!(
            v.warnings[0].code,
            "metadata-captured-classification-missing"
        );
    }

    #[test]
    fn missing_required_field_rejected() {
        // Omit `source.repo` while keeping the rest of the document
        // well-formed so the validator surfaces the missing-field code
        // rather than a YAML parse error.
        let input = r"
version: 1
source:
  host: github.com
  org_or_group_path: graysurf
  branch: main
  archive_commit: a2e8f227000000000000000000000000a2e8f227
  original_path: docs/plans/2026-05-27-something/
refs:
  issue: https://github.com/graysurf/r/issues/1
";
        let err = validate_metadata_yaml(input).expect_err("missing field");
        assert_eq!(err.code(), "metadata-missing-required-field");
    }

    #[test]
    fn no_refs_rejected() {
        let input = r"
version: 1
source:
  host: github.com
  org_or_group_path: graysurf
  repo: r
  branch: main
  archive_commit: deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
  original_path: docs/plans/foo/
refs: {}
";
        let err = validate_metadata_yaml(input).expect_err("no refs");
        assert_eq!(err.code(), "metadata-no-refs");
    }

    #[test]
    fn employer_missing_name_rejected() {
        let input = r"
version: 1
source:
  host: gitlab.example.com
  org_or_group_path: a/b
  repo: r
  branch: main
  archive_commit: deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
  original_path: docs/plans/foo/
captured_classification:
  class: employer
refs:
  mr: https://gitlab.example.com/a/b/r/-/merge_requests/1
";
        let err = validate_metadata_yaml(input).expect_err("missing employer");
        assert_eq!(err.code(), "metadata-employer-missing-name");
    }

    #[test]
    fn unknown_class_rejected() {
        let input = r"
version: 1
source:
  host: github.com
  org_or_group_path: graysurf
  repo: r
  branch: main
  archive_commit: deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
  original_path: docs/plans/foo/
captured_classification:
  class: visitor
refs:
  issue: https://github.com/graysurf/r/issues/1
";
        let err = validate_metadata_yaml(input).expect_err("unknown class");
        assert_eq!(err.code(), "metadata-unknown-class");
    }
}
