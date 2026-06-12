use std::collections::BTreeMap;

use nils_markdown::Engine;
use serde::Serialize;
use serde_json::Value;

use crate::commands::record::{LifecycleCommentKind, RecordProfile, TaskLedgerDisplay};

const DASHBOARD_TEMPLATE: &str = include_str!("../templates/lifecycle_record/dashboard.md.tera");
const DASHBOARD_TEMPLATE_NAME: &str = "lifecycle_record_dashboard";

const SNAPSHOT_TEMPLATE: &str = include_str!("../templates/lifecycle_record/snapshot.md.tera");
const SNAPSHOT_TEMPLATE_NAME: &str = "lifecycle_record_snapshot";

const POST_COMMENT_TEMPLATE: &str =
    include_str!("../templates/lifecycle_record/post_comment.md.tera");
const POST_COMMENT_TEMPLATE_NAME: &str = "lifecycle_record_post_comment";

const STATE_VISIBLE_TEMPLATE: &str = include_str!("../templates/lifecycle_record/state.md.tera");
const STATE_VISIBLE_TEMPLATE_NAME: &str = "lifecycle_record_state";

const SESSION_VISIBLE_TEMPLATE: &str =
    include_str!("../templates/lifecycle_record/session.md.tera");
const SESSION_VISIBLE_TEMPLATE_NAME: &str = "lifecycle_record_session";

const VALIDATION_VISIBLE_TEMPLATE: &str =
    include_str!("../templates/lifecycle_record/validation.md.tera");
const VALIDATION_VISIBLE_TEMPLATE_NAME: &str = "lifecycle_record_validation";

const REVIEW_VISIBLE_TEMPLATE: &str = include_str!("../templates/lifecycle_record/review.md.tera");
const REVIEW_VISIBLE_TEMPLATE_NAME: &str = "lifecycle_record_review";

const CLOSEOUT_VISIBLE_TEMPLATE: &str =
    include_str!("../templates/lifecycle_record/closeout.md.tera");
const CLOSEOUT_VISIBLE_TEMPLATE_NAME: &str = "lifecycle_record_closeout";

#[derive(Debug, Serialize)]
struct PostCommentView<'a> {
    marker: String,
    heading: &'static str,
    profile: &'a str,
    visible_content: String,
    envelope_carrier: String,
}

#[derive(Debug, Serialize)]
struct StateVisibleView<'a> {
    status: Option<&'static str>,
    target_scope: Option<&'a str>,
    current: Option<&'a str>,
    next_action: Option<&'a str>,
    tasks: Vec<StateTaskRow>,
}

#[derive(Debug, Serialize)]
struct StateTaskRow {
    id: String,
    status: &'static str,
    title: String,
}

#[derive(Debug, Serialize)]
struct SessionVisibleView<'a> {
    summary: &'a str,
    highlights: Vec<String>,
    links: Vec<KeyValuePair>,
    extras: Vec<KeyValuePair>,
}

#[derive(Debug, Serialize)]
struct KeyValuePair {
    key: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct ValidationVisibleView<'a> {
    overall: &'static str,
    commands: Vec<ValidationCommandRow<'a>>,
    waivers: Vec<ValidationWaiverRow<'a>>,
}

#[derive(Debug, Serialize)]
struct ValidationCommandRow<'a> {
    command: String,
    status: &'static str,
    evidence: String,
    _phantom: std::marker::PhantomData<&'a ()>,
}

#[derive(Debug, Serialize)]
struct ValidationWaiverRow<'a> {
    command: &'a str,
    reason: &'a str,
}

#[derive(Debug, Serialize)]
struct ReviewVisibleView<'a> {
    decision: &'static str,
    lenses: Option<String>,
    outcome_comment_url: Option<&'a str>,
    findings: Vec<ReviewFindingRow>,
}

#[derive(Debug, Serialize)]
struct ReviewFindingRow {
    id: String,
    severity: &'static str,
    disposition: &'static str,
    summary: String,
}

#[derive(Debug, Serialize)]
struct CloseoutVisibleView<'a> {
    final_status: &'a str,
    approver: Option<&'a str>,
    approval_url: Option<&'a str>,
    final_validation_url: Option<&'a str>,
    notes: Option<&'a str>,
    has_override: bool,
    override_reason: Option<String>,
    override_failures: Option<String>,
    linked_prs: Vec<CloseoutPrRow>,
}

#[derive(Debug, Serialize)]
struct CloseoutPrRow {
    label: String,
    merge_sha: String,
    checks: &'static str,
    required: String,
    non_required_failures: String,
}

#[derive(Debug, Serialize)]
struct SnapshotView<'a> {
    marker: String,
    heading: &'static str,
    profile: &'a str,
    path: Option<&'a str>,
    commit: Option<&'a str>,
    summary: Option<&'a str>,
    details_summary: &'static str,
    content: &'a str,
    envelope_carrier: String,
}

#[derive(Debug, Serialize)]
struct DashboardView<'a> {
    title: &'static str,
    status: String,
    profile: &'a str,
    target_scope: String,
    current: String,
    next_action: String,
    validation: String,
    linked_prs: String,
    blockers: String,
    approval: String,
    source_link: String,
    plan_link: String,
    state_link: String,
    session_link: String,
    validation_link: String,
    review_link: String,
    show_review: bool,
    closeout_link: String,
    tracker_block: String,
}

fn render_tracker_block(title: Option<&str>, issue_url: Option<&str>) -> String {
    let title = title.map(str::trim).filter(|value| !value.is_empty());
    let issue_url = issue_url.map(str::trim).filter(|value| !value.is_empty());
    if title.is_none() && issue_url.is_none() {
        return String::new();
    }
    let mut out = vec![
        String::new(),
        "## Original Tracker".to_string(),
        String::new(),
    ];
    if let Some(value) = title {
        out.push(format!("- Title: {value}"));
    }
    if let Some(value) = issue_url {
        out.push(format!("- Issue: {value}"));
    }
    out.join("\n")
}

fn render_dashboard_with_template(view: &DashboardView<'_>) -> String {
    let mut engine = Engine::builder().build();
    engine
        .register_template(DASHBOARD_TEMPLATE_NAME, DASHBOARD_TEMPLATE)
        .expect("dashboard template registers");
    engine
        .render(DASHBOARD_TEMPLATE_NAME, view)
        .expect("dashboard template renders")
}

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

#[derive(Debug)]
struct CommentJson {
    body: Option<String>,
    url: Option<String>,
    html_url: Option<String>,
    created_at: Option<String>,
}

