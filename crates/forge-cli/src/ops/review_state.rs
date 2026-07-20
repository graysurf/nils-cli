//! Durable review transaction and review-loop state primitives.
//!
//! Provider-visible state is append-only. Records are canonical JSON wrapped
//! in an owned HTML marker; each record binds its previous digest and expected
//! PR head. This module deliberately contains no provider I/O so parsing,
//! privacy, and fork rules stay deterministic.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

use crate::error::ForgeError;

pub const REVIEW_STATE_SCHEMA: &str = "forge-cli.review-loop.v1";
const STATE_MARKER_OPEN: &str = "<!-- forge-cli:review-state:v1 ";
const STATE_MARKER_CLOSE: &str = " -->";
const MAX_PROVIDER_STATE_MARKER_BYTES: usize = 64 * 1024;
const REVIEW_RUN_MARKER_PREFIX: &str = "<!-- forge-cli:review-run:v1 run=";
const FINDING_MARKER_PREFIX: &str = "<!-- forge-cli:review-finding:v1 run=";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewCommentManifestItem {
    pub index: usize,
    pub path: String,
    pub line: Option<u32>,
    pub side: String,
    pub start_line: Option<u32>,
    pub start_side: Option<String>,
    pub subject_type: String,
    pub body_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRunReceipt {
    pub review_run_id: String,
    pub route_lenses: Vec<String>,
    pub decision: String,
    pub expected_head: String,
    pub round: u32,
    pub summary_digest: String,
    pub inline_manifest: Vec<ReviewCommentManifestItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ReviewStatePayload {
    ReviewRunReceipt { receipt: ReviewRunReceipt },
    ReviewLoop { state: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewStateRecord {
    pub schema: String,
    pub repository: String,
    pub pr: u64,
    pub expected_head: String,
    pub generation: u64,
    pub previous_digest: Option<String>,
    pub payload: ReviewStatePayload,
    pub record_digest: String,
}

#[derive(Serialize)]
struct ReviewStatePreimage<'a> {
    schema: &'a str,
    repository: &'a str,
    pr: u64,
    expected_head: &'a str,
    generation: u64,
    previous_digest: &'a Option<String>,
    payload: &'a ReviewStatePayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStateChain {
    pub records: Vec<ReviewStateRecord>,
    pub tip_digest: Option<String>,
}

impl ReviewStateRecord {
    pub fn new(
        repository: impl Into<String>,
        pr: u64,
        expected_head: impl Into<String>,
        generation: u64,
        previous_digest: Option<String>,
        payload: ReviewStatePayload,
    ) -> Result<Self, ForgeError> {
        let mut record = Self {
            schema: REVIEW_STATE_SCHEMA.to_string(),
            repository: repository.into(),
            pr,
            expected_head: expected_head.into(),
            generation,
            previous_digest,
            payload,
            record_digest: String::new(),
        };
        record.record_digest = record.compute_digest()?;
        Ok(record)
    }

    pub fn compute_digest(&self) -> Result<String, ForgeError> {
        let preimage = ReviewStatePreimage {
            schema: &self.schema,
            repository: &self.repository,
            pr: self.pr,
            expected_head: &self.expected_head,
            generation: self.generation,
            previous_digest: &self.previous_digest,
            payload: &self.payload,
        };
        let bytes = serde_json::to_vec(&preimage).map_err(|error| {
            ForgeError::software(
                error_schema(),
                "failed to serialize review-state digest preimage",
                Some(error.to_string()),
            )
        })?;
        Ok(sha256_digest(&bytes))
    }

    pub fn marker(&self) -> Result<String, ForgeError> {
        let json = serde_json::to_string(self).map_err(|error| {
            ForgeError::software(
                error_schema(),
                "failed to serialize review-state record",
                Some(error.to_string()),
            )
        })?;
        let marker = format!(
            "{STATE_MARKER_OPEN}{}{STATE_MARKER_CLOSE}",
            hex_encode(json.as_bytes())
        );
        if marker.len() > MAX_PROVIDER_STATE_MARKER_BYTES {
            return Err(ForgeError::validation(
                error_schema(),
                "review_state_record_too_large",
                "the provider-visible review-state record exceeds the safe comment-body limit",
                Some(format!(
                    "marker_bytes={}; max_bytes={MAX_PROVIDER_STATE_MARKER_BYTES}",
                    marker.len()
                )),
            ));
        }
        Ok(marker)
    }
}

pub fn parse_chain<'a>(
    comments: impl IntoIterator<Item = &'a str>,
    repository: &str,
    pr: u64,
) -> Result<ReviewStateChain, ForgeError> {
    let mut records = Vec::new();
    for comment in comments {
        if let Some(record) = parse_state_marker(comment)? {
            if record.repository != repository || record.pr != pr {
                return Err(state_conflict(
                    "review-state record targets a different pull request",
                    Some(format!(
                        "expected={repository}#{pr}; observed={}#{}",
                        record.repository, record.pr
                    )),
                ));
            }
            records.push(record);
        }
    }
    if records.is_empty() {
        return Ok(ReviewStateChain {
            records,
            tip_digest: None,
        });
    }

    let mut by_digest: BTreeMap<String, &ReviewStateRecord> = BTreeMap::new();
    let mut children: BTreeMap<Option<String>, BTreeSet<String>> = BTreeMap::new();
    for record in &records {
        if record.schema != REVIEW_STATE_SCHEMA || record.compute_digest()? != record.record_digest
        {
            return Err(state_conflict(
                "review-state record digest is invalid",
                Some(format!("record_digest={}", record.record_digest)),
            ));
        }
        if let Some(existing) = by_digest.get(&record.record_digest) {
            if **existing == *record {
                continue;
            }
            return Err(state_conflict(
                "review-state chain contains conflicting records with one digest",
                Some(format!("record_digest={}", record.record_digest)),
            ));
        }
        by_digest.insert(record.record_digest.clone(), record);
        children
            .entry(record.previous_digest.clone())
            .or_default()
            .insert(record.record_digest.clone());
    }
    if children.values().any(|digests| digests.len() > 1) {
        return Err(state_conflict(
            "review-state chain contains competing generations",
            None,
        ));
    }
    let roots = children.get(&None).cloned().unwrap_or_default();
    if roots.len() != 1 {
        return Err(state_conflict(
            "review-state chain does not have exactly one genesis record",
            Some(format!("genesis_count={}", roots.len())),
        ));
    }

    let mut ordered = Vec::with_capacity(records.len());
    let mut current = roots.iter().next().cloned();
    while let Some(digest) = current {
        let record = by_digest.get(&digest).ok_or_else(|| {
            state_conflict(
                "review-state chain references a missing record",
                Some(format!("record_digest={digest}")),
            )
        })?;
        if record.generation != ordered.len() as u64 {
            return Err(state_conflict(
                "review-state generation is not contiguous",
                Some(format!(
                    "expected_generation={}; observed_generation={}",
                    ordered.len(),
                    record.generation
                )),
            ));
        }
        ordered.push((*record).clone());
        current = children
            .get(&Some(digest))
            .and_then(|children| children.iter().next().cloned());
    }
    if ordered.len() != by_digest.len() {
        return Err(state_conflict(
            "review-state chain contains unreachable records",
            Some(format!(
                "reachable={}; total={}",
                ordered.len(),
                records.len()
            )),
        ));
    }
    let tip_digest = ordered.last().map(|record| record.record_digest.clone());
    Ok(ReviewStateChain {
        records: ordered,
        tip_digest,
    })
}

pub fn parse_state_marker(body: &str) -> Result<Option<ReviewStateRecord>, ForgeError> {
    let Some(start) = body.find(STATE_MARKER_OPEN) else {
        return Ok(None);
    };
    let payload_start = start + STATE_MARKER_OPEN.len();
    let Some(relative_end) = body[payload_start..].find(STATE_MARKER_CLOSE) else {
        return Err(state_conflict("review-state marker is unterminated", None));
    };
    let encoded = &body[payload_start..payload_start + relative_end];
    let bytes = hex_decode(encoded)?;
    let json = String::from_utf8(bytes).map_err(|error| {
        state_conflict(
            "review-state marker payload is not UTF-8",
            Some(error.to_string()),
        )
    })?;
    serde_json::from_str(&json).map(Some).map_err(|error| {
        state_conflict(
            "review-state marker payload is invalid JSON",
            Some(error.to_string()),
        )
    })
}

pub fn review_run_marker(review_run_id: &str) -> String {
    format!("{REVIEW_RUN_MARKER_PREFIX}{review_run_id} -->")
}

pub fn finding_marker(review_run_id: &str, body_digest: &str) -> String {
    format!("{FINDING_MARKER_PREFIX}{review_run_id} digest={body_digest} -->")
}

pub fn parse_review_run_id(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        line.trim()
            .strip_prefix(REVIEW_RUN_MARKER_PREFIX)
            .and_then(|rest| rest.strip_suffix(" -->"))
            .filter(|value| !value.is_empty() && !value.contains(char::is_whitespace))
            .map(str::to_string)
    })
}

