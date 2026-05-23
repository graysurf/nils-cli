use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::commands::record::{LifecycleCommentKind, RecordProfile};

#[derive(Debug, Clone)]
pub struct DashboardInput {
    pub profile: RecordProfile,
    pub status: String,
    pub target_scope: String,
    pub current: String,
    pub next_action: String,
    pub validation: String,
    pub linked_prs: Vec<String>,
    pub blockers: Vec<String>,
    pub approval: String,
    pub source_url: Option<String>,
    pub plan_url: Option<String>,
    pub state_url: Option<String>,
    pub session_url: Option<String>,
    pub validation_url: Option<String>,
    pub review_url: Option<String>,
    pub closeout_url: Option<String>,
    pub title: Option<String>,
    pub issue_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CommentInput {
    pub profile: RecordProfile,
    pub kind: LifecycleCommentKind,
    pub path: Option<String>,
    pub commit: Option<String>,
    pub content: Option<String>,
    pub title: Option<String>,
    pub details_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LifecycleEvidence {
    pub role: PayloadRole,
    pub profile: PayloadProfile,
    pub url: Option<String>,
    pub created_at: Option<String>,
    /// Stable status string derived from the structured payload (e.g.
    /// state `status`, validation `overall`, review `decision`). `None`
    /// when the role does not declare a status or the payload could not
    /// be parsed.
    pub status: Option<String>,
    /// Parsed structured payload. `None` when the comment lacks a
    /// `plan-issue-record-payload` fence; audit still records the marker
    /// for visibility but downstream gates treat missing payloads as
    /// unparseable evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<RecordPayload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnsupportedMarker {
    pub marker_prefix: String,
    pub url: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordAudit {
    pub profile_filter: Option<String>,
    pub body_sections: BodySections,
    /// Latest v2 lifecycle evidence indexed by role name (`source`,
    /// `plan`, `state`, `session`, `validation`, `review`, `closeout`).
    pub evidence: BTreeMap<String, LifecycleEvidence>,
    /// Stable machine-readable codes for missing required evidence
    /// (e.g. `source-missing`, `plan-missing`, `state-missing`).
    pub missing_required: Vec<String>,
    /// Pre-v2 markers seen during audit. Reported for visibility but not
    /// counted as current lifecycle evidence.
    pub unsupported_markers: Vec<UnsupportedMarker>,
    pub recognized_count: usize,
    #[serde(skip_serializing)]
    pub evidence_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BodySections {
    pub current_dashboard: bool,
    pub final_dashboard: bool,
    pub durable_record: bool,
    pub closeout_checks: bool,
    pub task_decomposition: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloseoutCheck {
    pub check: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloseoutGateResult {
    pub ready: bool,
    pub checks: Vec<CloseoutCheck>,
}

#[derive(Debug, Clone)]
pub struct CloseoutGateInput {
    pub profile: RecordProfile,
    pub require_complete: bool,
    pub require_session: bool,
    pub require_validation: bool,
    pub require_review: bool,
    pub require_closeout: bool,
    pub approval: Option<String>,
    pub linked_prs: Vec<String>,
}

#[derive(Debug)]
struct CommentJson {
    body: Option<String>,
    url: Option<String>,
    html_url: Option<String>,
    created_at: Option<String>,
}

pub fn render_dashboard(input: DashboardInput) -> String {
    let linked_prs = non_empty_join(&input.linked_prs, "none yet");
    let blockers = non_empty_join(&input.blockers, "none");

    let mut out = Vec::new();
    let dashboard_title = if input.status.trim().eq_ignore_ascii_case("complete") {
        "## Final Dashboard"
    } else {
        "## Current Dashboard"
    };
    out.push(dashboard_title.to_string());
    out.push(String::new());
    out.push("This issue is the durable tracking surface for an issue-backed plan execution. The full source, plan, and execution logs remain in".to_string());
    out.push("append-only issue comments.".to_string());
    out.push(String::new());
    out.push(format!("- Status: {}", input.status.trim()));
    out.push(format!("- Profile: {}", input.profile.as_str()));
    out.push(format!("- Target scope: {}", input.target_scope.trim()));
    out.push(format!("- Current task: {}", input.current.trim()));
    out.push(format!("- Next action: {}", input.next_action.trim()));
    out.push(format!("- Validation: {}", input.validation.trim()));
    out.push(format!("- Linked PRs: {linked_prs}"));
    out.push(format!("- Blockers: {blockers}"));
    out.push(format!("- Review approval: {}", input.approval.trim()));
    out.push(String::new());
    out.push("## Durable Record".to_string());
    out.push(String::new());
    out.push(format!(
        "- Source snapshot: {}",
        dashboard_link(input.source_url.as_deref(), "source snapshot")
    ));
    out.push(format!(
        "- Plan snapshot: {}",
        dashboard_link(input.plan_url.as_deref(), "plan snapshot")
    ));
    out.push(format!(
        "- Execution state: {}",
        dashboard_link(input.state_url.as_deref(), "execution state")
    ));
    out.push(format!(
        "- Latest session: {}",
        dashboard_link(input.session_url.as_deref(), "Execution Session")
    ));
    out.push(format!(
        "- Latest validation: {}",
        dashboard_link(input.validation_url.as_deref(), "Validation Evidence")
    ));
    if input.profile == RecordProfile::Dispatch || input.review_url.is_some() {
        out.push(format!(
            "- Latest review: {}",
            dashboard_link(input.review_url.as_deref(), "Review Evidence")
        ));
    }
    out.push(format!(
        "- Closeout comment: {}",
        dashboard_link(input.closeout_url.as_deref(), "closeout")
    ));
    out.push(String::new());
    out.push("## Guardrails".to_string());
    out.push(String::new());
    out.push("- The issue body is a mutable dashboard only.".to_string());
    out.push("- Append-only issue comments are the durable source of truth.".to_string());
    out.push(
        "- `plan-tooling` owns plan parsing, validation, batching, and PR split modeling only."
            .to_string(),
    );
    out.push("- Provider create, comment, edit, and close operations remain owned by `forge-cli` or provider atoms.".to_string());

    if input.title.is_some() || input.issue_url.is_some() {
        out.push(String::new());
        out.push("## Original Tracker".to_string());
        out.push(String::new());
        if let Some(title) = input
            .title
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            out.push(format!("- Title: {}", title.trim()));
        }
        if let Some(url) = input
            .issue_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            out.push(format!("- Issue: {}", url.trim()));
        }
    }

    finalize_markdown(out)
}

pub fn render_comment(input: CommentInput) -> Result<String, String> {
    let marker = marker_for(input.profile, input.kind);
    let heading = input
        .title
        .clone()
        .unwrap_or_else(|| default_heading(input.profile, input.kind).to_string());
    let content = input.content.unwrap_or_default();

    let mut out = Vec::new();
    out.push(marker);
    out.push(String::new());
    out.push(format!("## {heading}"));
    out.push(String::new());

    out.push(format!("- Profile: {}", input.profile.as_str()));
    if let Some(path) = input
        .path
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        out.push(format!("- Path: `{}`", path.trim()));
    }
    if let Some(commit) = input
        .commit
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        out.push(format!("- Commit: `{}`", commit.trim()));
    }

    if matches!(
        input.kind,
        LifecycleCommentKind::Source | LifecycleCommentKind::Plan
    ) {
        if !out.last().is_some_and(String::is_empty) {
            out.push(String::new());
        }
        out.push("- Snapshot mode: local committed Markdown".to_string());
        out.push(String::new());
        out.push("<details>".to_string());
        out.push(format!(
            "<summary>{}</summary>",
            input
                .details_summary
                .as_deref()
                .unwrap_or_else(|| default_details_summary(input.kind))
        ));
        out.push(String::new());
        out.push(content);
        out.push(String::new());
        out.push("</details>".to_string());
    } else if !content.trim().is_empty() {
        out.push(String::new());
        out.push(content);
    }

    Ok(finalize_markdown(out))
}

pub fn audit_record(
    body: Option<&str>,
    comments_json: &str,
    profile_filter: Option<RecordProfile>,
) -> Result<RecordAudit, String> {
    let comments = parse_comments_json(comments_json)?;
    let mut evidence: BTreeMap<String, LifecycleEvidence> = BTreeMap::new();
    let mut unsupported_markers = Vec::new();
    let mut recognized_count = 0usize;
    let mut evidence_text = String::new();
    if let Some(body) = body {
        evidence_text.push_str(body);
        evidence_text.push('\n');
    }

    for comment in comments {
        let Some(comment_body) = comment.body.as_deref() else {
            continue;
        };
        let Some(first_marker) = first_comment_marker(comment_body) else {
            continue;
        };
        let Some(parsed) = parse_marker_line(first_marker) else {
            continue;
        };
        match parsed {
            MarkerParse::V2 { role, profile } => {
                if profile_filter.is_some_and(|expected| profile != PayloadProfile::from(expected))
                {
                    continue;
                }
                let url = comment.url.clone().or_else(|| comment.html_url.clone());
                let created_at = comment.created_at.clone();
                let payload = match extract_payload(comment_body) {
                    Ok(payload) => Some(payload),
                    Err(err) if err.kind == PayloadErrorKind::NoFence => None,
                    Err(err) => {
                        return Err(format!(
                            "comment at {} has malformed payload: {}",
                            url.as_deref().unwrap_or("(unknown url)"),
                            err.message
                        ));
                    }
                };
                let status = payload.as_ref().and_then(derive_status_from_payload);
                let candidate = LifecycleEvidence {
                    role,
                    profile,
                    url,
                    created_at: created_at.clone(),
                    status,
                    payload,
                };
                let key = role.as_str().to_string();
                let supersedes = evidence
                    .get(&key)
                    .map(|existing| {
                        compare_created_at(
                            candidate.created_at.as_deref(),
                            existing.created_at.as_deref(),
                        ) != std::cmp::Ordering::Less
                    })
                    .unwrap_or(true);
                if supersedes {
                    evidence_text.push_str(comment_body);
                    evidence_text.push('\n');
                    recognized_count += 1;
                    evidence.insert(key, candidate);
                }
            }
            MarkerParse::Unsupported { prefix } => {
                unsupported_markers.push(UnsupportedMarker {
                    marker_prefix: prefix,
                    url: comment.url.or(comment.html_url),
                    created_at: comment.created_at,
                });
            }
        }
    }

    let mut missing_required = Vec::new();
    for code in ["source-missing", "plan-missing", "state-missing"] {
        let role_key = code.trim_end_matches("-missing");
        if !evidence.contains_key(role_key) {
            missing_required.push(code.to_string());
        }
    }

    Ok(RecordAudit {
        profile_filter: profile_filter.map(|profile| profile.as_str().to_string()),
        body_sections: inspect_body_sections(body.unwrap_or_default()),
        evidence,
        missing_required,
        unsupported_markers,
        recognized_count,
        evidence_text,
    })
}

/// Compare two RFC3339 created-at strings. `None` is considered older than
/// any `Some(_)`. This keeps latest-by-role selection deterministic even
/// when GitHub returns comments out of order.
fn compare_created_at(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(l), Some(r)) => l.cmp(r),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Derive a stable visible status string from a parsed payload, suitable
/// for dashboard rendering and closeout gating. Returns `None` when the
/// role does not declare a status or the payload's status field is
/// unparseable.
fn derive_status_from_payload(payload: &RecordPayload) -> Option<String> {
    match payload.role {
        PayloadRole::State => payload
            .parse_state()
            .ok()
            .and_then(|data| data.status.map(|s| status_state_label(s).to_string())),
        PayloadRole::Validation => payload
            .parse_validation()
            .ok()
            .map(|data| validation_overall_label(data.overall).to_string()),
        PayloadRole::Review => payload
            .parse_review()
            .ok()
            .map(|data| review_decision_label(data.decision).to_string()),
        PayloadRole::Closeout => payload.parse_closeout().ok().map(|data| data.final_status),
        PayloadRole::Source | PayloadRole::Plan | PayloadRole::Session => None,
    }
}

fn status_state_label(value: StateStatus) -> &'static str {
    match value {
        StateStatus::InProgress => "in-progress",
        StateStatus::Complete => "complete",
        StateStatus::Blocked => "blocked",
    }
}

fn validation_overall_label(value: ValidationOverall) -> &'static str {
    match value {
        ValidationOverall::Pass => "pass",
        ValidationOverall::Fail => "fail",
        ValidationOverall::Partial => "partial",
    }
}