pub fn render_dashboard(input: DashboardInput) -> String {
    let title = if input.status.trim().eq_ignore_ascii_case("complete") {
        "## Final Dashboard"
    } else {
        "## Current Dashboard"
    };

    let show_review = input.profile == RecordProfile::Dispatch || input.review_url.is_some();
    let tracker_block = render_tracker_block(input.title.as_deref(), input.issue_url.as_deref());

    let view = DashboardView {
        title,
        status: input.status.trim().to_string(),
        profile: input.profile.as_str(),
        target_scope: input.target_scope.trim().to_string(),
        current: input.current.trim().to_string(),
        next_action: input.next_action.trim().to_string(),
        validation: input.validation.trim().to_string(),
        linked_prs: non_empty_join(&input.linked_prs, "none yet"),
        blockers: non_empty_join(&input.blockers, "none"),
        approval: input.approval.trim().to_string(),
        source_link: dashboard_link(input.source_url.as_deref(), "source snapshot"),
        plan_link: dashboard_link(input.plan_url.as_deref(), "plan snapshot"),
        state_link: dashboard_link(input.state_url.as_deref(), "execution state"),
        session_link: dashboard_link(input.session_url.as_deref(), "Execution Session"),
        validation_link: dashboard_link(input.validation_url.as_deref(), "Validation Evidence"),
        review_link: dashboard_link(input.review_url.as_deref(), "Review Evidence"),
        show_review,
        closeout_link: dashboard_link(input.closeout_url.as_deref(), "closeout"),
        tracker_block,
    };

    render_dashboard_with_template(&view)
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
    let mut comments = parse_comments_json(comments_json)?;
    comments.sort_by(|left, right| {
        compare_created_at(right.created_at.as_deref(), left.created_at.as_deref())
    });
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
                let key = role.as_str().to_string();
                if evidence.contains_key(&key) {
                    continue;
                }
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
                if let Some(payload) = payload.as_ref() {
                    validate_payload_data_for_role(payload.role, &payload.data).map_err(|err| {
                        format!(
                            "comment at {} has malformed payload for role `{}`: {}",
                            url.as_deref().unwrap_or("(unknown url)"),
                            payload.role.as_str(),
                            err
                        )
                    })?;
                }
                let status = payload.as_ref().and_then(derive_status_from_payload);
                let candidate = LifecycleEvidence {
                    role,
                    profile,
                    url,
                    created_at: created_at.clone(),
                    status,
                    payload,
                };
                evidence_text.push_str(comment_body);
                evidence_text.push('\n');
                recognized_count += 1;
                evidence.insert(key, candidate);
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

/// Return the latest visible comment body per lifecycle role, indexed by
/// [`PayloadRole`]. Mirrors the latest-per-role selection inside
/// [`audit_record`] and is the input the visible-completeness lint operates
/// on (see [`crate::lifecycle_vnext::visible_lint`]).
///
/// `profile_filter` matches the same semantics as [`audit_record`] —
/// comments whose marker carries a different profile are skipped.
pub fn latest_role_bodies(
    comments_json: &str,
    profile_filter: Option<crate::commands::record::RecordProfile>,
) -> Result<BTreeMap<PayloadRole, String>, String> {
    let mut comments = parse_comments_json(comments_json)?;
    comments.sort_by(|left, right| {
        compare_created_at(right.created_at.as_deref(), left.created_at.as_deref())
    });
    let mut bodies: BTreeMap<PayloadRole, String> = BTreeMap::new();
    for comment in comments {
        let Some(body) = comment.body.as_deref() else {
            continue;
        };
        let Some(first_marker) = first_comment_marker(body) else {
            continue;
        };
        let Some(parsed) = parse_marker_line(first_marker) else {
            continue;
        };
        if let MarkerParse::V2 { role, profile } = parsed {
            if profile_filter.is_some_and(|expected| profile != PayloadProfile::from(expected)) {
                continue;
            }
            bodies.entry(role).or_insert_with(|| body.to_string());
        }
    }
    Ok(bodies)
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

    let is_dispatch_profile = profile_str == "dispatch";
    let show_review = is_dispatch_profile || audit.evidence.contains_key("review");
    let tracker_block = render_tracker_block(title, issue_url);

    let view = DashboardView {
        title: dashboard_title,
        status: status_value,
        profile: &profile_str,
        target_scope,
        current,
        next_action,
        validation: validation_status,
        linked_prs: non_empty_join(&linked_prs, "none yet"),
        blockers: non_empty_join(&blockers, "none"),
        approval,
        source_link: dashboard_link(evidence_url(audit, "source").as_deref(), "source snapshot"),
        plan_link: dashboard_link(evidence_url(audit, "plan").as_deref(), "plan snapshot"),
        state_link: dashboard_link(evidence_url(audit, "state").as_deref(), "execution state"),
        session_link: dashboard_link(
            evidence_url(audit, "session").as_deref(),
            "Execution Session",
        ),
        validation_link: dashboard_link(
            evidence_url(audit, "validation").as_deref(),
            "Validation Evidence",
        ),
        review_link: dashboard_link(evidence_url(audit, "review").as_deref(), "Review Evidence"),
        show_review,
        closeout_link: dashboard_link(evidence_url(audit, "closeout").as_deref(), "closeout"),
        tracker_block,
    };

    render_dashboard_with_template(&view)
}

fn evidence_url(audit: &RecordAudit, role: &str) -> Option<String> {
    audit
        .evidence
        .get(role)
        .and_then(|hit| hit.url.clone())
        .filter(|value| !value.trim().is_empty())
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
///
/// This is the active schema identity for today's lifecycle comments, not a
/// promise that future state payload replacements keep v2 readable forever.
pub const PAYLOAD_SCHEMA_V2: &str = "plan-issue-record.payload.v2";

/// Older fenced-code-block info-string used to mark a lifecycle payload.
pub const PAYLOAD_FENCE_INFO: &str = "plan-issue-record-payload";
const PAYLOAD_COMMENT_PREFIX: &str = "plan-issue-record-payload:hex:";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, serde::Deserialize,
)]
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
    Blocked,
    Waived,
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
    /// Aggregate rollup over every PR check (required and non-required
    /// combined). Kept for backward compatibility with the closeout
    /// payload schema; the close gate now consults `required_state`
    /// first and only falls back to `checks` when required-check state
    /// is unknown.
    pub checks: CheckStatus,
    /// Required-check rollup when the provider exposes the
    /// required/non-required distinction. `None` means the adapter
    /// could not resolve a required-only summary (e.g. GitLab today,
    /// or a degraded `gh` call), in which case the gate falls back to
    /// the aggregate `checks` value.
    #[serde(default)]
    pub required_state: Option<CheckStatus>,
    /// Number of required checks reported by the provider. `None`
    /// when required-check classification is unavailable; `Some(0)`
    /// means the PR has zero required checks.
    #[serde(default)]
    pub required_count: Option<u32>,
    /// Names of non-required checks that ended in a failure-class
    /// state. Surfaced as informational evidence in the closeout
    /// comment; never blocks the gate on its own.
    #[serde(default)]
    pub non_required_failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CloseoutData {
    pub final_status: String,
    pub approval: ApprovalEvidence,
    #[serde(default)]
    pub linked_prs: Vec<LinkedPrEvidence>,
    #[serde(default)]
    pub non_required_check_override: Option<Value>,
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
///   [`PAYLOAD_SCHEMA_V2`]. The older fence support is carrier-level support
///   for current v2 records; it is not a future old-schema reader contract.
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

pub(crate) fn raw_payload_marker_count(body: &str) -> usize {
    raw_payload_comment_marker_count(body) + raw_payload_fence_marker_count(body)
}

fn raw_payload_comment_marker_count(body: &str) -> usize {
    body.lines()
        .filter(|line| {
            line.trim()
                .strip_prefix("<!--")
                .and_then(|value| value.strip_suffix("-->"))
                .is_some_and(|inner| inner.trim().starts_with(PAYLOAD_COMMENT_PREFIX))
        })
        .count()
}

fn raw_payload_fence_marker_count(body: &str) -> usize {
    body.lines()
        .filter(|line| {
            line.trim_start()
                .strip_prefix("```")
                .is_some_and(|rest| rest.trim() == PAYLOAD_FENCE_INFO)
        })
        .count()
}

fn collect_payload_comment_carriers(body: &str) -> Result<Vec<String>, PayloadError> {
    let mut out = Vec::new();
    let mut details_depth = 0usize;
    for line in body.lines() {
        if update_details_depth(line, &mut details_depth) {
            continue;
        }
        if details_depth > 0 {
            continue;
        }
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
    let mut details_depth = 0usize;
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
        } else {
            if update_details_depth(line, &mut details_depth) {
                continue;
            }
            if details_depth > 0 {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("```")
                && rest.trim() == PAYLOAD_FENCE_INFO
            {
                current = Some(Vec::new());
            }
        }
    }
    out
}

fn update_details_depth(line: &str, depth: &mut usize) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("<details") {
        *depth += 1;
        return true;
    }
    if trimmed.starts_with("</details>") {
        *depth = depth.saturating_sub(1);
        return true;
    }
    false
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

pub fn validate_payload_data_for_kind(
    kind: LifecycleCommentKind,
    data: &Value,
) -> Result<(), PayloadError> {
    validate_payload_data_for_role(payload_role_for_kind(kind), data)
}

fn validate_payload_data_for_role(role: PayloadRole, data: &Value) -> Result<(), PayloadError> {
    let payload = RecordPayload {
        schema: PAYLOAD_SCHEMA_V2.to_string(),
        role,
        profile: PayloadProfile::Tracking,
        updated_at: None,
        data: data.clone(),
    };
    validate_payload_data(&payload)
}

fn validate_payload_data(payload: &RecordPayload) -> Result<(), PayloadError> {
    match payload.role {
        PayloadRole::Source | PayloadRole::Plan => payload.parse_snapshot().map(|_| ()),
        PayloadRole::State => payload.parse_state().map(|_| ()),
        PayloadRole::Session => payload.parse_session().map(|_| ()),
        PayloadRole::Validation => payload.parse_validation().map(|_| ()),
        PayloadRole::Review => payload.parse_review().map(|_| ()),
        PayloadRole::Closeout => payload.parse_closeout().map(|_| ()),
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

    let path = Some(snapshot.path.trim()).filter(|value| !value.is_empty());
    let commit = Some(snapshot.commit.trim()).filter(|value| !value.is_empty());
    let summary = snapshot
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let view = SnapshotView {
        marker: marker_for(profile, kind),
        heading: default_heading(profile, kind),
        profile: profile.as_str(),
        path,
        commit,
        summary,
        details_summary: default_details_summary(kind),
        content,
        envelope_carrier,
    };

    let mut engine = Engine::builder().build();
    engine
        .register_template(SNAPSHOT_TEMPLATE_NAME, SNAPSHOT_TEMPLATE)
        .map_err(|err| format!("snapshot template register failed: {err}"))?;
    engine
        .render(SNAPSHOT_TEMPLATE_NAME, &view)
        .map_err(|err| format!("snapshot template render failed: {err}"))
}

/// Extract the verbatim file content embedded in a `source`/`plan` snapshot
/// comment's `<details>` block. This is the inverse of the content placement
/// in [`render_record_snapshot_comment`]: that renderer emits
/// `<details>`/`<summary>…</summary>` then a blank line, the file content
/// verbatim, a blank line, and `</details>` (see `snapshot.md.tera`). This
/// returns the lines between the blank line after `<summary>` and the
/// snapshot wrapper's matching `</details>`, accounting for any `<details>`
/// blocks nested inside the content itself.
///
/// The renderer surrounds the content with structural blank lines, so the
/// trailing blank padding is dropped and the result is normalized to end with
/// exactly one `\n`. Bundle Markdown files conventionally end with a single
/// trailing newline, so this round-trips byte-for-byte for well-formed files.
pub fn extract_snapshot_content(comment_body: &str) -> Result<String, String> {
    let lines: Vec<&str> = comment_body.lines().collect();
    let open_idx = lines
        .iter()
        .position(|line| line.trim().starts_with("<details"))
        .ok_or_else(|| "snapshot comment has no <details> block".to_string())?;

    // The wrapper opener is followed by `<summary>…</summary>` and one blank
    // line of template padding before the content begins.
    let mut start = open_idx + 1;
    if start < lines.len() && lines[start].trim_start().starts_with("<summary") {
        start += 1;
    }
    if start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }

    // Collect content until the wrapper's matching `</details>`, tracking
    // nested `<details>` blocks that may appear inside the file content.
    let mut depth = 1usize;
    let mut content: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in &lines[start..] {
        let trimmed = line.trim();
        if trimmed.starts_with("<details") {
            depth += 1;
        } else if trimmed.starts_with("</details>") {
            depth -= 1;
            if depth == 0 {
                closed = true;
                break;
            }
        }
        content.push(line);
    }
    if !closed {
        return Err("snapshot <details> block is not closed".to_string());
    }

    // Drop the renderer's trailing blank-line padding, then normalize to a
    // single trailing newline.
    while content.last().is_some_and(|line| line.trim().is_empty()) {
        content.pop();
    }
    let mut out = content.join("\n");
    out.push('\n');
    Ok(out)
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
    render_record_post_comment_with_display(
        profile,
        kind,
        payload_data,
        None,
        summary,
        updated_at,
        TaskLedgerDisplay::Auto,
    )
}

/// Controls how the visible Execution State header is produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StateHeaderMode {
    /// Preserve the authored execution-state header verbatim. Used by
    /// `record open` and `record post`, where the caller supplies the canonical
    /// execution-state markdown and expects its metadata bullets preserved.
    Authored,
    /// Re-render the header (`Status` / `Target scope` / `Current task` /
    /// `Next task`) from the derived payload. Used by `tracking checkpoint`,
    /// where the controller owns deriving live state from run-state so a
    /// completed plan never keeps a frozen pre-flight header
    /// (graysurf/plan-tracking-testbed#54 / sympoies/nils-cli#700).
    DeriveFromPayload,
}

