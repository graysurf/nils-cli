//! Validator for an archived `skill-usage.rollup.json` (the
//! `evidence validate-record` subcommand).
//!
//! Checks that a rollup carries the required fields and a recognized schema.
//! Following Decision 6, an out-of-readable-range `schema` is reported as a
//! **warning**, not a hard rejection, so the validator stays forward-compat.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::ValidationWarning;
use crate::query::READABLE_SCHEMA_RANGE;

/// Successful validation result.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RecordValidation {
    pub data: RecordValidationData,
    pub warnings: Vec<ValidationWarning>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RecordValidationData {
    pub id: String,
    pub schema: String,
    pub host: String,
    pub org: String,
    pub repo: String,
    pub outcome_status: String,
}

#[derive(Debug, Error)]
pub enum RecordValidationError {
    #[error("rollup could not be parsed as JSON: {0}")]
    Parse(String),
    #[error("rollup is missing required field `{0}`")]
    MissingField(&'static str),
}

impl RecordValidationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Parse(_) => "record-parse-error",
            Self::MissingField(_) => "record-missing-field",
        }
    }
}

/// Validate a raw `skill-usage.rollup.json` string.
pub fn validate_rollup_yaml(input: &str) -> Result<RecordValidation, RecordValidationError> {
    // Rollups are JSON; serde_json also reads YAML's JSON subset, but parse as
    // JSON to match the on-disk format precisely.
    let raw: RawRollup =
        serde_json::from_str(input).map_err(|e| RecordValidationError::Parse(e.to_string()))?;

    let schema = require(raw.schema, "schema")?;
    let id = require(raw.id, "id")?;
    let _archived_at = require(raw.archived_at, "archived_at")?;
    let _skill = require(raw.skill, "skill")?;
    let _started_at = require(raw.started_at, "started_at")?;
    let _source_digest = require(raw.source_digest, "source_digest")?;
    let repo = raw
        .repo
        .ok_or(RecordValidationError::MissingField("repo"))?;
    let host = require(repo.host, "repo.host")?;
    let org = require(repo.org, "repo.org")?;
    let repo_name = require(repo.repo, "repo.repo")?;
    let outcome = raw
        .outcome
        .ok_or(RecordValidationError::MissingField("outcome"))?;
    let outcome_status = require(outcome.status, "outcome.status")?;
    require(raw.producer, "producer")?;
    raw.counts
        .ok_or(RecordValidationError::MissingField("counts"))?;

    let mut warnings = Vec::new();
    if !READABLE_SCHEMA_RANGE.contains(&schema.as_str()) {
        warnings.push(ValidationWarning::new(
            "record-schema-version-out-of-range",
            format!(
                "schema `{schema}` is outside the readable range {READABLE_SCHEMA_RANGE:?}; readers will report it as out-of-range"
            ),
        ));
    }

    Ok(RecordValidation {
        data: RecordValidationData {
            id,
            schema,
            host,
            org,
            repo: repo_name,
            outcome_status,
        },
        warnings,
    })
}

fn require<T>(value: Option<T>, field: &'static str) -> Result<T, RecordValidationError> {
    value.ok_or(RecordValidationError::MissingField(field))
}

#[derive(Debug, Deserialize)]
struct RawRollup {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    archived_at: Option<String>,
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    source_digest: Option<String>,
    #[serde(default)]
    repo: Option<RawRepo>,
    #[serde(default)]
    outcome: Option<RawOutcome>,
    #[serde(default)]
    producer: Option<serde_json::Value>,
    #[serde(default)]
    counts: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawRepo {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawOutcome {
    #[serde(default)]
    status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPLETE: &str = r#"{
        "schema": "skill-usage.rollup.v1",
        "id": "20260614T100000Z-deliver-pr-deadbeef",
        "archived_at": "2026-06-14T11:00:00Z",
        "skill": "deliver-pr",
        "intent": "deliver pr",
        "trigger": "user_explicit",
        "repo": { "host": "github.com", "org": "graysurf", "repo": "kit" },
        "cwd": "~/Project/kit",
        "started_at": "2026-06-14T10:00:00Z",
        "ended_at": "2026-06-14T10:30:00Z",
        "outcome": { "status": "pass", "summary": "done" },
        "producer": { "tool": "skill-usage", "nils_cli_version": "1.4.0" },
        "counts": { "validation": 2, "failures": 0 },
        "linked_evidence": [],
        "source_digest": "sha256:deadbeef"
    }"#;

    #[test]
    fn accepts_complete_rollup() {
        let v = validate_rollup_yaml(COMPLETE).expect("valid");
        assert_eq!(v.data.schema, "skill-usage.rollup.v1");
        assert_eq!(v.data.host, "github.com");
        assert_eq!(v.data.outcome_status, "pass");
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn rejects_missing_required_field() {
        let bad = r#"{ "schema": "skill-usage.rollup.v1" }"#;
        let err = validate_rollup_yaml(bad).expect_err("missing id");
        assert_eq!(err.code(), "record-missing-field");
    }

    #[test]
    fn rejects_missing_producer() {
        let no_producer = COMPLETE.replace(
            r#""producer": { "tool": "skill-usage", "nils_cli_version": "1.4.0" },"#,
            "",
        );
        let err = validate_rollup_yaml(&no_producer).expect_err("missing producer");
        assert_eq!(err.code(), "record-missing-field");
    }

    #[test]
    fn out_of_range_schema_warns_not_rejects() {
        let future = COMPLETE.replace("skill-usage.rollup.v1", "skill-usage.rollup.v2");
        let v = validate_rollup_yaml(&future).expect("v2 still validates structurally");
        assert_eq!(v.warnings.len(), 1);
        assert_eq!(v.warnings[0].code, "record-schema-version-out-of-range");
    }

    #[test]
    fn rejects_malformed_json() {
        let err = validate_rollup_yaml("{not json").expect_err("parse");
        assert_eq!(err.code(), "record-parse-error");
    }
}