fn review_decision_label(value: ReviewDecision) -> &'static str {
    match value {
        ReviewDecision::Approve => "approve",
        ReviewDecision::RequestChanges => "request-changes",
        ReviewDecision::CommentsOnly => "comments-only",
    }
}

pub fn evaluate_closeout_gate(audit: &RecordAudit, input: CloseoutGateInput) -> CloseoutGateResult {
    let mut checks = Vec::new();

    push_evidence_check(&mut checks, audit, "source", "source snapshot");
    push_evidence_check(&mut checks, audit, "plan", "plan snapshot");
    push_evidence_check(&mut checks, audit, "state", "execution state");

    if input.require_complete {
        let (status, detail) = match audit
            .evidence
            .get("state")
            .and_then(|hit| hit.status.as_deref())
        {
            Some(value) if value.eq_ignore_ascii_case("complete") => {
                ("pass", "complete".to_string())
            }
            Some(value) => ("fail", format!("latest state status is `{value}`")),
            None => ("fail", "missing execution state".to_string()),
        };
        checks.push(CloseoutCheck {
            check: "execution completion".to_string(),
            status: status.to_string(),
            detail,
        });
    }

    if input.require_session {
        push_evidence_check(&mut checks, audit, "session", "completed session");
    }
    if input.require_validation {
        push_evidence_check(&mut checks, audit, "validation", "validation evidence");
    }
    if input.require_review || input.profile == RecordProfile::Dispatch {
        push_evidence_check(&mut checks, audit, "review", "review evidence");
    }
    if input.require_closeout {
        push_evidence_check(&mut checks, audit, "closeout", "closeout comment");
    }

    let approval = input.approval.as_deref().unwrap_or("").trim();
    let (status, detail) = if approval.is_empty() {
        ("fail", "missing explicit approval".to_string())
    } else {
        ("pass", approval.to_string())
    };
    checks.push(CloseoutCheck {
        check: "close approval".to_string(),
        status: status.to_string(),
        detail,
    });

    if !input.linked_prs.is_empty() {
        let body_text = audit_text_for_pr_search(audit);
        let missing = input
            .linked_prs
            .iter()
            .filter(|pr| !body_text.contains(pr.trim()))
            .map(|pr| pr.trim().to_string())
            .collect::<Vec<_>>();
        let (status, detail) = if missing.is_empty() {
            (
                "pass",
                format!("linked PRs referenced: {}", input.linked_prs.join(", ")),
            )
        } else {
            (
                "fail",
                format!(
                    "linked PRs not found in lifecycle evidence: {}",
                    missing.join(", ")
                ),
            )
        };
        checks.push(CloseoutCheck {
            check: "linked PRs".to_string(),
            status: status.to_string(),
            detail,
        });
    }

    let ready = checks.iter().all(|check| check.status == "pass");
    CloseoutGateResult { ready, checks }
}