/// `execution_state` carries the canonical execution-state Markdown for
/// `state` comments; `summary` carries free-form commentary rendered after
/// the comment header, above the generated body. When both are given for a
/// `state` comment, the summary renders above the execution-state document.
pub fn render_record_post_comment_with_display(
    profile: RecordProfile,
    kind: LifecycleCommentKind,
    payload_data: Value,
    execution_state: Option<&str>,
    summary: Option<&str>,
    updated_at: Option<&str>,
    task_ledger_display: TaskLedgerDisplay,
) -> Result<String, String> {
    render_record_post_comment_with_display_mode(
        profile,
        kind,
        payload_data,
        execution_state,
        summary,
        updated_at,
        task_ledger_display,
        StateHeaderMode::Authored,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_record_post_comment_with_display_mode(
    profile: RecordProfile,
    kind: LifecycleCommentKind,
    payload_data: Value,
    execution_state: Option<&str>,
    summary: Option<&str>,
    updated_at: Option<&str>,
    task_ledger_display: TaskLedgerDisplay,
    header_mode: StateHeaderMode,
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
    if execution_state.is_some() && kind != LifecycleCommentKind::State {
        return Err(format!(
            "render_record_post_comment: execution-state markdown is only valid for `state`, got `{}`",
            kind.as_str()
        ));
    }

    let visible_content = render_visible_post_content(
        kind,
        &payload_data,
        execution_state,
        summary,
        task_ledger_display,
        header_mode,
    )?;
    let envelope = RecordPayload {
        schema: PAYLOAD_SCHEMA_V2.to_string(),
        role: payload_role_for_kind(kind),
        profile: PayloadProfile::from(profile),
        updated_at: updated_at.map(str::to_string),
        data: payload_data,
    };
    let envelope_carrier = render_payload_carrier(&envelope)?;

    let view = PostCommentView {
        marker: marker_for(profile, kind),
        heading: default_heading(profile, kind),
        profile: profile.as_str(),
        visible_content,
        envelope_carrier,
    };

    let mut engine = Engine::builder().build();
    engine
        .register_template(POST_COMMENT_TEMPLATE_NAME, POST_COMMENT_TEMPLATE)
        .map_err(|err| format!("post_comment template register failed: {err}"))?;
    engine
        .render(POST_COMMENT_TEMPLATE_NAME, &view)
        .map_err(|err| format!("post_comment template render failed: {err}"))
}

fn render_visible_post_content(
    kind: LifecycleCommentKind,
    payload_data: &Value,
    execution_state: Option<&str>,
    summary: Option<&str>,
    task_ledger_display: TaskLedgerDisplay,
    header_mode: StateHeaderMode,
) -> Result<String, String> {
    let execution_state = execution_state
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let summary = summary.map(str::trim).filter(|value| !value.is_empty());
    let generated = match kind {
        LifecycleCommentKind::State => {
            let state = serde_json::from_value::<StateData>(payload_data.clone())
                .map_err(|err| format!("state payload invalid for visible rendering: {err}"))?;
            match (execution_state, summary) {
                (Some(document), summary) => {
                    let rendered = render_state_markdown_with_task_ledger_display(
                        document,
                        task_ledger_display,
                        &state,
                        header_mode,
                    )?;
                    combine_summary_and_generated(summary, rendered)
                }
                // Single-input path: a summary that carries the ledger
                // document is rendered as the execution-state body. Still
                // load-bearing for `record open` seeding and `tracking
                // checkpoint`, which pass the document through `summary`.
                (None, Some(text)) if text.contains("## Task Ledger") => {
                    render_state_markdown_with_task_ledger_display(
                        text,
                        task_ledger_display,
                        &state,
                        header_mode,
                    )?
                }
                (None, Some(text)) => text.to_string(),
                (None, None) => render_state_payload_visible(&state),
            }
        }
        LifecycleCommentKind::Session => {
            let session = serde_json::from_value::<SessionData>(payload_data.clone())
                .map_err(|err| format!("session payload invalid for visible rendering: {err}"))?;
            combine_summary_and_generated(
                summary,
                render_session_payload_visible(&session, payload_data),
            )
        }
        LifecycleCommentKind::Validation => {
            let validation = serde_json::from_value::<ValidationData>(payload_data.clone())
                .map_err(|err| {
                    format!("validation payload invalid for visible rendering: {err}")
                })?;
            combine_summary_and_generated(summary, render_validation_payload_visible(&validation))
        }
        LifecycleCommentKind::Review => {
            let review = serde_json::from_value::<ReviewData>(payload_data.clone())
                .map_err(|err| format!("review payload invalid for visible rendering: {err}"))?;
            combine_summary_and_generated(summary, render_review_payload_visible(&review))
        }
        LifecycleCommentKind::Closeout => {
            let closeout = serde_json::from_value::<CloseoutData>(payload_data.clone())
                .map_err(|err| format!("closeout payload invalid for visible rendering: {err}"))?;
            combine_summary_and_generated(summary, render_closeout_payload_visible(&closeout))
        }
        LifecycleCommentKind::Source | LifecycleCommentKind::Plan => unreachable!(),
    };

    if generated.trim().is_empty() {
        return Err(format!(
            "`record post --kind {}` would render no visible lifecycle content",
            kind.as_str()
        ));
    }
    Ok(generated)
}

fn combine_summary_and_generated(summary: Option<&str>, generated: String) -> String {
    match (summary, generated.trim().is_empty()) {
        (Some(text), false) => format!("{}\n\n{}", text.trim(), generated.trim()),
        (Some(text), true) => text.trim().to_string(),
        (None, _) => generated,
    }
}

fn render_state_markdown_with_task_ledger_display(
    markdown: &str,
    display: TaskLedgerDisplay,
    state: &StateData,
    header_mode: StateHeaderMode,
) -> Result<String, String> {
    let markdown = normalize_state_markdown_for_comment(markdown)?;
    // On the `tracking checkpoint` path, re-render the authored header
    // (everything before the first `## ` section) from the derived payload so a
    // completed plan reflects live progress instead of a frozen pre-flight
    // header (graysurf/plan-tracking-testbed#54 / sympoies/nils-cli#700).
    // `record open` / `record post` keep the authored header verbatim. Authored
    // sections — `## Task Ledger`, `## Validation Plan`, … — are preserved
    // either way.
    let markdown = match header_mode {
        StateHeaderMode::DeriveFromPayload => replace_state_header_from_payload(&markdown, state),
        StateHeaderMode::Authored => markdown,
    };
    let effective = match display {
        TaskLedgerDisplay::Expanded => TaskLedgerDisplay::Expanded,
        TaskLedgerDisplay::Collapsed => TaskLedgerDisplay::Collapsed,
        TaskLedgerDisplay::Open => TaskLedgerDisplay::Open,
        TaskLedgerDisplay::Auto => {
            if is_terminal_state(state) {
                TaskLedgerDisplay::Expanded
            } else {
                TaskLedgerDisplay::Collapsed
            }
        }
    };
    if effective == TaskLedgerDisplay::Expanded {
        return Ok(markdown);
    }
    // `Collapsed` renders a closed fold; `Open` keeps the same fold toggle but
    // adds the `open` attribute so the ledger is visible by default.
    let details_open_tag = match effective {
        TaskLedgerDisplay::Open => "<details open>",
        _ => "<details>",
    };

    let lines: Vec<&str> = markdown.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|line| line.trim() == "## Task Ledger")
    else {
        return Err("execution-state markdown is missing `## Task Ledger`".to_string());
    };
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(idx, line)| {
            if line.starts_with("## ") {
                Some(idx)
            } else {
                None
            }
        })
        .unwrap_or(lines.len());
    let body = lines[start + 1..end].join("\n").trim().to_string();
    if body.is_empty() {
        return Err("execution-state Task Ledger section is empty".to_string());
    }

    let mut out = Vec::new();
    out.extend(lines[..=start].iter().map(|line| (*line).to_string()));
    out.push(String::new());
    out.push(details_open_tag.to_string());
    out.push("<summary>Show task ledger</summary>".to_string());
    out.push(String::new());
    out.push(body);
    out.push(String::new());
    out.push("</details>".to_string());
    if end < lines.len() {
        out.push(String::new());
        out.extend(lines[end..].iter().map(|line| (*line).to_string()));
    }
    Ok(finalize_markdown(out).trim().to_string())
}

