//! Net-new deserialization structs for `skill-usage.record.json`.
//!
//! The authoritative `SkillUsageRecord` struct lives in
//! `agent-workflow-primitives/src/skill_usage.rs` but is **private** to that
//! crate, so `nils-evidence` cannot import it. We therefore define our own
//! `#[derive(Deserialize)]` mirror of the on-disk JSON shape. Only the fields
//! the rollup derivation actually reads are modelled; unknown/extra fields are
//! tolerated so additive schema growth upstream does not break migration.
//!
//! Two compatibility notes folded in from the design review:
//!
//! - `outcome.status` is a **free string** in the source record (it is
//!   `status: String` upstream, not a closed enum). We keep it a `String` and
//!   never hard-reject an unknown value.
//! - `producer { tool, nils_cli_version }` is an **additive** block stamped by
//!   nils-cli v1.4.0+. Older records have no `producer`; we model it as
//!   `Option<Producer>` and the migrate pipeline synthesizes a fallback with a
//!   warning when it is absent.

use serde::Deserialize;

/// On-disk `skill-usage.record.json` shape (only the read fields).
#[derive(Debug, Clone, Deserialize)]
pub struct SkillUsageRecord {
    pub schema: String,
    /// Additive provenance block; absent on pre-v1.4.0 records.
    #[serde(default)]
    pub producer: Option<Producer>,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub owner: Option<Owner>,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub intent: String,
    pub outcome: Outcome,
    #[serde(default)]
    pub linked_records: Vec<LinkedRecord>,
    #[serde(default)]
    pub validation: Vec<Validation>,
    #[serde(default)]
    pub failures: Vec<Failure>,
}

/// Additive provenance block: which tool produced the record and the
/// nils-cli version at record-creation time.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Producer {
    pub tool: String,
    pub nils_cli_version: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Owner {
    pub kind: String,
    pub id: String,
}

/// Free-string outcome status plus its summary. `status` is deliberately
/// **not** an enum (see module docs).
#[derive(Debug, Clone, Deserialize)]
pub struct Outcome {
    pub status: String,
    #[serde(default)]
    pub summary: String,
}

/// A linked child evidence record (e.g. `test-first-evidence`,
/// `review-evidence`, or a `heuristic-inbox` promotion case).
#[derive(Debug, Clone, Deserialize)]
pub struct LinkedRecord {
    #[serde(rename = "type")]
    pub record_type: String,
    pub path: String,
}

/// A validation command result. Only the count is used in the rollup, but
/// the full shape is modelled so deserialization tolerates real records.
#[derive(Debug, Clone, Deserialize)]
pub struct Validation {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: String,
}

/// A recorded failure. Only the count is used in the rollup.
#[derive(Debug, Clone, Deserialize)]
pub struct Failure {
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub symptom: String,
}