/// Render the canonical dashboard for an issue-backed plan record from
/// audit evidence alone — callers no longer need to pass every per-role
/// URL. Returns a `## Final Dashboard` when the latest state payload
/// reports `status=complete`, otherwise `## Current Dashboard`. Pending
/// roles render as `pending` so the dashboard remains idempotent across
/// repeated calls with the same evidence.
pub fn render_dashboard_from_audit(
    audit: &RecordAudit,
    title: Option<&str>,
    issue_url: Option<&str>,
) -> String {
    let state_evidence = audit.evidence.get("state");
    let state_data = state_evidence
        .and_then(|hit| hit.payload.as_ref())
        .and_then(|payload| payload.parse_state().ok());
    let is_complete = state_evidence
        .and_then(|hit| hit.status.as_deref())
        .map(|status| status.eq_ignore_ascii_case("complete"))
        .unwrap_or(false);

    let dashboard_title = if is_complete {
        "## Final Dashboard"
    } else {
        "## Current Dashboard"
    };

    let profile_str = state_evidence
        .map(|hit| hit.profile.as_str().to_string())
        .or_else(|| {
            audit
                .evidence
                .values()
                .next()
                .map(|hit| hit.profile.as_str().to_string())
        })
        .unwrap_or_else(|| "tracking".to_string());

    let status_value = state_evidence
        .and_then(|hit| hit.status.clone())
        .unwrap_or_else(|| "pending".to_string());

    let target_scope = state_data
        .as_ref()
        .and_then(|data| data.target_scope.clone())
        .unwrap_or_else(|| "pending".to_string());
    let current = state_data
        .as_ref()
        .and_then(|data| data.current.clone())
        .unwrap_or_else(|| "pending".to_string());
    let next_action = state_data
        .as_ref()
        .and_then(|data| data.next_action.clone())
        .unwrap_or_else(|| "pending".to_string());
    let validation_status = audit
        .evidence
        .get("validation")
        .and_then(|hit| hit.status.clone())
        .unwrap_or_else(|| "pending".to_string());
    let linked_prs = state_data
        .as_ref()
        .map(|data| {
            data.prs
                .iter()
                .map(|pr| pr.url.clone().unwrap_or_else(|| pr.pr_ref.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let blockers = state_data
        .as_ref()
        .map(|data| data.blockers.clone())
        .unwrap_or_default();
    let approval = audit
        .evidence
        .get("closeout")
        .and_then(|hit| hit.payload.as_ref())
        .and_then(|payload| payload.parse_closeout().ok())
        .and_then(|data| data.approval.comment_url)
        .unwrap_or_else(|| "pending".to_string());

    let mut out = Vec::new();
    out.push(dashboard_title.to_string());
    out.push(String::new());
    out.push("This issue is the durable tracking surface for an issue-backed plan execution. The full source, plan, and execution logs remain in".to_string());
    out.push("append-only issue comments.".to_string());
    out.push(String::new());
    out.push(format!("- Status: {status_value}"));
    out.push(format!("- Profile: {profile_str}"));
    out.push(format!("- Target scope: {target_scope}"));
    out.push(format!("- Current task: {current}"));
    out.push(format!("- Next action: {next_action}"));
    out.push(format!("- Validation: {validation_status}"));
    out.push(format!(
        "- Linked PRs: {}",
        non_empty_join(&linked_prs, "none yet")
    ));
    out.push(format!("- Blockers: {}", non_empty_join(&blockers, "none")));
    out.push(format!("- Review approval: {approval}"));
    out.push(String::new());
    out.push("## Durable Record".to_string());
    out.push(String::new());
    out.push(format!(
        "- Source snapshot: {}",
        dashboard_link(evidence_url(audit, "source").as_deref(), "source snapshot")
    ));
    out.push(format!(
        "- Plan snapshot: {}",
        dashboard_link(evidence_url(audit, "plan").as_deref(), "plan snapshot")
    ));
    out.push(format!(
        "- Execution state: {}",
        dashboard_link(evidence_url(audit, "state").as_deref(), "execution state")
    ));
    out.push(format!(
        "- Latest session: {}",
        dashboard_link(
            evidence_url(audit, "session").as_deref(),
            "Execution Session"
        )
    ));
    out.push(format!(
        "- Latest validation: {}",
        dashboard_link(
            evidence_url(audit, "validation").as_deref(),
            "Validation Evidence"
        )
    ));
    let is_dispatch_profile = profile_str == "dispatch";
    if is_dispatch_profile || audit.evidence.contains_key("review") {
        out.push(format!(
            "- Latest review: {}",
            dashboard_link(evidence_url(audit, "review").as_deref(), "Review Evidence")
        ));
    }
    out.push(format!(
        "- Closeout comment: {}",
        dashboard_link(evidence_url(audit, "closeout").as_deref(), "closeout")
    ));
    out.push(String::new());
    out.push("## Guardrails".to_string());
    out.push(String::new());
    out.push("- The issue body is a mutable dashboard only.".to_string());
    out.push("- Append-only issue comments are the durable source of truth.".to_string());
    out.push(
        "- `plan-tooling` owns plan parsing, validation, batching, and PR split modeling only."
            .to_string(),
    );
    out.push("- Provider create, comment, edit, and close operations remain owned by `forge-cli` or provider atoms.".to_string());

    if title.is_some() || issue_url.is_some() {
        out.push(String::new());
        out.push("## Original Tracker".to_string());
        out.push(String::new());
        if let Some(title) = title.map(str::trim).filter(|value| !value.is_empty()) {
            out.push(format!("- Title: {title}"));
        }
        if let Some(url) = issue_url.map(str::trim).filter(|value| !value.is_empty()) {
            out.push(format!("- Issue: {url}"));
        }
    }

    finalize_markdown(out)
}

fn evidence_url(audit: &RecordAudit, role: &str) -> Option<String> {
    audit
        .evidence
        .get(role)
        .and_then(|hit| hit.url.clone())
        .filter(|value| !value.trim().is_empty())
}

pub fn render_closeout_checks(checks: &[CloseoutCheck]) -> String {
    let mut out = Vec::new();
    out.push("| Check | Status | Detail |".to_string());
    out.push("| --- | --- | --- |".to_string());
    for check in checks {
        out.push(format!(
            "| {} | {} | {} |",
            table_cell(&check.check),
            table_cell(&check.status),
            table_cell(&check.detail)
        ));
    }
    finalize_markdown(out)
}

fn non_empty_join(values: &[String], fallback: &str) -> String {
    let joined = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        fallback.to_string()
    } else {
        joined
    }
}

fn dashboard_link(url: Option<&str>, label: &str) -> String {
    match url.map(str::trim).filter(|value| !value.is_empty()) {
        Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
            format!("[{label}]({url})")
        }
        Some(value) => value.to_string(),
        None => "pending".to_string(),
    }
}

fn default_heading(profile: RecordProfile, kind: LifecycleCommentKind) -> &'static str {
    match kind {
        LifecycleCommentKind::Source => "Source Snapshot",
        LifecycleCommentKind::Plan => "Plan Snapshot",
        LifecycleCommentKind::State => "Execution State",
        LifecycleCommentKind::Session => "Execution Session",
        LifecycleCommentKind::Validation => "Validation Evidence",
        LifecycleCommentKind::Review => "Review Evidence",
        LifecycleCommentKind::Closeout => match profile {
            RecordProfile::Tracking => "Tracking Issue Closeout",
            RecordProfile::Dispatch => "Dispatch Issue Closeout",
        },
    }
}

fn default_details_summary(kind: LifecycleCommentKind) -> &'static str {
    match kind {
        LifecycleCommentKind::Source => "Source snapshot",
        LifecycleCommentKind::Plan => "Plan snapshot",
        _ => "Details",
    }
}