fn normalize_state_markdown_for_comment(markdown: &str) -> Result<String, String> {
    let stripped = markdown
        .trim()
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("<!-- plan-issue-record:")
                && !trimmed.starts_with("<!-- execute-from-tracking-issue:")
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    let Some(execution_heading) = stripped
        .iter()
        .position(|line| line.trim() == "## Execution State")
    else {
        return Err("execution-state markdown is missing `## Execution State`".to_string());
    };

    let mut out = stripped
        .into_iter()
        .skip(execution_heading + 1)
        .filter(|line| !line.trim().starts_with("- Profile:"))
        .collect::<Vec<_>>();
    while out.first().is_some_and(|line| line.trim().is_empty()) {
        out.remove(0);
    }
    let normalized = finalize_markdown(out).trim().to_string();
    if normalized.is_empty() {
        return Err("execution-state markdown has no visible state content".to_string());
    }
    Ok(normalized)
}

/// Shared terminal-status contract for closeout gates; must stay aligned with
/// the close-ready terminal set in `execute.rs`.
fn is_terminal_task_status(status: TaskRowStatus) -> bool {
    matches!(
        status,
        TaskRowStatus::Done | TaskRowStatus::Deferred | TaskRowStatus::Waived
    )
}

fn is_terminal_state(state: &StateData) -> bool {
    state.status == Some(StateStatus::Complete)
        && state
            .tasks
            .iter()
            .all(|task| is_terminal_task_status(task.status))
}

fn render_state_payload_visible(state: &StateData) -> String {
    let view = StateVisibleView {
        status: state.status.map(status_state_label),
        target_scope: state
            .target_scope
            .as_deref()
            .filter(|value| !value.is_empty()),
        current: state.current.as_deref().filter(|value| !value.is_empty()),
        next_action: state
            .next_action
            .as_deref()
            .filter(|value| !value.is_empty()),
        tasks: state
            .tasks
            .iter()
            .map(|task| StateTaskRow {
                id: table_cell(&task.id),
                status: task_row_status_label(task.status),
                title: table_cell(task.title.as_deref().unwrap_or("")),
            })
            .collect(),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(STATE_VISIBLE_TEMPLATE_NAME, STATE_VISIBLE_TEMPLATE)
        .expect("state template registers");
    let rendered = engine
        .render(STATE_VISIBLE_TEMPLATE_NAME, &view)
        .expect("state template renders");
    rendered.trim().to_string()
}

/// Rebuild a normalized execution-state body with its header bullets derived
/// from the payload, keeping every `## ` section (Task Ledger, Validation Plan,
/// …) from the authored markdown. The input must already be normalized (marker
/// and `- Profile:` lines stripped, header starting at the top). When the
/// payload yields no header bullets the authored body is returned unchanged so
/// we never drop all visible content.
fn replace_state_header_from_payload(markdown: &str, state: &StateData) -> String {
    let header = render_state_header_lines_from_payload(state);
    if header.is_empty() {
        return markdown.to_string();
    }
    let lines: Vec<&str> = markdown.lines().collect();
    let first_section = lines
        .iter()
        .position(|line| line.trim_start().starts_with("## "));
    let mut out = header;
    if let Some(idx) = first_section {
        out.push(String::new());
        out.extend(lines[idx..].iter().map(|line| (*line).to_string()));
    }
    finalize_markdown(out).trim().to_string()
}

/// Render the canonical Execution State header bullets (`Status` / `Target
/// scope` / `Current task` / `Next task`) from the payload, omitting any field
/// that is absent or empty.
fn render_state_header_lines_from_payload(state: &StateData) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(status) = state.status.map(status_state_label) {
        lines.push(format!("- Status: {status}"));
    }
    if let Some(scope) = state
        .target_scope
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("- Target scope: {scope}"));
    }
    if let Some(current) = state.current.as_deref().filter(|value| !value.is_empty()) {
        lines.push(format!("- Current task: {current}"));
    }
    if let Some(next) = state
        .next_action
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("- Next task: {next}"));
    }
    lines
}