pub fn parse_finding_marker(body: &str) -> Option<(String, String)> {
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(FINDING_MARKER_PREFIX)?;
        let rest = rest.strip_suffix(" -->")?;
        let (run, digest) = rest.split_once(" digest=")?;
        if run.is_empty() || digest.is_empty() {
            return None;
        }
        Some((run.to_string(), digest.to_string()))
    })
}

pub fn strip_owned_markers(body: &str) -> String {
    body.lines()
        .filter(|line| {
            let line = line.trim();
            !line.starts_with(REVIEW_RUN_MARKER_PREFIX)
                && !line.starts_with(FINDING_MARKER_PREFIX)
                && !line.starts_with(STATE_MARKER_OPEN)
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

pub fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", hex_encode(&digest))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn hex_decode(encoded: &str) -> Result<Vec<u8>, ForgeError> {
    if encoded.is_empty()
        || !encoded.len().is_multiple_of(2)
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(state_conflict(
            "review-state marker payload is not canonical hex",
            None,
        ));
    }
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("ASCII checked");
            u8::from_str_radix(pair, 16).map_err(|error| {
                state_conflict(
                    "review-state marker payload is not canonical hex",
                    Some(error.to_string()),
                )
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn compute_review_run_id(
    repository: &str,
    pr: u64,
    expected_head: &str,
    round: u32,
    route_lenses: &[String],
    decision: &str,
    summary_digest: &str,
    inline_manifest: &[ReviewCommentManifestItem],
) -> Result<String, ForgeError> {
    #[derive(Serialize)]
    struct RunIdPreimage<'a> {
        repository: &'a str,
        pr: u64,
        expected_head: &'a str,
        round: u32,
        route_lenses: &'a [String],
        decision: &'a str,
        summary_digest: &'a str,
        inline_manifest: &'a [ReviewCommentManifestItem],
    }
    let bytes = serde_json::to_vec(&RunIdPreimage {
        repository,
        pr,
        expected_head,
        round,
        route_lenses,
        decision,
        summary_digest,
        inline_manifest,
    })
    .map_err(|error| {
        ForgeError::software(
            error_schema(),
            "failed to serialize review-run id preimage",
            Some(error.to_string()),
        )
    })?;
    Ok(sha256_digest(&bytes))
}

fn state_conflict(message: &str, detail: Option<String>) -> ForgeError {
    ForgeError::validation(error_schema(), "review_state_conflict", message, detail)
}

fn error_schema() -> String {
    "cli.forge-cli.error.v1".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn fixture_receipt() -> ReviewRunReceipt {
        ReviewRunReceipt {
            review_run_id: "sha256:run".to_string(),
            route_lenses: vec!["testing".to_string(), "maintainability".to_string()],
            decision: "comments-only".to_string(),
            expected_head: "head-abc".to_string(),
            round: 0,
            summary_digest: "sha256:summary".to_string(),
            inline_manifest: Vec::new(),
        }
    }

    #[test]
    fn markers_round_trip_without_changing_semantic_body() {
        let run = "sha256:abc";
        let body = format!("Finding\n{}", finding_marker(run, "sha256:def"));
        assert_eq!(
            parse_finding_marker(&body),
            Some((run.to_string(), "sha256:def".to_string()))
        );
        assert_eq!(strip_owned_markers(&body), "Finding");
    }

    #[test]
    fn chain_rejects_competing_children() {
        let genesis = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "head",
            0,
            None,
            ReviewStatePayload::ReviewLoop {
                state: serde_json::json!({"round": 0}),
            },
        )
        .expect("genesis");
        let child_a = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "head",
            1,
            Some(genesis.record_digest.clone()),
            ReviewStatePayload::ReviewLoop {
                state: serde_json::json!({"round": 1}),
            },
        )
        .expect("child a");
        let child_b = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "head",
            1,
            Some(genesis.record_digest.clone()),
            ReviewStatePayload::ReviewLoop {
                state: serde_json::json!({"round": 2}),
            },
        )
        .expect("child b");
        let comments = [
            genesis.marker().expect("marker"),
            child_a.marker().expect("marker"),
            child_b.marker().expect("marker"),
        ];
        let err = parse_chain(comments.iter().map(String::as_str), "acme/widgets", 7)
            .expect_err("fork must fail");
        assert_eq!(err.kind(), "review_state_conflict");
    }

    #[test]
    fn chain_deduplicates_byte_identical_retry_records() {
        let genesis = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "head",
            0,
            None,
            ReviewStatePayload::ReviewRunReceipt {
                receipt: fixture_receipt(),
            },
        )
        .expect("genesis");
        let marker = genesis.marker().expect("marker");

        let chain = parse_chain([marker.as_str(), marker.as_str()], "acme/widgets", 7)
            .expect("an identical lost-response retry is one logical append");

        assert_eq!(chain.records, vec![genesis.clone()]);
        assert_eq!(chain.tip_digest, Some(genesis.record_digest));
    }

    #[test]
    fn state_marker_rejects_a_provider_oversized_receipt_before_post() {
        let mut receipt = fixture_receipt();
        receipt.inline_manifest = (0..50)
            .map(|index| ReviewCommentManifestItem {
                index,
                path: "x".repeat(1024),
                line: Some(10),
                side: "RIGHT".to_string(),
                start_line: None,
                start_side: None,
                subject_type: "LINE".to_string(),
                body_digest: "sha256:finding".to_string(),
            })
            .collect();
        let record = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "head",
            0,
            None,
            ReviewStatePayload::ReviewRunReceipt { receipt },
        )
        .expect("record");

        let err = record
            .marker()
            .expect_err("oversized provider-visible state must fail before mutation");
        assert_eq!(err.kind(), "review_state_record_too_large");
    }

    #[test]
    fn state_marker_matches_canonical_golden_fixture() {
        let fixture =
            include_str!("../../tests/fixtures/github/pr_review/review_state_genesis.json");
        let record: ReviewStateRecord = serde_json::from_str(fixture).expect("golden record");
        assert_eq!(
            record.compute_digest().expect("digest"),
            record.record_digest
        );
        let marker = record.marker().expect("marker");
        assert_eq!(parse_state_marker(&marker).expect("parse"), Some(record));
    }

    #[test]
    fn malformed_state_markers_fail_closed() {
        for marker in [
            "<!-- forge-cli:review-state:v1 xyz -->",
            "<!-- forge-cli:review-state:v1 7B7D -->",
            "<!-- forge-cli:review-state:v1 7b7d -->",
            "<!-- forge-cli:review-state:v1 7b",
        ] {
            let err = parse_state_marker(marker).expect_err("malformed marker must fail");
            assert_eq!(err.kind(), "review_state_conflict");
        }
    }

    #[test]
    fn review_run_id_is_stable_and_binds_every_semantic_input() {
        let manifest = vec![ReviewCommentManifestItem {
            index: 0,
            path: "src/lib.rs".to_string(),
            line: Some(10),
            side: "RIGHT".to_string(),
            start_line: None,
            start_side: None,
            subject_type: "LINE".to_string(),
            body_digest: "sha256:finding".to_string(),
        }];
        let run = compute_review_run_id(
            "acme/widgets",
            7,
            "head-abc",
            0,
            &["testing".to_string()],
            "comments-only",
            "sha256:summary",
            &manifest,
        )
        .expect("run id");
        let identical = compute_review_run_id(
            "acme/widgets",
            7,
            "head-abc",
            0,
            &["testing".to_string()],
            "comments-only",
            "sha256:summary",
            &manifest,
        )
        .expect("identical run id");
        assert_eq!(run, identical);

        for changed in [
            compute_review_run_id(
                "acme/widgets",
                7,
                "head-def",
                0,
                &["testing".to_string()],
                "comments-only",
                "sha256:summary",
                &manifest,
            ),
            compute_review_run_id(
                "acme/widgets",
                7,
                "head-abc",
                0,
                &["testing".to_string()],
                "approve",
                "sha256:summary",
                &manifest,
            ),
            compute_review_run_id(
                "acme/widgets",
                7,
                "head-abc",
                0,
                &["testing".to_string()],
                "comments-only",
                "sha256:different-summary",
                &manifest,
            ),
            compute_review_run_id(
                "acme/widgets",
                7,
                "head-abc",
                0,
                &["maintainability".to_string()],
                "comments-only",
                "sha256:summary",
                &manifest,
            ),
        ] {
            assert_ne!(run, changed.expect("changed run id"));
        }
    }

    #[test]
    fn receipt_schema_has_no_private_identity_or_credential_fields() {
        let value = serde_json::to_value(fixture_receipt()).expect("receipt JSON");
        let keys = value
            .as_object()
            .expect("receipt object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from([
                "decision",
                "expected_head",
                "inline_manifest",
                "review_run_id",
                "round",
                "route_lenses",
                "summary_digest",
            ])
        );
        let serialized = serde_json::to_string(&value).expect("receipt JSON");
        for forbidden in [
            "token",
            "credential",
            "profile",
            "environment",
            "local_path",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "forbidden field {forbidden}"
            );
        }
    }
}