/// Render the canonical v2 marker for a lifecycle comment kind.
fn marker_for(profile: RecordProfile, kind: LifecycleCommentKind) -> String {
    format!(
        "<!-- plan-issue-record:v2 role={} profile={} -->",
        kind.as_str(),
        profile.as_str()
    )
}

fn parse_comments_json(raw: &str) -> Result<Vec<CommentJson>, String> {
    let value = serde_json::from_str::<Value>(raw)
        .map_err(|err| format!("failed to parse comments JSON: {err}"))?;
    let comments_value = match value {
        Value::Object(mut object) => object
            .remove("comments")
            .ok_or_else(|| "comments JSON object is missing `comments`".to_string())?,
        Value::Array(items) => Value::Array(items),
        _ => {
            return Err(
                "comments JSON must be an array or an object with a `comments` array".to_string(),
            );
        }
    };
    let Value::Array(items) = comments_value else {
        return Err("`comments` must be an array".to_string());
    };

    Ok(items
        .into_iter()
        .filter_map(|item| {
            let Value::Object(mut object) = item else {
                return None;
            };
            Some(CommentJson {
                body: string_field(&mut object, "body"),
                url: string_field(&mut object, "url"),
                html_url: string_field(&mut object, "html_url"),
                created_at: string_field(&mut object, "created_at")
                    .or_else(|| string_field(&mut object, "createdAt")),
            })
        })
        .collect())
}