fn render_session_payload_visible(session: &SessionData, raw: &Value) -> String {
    let extras: Vec<KeyValuePair> = raw
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(key, value)| {
                    !matches!(key.as_str(), "summary" | "highlights" | "links") && !value.is_null()
                })
                .map(|(key, value)| KeyValuePair {
                    key: key.trim().to_string(),
                    value: visible_value(value),
                })
                .collect()
        })
        .unwrap_or_default();

    let view = SessionVisibleView {
        summary: session.summary.trim(),
        highlights: session
            .highlights
            .iter()
            .map(|item| item.trim().to_string())
            .collect(),
        links: session
            .links
            .iter()
            .map(|(key, value)| KeyValuePair {
                key: key.trim().to_string(),
                value: value.trim().to_string(),
            })
            .collect(),
        extras,
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(SESSION_VISIBLE_TEMPLATE_NAME, SESSION_VISIBLE_TEMPLATE)
        .expect("session template registers");
    let rendered = engine
        .render(SESSION_VISIBLE_TEMPLATE_NAME, &view)
        .expect("session template renders");
    rendered.trim().to_string()
}

fn render_validation_payload_visible(validation: &ValidationData) -> String {
    let view = ValidationVisibleView {
        overall: validation_overall_label(validation.overall),
        commands: validation
            .commands
            .iter()
            .map(|command| ValidationCommandRow {
                command: table_cell(&command.command),
                status: validation_command_status_label(command.status),
                evidence: table_cell(command.evidence.as_deref().unwrap_or("")),
                _phantom: std::marker::PhantomData,
            })
            .collect(),
        waivers: validation
            .waivers
            .iter()
            .map(|waiver| ValidationWaiverRow {
                command: waiver.command.trim(),
                reason: waiver.reason.trim(),
            })
            .collect(),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(
            VALIDATION_VISIBLE_TEMPLATE_NAME,
            VALIDATION_VISIBLE_TEMPLATE,
        )
        .expect("validation template registers");
    let rendered = engine
        .render(VALIDATION_VISIBLE_TEMPLATE_NAME, &view)
        .expect("validation template renders");
    rendered.trim().to_string()
}

fn render_review_payload_visible(review: &ReviewData) -> String {
    let view = ReviewVisibleView {
        decision: review_decision_label(review.decision),
        lenses: if review.lenses.is_empty() {
            None
        } else {
            Some(review.lenses.join(", "))
        },
        outcome_comment_url: review
            .outcome_comment_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        findings: review
            .findings
            .iter()
            .map(|finding| ReviewFindingRow {
                id: table_cell(&finding.id),
                severity: finding_severity_label(finding.severity),
                disposition: finding_disposition_label(finding.disposition),
                summary: table_cell(&finding.summary),
            })
            .collect(),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(REVIEW_VISIBLE_TEMPLATE_NAME, REVIEW_VISIBLE_TEMPLATE)
        .expect("review template registers");
    let rendered = engine
        .render(REVIEW_VISIBLE_TEMPLATE_NAME, &view)
        .expect("review template renders");
    rendered.trim().to_string()
}

fn render_closeout_payload_visible(closeout: &CloseoutData) -> String {
    let override_block = closeout
        .non_required_check_override
        .as_ref()
        .filter(|value| !value.is_null());
    let override_reason = override_block.and_then(|block| {
        block
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    });
    let override_failures = override_block.and_then(|block| {
        let items = block
            .get("observed_non_required_failures")
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty())?;
        Some(
            items
                .iter()
                .map(visible_value)
                .filter(|value| !value.trim().is_empty())
                .collect::<Vec<_>>()
                .join(", "),
        )
    });
    let has_override = override_block.is_some();

    let view = CloseoutVisibleView {
        final_status: closeout.final_status.trim(),
        approver: closeout
            .approval
            .approver
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        approval_url: closeout
            .approval
            .comment_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        final_validation_url: closeout
            .final_validation_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        notes: closeout
            .notes
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        has_override,
        override_reason,
        override_failures,
        linked_prs: closeout
            .linked_prs
            .iter()
            .map(|pr| {
                let pr_label = pr.url.as_deref().unwrap_or(&pr.pr_ref);
                let required_label = required_check_label(pr.required_state, pr.required_count);
                CloseoutPrRow {
                    label: table_cell(pr_label),
                    merge_sha: table_cell(pr.merge_sha.as_deref().unwrap_or("")),
                    checks: check_status_label(pr.checks),
                    required: table_cell(&required_label),
                    non_required_failures: table_cell(&non_empty_join(
                        &pr.non_required_failures,
                        "none",
                    )),
                }
            })
            .collect(),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(CLOSEOUT_VISIBLE_TEMPLATE_NAME, CLOSEOUT_VISIBLE_TEMPLATE)
        .expect("closeout template registers");
    let rendered = engine
        .render(CLOSEOUT_VISIBLE_TEMPLATE_NAME, &view)
        .expect("closeout template renders");
    rendered.trim().to_string()
}

fn task_row_status_label(status: TaskRowStatus) -> &'static str {
    match status {
        TaskRowStatus::Pending => "pending",
        TaskRowStatus::InProgress => "in-progress",
        TaskRowStatus::Done => "done",
        TaskRowStatus::Deferred => "deferred",
        TaskRowStatus::Blocked => "blocked",
        TaskRowStatus::Waived => "waived",
    }
}

fn validation_command_status_label(status: ValidationCommandStatus) -> &'static str {
    match status {
        ValidationCommandStatus::Pass => "pass",
        ValidationCommandStatus::Fail => "fail",
        ValidationCommandStatus::Skipped => "skipped",
    }
}

fn finding_severity_label(severity: FindingSeverity) -> &'static str {
    match severity {
        FindingSeverity::Blocker => "blocker",
        FindingSeverity::Major => "major",
        FindingSeverity::Minor => "minor",
        FindingSeverity::Nit => "nit",
    }
}

fn finding_disposition_label(disposition: FindingDisposition) -> &'static str {
    match disposition {
        FindingDisposition::Fixed => "fixed",
        FindingDisposition::Residual => "residual",
        FindingDisposition::FollowUp => "follow-up",
        FindingDisposition::Deferred => "deferred",
        FindingDisposition::NoAction => "no-action",
    }
}

fn check_status_label(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "pass",
        CheckStatus::Fail => "fail",
        CheckStatus::None => "none",
    }
}

