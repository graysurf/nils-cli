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
    /// Parsed structured payload. `None` when the comment lacks a hidden
    /// payload carrier or older `plan-issue-record-payload` fence; audit
    /// still records the marker for visibility but downstream gates treat
    /// missing payloads as unparseable evidence.
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
// Each lifecycle comment carries one hidden payload carrier. Audit, dashboard
// repair, and closeout gate evaluation consume the structured payload
// exclusively. Visible Markdown around the carrier is human commentary only.
// The older PAYLOAD_FENCE_INFO fenced block remains accepted for existing
// records created before the hidden carrier renderer.
// -----------------------------------------------------------------------------

/// On-wire schema identity for v2 lifecycle payloads.
pub const PAYLOAD_SCHEMA_V2: &str = "plan-issue-record.payload.v2";

/// Older fenced-code-block info-string used to mark a lifecycle payload.
pub const PAYLOAD_FENCE_INFO: &str = "plan-issue-record-payload";
const PAYLOAD_COMMENT_PREFIX: &str = "plan-issue-record-payload:hex:";

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

/// Extract the structured lifecycle payload carried inside `comment_body`.
///
/// - Returns `Ok(payload)` on a single well-formed hidden carrier or older
///   `plan-issue-record-payload` fence whose envelope `schema` matches
///   [`PAYLOAD_SCHEMA_V2`].
/// - Returns `Err(NoFence)` when the comment does not contain either payload
///   carrier.
/// - Returns `Err(MultipleFences)` when multiple payload carriers are present;
///   each comment carries at most one payload.
/// - Returns `Err(SchemaMismatch)` when the payload parses but its `schema`
///   does not match the v2 wire identity.
/// - Returns `Err(InvalidJson)` when the payload body is not valid JSON or
///   does not deserialize into the envelope.
pub fn extract_payload(comment_body: &str) -> Result<RecordPayload, PayloadError> {
    let carriers = collect_payload_comment_carriers(comment_body)?;
    let fences = collect_payload_fences(comment_body);
    let payload_count = carriers.len() + fences.len();
    if payload_count == 0 {
        return Err(PayloadError::new(
            PayloadErrorKind::NoFence,
            "no plan-issue-record-payload carrier or fence in comment body",
        ));
    }
    if payload_count > 1 {
        return Err(PayloadError::new(
            PayloadErrorKind::MultipleFences,
            "multiple plan-issue-record-payload carriers or fences in comment body",
        ));
    }

    let raw = carriers.first().or_else(|| fences.first()).ok_or_else(|| {
        PayloadError::new(
            PayloadErrorKind::NoFence,
            "no plan-issue-record-payload carrier or fence in comment body",
        )
    })?;
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

fn collect_payload_comment_carriers(body: &str) -> Result<Vec<String>, PayloadError> {
    let mut out = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        let Some(inner) = trimmed
            .strip_prefix("<!--")
            .and_then(|value| value.strip_suffix("-->"))
        else {
            continue;
        };
        let inner = inner.trim();
        let Some(encoded) = inner.strip_prefix(PAYLOAD_COMMENT_PREFIX) else {
            continue;
        };
        let payload = decode_hex(encoded.trim()).map_err(|err| {
            PayloadError::new(
                PayloadErrorKind::InvalidJson,
                format!("invalid hidden payload carrier: {err}"),
            )
        })?;
        let payload = String::from_utf8(payload).map_err(|err| {
            PayloadError::new(
                PayloadErrorKind::InvalidJson,
                format!("hidden payload carrier is not UTF-8: {err}"),
            )
        })?;
        out.push(payload);
    }
    Ok(out)
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

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    if !input.len().is_multiple_of(2) {
        return Err("hex payload has odd length".to_string());
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let bytes = input.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = hex_value(pair[0])
            .ok_or_else(|| format!("invalid hex digit `{}`", char::from(pair[0])))?;
        let lo = hex_value(pair[1])
            .ok_or_else(|| format!("invalid hex digit `{}`", char::from(pair[1])))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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

// -----------------------------------------------------------------------------
// v2 provider-backed renderers (Sprint 3)
//
// `render_record_snapshot_comment` and `render_record_post_comment` produce the
// canonical Markdown body for `record open` and `record post`: every comment
// carries the v2 marker on its first line plus a hidden payload carrier as the
// structured source of truth. Audit still accepts the older visible payload
// fence for records created before this renderer was fixed.
// -----------------------------------------------------------------------------

fn payload_role_for_kind(kind: LifecycleCommentKind) -> PayloadRole {
    match kind {
        LifecycleCommentKind::Source => PayloadRole::Source,
        LifecycleCommentKind::Plan => PayloadRole::Plan,
        LifecycleCommentKind::State => PayloadRole::State,
        LifecycleCommentKind::Session => PayloadRole::Session,
        LifecycleCommentKind::Validation => PayloadRole::Validation,
        LifecycleCommentKind::Review => PayloadRole::Review,
        LifecycleCommentKind::Closeout => PayloadRole::Closeout,
    }
}

fn render_payload_carrier(envelope: &RecordPayload) -> Result<String, String> {
    let envelope_json = serde_json::to_string(envelope).map_err(|err| err.to_string())?;
    Ok(format!(
        "<!-- {PAYLOAD_COMMENT_PREFIX}{} -->",
        encode_hex(envelope_json.as_bytes())
    ))
}

/// Render the canonical v2 source/plan snapshot comment used by
/// `record open`. The body carries the v2 marker, visible details, and a
/// hidden structured payload carrying [`SnapshotData`].
pub fn render_record_snapshot_comment(
    profile: RecordProfile,
    kind: LifecycleCommentKind,
    snapshot: &SnapshotData,
    content: &str,
    updated_at: Option<&str>,
) -> Result<String, String> {
    if !matches!(
        kind,
        LifecycleCommentKind::Source | LifecycleCommentKind::Plan
    ) {
        return Err(format!(
            "render_record_snapshot_comment: expected source or plan kind, got `{}`",
            kind.as_str()
        ));
    }

    let envelope = RecordPayload {
        schema: PAYLOAD_SCHEMA_V2.to_string(),
        role: payload_role_for_kind(kind),
        profile: PayloadProfile::from(profile),
        updated_at: updated_at.map(str::to_string),
        data: serde_json::to_value(snapshot).map_err(|err| err.to_string())?,
    };
    let envelope_carrier = render_payload_carrier(&envelope)?;

    let marker = marker_for(profile, kind);
    let heading = default_heading(profile, kind);
    let details_summary = default_details_summary(kind);

    let mut out = Vec::new();
    out.push(marker);
    out.push(String::new());
    out.push(format!("## {heading}"));
    out.push(String::new());
    out.push(format!("- Profile: {}", profile.as_str()));
    if !snapshot.path.trim().is_empty() {
        out.push(format!("- Path: `{}`", snapshot.path.trim()));
    }
    if !snapshot.commit.trim().is_empty() {
        out.push(format!("- Commit: `{}`", snapshot.commit.trim()));
    }
    if let Some(summary) = snapshot
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        out.push(format!("- Summary: {summary}"));
    }
    out.push("- Snapshot mode: local committed Markdown".to_string());
    out.push(String::new());
    out.push("<details>".to_string());
    out.push(format!("<summary>{details_summary}</summary>"));
    out.push(String::new());
    out.push(content.to_string());
    out.push(String::new());
    out.push("</details>".to_string());
    out.push(String::new());
    out.push(envelope_carrier);
    Ok(finalize_markdown(out))
}

/// Render the canonical v2 lifecycle comment used by `record post` for
/// state, session, validation, review, and closeout kinds. Source/plan
/// kinds are rejected because `record open` owns them.
pub fn render_record_post_comment(
    profile: RecordProfile,
    kind: LifecycleCommentKind,
    payload_data: Value,
    summary: Option<&str>,
    updated_at: Option<&str>,
) -> Result<String, String> {
    if matches!(
        kind,
        LifecycleCommentKind::Source | LifecycleCommentKind::Plan
    ) {
        return Err(format!(
            "render_record_post_comment: source/plan kinds are owned by `record open`, got `{}`",
            kind.as_str()
        ));
    }

    let envelope = RecordPayload {
        schema: PAYLOAD_SCHEMA_V2.to_string(),
        role: payload_role_for_kind(kind),
        profile: PayloadProfile::from(profile),
        updated_at: updated_at.map(str::to_string),
        data: payload_data,
    };
    let envelope_carrier = render_payload_carrier(&envelope)?;

    let marker = marker_for(profile, kind);
    let heading = default_heading(profile, kind);

    let mut out = Vec::new();
    out.push(marker);
    out.push(String::new());
    out.push(format!("## {heading}"));
    out.push(String::new());
    out.push(format!("- Profile: {}", profile.as_str()));
    if let Some(text) = summary.map(str::trim).filter(|value| !value.is_empty()) {
        out.push(String::new());
        out.push(text.to_string());
    }
    out.push(String::new());
    out.push(envelope_carrier);
    Ok(finalize_markdown(out))
}

// -----------------------------------------------------------------------------
// Strict closeout gate (Sprint 3)
//
// Sprint 3 introduces `evaluate_strict_closeout_gate` for `record close`. It
// supersedes the v1 `evaluate_closeout_gate`, which is retained as a
// transitional helper for the `record closeout-gate` subcommand until
// Sprint 4 retires that surface.
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct StrictCloseoutGateResult {
    pub ready: bool,
    pub checks: Vec<CloseoutCheck>,
    /// Stable machine-readable codes for blocked items, one per failure.
    pub blocked_codes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StrictCloseoutGateInput<'a> {
    pub profile: RecordProfile,
    pub approval: Option<&'a str>,
    /// Provider-verified linked PR evidence. Each entry must carry a
    /// `merge_sha`; missing merge_sha is treated as `linked-pr-not-merged`.
    pub linked_prs: &'a [LinkedPrEvidence],
    /// Current issue body. When paired with `expected_dashboard`, the gate
    /// fails with `dashboard-out-of-date` if the recomputed dashboard does
    /// not appear in the body.
    pub current_body: Option<&'a str>,
    pub expected_dashboard: Option<&'a str>,
}

pub fn evaluate_strict_closeout_gate(
    audit: &RecordAudit,
    input: StrictCloseoutGateInput<'_>,
) -> StrictCloseoutGateResult {
    let mut checks = Vec::new();
    let mut blocked_codes: Vec<String> = Vec::new();

    let push_pass = |checks: &mut Vec<CloseoutCheck>, check: &str, detail: String| {
        checks.push(CloseoutCheck {
            check: check.to_string(),
            status: "pass".to_string(),
            detail,
        });
    };
    let push_fail = |checks: &mut Vec<CloseoutCheck>,
                     blocked: &mut Vec<String>,
                     check: &str,
                     detail: String,
                     code: &str| {
        checks.push(CloseoutCheck {
            check: check.to_string(),
            status: "fail".to_string(),
            detail,
        });
        blocked.push(code.to_string());
    };

    for (role, label, code) in [
        ("source", "source snapshot", "source-missing"),
        ("plan", "plan snapshot", "plan-missing"),
    ] {
        if audit.evidence.contains_key(role) {
            push_pass(&mut checks, label, "present".to_string());
        } else {
            push_fail(
                &mut checks,
                &mut blocked_codes,
                label,
                "missing".to_string(),
                code,
            );
        }
    }

    match audit.evidence.get("state") {
        Some(hit) => {
            let status = hit.status.as_deref();
            let parsed = hit
                .payload
                .as_ref()
                .and_then(|payload| payload.parse_state().ok());
            match status {
                Some(value) if value.eq_ignore_ascii_case("complete") => {
                    let tasks_incomplete = parsed
                        .as_ref()
                        .map(|data| {
                            data.tasks.iter().any(|task| {
                                !matches!(
                                    task.status,
                                    TaskRowStatus::Done | TaskRowStatus::Deferred
                                )
                            })
                        })
                        .unwrap_or(false);
                    if tasks_incomplete {
                        push_fail(
                            &mut checks,
                            &mut blocked_codes,
                            "execution state",
                            "complete but tasks are not all done/deferred".to_string(),
                            "state-tasks-incomplete",
                        );
                    } else {
                        push_pass(&mut checks, "execution state", "complete".to_string());
                    }
                }
                Some(value) => push_fail(
                    &mut checks,
                    &mut blocked_codes,
                    "execution state",
                    format!("latest state status is `{value}`"),
                    "state-not-complete",
                ),
                None => push_fail(
                    &mut checks,
                    &mut blocked_codes,
                    "execution state",
                    "missing payload status".to_string(),
                    "state-not-complete",
                ),
            }
        }
        None => push_fail(
            &mut checks,
            &mut blocked_codes,
            "execution state",
            "missing".to_string(),
            "state-missing",
        ),
    }

    match audit.evidence.get("validation") {
        Some(hit) => match hit.status.as_deref() {
            Some("pass") => push_pass(&mut checks, "validation", "pass".to_string()),
            Some(value) => push_fail(
                &mut checks,
                &mut blocked_codes,
                "validation",
                format!("latest validation overall = `{value}`"),
                "validation-failed",
            ),
            None => push_fail(
                &mut checks,
                &mut blocked_codes,
                "validation",
                "missing payload status".to_string(),
                "validation-failed",
            ),
        },
        None => push_fail(
            &mut checks,
            &mut blocked_codes,
            "validation",
            "missing".to_string(),
            "validation-missing",
        ),
    }

    match audit.evidence.get("review") {
        Some(hit) => {
            let parsed = hit.payload.as_ref().map(|payload| payload.parse_review());
            match parsed {
                Some(Ok(data)) => match data.decision {
                    ReviewDecision::RequestChanges => push_fail(
                        &mut checks,
                        &mut blocked_codes,
                        "review",
                        "decision = request-changes".to_string(),
                        "review-rejected",
                    ),
                    decision => {
                        let unresolved = data.findings.iter().any(|finding| {
                            matches!(finding.disposition, FindingDisposition::Residual)
                                && matches!(
                                    finding.severity,
                                    FindingSeverity::Blocker | FindingSeverity::Major
                                )
                        });
                        if unresolved {
                            push_fail(
                                &mut checks,
                                &mut blocked_codes,
                                "review",
                                "unresolved blocker/major findings".to_string(),
                                "review-unresolved-findings",
                            );
                        } else {
                            let label = match decision {
                                ReviewDecision::Approve => "approve",
                                ReviewDecision::CommentsOnly => "comments-only",
                                ReviewDecision::RequestChanges => unreachable!(),
                            };
                            push_pass(&mut checks, "review", format!("decision = {label}"));
                        }
                    }
                },
                Some(Err(err)) => push_fail(
                    &mut checks,
                    &mut blocked_codes,
                    "review",
                    format!("malformed review payload: {}", err.message),
                    "review-rejected",
                ),
                None => push_fail(
                    &mut checks,
                    &mut blocked_codes,
                    "review",
                    "missing payload".to_string(),
                    "review-missing",
                ),
            }
        }
        None => push_fail(
            &mut checks,
            &mut blocked_codes,
            "review",
            "missing".to_string(),
            "review-missing",
        ),
    }

    let approval_text = input.approval.unwrap_or("").trim();
    if approval_text.is_empty() {
        push_fail(
            &mut checks,
            &mut blocked_codes,
            "close approval",
            "missing explicit approval".to_string(),
            "approval-missing",
        );
    } else {
        push_pass(&mut checks, "close approval", approval_text.to_string());
    }

    if input.linked_prs.is_empty() {
        push_pass(&mut checks, "linked PRs", "none provided".to_string());
    } else {
        let mut failing = Vec::new();
        for pr in input.linked_prs {
            let sha = pr.merge_sha.as_deref().map(str::trim).unwrap_or("");
            if sha.is_empty() {
                failing.push(format!("{} (no merge_sha)", pr.pr_ref));
            } else if !matches!(pr.checks, CheckStatus::Pass | CheckStatus::None) {
                failing.push(format!("{} (checks={:?})", pr.pr_ref, pr.checks));
            }
        }
        if failing.is_empty() {
            push_pass(
                &mut checks,
                "linked PRs",
                format!("{} merged", input.linked_prs.len()),
            );
        } else {
            push_fail(
                &mut checks,
                &mut blocked_codes,
                "linked PRs",
                failing.join(", "),
                "linked-pr-not-merged",
            );
        }
    }

    if let (Some(current), Some(expected)) = (input.current_body, input.expected_dashboard) {
        let current_norm = normalize_for_dashboard_compare(current);
        let expected_norm = normalize_for_dashboard_compare(expected);
        if current_norm.contains(&expected_norm) {
            push_pass(&mut checks, "dashboard", "matches canonical".to_string());
        } else {
            push_fail(
                &mut checks,
                &mut blocked_codes,
                "dashboard",
                "dashboard differs from recomputed canonical".to_string(),
                "dashboard-out-of-date",
            );
        }
    }

    let ready = checks.iter().all(|check| check.status == "pass");
    StrictCloseoutGateResult {
        ready,
        checks,
        blocked_codes,
    }
}

fn normalize_for_dashboard_compare(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod sprint3_tests {
    use super::*;
    use serde_json::json;

    fn build_audit_with_evidence(comments: Vec<(serde_json::Value, &str)>) -> RecordAudit {
        let payload = json!({
            "comments": comments
                .into_iter()
                .map(|(body, url)| json!({"body": body, "url": url, "created_at": "2026-05-23T08:00:00Z"}))
                .collect::<Vec<_>>()
        });
        audit_record(None, &payload.to_string(), None).expect("audit ok")
    }

    fn v2_body(role: &str, data: Value) -> Value {
        let envelope = json!({
            "schema": PAYLOAD_SCHEMA_V2,
            "role": role,
            "profile": "tracking",
            "data": data,
        });
        let payload_json = serde_json::to_string(&envelope).expect("serialize");
        json!(format!(
            "<!-- plan-issue-record:v2 role={role} profile=tracking -->\n\n```{PAYLOAD_FENCE_INFO}\n{payload_json}\n```\n",
        ))
    }

    #[test]
    fn audit_treats_v2_marker_without_payload_fence_as_payload_none() {
        // Reproduces [F11] deferred follow-up: v2 marker with no payload
        // fence should leave evidence.payload = None instead of erroring.
        let body_only_marker = json!(
            "<!-- plan-issue-record:v2 role=session profile=tracking -->\n\n## Execution Session\n\nfreeform notes, no payload\n"
        );
        let audit = build_audit_with_evidence(vec![(
            body_only_marker,
            "https://github.com/owner/repo/issues/1#issuecomment-session",
        )]);
        let session = audit
            .evidence
            .get("session")
            .expect("session evidence registered");
        assert!(session.payload.is_none(), "payload should be None");
        assert_eq!(audit.recognized_count, 1);
    }

    #[test]
    fn audit_strict_fails_on_malformed_payload() {
        // [F11] deferred: malformed payload fence must error rather than
        // silently degrade to payload=None.
        let body = json!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n```plan-issue-record-payload\n{not valid json\n```\n"
        );
        let payload = json!({
            "comments": [{
                "body": body,
                "url": "https://github.com/owner/repo/issues/1#issuecomment-bad",
                "created_at": "2026-05-23T08:00:00Z",
            }]
        });
        let err = audit_record(None, &payload.to_string(), None)
            .expect_err("malformed payload should fail audit");
        assert!(
            err.contains("malformed payload"),
            "error should mention malformed payload: {err}"
        );
    }

    #[test]
    fn strict_gate_passes_when_all_v2_evidence_complete_and_merged() {
        let state = v2_body(
            "state",
            json!({
                "status": "complete",
                "target_scope": "scope",
                "tasks": [
                    {"id": "1.1", "status": "done", "title": "x"},
                    {"id": "1.2", "status": "deferred", "title": "y"},
                ],
                "prs": [{"ref": "owner/repo#1", "url": "u", "status": "merged"}],
                "blockers": [],
                "links": {},
            }),
        );
        let validation = v2_body(
            "validation",
            json!({"overall": "pass", "commands": [], "waivers": []}),
        );
        let review = v2_body(
            "review",
            json!({
                "decision": "approve",
                "lenses": ["testing"],
                "findings": [],
            }),
        );
        let source = v2_body("source", json!({"path": "p", "commit": "c"}));
        let plan = v2_body("plan", json!({"path": "p", "commit": "c"}));
        let audit = build_audit_with_evidence(vec![
            (source, "u-src"),
            (plan, "u-plan"),
            (state, "u-state"),
            (validation, "u-val"),
            (review, "u-rev"),
        ]);

        let linked_prs = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: Some("https://github.com/owner/repo/pull/1".to_string()),
            merge_sha: Some("abcdef1234567890".to_string()),
            checks: CheckStatus::Pass,
        }];
        let result = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("https://github.com/owner/repo/issues/1#issuecomment-9"),
                linked_prs: &linked_prs,
                current_body: None,
                expected_dashboard: None,
            },
        );
        assert!(result.ready, "gate should pass: {:?}", result.checks);
        assert!(result.blocked_codes.is_empty());
    }

    #[test]
    fn strict_gate_blocks_when_state_not_complete() {
        let state = v2_body(
            "state",
            json!({"status": "in-progress", "target_scope": "s", "tasks": [], "prs": [], "blockers": [], "links": {}}),
        );
        let source = v2_body("source", json!({"path": "p", "commit": "c"}));
        let plan = v2_body("plan", json!({"path": "p", "commit": "c"}));
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review = v2_body("review", json!({"decision": "approve"}));
        let audit = build_audit_with_evidence(vec![
            (source, "a"),
            (plan, "b"),
            (state, "c"),
            (validation, "d"),
            (review, "e"),
        ]);
        let result = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &[],
                current_body: None,
                expected_dashboard: None,
            },
        );
        assert!(!result.ready);
        assert!(
            result
                .blocked_codes
                .iter()
                .any(|c| c == "state-not-complete"),
            "{:?}",
            result.blocked_codes
        );
    }

    #[test]
    fn strict_gate_blocks_when_review_rejected_or_unresolved() {
        let source = v2_body("source", json!({"path": "p", "commit": "c"}));
        let plan = v2_body("plan", json!({"path": "p", "commit": "c"}));
        let state = v2_body(
            "state",
            json!({"status": "complete", "tasks": [], "prs": [], "blockers": [], "links": {}}),
        );
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review_rejected = v2_body("review", json!({"decision": "request-changes"}));
        let audit_rej = build_audit_with_evidence(vec![
            (source.clone(), "a"),
            (plan.clone(), "b"),
            (state.clone(), "c"),
            (validation.clone(), "d"),
            (review_rejected, "e"),
        ]);
        let res_rej = evaluate_strict_closeout_gate(
            &audit_rej,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &[],
                current_body: None,
                expected_dashboard: None,
            },
        );
        assert!(res_rej.blocked_codes.iter().any(|c| c == "review-rejected"));

        let review_unresolved = v2_body(
            "review",
            json!({
                "decision": "approve",
                "findings": [
                    {"id": "F1", "severity": "blocker", "disposition": "residual", "summary": "x"}
                ]
            }),
        );
        let audit_un = build_audit_with_evidence(vec![
            (source, "a"),
            (plan, "b"),
            (state, "c"),
            (validation, "d"),
            (review_unresolved, "e"),
        ]);
        let res_un = evaluate_strict_closeout_gate(
            &audit_un,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &[],
                current_body: None,
                expected_dashboard: None,
            },
        );
        assert!(
            res_un
                .blocked_codes
                .iter()
                .any(|c| c == "review-unresolved-findings")
        );
    }

    #[test]
    fn strict_gate_blocks_when_linked_pr_missing_merge_sha() {
        let source = v2_body("source", json!({"path": "p", "commit": "c"}));
        let plan = v2_body("plan", json!({"path": "p", "commit": "c"}));
        let state = v2_body(
            "state",
            json!({"status": "complete", "tasks": [], "prs": [], "blockers": [], "links": {}}),
        );
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review = v2_body("review", json!({"decision": "approve"}));
        let audit = build_audit_with_evidence(vec![
            (source, "a"),
            (plan, "b"),
            (state, "c"),
            (validation, "d"),
            (review, "e"),
        ]);
        let linked = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: None,
            merge_sha: None,
            checks: CheckStatus::Pass,
        }];
        let res = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &linked,
                current_body: None,
                expected_dashboard: None,
            },
        );
        assert!(
            res.blocked_codes
                .iter()
                .any(|c| c == "linked-pr-not-merged")
        );
    }

    #[test]
    fn strict_gate_blocks_when_approval_empty() {
        let source = v2_body("source", json!({"path": "p", "commit": "c"}));
        let plan = v2_body("plan", json!({"path": "p", "commit": "c"}));
        let state = v2_body(
            "state",
            json!({"status": "complete", "tasks": [], "prs": [], "blockers": [], "links": {}}),
        );
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review = v2_body("review", json!({"decision": "approve"}));
        let audit = build_audit_with_evidence(vec![
            (source, "a"),
            (plan, "b"),
            (state, "c"),
            (validation, "d"),
            (review, "e"),
        ]);
        let res = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("   "),
                linked_prs: &[],
                current_body: None,
                expected_dashboard: None,
            },
        );
        assert!(res.blocked_codes.iter().any(|c| c == "approval-missing"));
    }

    #[test]
    fn render_record_post_comment_emits_marker_and_hidden_payload_carrier() {
        let body = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::State,
            json!({"status": "complete", "tasks": [], "prs": []}),
            Some("session summary"),
            Some("2026-05-23T08:42:11Z"),
        )
        .expect("render");
        assert!(
            body.starts_with("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
            "{body}"
        );
        assert!(
            !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
            "{body}"
        );
        assert!(body.contains(PAYLOAD_COMMENT_PREFIX), "{body}");
        let payload = extract_payload(&body).expect("payload");
        assert_eq!(payload.schema, PAYLOAD_SCHEMA_V2);
        assert_eq!(payload.role, PayloadRole::State);
        assert!(body.contains("session summary"), "{body}");
    }

    #[test]
    fn render_record_post_comment_rejects_source_or_plan() {
        let err = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Source,
            json!({}),
            None,
            None,
        )
        .expect_err("must reject source");
        assert!(err.contains("source"), "{err}");
    }

    #[test]
    fn render_record_snapshot_comment_includes_details_and_hidden_payload() {
        let snapshot = SnapshotData {
            path: "docs/plans/sample/sample-plan.md".to_string(),
            commit: "abc1234".to_string(),
            title: Some("Sample Plan".to_string()),
            summary: Some("One-liner".to_string()),
        };
        let body = render_record_snapshot_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Plan,
            &snapshot,
            "# Sample Plan\n\nbody...\n",
            Some("2026-05-23T08:42:11Z"),
        )
        .expect("render");
        assert!(
            body.contains("- Path: `docs/plans/sample/sample-plan.md`"),
            "{body}"
        );
        assert!(body.contains("- Commit: `abc1234`"), "{body}");
        assert!(body.contains("- Summary: One-liner"), "{body}");
        assert!(body.contains("<details>"), "{body}");
        assert!(
            !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
            "{body}"
        );
        assert!(body.contains(PAYLOAD_COMMENT_PREFIX), "{body}");
        let payload = extract_payload(&body).expect("payload");
        assert_eq!(payload.schema, PAYLOAD_SCHEMA_V2);
        assert_eq!(payload.role, PayloadRole::Plan);
    }
}