fn string_field(object: &mut serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .remove(key)
        .and_then(|value| value.as_str().map(ToString::to_string))
}

fn first_comment_marker(body: &str) -> Option<&str> {
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("<!--") && trimmed.ends_with("-->") {
            return Some(trimmed);
        }
        return None;
    }
    None
}

/// Outcome of marker parsing on a comment's first non-empty line.
#[derive(Debug, Clone)]
enum MarkerParse {
    /// Canonical v2 marker.
    V2 {
        role: PayloadRole,
        profile: PayloadProfile,
    },
    /// Pre-v2 marker family the v3 lifecycle no longer recognizes as
    /// current lifecycle evidence (but tracks for reporting).
    Unsupported { prefix: String },
}

fn parse_marker_line(marker: &str) -> Option<MarkerParse> {
    let inner = marker.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let attrs = parse_attrs(inner);

    if let Some(rest) = inner.strip_prefix("plan-issue-record:v2") {
        let _ = rest;
        let role = attrs.get("role").and_then(|value| parse_role(value))?;
        let profile = attrs
            .get("profile")
            .and_then(|value| parse_profile(value))
            .unwrap_or(PayloadProfile::Tracking);
        return Some(MarkerParse::V2 { role, profile });
    }

    // Known pre-v2 marker families. They are no longer recognized as
    // current lifecycle evidence but are reported by audit so callers
    // can identify v1-marker comments that need migration to v2.
    for prefix in [
        "issue-backed-plan:",
        "plan-tracking-issue:",
        "execute-from-tracking-issue:",
        "execute-plan-tracking-issue:",
        "tracking-issue-closeout:",
        "plan-tracking-issue-closeout:",
        "deliver-dispatch-plan:",
        "dispatch-plan:",
    ] {
        if inner.starts_with(prefix) {
            return Some(MarkerParse::Unsupported {
                prefix: prefix.trim_end_matches(':').to_string(),
            });
        }
    }

    None
}