/// Render the closeout-comment `Required` column from the
/// `(required_state, required_count)` pair on a [`LinkedPrEvidence`].
///
/// Five label branches:
///
/// - `Some(Pass) + Some(0)` → `"none required"` — no required-check
///   rule exists for the branch (or rule explicitly declares zero
///   required checks). The earlier rendering collapsed this into
///   `"unknown"` even on healthy PRs (sympoies/nils-cli#541 closeout).
/// - `Some(Pass) + Some(N>=1)` → `"pass (N)"` — required checks
///   enforced and green.
/// - `Some(Pass) + None` → `"pass"` — required-state known but count
///   not surfaced by the provider; defensive case kept for future
///   adapters.
/// - `Some(Fail) + …` → `"fail (N)"` or `"fail"` — required checks
///   enforced and at least one is red. Non-required failures are
///   carried in the adjacent column.
/// - `Some(None) + …` → `"none"` — provider reported no aggregate
///   rollup at all (e.g. PR #554 on #541's closeout, where GHA never
///   registered any check suite).
/// - `None + …` → `"unknown"` — adapter probe failed (e.g. `gh` spawn
///   error, `gh pr checks --required` non-zero with unrecognised
///   stderr, fixture omits the field). Kept as the catch-all so a
///   future probe regression remains visible.
fn required_check_label(state: Option<CheckStatus>, count: Option<u32>) -> String {
    match (state, count) {
        (Some(CheckStatus::Pass), Some(0)) => "none required".to_string(),
        (Some(CheckStatus::Pass), Some(n)) => format!("pass ({n})"),
        (Some(CheckStatus::Pass), None) => "pass".to_string(),
        (Some(CheckStatus::Fail), Some(n)) => format!("fail ({n})"),
        (Some(CheckStatus::Fail), None) => "fail".to_string(),
        (Some(CheckStatus::None), _) => "none".to_string(),
        (None, _) => "unknown".to_string(),
    }
}

fn table_cell(value: &str) -> String {
    value.trim().replace('|', "\\|").replace('\n', "<br>")
}