impl SkillUsageRecord {
    /// Normalize v1 skill ownership and v2 owner objects to one shape.
    pub fn normalized_owner(&self) -> Result<Owner, String> {
        match self.schema.as_str() {
            "skill-usage.record.v1" => self
                .skill
                .as_ref()
                .filter(|skill| !skill.trim().is_empty())
                .map(|skill| Owner {
                    kind: "skill".to_string(),
                    id: skill.clone(),
                })
                .ok_or_else(|| "v1 record is missing skill ownership".to_string()),
            "skill-usage.record.v2" => self
                .owner
                .clone()
                .filter(|owner| {
                    matches!(owner.kind.as_str(), "skill" | "workflow" | "intent")
                        && !owner.id.trim().is_empty()
                })
                .ok_or_else(|| "v2 record has invalid owner ownership".to_string()),
            other => Err(format!("unsupported source schema `{other}`")),
        }
    }
    /// Parse a record from raw JSON bytes.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("skill-usage.record.json parse: {e}"))
    }

    /// Peek only the `schema` discriminator without committing to the full v1
    /// shape. Callers gate on the schema *before* a `from_json_bytes` that, for
    /// an incompatible future schema, might fail or mis-parse — so an
    /// unsupported schema is reported as such rather than as a parse error.
    pub fn peek_schema(bytes: &[u8]) -> Result<String, String> {
        #[derive(Deserialize)]
        struct SchemaOnly {
            schema: String,
        }
        let probe: SchemaOnly = serde_json::from_slice(bytes)
            .map_err(|e| format!("skill-usage.record.json parse: {e}"))?;
        Ok(probe.schema)
    }

    /// Parse a record from a JSON string.
    pub fn from_json_str(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| format!("skill-usage.record.json parse: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_PRODUCER: &str = r#"{
        "schema": "skill-usage.record.v1",
        "producer": { "tool": "skill-usage", "nils_cli_version": "1.4.0" },
        "skill": "deliver-pr",
        "started_at": "2026-06-14T10:00:00Z",
        "ended_at": "2026-06-14T10:30:00Z",
        "cwd": "/Users/test/Project/x",
        "trigger": "user_explicit",
        "intent": "deliver pr",
        "inputs": { "user_request_summary": "x", "referenced_files": [], "external_sources": [] },
        "outcome": { "status": "pass", "summary": "done" },
        "artifacts": [],
        "linked_records": [ { "type": "test-first-evidence", "path": "a/b" } ],
        "validation": [ { "command": "cargo test", "status": "pass", "summary": "ok" } ],
        "failures": [],
        "follow_up": []
    }"#;

    const WITHOUT_PRODUCER: &str = r#"{
        "schema": "skill-usage.record.v1",
        "skill": "deliver-pr",
        "started_at": "2026-05-28T19:53:44Z",
        "ended_at": "2026-05-28T20:28:27Z",
        "cwd": "/Users/test/Project/x",
        "trigger": "user_explicit",
        "intent": "deliver issue #624",
        "inputs": { "user_request_summary": "x", "referenced_files": [], "external_sources": [] },
        "outcome": { "status": "weird-custom-status", "summary": "done" },
        "artifacts": [],
        "linked_records": [],
        "validation": [
            { "command": "a", "status": "pass", "summary": "" },
            { "command": "b", "status": "pass", "summary": "" }
        ],
        "follow_up": []
    }"#;

    #[test]
    fn deserializes_record_with_producer() {
        let r = SkillUsageRecord::from_json_str(WITH_PRODUCER).expect("parse");
        let p = r.producer.as_ref().expect("producer present");
        assert_eq!(p.tool, "skill-usage");
        assert_eq!(p.nils_cli_version, "1.4.0");
        assert_eq!(r.outcome.status, "pass");
        assert_eq!(r.normalized_owner().unwrap().id, "deliver-pr");
        assert_eq!(r.linked_records.len(), 1);
        assert_eq!(r.linked_records[0].record_type, "test-first-evidence");
        assert_eq!(r.validation.len(), 1);
        assert!(r.failures.is_empty());
    }

    #[test]
    fn deserializes_record_without_producer() {
        let r = SkillUsageRecord::from_json_str(WITHOUT_PRODUCER).expect("parse");
        assert!(r.producer.is_none());
        // Free-string outcome status round-trips verbatim.
        assert_eq!(r.outcome.status, "weird-custom-status");
        assert_eq!(r.validation.len(), 2);
        assert!(r.failures.is_empty());
        assert!(r.ended_at.is_some());
    }

    #[test]
    fn free_string_outcome_status_is_not_an_enum() {
        // Any string must deserialize, including ones outside a documented set.
        let r = SkillUsageRecord::from_json_str(WITHOUT_PRODUCER).expect("parse");
        assert_eq!(r.outcome.status, "weird-custom-status");
    }

    #[test]
    fn tolerates_missing_optional_collections() {
        let json = r#"{
            "schema": "skill-usage.record.v1",
            "skill": "s",
            "started_at": "2026-06-14T10:00:00Z",
            "outcome": { "status": "pass" }
        }"#;
        let r = SkillUsageRecord::from_json_str(json).expect("parse");
        assert!(r.linked_records.is_empty());
        assert!(r.validation.is_empty());
        assert!(r.failures.is_empty());
        assert!(r.ended_at.is_none());
        assert_eq!(r.outcome.summary, "");
    }

    #[test]
    fn normalizes_v2_workflow_owner() {
        let json = r#"{
            "schema": "skill-usage.record.v2",
            "owner": { "kind": "workflow", "id": "deliver-pr" },
            "started_at": "2026-06-14T10:00:00Z",
            "outcome": { "status": "pass" }
        }"#;
        let record = SkillUsageRecord::from_json_str(json).expect("parse v2");
        assert!(record.skill.is_none());
        assert_eq!(
            record.normalized_owner().unwrap(),
            Owner {
                kind: "workflow".to_string(),
                id: "deliver-pr".to_string(),
            }
        );
    }
}