fn parse_role(value: &str) -> Option<PayloadRole> {
    Some(match value {
        "source" => PayloadRole::Source,
        "plan" => PayloadRole::Plan,
        "state" => PayloadRole::State,
        "session" => PayloadRole::Session,
        "validation" => PayloadRole::Validation,
        "review" => PayloadRole::Review,
        "closeout" => PayloadRole::Closeout,
        _ => return None,
    })
}

fn parse_profile(value: &str) -> Option<PayloadProfile> {
    Some(match value {
        "tracking" => PayloadProfile::Tracking,
        "dispatch" => PayloadProfile::Dispatch,
        _ => return None,
    })
}

fn parse_attrs(marker: &str) -> BTreeMap<String, String> {
    let mut attrs = BTreeMap::new();
    for token in marker.split_whitespace().skip(1) {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        attrs.insert(
            key.trim().to_string(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    attrs
}

fn inspect_body_sections(body: &str) -> BodySections {
    BodySections {
        current_dashboard: body.contains("## Current Dashboard"),
        final_dashboard: body.contains("## Final Dashboard"),
        durable_record: body.contains("## Durable Record"),
        closeout_checks: body.contains("## Closeout Checks"),
        task_decomposition: body.contains("## Task Decomposition"),
    }
}

fn push_evidence_check(
    checks: &mut Vec<CloseoutCheck>,
    audit: &RecordAudit,
    role: &str,
    label: &str,
) {
    let (status, detail) = match audit.evidence.get(role) {
        Some(hit) => (
            "pass",
            hit.url
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("present")
                .to_string(),
        ),
        None => ("fail", format!("missing {label}")),
    };
    checks.push(CloseoutCheck {
        check: label.to_string(),
        status: status.to_string(),
        detail,
    });
}

fn audit_text_for_pr_search(audit: &RecordAudit) -> String {
    let mut values = BTreeSet::new();
    if !audit.evidence_text.trim().is_empty() {
        values.insert(audit.evidence_text.clone());
    }
    for hit in audit.evidence.values() {
        if let Some(url) = hit.url.as_deref() {
            values.insert(url.to_string());
        }
    }
    values.into_iter().collect::<Vec<_>>().join("\n")
}

fn table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', "<br>")
}

fn finalize_markdown(lines: Vec<String>) -> String {
    let mut rendered = lines.join("\n");
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered
}

// -----------------------------------------------------------------------------
// Structured lifecycle payload (issue-backed plan record contract v2)
//
// Each lifecycle comment carries one fenced JSON block whose info-string is the
// literal token PAYLOAD_FENCE_INFO. Audit, dashboard repair, and closeout gate
// evaluation consume the structured payload exclusively. Visible Markdown
// around the fence is human commentary only.
// -----------------------------------------------------------------------------

/// On-wire schema identity for v2 lifecycle payloads.
pub const PAYLOAD_SCHEMA_V2: &str = "plan-issue-record.payload.v2";

/// Fenced-code-block info-string used to mark a lifecycle payload fence.
pub const PAYLOAD_FENCE_INFO: &str = "plan-issue-record-payload";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadRole {
    Source,
    Plan,
    State,
    Session,
    Validation,
    Review,
    Closeout,
}

impl PayloadRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Plan => "plan",
            Self::State => "state",
            Self::Session => "session",
            Self::Validation => "validation",
            Self::Review => "review",
            Self::Closeout => "closeout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadProfile {
    Tracking,
    Dispatch,
}

impl PayloadProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tracking => "tracking",
            Self::Dispatch => "dispatch",
        }
    }
}

