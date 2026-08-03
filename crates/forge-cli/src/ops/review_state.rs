//! Durable review transaction and review-loop state primitives.
//!
//! Provider-visible state is append-only. Records are canonical JSON wrapped
//! in an owned HTML marker; each record binds its previous digest and expected
//! PR head. This module deliberately contains no provider I/O so parsing,
//! privacy, and fork rules stay deterministic.
//!
//! Encoding and presentation are separate concerns here. [`ReviewStateRecord::marker`]
//! owns the canonical machine encoding that digests and chain validation depend
//! on; [`render_state_comment_body`] owns the human-facing comment body that
//! wraps it. Presentation text never reaches a digest, so a rendering change can
//! never fork or invalidate an existing chain.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

use crate::error::ForgeError;

pub const REVIEW_STATE_SCHEMA: &str = "forge-cli.review-loop.v1";
const STATE_MARKER_OPEN: &str = "<!-- forge-cli:review-state:v1 ";
const STATE_MARKER_CLOSE: &str = " -->";
const MAX_PROVIDER_STATE_MARKER_BYTES: usize = 64 * 1024;
/// GitHub caps an issue-comment body at 65536 bytes, and the marker is only one
/// part of that body once visible text wraps it. The complete rendered body is
/// what the provider actually stores, so it carries the binding limit.
const MAX_PROVIDER_STATE_COMMENT_BYTES: usize = 64 * 1024;
const STATE_COMMENT_LABEL: &str = "forge-cli review ledger";
/// Abbreviated-SHA width for the visible metadata line.
const STATE_COMMENT_HEAD_CHARS: usize = 12;
const REVIEW_RUN_MARKER_PREFIX: &str = "<!-- forge-cli:review-run:v1 run=";
const FINDING_MARKER_PREFIX: &str = "<!-- forge-cli:review-finding:v1 run=";
const THREAD_DISPOSITION_MARKER_PREFIX: &str = "<!-- forge-cli:thread-disposition:v1 thread=";

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
#[serde(deny_unknown_fields)]
pub struct ReviewLoopBudget {
    pub max_repair_rounds: u32,
    pub max_no_progress_rounds: u32,
    pub max_auto_reopens_per_fingerprint: u32,
}