fn visible_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.trim().to_string(),
        Value::Array(items) => items
            .iter()
            .map(visible_value)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => value.to_string(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

// -----------------------------------------------------------------------------
// Strict closeout gate for `record close`.
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
    /// When `true`, the linked-PR branch skips the conservative
    /// "unknown required-check state with aggregate failure" check
    /// and lets the gate pass on non-required failures alone. The
    /// caller is responsible for surfacing the override decision in
    /// closeout-comment evidence; the gate itself does not record it.
    pub allow_non_required_check_failure: bool,
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
                            data.tasks
                                .iter()
                                .any(|task| !is_terminal_task_status(task.status))
                        })
                        .unwrap_or(false);
                    if tasks_incomplete {
                        push_fail(
                            &mut checks,
                            &mut blocked_codes,
                            "execution state",
                            "complete but tasks are not all done/deferred/waived".to_string(),
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

    match audit.evidence.get("session") {
        Some(hit) => push_pass(
            &mut checks,
            "execution session",
            hit.url.as_deref().unwrap_or("present").to_string(),
        ),
        None => push_fail(
            &mut checks,
            &mut blocked_codes,
            "execution session",
            "missing role=session lifecycle record".to_string(),
            "session-missing",
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
        let mut unmerged: Vec<String> = Vec::new();
        let mut required_failed: Vec<String> = Vec::new();
        for pr in input.linked_prs {
            let sha = pr.merge_sha.as_deref().map(str::trim).unwrap_or("");
            if sha.is_empty() {
                unmerged.push(format!("{} (no merge_sha)", pr.pr_ref));
                continue;
            }
            match pr.required_state {
                Some(CheckStatus::Fail) => {
                    required_failed.push(format!("{} (required checks failed)", pr.pr_ref));
                }
                Some(CheckStatus::Pass | CheckStatus::None) => {
                    // Required checks resolved cleanly (including the
                    // `required_count == 0` case). Non-required failures
                    // are informational only and never block.
                }
                None => {
                    // Provider could not classify required-vs-non-required
                    // (e.g. GitLab today, or a degraded `gh` call). Stay
                    // conservative: aggregate failure blocks unless the
                    // caller has set the explicit override flag.
                    if matches!(pr.checks, CheckStatus::Fail)
                        && !input.allow_non_required_check_failure
                    {
                        required_failed.push(format!(
                            "{} (checks={:?}; required-state unknown)",
                            pr.pr_ref, pr.checks
                        ));
                    }
                }
            }
        }
        if !unmerged.is_empty() {
            push_fail(
                &mut checks,
                &mut blocked_codes,
                "linked PRs",
                unmerged.join(", "),
                "linked-pr-not-merged",
            );
        }
        if !required_failed.is_empty() {
            push_fail(
                &mut checks,
                &mut blocked_codes,
                "linked PRs required checks",
                required_failed.join(", "),
                "linked-pr-checks-failed",
            );
        }
        if unmerged.is_empty() && required_failed.is_empty() {
            push_pass(
                &mut checks,
                "linked PRs",
                format!("{} merged", input.linked_prs.len()),
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
        let session = v2_body("session", json!({"summary": "session complete"}));
        let audit = build_audit_with_evidence(vec![
            (source, "u-src"),
            (plan, "u-plan"),
            (state, "u-state"),
            (session, "u-session"),
            (validation, "u-val"),
            (review, "u-rev"),
        ]);

        let linked_prs = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: Some("https://github.com/owner/repo/pull/1".to_string()),
            merge_sha: Some("abcdef1234567890".to_string()),
            checks: CheckStatus::Pass,
            required_state: Some(CheckStatus::Pass),
            required_count: Some(1),
            non_required_failures: Vec::new(),
        }];
        let result = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("https://github.com/owner/repo/issues/1#issuecomment-9"),
                linked_prs: &linked_prs,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: false,
            },
        );
        assert!(result.ready, "gate should pass: {:?}", result.checks);
        assert!(result.blocked_codes.is_empty());
    }

    #[test]
    fn strict_gate_passes_when_state_tasks_include_waived() {
        // Reproduces plan-tracking-testbed#65: close-ready and
        // `is_terminal_state` already treat `waived` as terminal, so the
        // strict record-close gate must accept it too instead of blocking
        // with `state-tasks-incomplete`.
        let state = v2_body(
            "state",
            json!({
                "status": "complete",
                "target_scope": "scope",
                "tasks": [
                    {"id": "1.1", "status": "done", "title": "x"},
                    {"id": "1.2", "status": "waived", "title": "y"},
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
        let session = v2_body("session", json!({"summary": "session complete"}));
        let audit = build_audit_with_evidence(vec![
            (source, "u-src"),
            (plan, "u-plan"),
            (state, "u-state"),
            (session, "u-session"),
            (validation, "u-val"),
            (review, "u-rev"),
        ]);

        let linked_prs = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: Some("https://github.com/owner/repo/pull/1".to_string()),
            merge_sha: Some("abcdef1234567890".to_string()),
            checks: CheckStatus::Pass,
            required_state: Some(CheckStatus::Pass),
            required_count: Some(1),
            non_required_failures: Vec::new(),
        }];
        let result = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("https://github.com/owner/repo/issues/1#issuecomment-9"),
                linked_prs: &linked_prs,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: false,
            },
        );
        assert!(
            result.ready,
            "waived task row should be terminal: {:?}",
            result.checks
        );
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
        let session = v2_body("session", json!({"summary": "session complete"}));
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review = v2_body("review", json!({"decision": "approve"}));
        let audit = build_audit_with_evidence(vec![
            (source, "a"),
            (plan, "b"),
            (state, "c"),
            (session, "d"),
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
                allow_non_required_check_failure: false,
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
        let session = v2_body("session", json!({"summary": "session complete"}));
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review_rejected = v2_body("review", json!({"decision": "request-changes"}));
        let audit_rej = build_audit_with_evidence(vec![
            (source.clone(), "a"),
            (plan.clone(), "b"),
            (state.clone(), "c"),
            (session.clone(), "d"),
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
                allow_non_required_check_failure: false,
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
            (session, "d"),
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
                allow_non_required_check_failure: false,
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
        let session = v2_body("session", json!({"summary": "session complete"}));
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review = v2_body("review", json!({"decision": "approve"}));
        let audit = build_audit_with_evidence(vec![
            (source, "a"),
            (plan, "b"),
            (state, "c"),
            (session, "d"),
            (validation, "d"),
            (review, "e"),
        ]);
        let linked = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: None,
            merge_sha: None,
            checks: CheckStatus::Pass,
            required_state: Some(CheckStatus::Pass),
            required_count: Some(0),
            non_required_failures: Vec::new(),
        }];
        let res = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &linked,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: false,
            },
        );
        assert!(
            res.blocked_codes
                .iter()
                .any(|c| c == "linked-pr-not-merged")
        );
    }

    #[test]
    fn strict_gate_passes_with_non_required_failure_when_required_pass() {
        // Regression for sympoies/nils-cli#502: a non-required check
        // failure with required-state success must not block the gate.
        let audit = build_audit_with_evidence(vec![
            (v2_body("source", json!({"path": "p", "commit": "c"})), "a"),
            (v2_body("plan", json!({"path": "p", "commit": "c"})), "b"),
            (
                v2_body(
                    "state",
                    json!({"status": "complete", "tasks": [], "prs": [], "blockers": [], "links": {}}),
                ),
                "c",
            ),
            (
                v2_body("session", json!({"summary": "session complete"})),
                "d",
            ),
            (v2_body("validation", json!({"overall": "pass"})), "d"),
            (v2_body("review", json!({"decision": "approve"})), "e"),
        ]);
        let linked = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: None,
            merge_sha: Some("abc".to_string()),
            checks: CheckStatus::Fail,
            required_state: Some(CheckStatus::Pass),
            required_count: Some(0),
            non_required_failures: vec!["scripts/ci/all.sh".to_string()],
        }];
        let res = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &linked,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: false,
            },
        );
        assert!(res.ready, "blocked: {:?}", res.blocked_codes);
        assert!(res.blocked_codes.is_empty(), "{:?}", res.blocked_codes);
    }

    #[test]
    fn strict_gate_emits_linked_pr_checks_failed_when_required_fail() {
        let audit = build_audit_with_evidence(vec![
            (v2_body("source", json!({"path": "p", "commit": "c"})), "a"),
            (v2_body("plan", json!({"path": "p", "commit": "c"})), "b"),
            (
                v2_body(
                    "state",
                    json!({"status": "complete", "tasks": [], "prs": [], "blockers": [], "links": {}}),
                ),
                "c",
            ),
            (
                v2_body("session", json!({"summary": "session complete"})),
                "d",
            ),
            (v2_body("validation", json!({"overall": "pass"})), "d"),
            (v2_body("review", json!({"decision": "approve"})), "e"),
        ]);
        let linked = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: None,
            merge_sha: Some("abc".to_string()),
            checks: CheckStatus::Fail,
            required_state: Some(CheckStatus::Fail),
            required_count: Some(2),
            non_required_failures: Vec::new(),
        }];
        let res = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &linked,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: false,
            },
        );
        assert!(
            res.blocked_codes
                .iter()
                .any(|c| c == "linked-pr-checks-failed"),
            "expected linked-pr-checks-failed, got {:?}",
            res.blocked_codes
        );
        assert!(
            !res.blocked_codes
                .iter()
                .any(|c| c == "linked-pr-not-merged"),
            "must not collapse into linked-pr-not-merged"
        );
    }

    #[test]
    fn strict_gate_override_unblocks_unknown_required_state_aggregate_fail() {
        let audit = build_audit_with_evidence(vec![
            (v2_body("source", json!({"path": "p", "commit": "c"})), "a"),
            (v2_body("plan", json!({"path": "p", "commit": "c"})), "b"),
            (
                v2_body(
                    "state",
                    json!({"status": "complete", "tasks": [], "prs": [], "blockers": [], "links": {}}),
                ),
                "c",
            ),
            (
                v2_body("session", json!({"summary": "session complete"})),
                "d",
            ),
            (v2_body("validation", json!({"overall": "pass"})), "d"),
            (v2_body("review", json!({"decision": "approve"})), "e"),
        ]);
        let linked = vec![LinkedPrEvidence {
            pr_ref: "owner/repo#1".to_string(),
            url: None,
            merge_sha: Some("abc".to_string()),
            checks: CheckStatus::Fail,
            required_state: None,
            required_count: None,
            non_required_failures: vec!["opt-in/lint".to_string()],
        }];

        let blocked = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &linked,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: false,
            },
        );
        assert!(
            blocked
                .blocked_codes
                .iter()
                .any(|c| c == "linked-pr-checks-failed"),
            "conservative path blocks: {:?}",
            blocked.blocked_codes
        );

        let unblocked = evaluate_strict_closeout_gate(
            &audit,
            StrictCloseoutGateInput {
                profile: RecordProfile::Tracking,
                approval: Some("ok"),
                linked_prs: &linked,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: true,
            },
        );
        assert!(unblocked.ready, "{:?}", unblocked.blocked_codes);
    }

    #[test]
    fn strict_gate_blocks_when_approval_empty() {
        let source = v2_body("source", json!({"path": "p", "commit": "c"}));
        let plan = v2_body("plan", json!({"path": "p", "commit": "c"}));
        let state = v2_body(
            "state",
            json!({"status": "complete", "tasks": [], "prs": [], "blockers": [], "links": {}}),
        );
        let session = v2_body("session", json!({"summary": "session complete"}));
        let validation = v2_body("validation", json!({"overall": "pass"}));
        let review = v2_body("review", json!({"decision": "approve"}));
        let audit = build_audit_with_evidence(vec![
            (source, "a"),
            (plan, "b"),
            (state, "c"),
            (session, "d"),
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
                allow_non_required_check_failure: false,
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

    fn state_summary_with_task_ledger() -> &'static str {
        "## Execution State\n\n\
         - Status: in-progress\n\n\
         ## Task Ledger\n\n\
         | ID | Status | Task |\n\
         | --- | --- | --- |\n\
         | 1.1 | pending | Demo task |\n"
    }

    fn render_state_with_display(display: TaskLedgerDisplay) -> String {
        render_record_post_comment_with_display(
            RecordProfile::Tracking,
            LifecycleCommentKind::State,
            json!({
                "status": "in-progress",
                "tasks": [{"id": "1.1", "status": "pending", "title": "Demo task"}],
                "prs": [],
                "blockers": [],
                "links": {}
            }),
            None,
            Some(state_summary_with_task_ledger()),
            None,
            display,
        )
        .expect("render")
    }

    #[test]
    fn task_ledger_display_open_emits_open_fold() {
        let body = render_state_with_display(TaskLedgerDisplay::Open);
        assert!(body.contains("<details open>"), "{body}");
        assert!(
            body.contains("<summary>Show task ledger</summary>"),
            "{body}"
        );
        assert!(body.contains("| 1.1 | pending | Demo task |"), "{body}");
    }

    #[test]
    fn task_ledger_display_collapsed_emits_closed_fold() {
        let body = render_state_with_display(TaskLedgerDisplay::Collapsed);
        assert!(body.contains("<details>"), "{body}");
        assert!(!body.contains("<details open>"), "{body}");
    }

    #[test]
    fn task_ledger_display_expanded_emits_no_fold() {
        let body = render_state_with_display(TaskLedgerDisplay::Expanded);
        assert!(!body.contains("<details"), "{body}");
        assert!(body.contains("| 1.1 | pending | Demo task |"), "{body}");
    }

    #[test]
    fn render_record_post_comment_synthesizes_validation_review_and_closeout() {
        let validation = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Validation,
            json!({
                "overall": "pass",
                "commands": [{"command": "cargo test", "status": "pass", "evidence": "ok"}],
                "waivers": []
            }),
            None,
            None,
        )
        .expect("validation render");
        assert!(validation.contains("- Overall: pass"), "{validation}");
        assert!(
            validation.contains("| cargo test | pass | ok |"),
            "{validation}"
        );
        assert!(validation.contains(PAYLOAD_COMMENT_PREFIX), "{validation}");

        let review = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Review,
            json!({
                "decision": "approve",
                "lenses": ["testing", "maintainability"],
                "findings": [{
                    "id": "F1",
                    "severity": "minor",
                    "disposition": "fixed",
                    "summary": "covered"
                }],
                "outcome_comment_url": "https://example.test/review"
            }),
            None,
            None,
        )
        .expect("review render");
        assert!(review.contains("- Decision: approve"), "{review}");
        assert!(
            review.contains("- Lenses: testing, maintainability"),
            "{review}"
        );
        assert!(
            review.contains("| F1 | minor | fixed | covered |"),
            "{review}"
        );

        let closeout = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Closeout,
            json!({
                "final_status": "complete",
                "approval": {"comment_url": "https://example.test/approval"},
                "linked_prs": [{
                    "ref": "owner/repo#1",
                    "url": "https://example.test/pr/1",
                    "merge_sha": "abc123",
                    "checks": "pass",
                    "required_state": "pass",
                    "required_count": 2,
                    "non_required_failures": []
                }],
                "non_required_check_override": {
                    "reason": "operator accepted non-required lint",
                    "observed_non_required_failures": ["owner/repo#1: opt-in/lint"]
                },
                "notes": "closed"
            }),
            Some("Closeout summary."),
            None,
        )
        .expect("closeout render");
        assert!(closeout.contains("Closeout summary."), "{closeout}");
        assert!(closeout.contains("- Final status: complete"), "{closeout}");
        assert!(
            closeout.contains("| https://example.test/pr/1 | abc123 | pass | pass (2) | none |"),
            "{closeout}"
        );
        assert!(
            closeout.contains("- Reason: operator accepted non-required lint"),
            "{closeout}"
        );
        assert!(
            closeout.contains("- Observed failures: owner/repo#1: opt-in/lint"),
            "{closeout}"
        );

        let no_pr_closeout = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Closeout,
            json!({
                "final_status": "complete",
                "approval": {"comment_url": "https://example.test/approval"},
                "linked_prs": [],
                "notes": "closed without linked PR"
            }),
            Some("Closeout summary."),
            None,
        )
        .expect("closeout render");
        assert!(
            no_pr_closeout.contains("- Linked PRs: none"),
            "{no_pr_closeout}"
        );
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

    #[test]
    fn extract_payload_ignores_payload_markers_inside_snapshot_details() {
        let nested_payload = RecordPayload {
            schema: PAYLOAD_SCHEMA_V2.to_string(),
            role: PayloadRole::State,
            profile: PayloadProfile::Tracking,
            updated_at: None,
            data: json!({"status": "complete"}),
        };
        let nested_carrier = render_payload_carrier(&nested_payload).expect("nested carrier");
        let snapshot = SnapshotData {
            path: "docs/plans/sample/sample-discussion-source.md".to_string(),
            commit: "abc1234".to_string(),
            title: None,
            summary: None,
        };
        let body = render_record_snapshot_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Source,
            &snapshot,
            &format!(
                "# Source\n\n{nested_carrier}\n\n```{PAYLOAD_FENCE_INFO}\n{{not valid json}}\n```\n"
            ),
            None,
        )
        .expect("render");

        let payload = extract_payload(&body).expect("payload");
        assert_eq!(payload.role, PayloadRole::Source);
    }

    #[test]
    fn required_check_label_emits_five_distinct_branches() {
        // `Some(Pass) + Some(0)` is the "no required-check rule" case
        // observed on sympoies/nils-cli#541's closeout — was previously
        // collapsed into "unknown".
        assert_eq!(
            required_check_label(Some(CheckStatus::Pass), Some(0)),
            "none required"
        );

        // `Some(Pass) + Some(N>=1)` keeps the existing "pass (N)" shape.
        assert_eq!(
            required_check_label(Some(CheckStatus::Pass), Some(3)),
            "pass (3)"
        );

        // `Some(Pass) + None` is the defensive case for adapters that
        // know the state but not the count.
        assert_eq!(required_check_label(Some(CheckStatus::Pass), None), "pass");

        // `Some(Fail) + Some(N)` keeps the existing "fail (N)" shape.
        assert_eq!(
            required_check_label(Some(CheckStatus::Fail), Some(2)),
            "fail (2)"
        );
        assert_eq!(required_check_label(Some(CheckStatus::Fail), None), "fail");

        // `Some(None)` is the aggregate-rollup-absent case (PR #554 on
        // #541's closeout — GHA never registered any check suite).
        assert_eq!(
            required_check_label(Some(CheckStatus::None), Some(0)),
            "none"
        );
        assert_eq!(required_check_label(Some(CheckStatus::None), None), "none");

        // `None` is the catch-all for probe failures / fixture omissions.
        assert_eq!(required_check_label(None, None), "unknown");
        assert_eq!(required_check_label(None, Some(0)), "unknown");
    }

    // Snapshot tests below lock the full byte-for-byte wire shape of
    // `render_record_post_comment` for each lifecycle kind. The existing
    // `contains` assertions cover individual fields; these goldens guard
    // against silent template/serializer drift that re-orders lines, drops
    // sections, or grows a new field downstream consumers don't expect.

    fn golden_dump(label: &str, body: &str) {
        if std::env::var("LIFECYCLE_RECORD_GOLDEN_DUMP").is_ok() {
            eprintln!("--- BEGIN {label} ---\n{body}\n--- END {label} ---");
        }
    }

    #[test]
    fn golden_state_post_comment_locks_full_wire_shape() {
        let body = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::State,
            json!({
                "status": "complete",
                "target_scope": "PR #599 follow-ups",
                "current": "delivering snapshot tests",
                "next_action": "open closeout comment",
                "tasks": [
                    {"id": "1.1", "status": "done", "title": "ship URL parser"},
                    {"id": "1.2", "status": "in-progress", "title": "ship snapshots"},
                ],
                "prs": [{"ref": "owner/repo#1", "url": "https://example.test/pr/1", "status": "merged"}],
                "blockers": [],
                "links": {},
            }),
            None,
            Some("2026-05-23T08:42:11Z"),
        )
        .expect("state render");
        golden_dump("state", &body);
        assert_eq!(
            body,
            include_str!("snapshots/state_post_comment.md"),
            "state post-comment shape drifted; run with LIFECYCLE_RECORD_GOLDEN_DUMP=1 to dump"
        );
    }

    #[test]
    fn golden_validation_post_comment_locks_full_wire_shape() {
        let body = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Validation,
            json!({
                "overall": "pass",
                "commands": [
                    {"command": "cargo test --workspace", "status": "pass", "evidence": "all green"},
                    {"command": "scripts/ci/local-fast.sh", "status": "pass", "evidence": "ok"},
                ],
                "waivers": [],
            }),
            None,
            Some("2026-05-23T08:42:11Z"),
        )
        .expect("validation render");
        golden_dump("validation", &body);
        assert_eq!(
            body,
            include_str!("snapshots/validation_post_comment.md"),
            "validation post-comment shape drifted"
        );
    }

    #[test]
    fn golden_review_post_comment_locks_full_wire_shape() {
        let body = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Review,
            json!({
                "decision": "approve",
                "lenses": ["testing", "maintainability"],
                "findings": [
                    {"id": "F1", "severity": "minor", "disposition": "fixed", "summary": "covered"},
                ],
                "outcome_comment_url": "https://example.test/review",
            }),
            None,
            Some("2026-05-23T08:42:11Z"),
        )
        .expect("review render");
        golden_dump("review", &body);
        assert_eq!(
            body,
            include_str!("snapshots/review_post_comment.md"),
            "review post-comment shape drifted"
        );
    }

    #[test]
    fn golden_closeout_post_comment_locks_full_wire_shape() {
        let body = render_record_post_comment(
            RecordProfile::Tracking,
            LifecycleCommentKind::Closeout,
            json!({
                "final_status": "complete",
                "approval": {"comment_url": "https://example.test/approval"},
                "linked_prs": [{
                    "ref": "owner/repo#1",
                    "url": "https://example.test/pr/1",
                    "merge_sha": "abc1234",
                    "checks": "pass",
                    "required_state": "pass",
                    "required_count": 2,
                    "non_required_failures": []
                }],
                "notes": "shipped"
            }),
            Some("Closeout summary."),
            Some("2026-05-23T08:42:11Z"),
        )
        .expect("closeout render");
        golden_dump("closeout", &body);
        assert_eq!(
            body,
            include_str!("snapshots/closeout_post_comment.md"),
            "closeout post-comment shape drifted"
        );
    }
}