impl From<RecordProfile> for PayloadProfile {
    fn from(value: RecordProfile) -> Self {
        match value {
            RecordProfile::Tracking => PayloadProfile::Tracking,
            RecordProfile::Dispatch => PayloadProfile::Dispatch,
        }
    }
}

/// Envelope for every lifecycle comment payload.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct RecordPayload {
    pub schema: String,
    pub role: PayloadRole,
    pub profile: PayloadProfile,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SnapshotData {
    pub path: String,
    pub commit: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateStatus {
    InProgress,
    Complete,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskRowStatus {
    Pending,
    InProgress,
    Done,
    Deferred,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct TaskRowPayload {
    pub id: String,
    pub status: TaskRowStatus,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrLifecycleStatus {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct PrRefPayload {
    #[serde(rename = "ref")]
    pub pr_ref: String,
    #[serde(default)]
    pub url: Option<String>,
    pub status: PrLifecycleStatus,
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct StateData {
    pub status: Option<StateStatus>,
    #[serde(default)]
    pub target_scope: Option<String>,
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub next_action: Option<String>,
    #[serde(default)]
    pub tasks: Vec<TaskRowPayload>,
    #[serde(default)]
    pub prs: Vec<PrRefPayload>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub links: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SessionData {
    pub summary: String,
    #[serde(default)]
    pub highlights: Vec<String>,
    #[serde(default)]
    pub links: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationOverall {
    Pass,
    Fail,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationCommandStatus {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ValidationCommand {
    pub command: String,
    pub status: ValidationCommandStatus,
    #[serde(default)]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ValidationWaiver {
    pub command: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ValidationData {
    pub overall: ValidationOverall,
    #[serde(default)]
    pub commands: Vec<ValidationCommand>,
    #[serde(default)]
    pub waivers: Vec<ValidationWaiver>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDecision {
    Approve,
    RequestChanges,
    CommentsOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Blocker,
    Major,
    Minor,
    Nit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FindingDisposition {
    Fixed,
    Residual,
    FollowUp,
    Deferred,
    NoAction,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: FindingSeverity,
    pub disposition: FindingDisposition,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ReviewData {
    pub decision: ReviewDecision,
    #[serde(default)]
    pub lenses: Vec<String>,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub outcome_comment_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ApprovalEvidence {
    #[serde(default)]
    pub comment_url: Option<String>,
    #[serde(default)]
    pub approver: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Fail,
    None,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct LinkedPrEvidence {
    #[serde(rename = "ref")]
    pub pr_ref: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub merge_sha: Option<String>,
    pub checks: CheckStatus,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CloseoutData {
    pub final_status: String,
    pub approval: ApprovalEvidence,
    #[serde(default)]
    pub linked_prs: Vec<LinkedPrEvidence>,
    #[serde(default)]
    pub final_validation_url: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadErrorKind {
    NoFence,
    MultipleFences,
    SchemaMismatch,
    InvalidJson,
}

#[derive(Debug, Clone)]
pub struct PayloadError {
    pub kind: PayloadErrorKind,
    pub message: String,
}

impl PayloadError {
    fn new(kind: PayloadErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PayloadError {}

/// Extract the structured lifecycle payload fenced inside `comment_body`.
///
/// - Returns `Ok(payload)` on a single well-formed `plan-issue-record-payload`
///   fence whose envelope `schema` matches [`PAYLOAD_SCHEMA_V2`].
/// - Returns `Err(NoFence)` when the comment does not contain the fence.
/// - Returns `Err(MultipleFences)` when multiple payload fences are present;
///   each comment carries at most one payload.
/// - Returns `Err(SchemaMismatch)` when the fence parses but its `schema`
///   does not match the v2 wire identity.
/// - Returns `Err(InvalidJson)` when the fence body is not valid JSON or
///   does not deserialize into the envelope.
pub fn extract_payload(comment_body: &str) -> Result<RecordPayload, PayloadError> {
    let fences = collect_payload_fences(comment_body);
    if fences.is_empty() {
        return Err(PayloadError::new(
            PayloadErrorKind::NoFence,
            "no plan-issue-record-payload fence in comment body",
        ));
    }
    if fences.len() > 1 {
        return Err(PayloadError::new(
            PayloadErrorKind::MultipleFences,
            "multiple plan-issue-record-payload fences in comment body",
        ));
    }

    let raw = &fences[0];
    let payload: RecordPayload = serde_json::from_str(raw)
        .map_err(|err| PayloadError::new(PayloadErrorKind::InvalidJson, err.to_string()))?;
    if payload.schema != PAYLOAD_SCHEMA_V2 {
        return Err(PayloadError::new(
            PayloadErrorKind::SchemaMismatch,
            format!(
                "expected schema `{PAYLOAD_SCHEMA_V2}`, got `{}`",
                payload.schema
            ),
        ));
    }
    Ok(payload)
}

fn collect_payload_fences(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(buf) = current.as_mut() {
            if trimmed.starts_with("```") {
                let mut block = String::new();
                for chunk in buf.iter() {
                    if !block.is_empty() {
                        block.push('\n');
                    }
                    block.push_str(chunk);
                }
                out.push(block);
                current = None;
            } else {
                buf.push(line.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("```")
            && rest.trim() == PAYLOAD_FENCE_INFO
        {
            current = Some(Vec::new());
        }
    }
    out
}

impl RecordPayload {
    pub fn parse_state(&self) -> Result<StateData, PayloadError> {
        self.decode_data(PayloadRole::State)
    }

    pub fn parse_session(&self) -> Result<SessionData, PayloadError> {
        self.decode_data(PayloadRole::Session)
    }

    pub fn parse_validation(&self) -> Result<ValidationData, PayloadError> {
        self.decode_data(PayloadRole::Validation)
    }

    pub fn parse_review(&self) -> Result<ReviewData, PayloadError> {
        self.decode_data(PayloadRole::Review)
    }

    pub fn parse_closeout(&self) -> Result<CloseoutData, PayloadError> {
        self.decode_data(PayloadRole::Closeout)
    }

    pub fn parse_snapshot(&self) -> Result<SnapshotData, PayloadError> {
        if !matches!(self.role, PayloadRole::Source | PayloadRole::Plan) {
            return Err(PayloadError::new(
                PayloadErrorKind::SchemaMismatch,
                format!(
                    "expected source or plan payload, got `{}`",
                    self.role.as_str()
                ),
            ));
        }
        serde_json::from_value::<SnapshotData>(self.data.clone())
            .map_err(|err| PayloadError::new(PayloadErrorKind::InvalidJson, err.to_string()))
    }

    fn decode_data<T: serde::de::DeserializeOwned>(
        &self,
        expected: PayloadRole,
    ) -> Result<T, PayloadError> {
        if self.role != expected {
            return Err(PayloadError::new(
                PayloadErrorKind::SchemaMismatch,
                format!(
                    "expected role `{}`, got `{}`",
                    expected.as_str(),
                    self.role.as_str()
                ),
            ));
        }
        serde_json::from_value::<T>(self.data.clone())
            .map_err(|err| PayloadError::new(PayloadErrorKind::InvalidJson, err.to_string()))
    }
}