impl Default for ReviewLoopBudget {
    fn default() -> Self {
        Self {
            max_repair_rounds: 5,
            max_no_progress_rounds: 2,
            max_auto_reopens_per_fingerprint: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewFindingStatus {
    Open,
    Fixed,
    Accepted,
    Preference,
    FollowUp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewLoopFinding {
    pub root_cause_fingerprint: Option<String>,
    pub status: ReviewFindingStatus,
    pub blocking: bool,
    pub first_seen_head: String,
    pub last_seen_head: String,
    pub seen_count: u32,
    #[serde(default)]
    pub reopen_count: u32,
    #[serde(default)]
    pub threads: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewLoopExtension {
    pub proposal_digest: String,
    pub approval_reference: String,
    pub budget_field: String,
    pub increment: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewLoopState {
    pub head_sha: String,
    pub round: u32,
    pub no_progress_rounds: u32,
    pub budget: ReviewLoopBudget,
    #[serde(default)]
    pub findings: BTreeMap<String, ReviewLoopFinding>,
    #[serde(default)]
    pub extensions: Vec<ReviewLoopExtension>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_stop: Option<ReviewLoopHardStop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewFindingObservation {
    pub fingerprint: String,
    pub root_cause_fingerprint: Option<String>,
    #[serde(default = "default_true")]
    pub blocking: bool,
    #[serde(default = "default_open_status")]
    pub status: ReviewFindingStatus,
    #[serde(default)]
    pub threads: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewLoopHardStop {
    pub code: String,
    pub budget_field: String,
    pub increment: u32,
    pub proposal_digest: String,
    pub attempted_head_sha: String,
    pub observation_digest: String,
    #[serde(default)]
    pub extension_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLoopTransition {
    pub state: ReviewLoopState,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ReviewStatePayload {
    ReviewRunReceipt { receipt: ReviewRunReceipt },
    ReviewLoop { state: ReviewLoopState },
}

impl ReviewStatePayload {
    /// The serialized `kind` tag, reused verbatim in the visible metadata line so
    /// the timeline names the same payload kind the record encodes.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ReviewRunReceipt { .. } => "review-run-receipt",
            Self::ReviewLoop { .. } => "review-loop",
        }
    }
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

/// The one-line, non-sensitive description of a ledger record shown in the
/// provider timeline.
///
/// Everything here is already public on the pull request: the generation index,
/// the payload kind, and an abbreviated head SHA. No credential, environment
/// value, local path, private identity, or finding body is representable.
pub fn state_comment_visible_metadata(record: &ReviewStateRecord) -> String {
    format!(
        "{STATE_COMMENT_LABEL} · generation {generation} · {kind} · head {head}",
        generation = record.generation,
        kind = record.payload.kind(),
        head = abbreviated_head(&record.expected_head),
    )
}

/// Renders the complete provider comment body for one ledger record, optionally
/// carrying a human-readable delivery outcome in the same comment.
///
/// GitHub hides HTML comments when it renders Markdown, so a body that is only
/// the canonical marker appears in the timeline as a blank comment authored by
/// the operator. Prefixing the visible metadata line keeps the same append-only
/// record identifiable as machine metadata. The marker itself is emitted
/// unchanged and on its own trailing line, so [`parse_state_marker`] — which
/// already searches within a larger body — reads new and historical bare-marker
/// comments identically, with no migration.
///
/// `visible_outcome`, when supplied, makes the ledger append and the delivery
/// outcome one provider mutation instead of two. It is rejected if it carries a
/// state marker of its own: [`parse_state_marker`] takes the first marker in the
/// body, so an embedded one would silently shadow this record's.
pub fn render_state_comment_body(
    record: &ReviewStateRecord,
    visible_outcome: Option<&str>,
) -> Result<String, ForgeError> {
    let marker = record.marker()?;
    let metadata = state_comment_visible_metadata(record);
    let body = match visible_outcome {
        Some(outcome) => {
            let outcome = validate_visible_outcome(outcome)?;
            format!("{outcome}\n\n---\n\n{metadata}\n{marker}")
        }
        None => format!("{metadata}\n{marker}"),
    };
    if body.len() > MAX_PROVIDER_STATE_COMMENT_BYTES {
        return Err(ForgeError::validation(
            error_schema(),
            "review_state_comment_too_large",
            "the complete provider review-state comment body exceeds the safe comment-body limit",
            Some(format!(
                "comment_bytes={}; marker_bytes={}; max_bytes={MAX_PROVIDER_STATE_COMMENT_BYTES}",
                body.len(),
                marker.len()
            )),
        ));
    }
    Ok(body)
}

/// Validates a caller-supplied visible outcome body for combined posting.
///
/// Only structural safety is checked here; portability rules (local paths,
/// escaped control markdown) belong to the command layer that owns the flag.
fn validate_visible_outcome(outcome: &str) -> Result<&str, ForgeError> {
    let trimmed = outcome.trim();
    if trimmed.is_empty() {
        return Err(ForgeError::validation(
            error_schema(),
            "review_state_comment_invalid",
            "the review-loop outcome body is empty",
            None,
        ));
    }
    if trimmed.contains(STATE_MARKER_OPEN) {
        return Err(ForgeError::validation(
            error_schema(),
            "review_state_comment_invalid",
            "the review-loop outcome body must not contain a review-state marker",
            Some(format!("marker_prefix={}", STATE_MARKER_OPEN.trim_end())),
        ));
    }
    Ok(trimmed)
}

/// Abbreviates a head SHA for the single-line visible metadata.
///
/// The provider supplies the head, so the visible line filters it to characters
/// that cannot break out of one line of plain Markdown.
fn abbreviated_head(head: &str) -> String {
    let abbreviated = head
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .take(STATE_COMMENT_HEAD_CHARS)
        .collect::<String>();
    if abbreviated.is_empty() {
        "-".to_string()
    } else {
        abbreviated
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
        if let ReviewStatePayload::ReviewLoop { state } = &record.payload {
            validate_review_loop_state(state)?;
            let stopped_head_matches = state
                .hard_stop
                .as_ref()
                .is_some_and(|stop| stop.attempted_head_sha == record.expected_head);
            if state.head_sha != record.expected_head && !stopped_head_matches {
                return Err(state_conflict(
                    "review-loop state head differs from its enclosing record",
                    Some(format!(
                        "record_head={}; state_head={}",
                        record.expected_head, state.head_sha
                    )),
                ));
            }
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

pub fn latest_review_loop_state(chain: &ReviewStateChain) -> Option<&ReviewLoopState> {
    chain
        .records
        .iter()
        .rev()
        .find_map(|record| match &record.payload {
            ReviewStatePayload::ReviewLoop { state } => Some(state),
            ReviewStatePayload::ReviewRunReceipt { .. } => None,
        })
}

pub fn observe_review_loop(
    previous: Option<&ReviewLoopState>,
    expected_head: &str,
    observations: &[ReviewFindingObservation],
) -> Result<ReviewLoopTransition, ForgeError> {
    if expected_head.trim().is_empty() {
        return Err(state_conflict(
            "review-loop observation head is empty",
            None,
        ));
    }
    let current = canonical_observations(observations)?;
    let Some(previous) = previous else {
        let findings = current
            .into_iter()
            .map(|(fingerprint, observation)| {
                (
                    fingerprint,
                    ReviewLoopFinding {
                        root_cause_fingerprint: observation.root_cause_fingerprint,
                        status: observation.status,
                        blocking: observation.status == ReviewFindingStatus::Open
                            && observation.blocking,
                        first_seen_head: expected_head.to_string(),
                        last_seen_head: expected_head.to_string(),
                        seen_count: 1,
                        reopen_count: 0,
                        threads: observation.threads,
                    },
                )
            })
            .collect();
        return Ok(ReviewLoopTransition {
            state: ReviewLoopState {
                head_sha: expected_head.to_string(),
                round: 0,
                no_progress_rounds: 0,
                budget: ReviewLoopBudget::default(),
                findings,
                extensions: Vec::new(),
                hard_stop: None,
            },
            changed: true,
        });
    };

    validate_review_loop_state(previous)?;
    let resumed_previous = if let Some(stop) = previous.hard_stop.as_ref() {
        let observation_digest = review_observation_digest(expected_head, &current)?;
        if stop.attempted_head_sha != expected_head || stop.observation_digest != observation_digest
        {
            return Err(state_conflict(
                "a different observation cannot replace the durable review hard stop",
                Some(format!(
                    "stop_head={}; attempted_head={expected_head}; stop_observation={}; attempted_observation={observation_digest}",
                    stop.attempted_head_sha, stop.observation_digest
                )),
            ));
        }
        if !stop.extension_applied {
            return Err(hard_stop_error(stop));
        }
        let mut resumed = previous.clone();
        resumed.hard_stop = None;
        Some(resumed)
    } else {
        None
    };
    let previous = resumed_previous.as_ref().unwrap_or(previous);
    let head_changed = previous.head_sha != expected_head;
    if !head_changed && observation_matches_state(previous, &current) {
        return Ok(ReviewLoopTransition {
            state: previous.clone(),
            changed: false,
        });
    }
    if !head_changed {
        for (fingerprint, finding) in &previous.findings {
            if finding.status != ReviewFindingStatus::Open {
                continue;
            }
            let Some(observation) = current.get(fingerprint) else {
                return Err(state_conflict(
                    "same-head observation cannot omit an open finding",
                    Some(format!("fingerprint={fingerprint}; head={expected_head}")),
                ));
            };
            if observation.status == ReviewFindingStatus::Fixed
                || (finding.blocking
                    && observation.status == ReviewFindingStatus::Open
                    && !observation.blocking)
            {
                return Err(state_conflict(
                    "same-head observation cannot implicitly clear an open blocking finding",
                    Some(format!("fingerprint={fingerprint}; head={expected_head}")),
                ));
            }
        }
    }
    let next_round = previous.round + u32::from(head_changed);
    if next_round > previous.budget.max_repair_rounds {
        return Err(review_stop(
            "review_round_limit_exceeded",
            "review repair-round budget is exhausted",
            format!(
                "round={next_round}; max_repair_rounds={}",
                previous.budget.max_repair_rounds
            ),
        ));
    }

    let previous_blocking = previous
        .findings
        .values()
        .filter(|finding| finding.status == ReviewFindingStatus::Open && finding.blocking)
        .count();
    let current_blocking = current
        .values()
        .filter(|finding| finding.status == ReviewFindingStatus::Open && finding.blocking)
        .count();
    let next_no_progress = if head_changed && current_blocking >= previous_blocking {
        previous.no_progress_rounds.saturating_add(1)
    } else if head_changed {
        0
    } else {
        previous.no_progress_rounds
    };
    if next_no_progress > previous.budget.max_no_progress_rounds {
        return Err(review_stop(
            "review_no_progress",
            "the blocking finding set did not shrink within the configured budget",
            format!(
                "previous_blocking={previous_blocking}; current_blocking={current_blocking}; no_progress_rounds={next_no_progress}; max_no_progress_rounds={}",
                previous.budget.max_no_progress_rounds
            ),
        ));
    }

    let observed_fingerprints = current.keys().cloned().collect::<BTreeSet<_>>();
    let mut findings = previous.findings.clone();
    for (fingerprint, observation) in current {
        match findings.get_mut(&fingerprint) {
            Some(finding) => {
                if finding.root_cause_fingerprint != observation.root_cause_fingerprint {
                    return Err(review_stop(
                        "review_fingerprint_collision",
                        "a lifecycle fingerprint changed root-cause identity",
                        format!("fingerprint={fingerprint}"),
                    ));
                }
                if finding.status == ReviewFindingStatus::Fixed
                    && observation.status == ReviewFindingStatus::Open
                {
                    let reopen_count = finding.reopen_count.saturating_add(1);
                    if reopen_count > previous.budget.max_auto_reopens_per_fingerprint {
                        return Err(review_stop(
                            "review_finding_reopened",
                            "a fixed lifecycle finding reappeared",
                            format!(
                                "fingerprint={fingerprint}; reopen_count={reopen_count}; max_auto_reopens_per_fingerprint={}",
                                previous.budget.max_auto_reopens_per_fingerprint
                            ),
                        ));
                    }
                    finding.reopen_count = reopen_count;
                }
                if observation.status == ReviewFindingStatus::Fixed && !head_changed {
                    return Err(state_conflict(
                        "a fixed disposition requires a repaired head",
                        Some(format!("fingerprint={fingerprint}; head={expected_head}")),
                    ));
                }
                finding.status = observation.status;
                finding.blocking =
                    observation.status == ReviewFindingStatus::Open && observation.blocking;
                finding.last_seen_head = expected_head.to_string();
                finding.seen_count = finding.seen_count.saturating_add(1);
                finding.threads.extend(observation.threads);
                finding.threads.sort();
                finding.threads.dedup();
            }
            None => {
                findings.insert(
                    fingerprint,
                    ReviewLoopFinding {
                        root_cause_fingerprint: observation.root_cause_fingerprint,
                        status: observation.status,
                        blocking: observation.status == ReviewFindingStatus::Open
                            && observation.blocking,
                        first_seen_head: expected_head.to_string(),
                        last_seen_head: expected_head.to_string(),
                        seen_count: 1,
                        reopen_count: 0,
                        threads: observation.threads,
                    },
                );
            }
        }
    }
    for (fingerprint, finding) in &mut findings {
        if head_changed
            && !observed_fingerprints.contains(fingerprint)
            && finding.status == ReviewFindingStatus::Open
        {
            finding.status = ReviewFindingStatus::Fixed;
        }
    }

    let state = ReviewLoopState {
        head_sha: expected_head.to_string(),
        round: next_round,
        no_progress_rounds: next_no_progress,
        budget: previous.budget.clone(),
        findings,
        extensions: previous.extensions.clone(),
        hard_stop: None,
    };
    Ok(ReviewLoopTransition {
        changed: state != *previous,
        state,
    })
}

pub fn extension_proposal_digest(
    state_tip_digest: &str,
    stop_code: &str,
    budget_field: &str,
    increment: u32,
) -> Result<String, ForgeError> {
    if increment == 0 || increment > 100 {
        return Err(ForgeError::validation(
            error_schema(),
            "review_extension_invalid",
            "review budget extension increment must be between 1 and 100",
            Some(format!("increment={increment}")),
        ));
    }
    if !matches!(
        budget_field,
        "max_repair_rounds" | "max_no_progress_rounds" | "max_auto_reopens_per_fingerprint"
    ) {
        return Err(ForgeError::validation(
            error_schema(),
            "review_extension_invalid",
            "unknown review budget field",
            Some(format!("budget_field={budget_field}")),
        ));
    }
    let bytes = serde_json::to_vec(&(
        REVIEW_STATE_SCHEMA,
        state_tip_digest,
        stop_code,
        budget_field,
        increment,
    ))
    .map_err(|error| {
        ForgeError::software(
            error_schema(),
            "failed to serialize review extension proposal",
            Some(error.to_string()),
        )
    })?;
    Ok(sha256_digest(&bytes))
}

pub fn record_review_loop_hard_stop(
    previous: &ReviewLoopState,
    attempted_head: &str,
    observations: &[ReviewFindingObservation],
    state_tip_digest: &str,
    error: &ForgeError,
) -> Result<ReviewLoopState, ForgeError> {
    let budget_field = stop_budget_field(error.kind()).ok_or_else(|| {
        state_conflict(
            "only a budget hard stop can be recorded for extension",
            Some(format!("error={}", error.kind())),
        )
    })?;
    let current = canonical_observations(observations)?;
    let increment = 1;
    let proposal_digest =
        extension_proposal_digest(state_tip_digest, error.kind(), budget_field, increment)?;
    let mut state = previous.clone();
    state.hard_stop = Some(ReviewLoopHardStop {
        code: error.kind().to_string(),
        budget_field: budget_field.to_string(),
        increment,
        proposal_digest,
        attempted_head_sha: attempted_head.to_string(),
        observation_digest: review_observation_digest(attempted_head, &current)?,
        extension_applied: false,
    });
    Ok(state)
}

pub fn apply_review_loop_extension(
    previous: &ReviewLoopState,
    proposal_digest: String,
    approval_reference: String,
    stop_code: &str,
    budget_field: &str,
    increment: u32,
) -> Result<ReviewLoopState, ForgeError> {
    let hard_stop = previous.hard_stop.as_ref().ok_or_else(|| {
        ForgeError::validation(
            error_schema(),
            "review_extension_invalid",
            "review budget extension requires a durable hard-stop record",
            None,
        )
    })?;
    if hard_stop.extension_applied
        || hard_stop.code != stop_code
        || hard_stop.budget_field != budget_field
        || hard_stop.increment != increment
        || hard_stop.proposal_digest != proposal_digest
        || stop_budget_field(stop_code) != Some(budget_field)
    {
        return Err(ForgeError::validation(
            error_schema(),
            "review_extension_invalid",
            "the extension does not match the current durable hard stop",
            Some(format!(
                "stop_code={}; budget_field={}; increment={}; proposal_digest={}",
                hard_stop.code,
                hard_stop.budget_field,
                hard_stop.increment,
                hard_stop.proposal_digest
            )),
        ));
    }
    if previous
        .extensions
        .iter()
        .any(|extension| extension.proposal_digest == proposal_digest)
    {
        return Err(ForgeError::validation(
            error_schema(),
            "review_extension_replayed",
            "the review budget extension proposal was already consumed",
            Some(format!("proposal_digest={proposal_digest}")),
        ));
    }
    let mut state = previous.clone();
    let target = match budget_field {
        "max_repair_rounds" => &mut state.budget.max_repair_rounds,
        "max_no_progress_rounds" => &mut state.budget.max_no_progress_rounds,
        "max_auto_reopens_per_fingerprint" => &mut state.budget.max_auto_reopens_per_fingerprint,
        _ => {
            return Err(ForgeError::validation(
                error_schema(),
                "review_extension_invalid",
                "unknown review budget field",
                Some(format!("budget_field={budget_field}")),
            ));
        }
    };
    if increment == 0 || increment > 100 {
        return Err(ForgeError::validation(
            error_schema(),
            "review_extension_invalid",
            "review budget extension increment must be between 1 and 100",
            Some(format!("increment={increment}")),
        ));
    }
    *target = target.checked_add(increment).ok_or_else(|| {
        ForgeError::validation(
            error_schema(),
            "review_extension_invalid",
            "review budget extension overflowed",
            None,
        )
    })?;
    state.extensions.push(ReviewLoopExtension {
        proposal_digest,
        approval_reference,
        budget_field: budget_field.to_string(),
        increment,
    });
    state
        .hard_stop
        .as_mut()
        .expect("validated hard stop exists")
        .extension_applied = true;
    Ok(state)
}

fn canonical_observations(
    observations: &[ReviewFindingObservation],
) -> Result<BTreeMap<String, ReviewFindingObservation>, ForgeError> {
    let mut result = BTreeMap::new();
    for observation in observations {
        validate_lifecycle_fingerprint(&observation.fingerprint)?;
        if let Some(root) = observation.root_cause_fingerprint.as_deref() {
            validate_lifecycle_fingerprint(root)?;
        }
        let lifecycle = observation
            .root_cause_fingerprint
            .as_deref()
            .unwrap_or(&observation.fingerprint)
            .to_string();
        let mut observation = observation.clone();
        if observation.status != ReviewFindingStatus::Open {
            observation.blocking = false;
        }
        observation.threads.sort();
        observation.threads.dedup();
        if let Some(previous) = result.insert(lifecycle.clone(), observation.clone())
            && previous != observation
        {
            return Err(review_stop(
                "review_fingerprint_collision",
                "one lifecycle identity describes incompatible observations",
                format!("fingerprint={lifecycle}"),
            ));
        }
    }
    Ok(result)
}

fn observation_matches_state(
    state: &ReviewLoopState,
    observations: &BTreeMap<String, ReviewFindingObservation>,
) -> bool {
    let active = state
        .findings
        .iter()
        .filter(|(_, finding)| finding.last_seen_head == state.head_sha)
        .collect::<BTreeMap<_, _>>();
    active.len() == observations.len()
        && observations.iter().all(|(fingerprint, observation)| {
            active.get(fingerprint).is_some_and(|finding| {
                finding.root_cause_fingerprint == observation.root_cause_fingerprint
                    && finding.status == observation.status
                    && finding.blocking == observation.blocking
                    && finding.threads == observation.threads
            })
        })
}

fn validate_review_loop_state(state: &ReviewLoopState) -> Result<(), ForgeError> {
    if state.head_sha.is_empty() {
        return Err(state_conflict("review-loop state head is empty", None));
    }
    for (fingerprint, finding) in &state.findings {
        validate_lifecycle_fingerprint(fingerprint)?;
        if let Some(root) = finding.root_cause_fingerprint.as_deref() {
            validate_lifecycle_fingerprint(root)?;
            if root != fingerprint {
                return Err(state_conflict(
                    "review-loop finding map key differs from its root-cause identity",
                    Some(format!("key={fingerprint}; root={root}")),
                ));
            }
        }
        if finding.first_seen_head.is_empty()
            || finding.last_seen_head.is_empty()
            || finding.seen_count == 0
        {
            return Err(state_conflict(
                "review-loop finding lifecycle counters are invalid",
                Some(format!("fingerprint={fingerprint}")),
            ));
        }
    }
    if let Some(stop) = state.hard_stop.as_ref()
        && (stop_budget_field(&stop.code) != Some(stop.budget_field.as_str())
            || stop.increment != 1
            || stop.proposal_digest.is_empty()
            || stop.attempted_head_sha.is_empty()
            || stop.observation_digest.is_empty())
    {
        return Err(state_conflict(
            "review-loop hard-stop record is invalid",
            Some(format!("code={}", stop.code)),
        ));
    }
    Ok(())
}

fn review_observation_digest(
    expected_head: &str,
    observations: &BTreeMap<String, ReviewFindingObservation>,
) -> Result<String, ForgeError> {
    let bytes = serde_json::to_vec(&(REVIEW_STATE_SCHEMA, expected_head, observations)).map_err(
        |error| {
            ForgeError::software(
                error_schema(),
                "failed to serialize review-loop observation digest",
                Some(error.to_string()),
            )
        },
    )?;
    Ok(sha256_digest(&bytes))
}

pub fn stop_budget_field(code: &str) -> Option<&'static str> {
    match code {
        "review_round_limit_exceeded" => Some("max_repair_rounds"),
        "review_no_progress" => Some("max_no_progress_rounds"),
        "review_finding_reopened" => Some("max_auto_reopens_per_fingerprint"),
        _ => None,
    }
}

fn hard_stop_error(stop: &ReviewLoopHardStop) -> ForgeError {
    let code = match stop.code.as_str() {
        "review_round_limit_exceeded" => "review_round_limit_exceeded",
        "review_no_progress" => "review_no_progress",
        "review_finding_reopened" => "review_finding_reopened",
        _ => "review_state_conflict",
    };
    ForgeError::validation(
        error_schema(),
        code,
        "the durable review-loop hard stop is still active",
        Some(format!(
            "attempted_head={}; observation_digest={}; proposal_digest={}; budget_field={}; increment={}",
            stop.attempted_head_sha,
            stop.observation_digest,
            stop.proposal_digest,
            stop.budget_field,
            stop.increment
        )),
    )
}

fn validate_lifecycle_fingerprint(fingerprint: &str) -> Result<(), ForgeError> {
    let parts = fingerprint.split(':').collect::<Vec<_>>();
    if parts.len() == 3 && parts.iter().all(|part| stable_part(part)) {
        return Ok(());
    }
    Err(review_stop(
        "review_fingerprint_collision",
        "lifecycle fingerprint must have <category>:<component>:<invariant> form",
        format!("fingerprint={fingerprint}"),
    ))
}

fn stable_part(part: &str) -> bool {
    !part.is_empty()
        && !part.starts_with('-')
        && !part.ends_with('-')
        && part
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn review_stop(code: &'static str, message: &'static str, detail: String) -> ForgeError {
    ForgeError::validation(error_schema(), code, message, Some(detail))
}

fn default_true() -> bool {
    true
}

fn default_open_status() -> ReviewFindingStatus {
    ReviewFindingStatus::Open
}

pub fn review_run_marker(review_run_id: &str) -> String {
    format!("{REVIEW_RUN_MARKER_PREFIX}{review_run_id} -->")
}

pub fn finding_marker(review_run_id: &str, body_digest: &str) -> String {
    format!("{FINDING_MARKER_PREFIX}{review_run_id} digest={body_digest} -->")
}

pub fn thread_disposition_marker(thread_id: &str) -> String {
    format!("{THREAD_DISPOSITION_MARKER_PREFIX}{thread_id} -->")
}

pub fn has_thread_disposition_marker(body: &str) -> bool {
    body.lines().any(|line| {
        line.trim()
            .strip_prefix(THREAD_DISPOSITION_MARKER_PREFIX)
            .and_then(|rest| rest.strip_suffix(" -->"))
            .is_some_and(|thread| !thread.is_empty())
    })
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
                && !line.starts_with(THREAD_DISPOSITION_MARKER_PREFIX)
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

    fn fixture_loop_state(round: u32) -> ReviewLoopState {
        ReviewLoopState {
            head_sha: "head".to_string(),
            round,
            no_progress_rounds: 0,
            budget: ReviewLoopBudget::default(),
            findings: BTreeMap::new(),
            extensions: Vec::new(),
            hard_stop: None,
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
                state: fixture_loop_state(0),
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
                state: fixture_loop_state(1),
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
                state: fixture_loop_state(2),
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

    // ---------------------------------------------------------------------
    // A marker-only body is what GitHub renders as a blank comment. These
    // guards keep the visible wrapper honest without letting presentation
    // reach the digest, the chain, or the parser.
    // ---------------------------------------------------------------------

    #[test]
    fn a_rendered_state_comment_is_visibly_identified_and_still_parses() {
        let record = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "0123456789abcdef0123456789abcdef01234567",
            3,
            Some("sha256:previous".to_string()),
            ReviewStatePayload::ReviewLoop {
                state: fixture_loop_state(1),
            },
        )
        .expect("record");

        let body = render_state_comment_body(&record, None).expect("rendered body");

        assert_eq!(
            body,
            format!(
                "forge-cli review ledger · generation 3 · review-loop · head 0123456789ab\n{}",
                record.marker().expect("marker")
            )
        );
        // The visible line is real Markdown text, so the comment can never
        // render empty.
        assert!(!body.lines().next().expect("first line").starts_with("<!--"),);
        assert_eq!(parse_state_marker(&body).expect("parse"), Some(record));
    }

    #[test]
    fn a_receipt_comment_names_its_own_payload_kind() {
        let record = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "head-abc",
            0,
            None,
            ReviewStatePayload::ReviewRunReceipt {
                receipt: fixture_receipt(),
            },
        )
        .expect("record");

        assert_eq!(
            state_comment_visible_metadata(&record),
            "forge-cli review ledger · generation 0 · review-run-receipt · head head-abc"
        );
    }

    #[test]
    fn historical_bare_markers_and_rendered_bodies_validate_as_one_chain() {
        // Existing history is bare markers; new appends are wrapped. A chain
        // that mixes both must be indistinguishable from either, with no
        // migration and no rewrite of historical comments.
        let genesis = record("head", 0, None, fixture_loop_state(0));
        let child = record(
            "head",
            1,
            Some(genesis.record_digest.clone()),
            fixture_loop_state(1),
        );
        let historical = genesis.marker().expect("historical marker");
        let rendered = render_state_comment_body(&child, None).expect("rendered body");

        let mixed =
            parse_chain([historical.as_str(), rendered.as_str()], REPO, PR).expect("mixed history");
        let all_legacy = parse_chain(
            [
                historical.as_str(),
                child.marker().expect("marker").as_str(),
            ],
            REPO,
            PR,
        )
        .expect("recorded history");

        assert_eq!(mixed, all_legacy);
        assert_eq!(mixed.tip_digest.as_deref(), Some(&*child.record_digest));
    }

    #[test]
    fn visible_presentation_never_reaches_the_record_digest() {
        let record = record("head", 0, None, fixture_loop_state(0));
        let plain = render_state_comment_body(&record, None).expect("plain body");
        let combined =
            render_state_comment_body(&record, Some("## Approved\n\nEvidence.")).expect("combined");

        let from_plain = parse_state_marker(&plain).expect("parse").expect("record");
        let from_combined = parse_state_marker(&combined)
            .expect("parse")
            .expect("record");

        assert_eq!(from_plain, from_combined);
        assert_eq!(from_plain.record_digest, record.record_digest);
        // Two different presentations of one record are still one logical
        // append, not a fork.
        let chain =
            parse_chain([plain.as_str(), combined.as_str()], REPO, PR).expect("deduplicated");
        assert_eq!(chain.records, vec![record]);
    }

    #[test]
    fn a_combined_comment_keeps_the_outcome_visible_and_the_marker_exact() {
        let record = record("head", 0, None, fixture_loop_state(0));
        let outcome = "## Delivery outcome\n\n- decision: approve";

        let body = render_state_comment_body(&record, Some(outcome)).expect("combined body");

        assert!(body.starts_with(outcome), "{body}");
        assert!(body.ends_with(&record.marker().expect("marker")), "{body}");
        // Stripping owned markers leaves the human-readable outcome intact.
        assert!(strip_owned_markers(&body).starts_with(outcome));
        assert_eq!(
            parse_state_marker(&body).expect("parse").as_ref(),
            Some(&record)
        );
    }

    #[test]
    fn an_outcome_body_carrying_a_state_marker_is_refused() {
        // `parse_state_marker` takes the FIRST marker in the body, so an
        // embedded one would shadow this record's and silently corrupt the
        // chain the comment claims to extend.
        let forged = record("head", 0, None, fixture_loop_state(9))
            .marker()
            .expect("marker");
        let record = record("head", 0, None, fixture_loop_state(0));

        for outcome in [
            forged.clone(),
            format!("Quoted history: {forged} — see above"),
            format!("{STATE_MARKER_OPEN}deadbeef{STATE_MARKER_CLOSE}"),
        ] {
            let error = render_state_comment_body(&record, Some(&outcome))
                .expect_err("marker injection must fail closed");
            assert_eq!(error.kind(), "review_state_comment_invalid");
        }

        let empty = render_state_comment_body(&record, Some("   \n\t "))
            .expect_err("an empty outcome is not an outcome");
        assert_eq!(empty.kind(), "review_state_comment_invalid");
    }

    #[test]
    fn the_complete_rendered_body_is_size_checked_before_any_mutation() {
        let record = record("head", 0, None, fixture_loop_state(0));
        let marker_bytes = record.marker().expect("marker").len();

        // A marker that fits on its own can still overflow once visible text
        // wraps it, which is why the limit binds the complete body.
        let headroom = MAX_PROVIDER_STATE_COMMENT_BYTES - marker_bytes;
        let error = render_state_comment_body(&record, Some(&"o".repeat(headroom)))
            .expect_err("oversized combined body must fail before post");
        assert_eq!(error.kind(), "review_state_comment_too_large");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains(&format!("max_bytes={MAX_PROVIDER_STATE_COMMENT_BYTES}")),
            "{:?}",
            error.detail()
        );

        // Just inside the limit still renders.
        let fitted = render_state_comment_body(&record, Some(&"o".repeat(headroom / 2)))
            .expect("a body within the limit renders");
        assert!(fitted.len() <= MAX_PROVIDER_STATE_COMMENT_BYTES);
    }

    #[test]
    fn visible_metadata_exposes_nothing_beyond_public_pull_request_facts() {
        let record = ReviewStateRecord::new(
            "acme/widgets",
            7,
            "abcdef0123456789abcdef0123456789abcdef01",
            2,
            Some("sha256:previous".to_string()),
            ReviewStatePayload::ReviewRunReceipt {
                receipt: fixture_receipt(),
            },
        )
        .expect("record");

        let metadata = state_comment_visible_metadata(&record);

        assert_eq!(metadata.lines().count(), 1, "{metadata}");
        for forbidden in [
            "token",
            "secret",
            "credential",
            "profile",
            "/home/",
            "/Users/",
            "C:\\",
            "@",
            "sha256:",
        ] {
            assert!(!metadata.contains(forbidden), "{forbidden} in {metadata}");
        }
        // The abbreviated head is a prefix of the public head, never the whole
        // record or any digest.
        assert!(record.expected_head.starts_with("abcdef012345"));
        assert!(metadata.ends_with("head abcdef012345"));
    }

    #[test]
    fn an_unusable_head_still_renders_one_plain_line() {
        // The head comes from the provider. Nothing it contains may break the
        // visible line into extra Markdown.
        for (head, expected) in [
            ("", "-"),
            ("  ", "-"),
            ("a\nb", "ab"),
            ("**bold**", "bold"),
            ("héad-1234567890", "had-12345678"),
        ] {
            let record = ReviewStateRecord::new(
                "acme/widgets",
                7,
                head,
                0,
                None,
                ReviewStatePayload::ReviewRunReceipt {
                    receipt: fixture_receipt(),
                },
            )
            .expect("record");
            let metadata = state_comment_visible_metadata(&record);
            assert_eq!(metadata.lines().count(), 1, "{metadata}");
            assert!(
                metadata.ends_with(&format!("head {expected}")),
                "{metadata}"
            );
        }
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
    fn untyped_review_loop_state_is_rejected() {
        let body = r#"{"schema":"forge-cli.review-loop.v1","repository":"acme/widgets","pr":7,"expected_head":"head","generation":0,"previous_digest":null,"payload":{"kind":"review-loop","state":{"round":1}},"record_digest":"sha256:invalid"}"#;
        let marker = format!(
            "{STATE_MARKER_OPEN}{}{STATE_MARKER_CLOSE}",
            hex_encode(body.as_bytes())
        );

        let err = parse_state_marker(&marker).expect_err("partial loop state must fail closed");
        assert_eq!(err.kind(), "review_state_conflict");
    }

    fn observation(fingerprint: &str) -> ReviewFindingObservation {
        ReviewFindingObservation {
            fingerprint: fingerprint.to_string(),
            root_cause_fingerprint: None,
            blocking: true,
            status: ReviewFindingStatus::Open,
            threads: Vec::new(),
        }
    }

    #[test]
    fn same_head_same_findings_is_an_idempotent_observation() {
        let initial = observe_review_loop(
            None,
            "head-a",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("genesis");

        let retry = observe_review_loop(
            Some(&initial.state),
            "head-a",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("retry");

        assert!(!retry.changed);
        assert_eq!(retry.state, initial.state);
    }

    #[test]
    fn same_head_observation_cannot_omit_an_open_blocking_finding() {
        let initial = observe_review_loop(
            None,
            "head-a",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("genesis");

        let error = observe_review_loop(Some(&initial.state), "head-a", &[])
            .expect_err("same-head omission must not disposition a blocking finding");

        assert_eq!(error.kind(), "review_state_conflict");
        assert_eq!(
            initial.state.findings["correctness:review-loop:typed-state"].status,
            ReviewFindingStatus::Open
        );
    }

    #[test]
    fn explicit_same_head_disposition_is_auditable_and_non_blocking() {
        let initial = observe_review_loop(
            None,
            "head-a",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("genesis");
        let mut accepted = observation("correctness:review-loop:typed-state");
        accepted.status = ReviewFindingStatus::Accepted;

        let transition = observe_review_loop(Some(&initial.state), "head-a", &[accepted])
            .expect("explicit disposition");
        let finding = &transition.state.findings["correctness:review-loop:typed-state"];

        assert_eq!(finding.status, ReviewFindingStatus::Accepted);
        assert!(!finding.blocking);
        assert_eq!(transition.state.round, 0);
    }

    #[test]
    fn extension_requires_and_consumes_the_exact_durable_hard_stop() {
        let mut state = observe_review_loop(
            None,
            "head-a",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("genesis")
        .state;
        state.budget.max_repair_rounds = 0;
        let attempted = [observation("correctness:review-loop:typed-state")];
        let error = observe_review_loop(Some(&state), "head-b", &attempted)
            .expect_err("round budget exhausted");
        let stopped =
            record_review_loop_hard_stop(&state, "head-b", &attempted, "sha256:state-tip", &error)
                .expect("durable stop");
        let stop = stopped.hard_stop.as_ref().expect("stop receipt");

        let restarted = observe_review_loop(Some(&stopped), "head-b", &attempted)
            .expect_err("restart returns the same stop");
        assert_eq!(restarted.kind(), "review_round_limit_exceeded");
        assert!(
            restarted
                .detail()
                .unwrap_or_default()
                .contains(&stop.proposal_digest)
        );

        let wrong_field = apply_review_loop_extension(
            &stopped,
            stop.proposal_digest.clone(),
            "https://github.com/acme/widgets/pull/7#issuecomment-9".into(),
            "review_round_limit_exceeded",
            "max_no_progress_rounds",
            1,
        )
        .expect_err("stop code and field must match");
        assert_eq!(wrong_field.kind(), "review_extension_invalid");

        let extended = apply_review_loop_extension(
            &stopped,
            stop.proposal_digest.clone(),
            "https://github.com/acme/widgets/pull/7#issuecomment-9".into(),
            "review_round_limit_exceeded",
            "max_repair_rounds",
            1,
        )
        .expect("exact extension");
        let resumed = observe_review_loop(Some(&extended), "head-b", &attempted)
            .expect("one approved round resumes");
        assert_eq!(resumed.state.round, 1);
        assert!(resumed.state.hard_stop.is_none());
    }

    #[test]
    fn fixed_finding_reappearance_stops_without_mutating_prior_state() {
        let initial = observe_review_loop(
            None,
            "head-a",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("genesis");
        let fixed = observe_review_loop(Some(&initial.state), "head-b", &[])
            .expect("finding fixed on repaired head");

        let error = observe_review_loop(
            Some(&fixed.state),
            "head-c",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect_err("zero automatic reopens must stop");

        assert_eq!(error.kind(), "review_finding_reopened");
        assert_eq!(
            fixed.state.findings["correctness:review-loop:typed-state"].status,
            ReviewFindingStatus::Fixed
        );
    }

    #[test]
    fn no_progress_and_round_limits_are_derived_from_durable_state() {
        let mut state = observe_review_loop(
            None,
            "head-a",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("genesis")
        .state;
        state.budget.max_repair_rounds = 2;
        state.budget.max_no_progress_rounds = 1;

        let round_one = observe_review_loop(
            Some(&state),
            "head-b",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect("first no-progress round");
        let no_progress = observe_review_loop(
            Some(&round_one.state),
            "head-c",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect_err("second no-progress round exceeds the configured maximum");
        assert_eq!(no_progress.kind(), "review_no_progress");

        let mut round_limited = round_one.state;
        round_limited.budget.max_no_progress_rounds = 10;
        round_limited.round = 2;
        let round_limit = observe_review_loop(
            Some(&round_limited),
            "head-z",
            &[observation("correctness:review-loop:typed-state")],
        )
        .expect_err("round three exceeds a two-round budget");
        assert_eq!(round_limit.kind(), "review_round_limit_exceeded");
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

    // ---------------------------------------------------------------------
    // The chain is the only durable proof that a bounded review ran. Every
    // guard below is what stops a forged, replayed, or forked comment history
    // from being accepted as that proof.
    // ---------------------------------------------------------------------

    const REPO: &str = "acme/widgets";
    const PR: u64 = 7;
    const FP: &str = "correctness:review-loop:one";

    fn record(
        head: &str,
        generation: u64,
        previous: Option<String>,
        state: ReviewLoopState,
    ) -> ReviewStateRecord {
        ReviewStateRecord::new(
            REPO,
            PR,
            head,
            generation,
            previous,
            ReviewStatePayload::ReviewLoop { state },
        )
        .expect("record")
    }

    fn observed(fingerprint: &str, status: ReviewFindingStatus) -> ReviewFindingObservation {
        ReviewFindingObservation {
            fingerprint: fingerprint.to_string(),
            root_cause_fingerprint: None,
            blocking: true,
            status,
            threads: Vec::new(),
        }
    }

    fn genesis_with(observations: &[ReviewFindingObservation]) -> ReviewLoopState {
        observe_review_loop(None, "head", observations)
            .expect("genesis")
            .state
    }

    #[test]
    fn an_empty_comment_history_is_a_valid_genesis_chain() {
        let chain = parse_chain(Vec::<&str>::new(), REPO, PR).expect("empty chain");

        assert!(chain.records.is_empty());
        assert_eq!(chain.tip_digest, None);
        assert_eq!(latest_review_loop_state(&chain), None);
    }

    #[test]
    fn a_record_for_another_pull_request_is_refused() {
        let marker = record("head", 0, None, fixture_loop_state(0))
            .marker()
            .expect("marker");

        let wrong_repo =
            parse_chain([marker.as_str()], "acme/other", PR).expect_err("repository mismatch");
        assert_eq!(wrong_repo.kind(), "review_state_conflict");

        let wrong_pr = parse_chain([marker.as_str()], REPO, 8).expect_err("pr mismatch");
        assert!(
            wrong_pr
                .detail()
                .unwrap_or_default()
                .contains("expected=acme/widgets#8"),
            "detail should name both identities: {:?}",
            wrong_pr.detail()
        );
    }

    #[test]
    fn a_tampered_record_digest_is_refused() {
        let mut forged = record("head", 0, None, fixture_loop_state(0));
        forged.record_digest = "sha256:forged".to_string();
        let marker = forged.marker().expect("marker");

        let error = parse_chain([marker.as_str()], REPO, PR).expect_err("forged digest");

        assert_eq!(error.kind(), "review_state_conflict");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("record_digest=sha256:forged"),
            "{:?}",
            error.detail()
        );
    }

    #[test]
    fn an_identical_record_seen_twice_is_deduplicated() {
        // A provider can echo the same comment on two pages; that must not be
        // read as a competing generation.
        let marker = record("head", 0, None, fixture_loop_state(0))
            .marker()
            .expect("marker");

        let chain =
            parse_chain([marker.as_str(), marker.as_str()], REPO, PR).expect("deduplicated");

        assert_eq!(chain.records.len(), 1);
    }

    #[test]
    fn a_chain_without_exactly_one_genesis_is_refused() {
        let orphan = record(
            "head",
            1,
            Some("sha256:absent".to_string()),
            fixture_loop_state(1),
        )
        .marker()
        .expect("marker");
        let error = parse_chain([orphan.as_str()], REPO, PR).expect_err("no genesis");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("genesis_count=0"),
            "{:?}",
            error.detail()
        );

        let genesis_a = record("head", 0, None, fixture_loop_state(0));
        let mut forked = fixture_loop_state(0);
        forked.no_progress_rounds = 1;
        let genesis_b = record("head", 0, None, forked);
        let error = parse_chain(
            [
                genesis_a.marker().unwrap().as_str(),
                genesis_b.marker().unwrap().as_str(),
            ],
            REPO,
            PR,
        )
        .expect_err("two genesis records");
        // Two roots are a fork, which the competing-generation guard catches
        // before the genesis count is ever consulted.
        assert_eq!(
            error.message(),
            "review-state chain contains competing generations"
        );
        assert_eq!(error.kind(), "review_state_conflict");
    }

    #[test]
    fn a_non_contiguous_generation_is_refused() {
        let genesis = record("head", 0, None, fixture_loop_state(0));
        let skipped = record(
            "head",
            5,
            Some(genesis.record_digest.clone()),
            fixture_loop_state(1),
        );

        let error = parse_chain(
            [
                genesis.marker().unwrap().as_str(),
                skipped.marker().unwrap().as_str(),
            ],
            REPO,
            PR,
        )
        .expect_err("generation gap");

        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("expected_generation=1; observed_generation=5"),
            "{:?}",
            error.detail()
        );
    }

    #[test]
    fn a_review_loop_state_bound_to_another_head_is_refused() {
        let mut state = fixture_loop_state(0);
        state.head_sha = "other-head".to_string();
        let marker = record("head", 0, None, state).marker().expect("marker");

        let error = parse_chain([marker.as_str()], REPO, PR).expect_err("head mismatch");

        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("record_head=head; state_head=other-head"),
            "{:?}",
            error.detail()
        );
    }

    #[test]
    fn a_receipt_record_does_not_shadow_the_latest_loop_state() {
        let loop_state = genesis_with(&[observed(FP, ReviewFindingStatus::Open)]);
        let genesis = record("head", 0, None, loop_state.clone());
        let receipt = ReviewStateRecord::new(
            REPO,
            PR,
            "head",
            1,
            Some(genesis.record_digest.clone()),
            ReviewStatePayload::ReviewRunReceipt {
                receipt: fixture_receipt(),
            },
        )
        .expect("receipt record");

        let chain = parse_chain(
            [
                genesis.marker().unwrap().as_str(),
                receipt.marker().unwrap().as_str(),
            ],
            REPO,
            PR,
        )
        .expect("chain");

        assert_eq!(chain.records.len(), 2);
        assert_eq!(chain.tip_digest.as_deref(), Some(&*receipt.record_digest));
        assert_eq!(
            latest_review_loop_state(&chain),
            Some(&loop_state),
            "the newest loop state must be found behind the receipt"
        );
    }

    #[test]
    fn state_markers_are_only_recognized_when_they_are_well_formed() {
        assert_eq!(parse_state_marker("no marker here").expect("none"), None);

        let unterminated = format!("{STATE_MARKER_OPEN}deadbeef");
        assert_eq!(
            parse_state_marker(&unterminated)
                .expect_err("unterminated")
                .kind(),
            "review_state_conflict"
        );

        for payload in ["", "xyz", "abc"] {
            let body = format!("{STATE_MARKER_OPEN}{payload}{STATE_MARKER_CLOSE}");
            assert!(
                parse_state_marker(&body).is_err(),
                "payload {payload:?} is not canonical hex"
            );
        }

        // Canonical hex that decodes to non-JSON is still refused.
        let not_json = format!("{STATE_MARKER_OPEN}7b7b{STATE_MARKER_CLOSE}");
        assert!(parse_state_marker(&not_json).is_err());

        // Canonical hex that decodes to invalid UTF-8 is refused too.
        let not_utf8 = format!("{STATE_MARKER_OPEN}ff{STATE_MARKER_CLOSE}");
        assert!(parse_state_marker(&not_utf8).is_err());
    }

    #[test]
    fn a_lifecycle_fingerprint_must_be_three_stable_parts() {
        for bad in [
            "correctness",
            "correctness:review-loop",
            "correctness:review-loop:one:extra",
            "Correctness:review-loop:one",
            "correctness::one",
            "-lead:review-loop:one",
            "trail-:review-loop:one",
        ] {
            let error =
                observe_review_loop(None, "head", &[observed(bad, ReviewFindingStatus::Open)])
                    .expect_err("bad fingerprint");
            assert_eq!(error.kind(), "review_fingerprint_collision", "{bad}");
        }

        observe_review_loop(
            None,
            "head",
            &[observed(
                "perf-2:review-loop:n-1",
                ReviewFindingStatus::Open,
            )],
        )
        .expect("digits and inner dashes are stable");
    }

    #[test]
    fn one_lifecycle_identity_cannot_describe_two_different_observations() {
        let mut second = observed(FP, ReviewFindingStatus::Open);
        second.threads = vec!["PRRT_1".to_string()];

        let error = observe_review_loop(
            None,
            "head",
            &[observed(FP, ReviewFindingStatus::Open), second.clone()],
        )
        .expect_err("collision");
        assert_eq!(error.kind(), "review_fingerprint_collision");

        // The same observation repeated is collapsed, not rejected.
        observe_review_loop(None, "head", &[second.clone(), second]).expect("idempotent repeat");
    }

    #[test]
    fn an_empty_head_is_refused_before_any_observation_work() {
        let error = observe_review_loop(None, "  ", &[observed(FP, ReviewFindingStatus::Open)]);
        assert_eq!(
            error.expect_err("empty head").kind(),
            "review_state_conflict"
        );
    }

    #[test]
    fn a_same_head_observation_cannot_drop_or_silently_clear_an_open_finding() {
        let previous = genesis_with(&[observed(FP, ReviewFindingStatus::Open)]);

        let omitted =
            observe_review_loop(Some(&previous), "head", &[]).expect_err("omitted open finding");
        assert!(
            omitted
                .detail()
                .unwrap_or_default()
                .contains(&format!("fingerprint={FP}")),
            "{:?}",
            omitted.detail()
        );

        // Declaring it fixed at the same head means nothing was repaired.
        let cleared = observe_review_loop(
            Some(&previous),
            "head",
            &[observed(FP, ReviewFindingStatus::Fixed)],
        )
        .expect_err("fixed without a new head");
        assert_eq!(cleared.kind(), "review_state_conflict");

        // Downgrading it to non-blocking at the same head is the same trick.
        let mut downgraded = observed(FP, ReviewFindingStatus::Open);
        downgraded.blocking = false;
        let error = observe_review_loop(Some(&previous), "head", &[downgraded])
            .expect_err("silent downgrade");
        assert_eq!(error.kind(), "review_state_conflict");
    }

    #[test]
    fn advancing_the_head_marks_unobserved_open_findings_fixed() {
        let previous = genesis_with(&[observed(FP, ReviewFindingStatus::Open)]);

        let transition =
            observe_review_loop(Some(&previous), "head-2", &[]).expect("repaired head");

        assert!(transition.changed);
        assert_eq!(transition.state.round, 1);
        assert_eq!(
            transition.state.findings[FP].status,
            ReviewFindingStatus::Fixed
        );
        // The sweep only flips the status; the finding's last observed head and
        // seen count stay pinned to the round that actually saw it.
        assert_eq!(transition.state.findings[FP].last_seen_head, "head");
        assert_eq!(transition.state.findings[FP].seen_count, 1);
    }

    #[test]
    fn a_reopened_finding_stops_the_loop_when_no_reopen_budget_remains() {
        let first = genesis_with(&[observed(FP, ReviewFindingStatus::Open)]);
        let fixed = observe_review_loop(Some(&first), "head-2", &[])
            .expect("repair")
            .state;

        // The default budget allows zero automatic reopens.
        let error = observe_review_loop(
            Some(&fixed),
            "head-3",
            &[observed(FP, ReviewFindingStatus::Open)],
        )
        .expect_err("reopened");
        assert_eq!(error.kind(), "review_finding_reopened");
        assert_eq!(
            stop_budget_field(error.kind()),
            Some("max_auto_reopens_per_fingerprint")
        );

        // With budget, the reopen is absorbed and counted.
        let mut generous = fixed;
        generous.budget.max_auto_reopens_per_fingerprint = 1;
        let transition = observe_review_loop(
            Some(&generous),
            "head-3",
            &[observed(FP, ReviewFindingStatus::Open)],
        )
        .expect("reopen within budget");
        assert_eq!(transition.state.findings[FP].reopen_count, 1);
        assert_eq!(
            transition.state.findings[FP].status,
            ReviewFindingStatus::Open
        );
    }

    #[test]
    fn a_repeated_identical_observation_reports_no_change() {
        let previous = genesis_with(&[observed(FP, ReviewFindingStatus::Open)]);

        let transition = observe_review_loop(
            Some(&previous),
            "head",
            &[observed(FP, ReviewFindingStatus::Open)],
        )
        .expect("unchanged");

        assert!(!transition.changed);
        assert_eq!(transition.state, previous);
    }

    #[test]
    fn an_extension_proposal_digest_is_bound_to_every_input() {
        let base = extension_proposal_digest(
            "sha256:tip",
            "review_round_limit_exceeded",
            "max_repair_rounds",
            1,
        )
        .expect("digest");
        assert!(base.starts_with("sha256:"));

        for (tip, code, field, increment) in [
            (
                "sha256:other",
                "review_round_limit_exceeded",
                "max_repair_rounds",
                1,
            ),
            ("sha256:tip", "review_no_progress", "max_repair_rounds", 1),
            (
                "sha256:tip",
                "review_round_limit_exceeded",
                "max_no_progress_rounds",
                1,
            ),
            (
                "sha256:tip",
                "review_round_limit_exceeded",
                "max_repair_rounds",
                2,
            ),
        ] {
            assert_ne!(
                extension_proposal_digest(tip, code, field, increment).expect("digest"),
                base,
                "digest must change for ({tip}, {code}, {field}, {increment})"
            );
        }

        for increment in [0, 101] {
            assert_eq!(
                extension_proposal_digest(
                    "sha256:tip",
                    "review_round_limit_exceeded",
                    "max_repair_rounds",
                    increment
                )
                .expect_err("out of range")
                .kind(),
                "review_extension_invalid"
            );
        }
        assert_eq!(
            extension_proposal_digest(
                "sha256:tip",
                "review_round_limit_exceeded",
                "max_everything",
                1
            )
            .expect_err("unknown field")
            .kind(),
            "review_extension_invalid"
        );
    }

    #[test]
    fn only_a_budget_stop_can_be_recorded_for_extension() {
        let previous = genesis_with(&[observed(FP, ReviewFindingStatus::Open)]);
        let not_a_budget_stop = ForgeError::validation(
            error_schema(),
            "review_state_conflict",
            "something else",
            None,
        );

        let error = record_review_loop_hard_stop(
            &previous,
            "head-2",
            &[observed(FP, ReviewFindingStatus::Open)],
            "sha256:tip",
            &not_a_budget_stop,
        )
        .expect_err("not a budget stop");
        assert_eq!(error.kind(), "review_state_conflict");

        let budget_stop = ForgeError::validation(
            error_schema(),
            "review_round_limit_exceeded",
            "budget exhausted",
            None,
        );
        let stopped = record_review_loop_hard_stop(
            &previous,
            "head-2",
            &[observed(FP, ReviewFindingStatus::Open)],
            "sha256:tip",
            &budget_stop,
        )
        .expect("hard stop");
        let stop = stopped.hard_stop.as_ref().expect("stop recorded");
        assert_eq!(stop.code, "review_round_limit_exceeded");
        assert_eq!(stop.budget_field, "max_repair_rounds");
        assert_eq!(stop.increment, 1);
        assert_eq!(stop.attempted_head_sha, "head-2");
        assert!(!stop.extension_applied);
        assert!(!stop.observation_digest.is_empty());
    }

    #[test]
    fn an_extension_proposal_cannot_be_consumed_twice() {
        let previous = genesis_with(&[observed(FP, ReviewFindingStatus::Open)]);
        let budget_stop = ForgeError::validation(
            error_schema(),
            "review_round_limit_exceeded",
            "budget exhausted",
            None,
        );
        let stopped = record_review_loop_hard_stop(
            &previous,
            "head-2",
            &[observed(FP, ReviewFindingStatus::Open)],
            "sha256:tip",
            &budget_stop,
        )
        .expect("hard stop");
        let proposal = stopped
            .hard_stop
            .as_ref()
            .expect("stop")
            .proposal_digest
            .clone();

        let extended = apply_review_loop_extension(
            &stopped,
            proposal.clone(),
            "https://github.com/acme/widgets/pull/7#issuecomment-1".to_string(),
            "review_round_limit_exceeded",
            "max_repair_rounds",
            1,
        )
        .expect("extension applied");
        assert_eq!(
            extended.budget.max_repair_rounds,
            previous.budget.max_repair_rounds + 1
        );
        assert!(extended.hard_stop.as_ref().expect("stop").extension_applied);

        // Applying it again is refused because the stop is already consumed.
        assert_eq!(
            apply_review_loop_extension(
                &extended,
                proposal.clone(),
                "https://example.invalid/2".to_string(),
                "review_round_limit_exceeded",
                "max_repair_rounds",
                1,
            )
            .expect_err("already applied")
            .kind(),
            "review_extension_invalid"
        );

        // A state with no hard stop has nothing to extend.
        assert_eq!(
            apply_review_loop_extension(
                &previous,
                proposal,
                "https://example.invalid/3".to_string(),
                "review_round_limit_exceeded",
                "max_repair_rounds",
                1,
            )
            .expect_err("no stop")
            .kind(),
            "review_extension_invalid"
        );
    }

    #[test]
    fn budget_field_mapping_is_closed() {
        assert_eq!(
            stop_budget_field("review_round_limit_exceeded"),
            Some("max_repair_rounds")
        );
        assert_eq!(
            stop_budget_field("review_no_progress"),
            Some("max_no_progress_rounds")
        );
        assert_eq!(
            stop_budget_field("review_finding_reopened"),
            Some("max_auto_reopens_per_fingerprint")
        );
        assert_eq!(stop_budget_field("review_state_conflict"), None);
        assert_eq!(stop_budget_field(""), None);
    }

    #[test]
    fn owned_markers_are_recognized_and_stripped_without_touching_prose() {
        let run = "sha256:run";
        let body = format!(
            "Review body\n{}\n{}\n{}\nTrailing prose\n",
            review_run_marker(run),
            finding_marker(run, "sha256:digest"),
            thread_disposition_marker("PRRT_1"),
        );

        assert_eq!(parse_review_run_id(&body).as_deref(), Some(run));
        assert_eq!(
            parse_finding_marker(&body),
            Some((run.to_string(), "sha256:digest".to_string()))
        );
        assert!(has_thread_disposition_marker(&body));
        assert_eq!(strip_owned_markers(&body), "Review body\nTrailing prose");

        // A body with no markers is left byte-identical apart from trailing space.
        assert_eq!(strip_owned_markers("just prose\n"), "just prose");
        assert_eq!(parse_review_run_id("just prose"), None);
        assert_eq!(parse_finding_marker("just prose"), None);
        assert!(!has_thread_disposition_marker("just prose"));
    }

    #[test]
    fn malformed_markers_are_not_recognized() {
        // An empty id, or one carrying whitespace, is not a usable run id.
        assert_eq!(parse_review_run_id(&review_run_marker("")), None);
        assert_eq!(parse_review_run_id(&review_run_marker("two words")), None);
        assert!(!has_thread_disposition_marker(&thread_disposition_marker(
            ""
        )));

        // A finding marker needs both halves.
        assert_eq!(parse_finding_marker(&finding_marker("", "sha256:d")), None);
        assert_eq!(parse_finding_marker(&finding_marker("run", "")), None);
    }

    #[test]
    fn a_review_run_id_is_bound_to_every_preimage_field() {
        let base = compute_review_run_id(
            REPO,
            PR,
            "head",
            0,
            &["testing".to_string()],
            "comments-only",
            "sha256:summary",
            &[],
        )
        .expect("run id");
        assert!(base.starts_with("sha256:"));

        let variants = [
            compute_review_run_id(
                "acme/other",
                PR,
                "head",
                0,
                &["testing".to_string()],
                "comments-only",
                "sha256:summary",
                &[],
            ),
            compute_review_run_id(
                REPO,
                8,
                "head",
                0,
                &["testing".to_string()],
                "comments-only",
                "sha256:summary",
                &[],
            ),
            compute_review_run_id(
                REPO,
                PR,
                "head-2",
                0,
                &["testing".to_string()],
                "comments-only",
                "sha256:summary",
                &[],
            ),
            compute_review_run_id(
                REPO,
                PR,
                "head",
                1,
                &["testing".to_string()],
                "comments-only",
                "sha256:summary",
                &[],
            ),
            compute_review_run_id(
                REPO,
                PR,
                "head",
                0,
                &["security".to_string()],
                "comments-only",
                "sha256:summary",
                &[],
            ),
            compute_review_run_id(
                REPO,
                PR,
                "head",
                0,
                &["testing".to_string()],
                "approve",
                "sha256:summary",
                &[],
            ),
            compute_review_run_id(
                REPO,
                PR,
                "head",
                0,
                &["testing".to_string()],
                "comments-only",
                "sha256:other",
                &[],
            ),
        ];
        for variant in variants {
            assert_ne!(variant.expect("run id"), base);
        }
    }

    #[test]
    fn the_digest_helper_is_prefixed_and_lowercase_hex() {
        let digest = sha256_digest(b"payload");

        assert!(digest.starts_with("sha256:"));
        let hex = digest.trim_start_matches("sha256:");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(sha256_digest(b"payload"), digest, "digest is deterministic");
        assert_ne!(sha256_digest(b"payload2"), digest);
    }
}
