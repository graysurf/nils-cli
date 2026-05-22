use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

use crate::commands::record::{LifecycleCommentKind, MarkerFamily, RecordProfile};

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
    pub marker_family: MarkerFamily,
    pub kind: LifecycleCommentKind,
    pub path: Option<String>,
    pub commit: Option<String>,
    pub content: Option<String>,
    pub title: Option<String>,
    pub details_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MarkerHit {
    pub role: String,
    pub profile: String,
    pub family: String,
    pub url: Option<String>,
    pub created_at: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordAudit {
    pub profile_filter: Option<String>,
    pub body_sections: BodySections,
    pub markers: BTreeMap<String, MarkerHit>,
    pub missing_required: Vec<String>,
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
    let marker = marker_for(input.marker_family, input.profile, input.kind)?;
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

    if input.marker_family == MarkerFamily::Shared || input.profile == RecordProfile::Dispatch {
        out.push(format!("- Profile: {}", input.profile.as_str()));
    }
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
    let mut markers = BTreeMap::new();
    let mut recognized_count = 0usize;
    let mut evidence_text = String::new();
    if let Some(body) = body {
        evidence_text.push_str(body);
        evidence_text.push('\n');
    }

    for comment in comments {
        let Some(body) = comment.body.as_deref() else {
            continue;
        };
        evidence_text.push_str(body);
        evidence_text.push('\n');
        let Some(mut hit) = marker_hit(body) else {
            continue;
        };
        if profile_filter.is_some_and(|profile| hit.profile != profile.as_str()) {
            continue;
        }
        hit.url = comment.url.or(comment.html_url);
        hit.created_at = comment.created_at;
        hit.status = extract_status(body);
        recognized_count += 1;
        markers.insert(hit.role.clone(), hit);
    }

    let mut missing_required = Vec::new();
    for role in ["source_snapshot", "plan_snapshot", "state"] {
        if !markers.contains_key(role) {
            missing_required.push(role.to_string());
        }
    }

    Ok(RecordAudit {
        profile_filter: profile_filter.map(|profile| profile.as_str().to_string()),
        body_sections: inspect_body_sections(body.unwrap_or_default()),
        markers,
        missing_required,
        recognized_count,
        evidence_text,
    })
}

pub fn evaluate_closeout_gate(audit: &RecordAudit, input: CloseoutGateInput) -> CloseoutGateResult {
    let mut checks = Vec::new();

    push_marker_check(&mut checks, audit, "source_snapshot", "source snapshot");
    push_marker_check(&mut checks, audit, "plan_snapshot", "plan snapshot");
    push_marker_check(&mut checks, audit, "state", "execution state");

    if input.require_complete {
        let (status, detail) = match audit
            .markers
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
        push_marker_check(&mut checks, audit, "session", "completed session");
    }
    if input.require_validation {
        push_marker_check(&mut checks, audit, "validation", "validation evidence");
    }
    if input.require_review || input.profile == RecordProfile::Dispatch {
        push_marker_check(&mut checks, audit, "review", "review evidence");
    }
    if input.require_closeout {
        push_marker_check(&mut checks, audit, "closeout", "closeout comment");
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

fn marker_for(
    family: MarkerFamily,
    profile: RecordProfile,
    kind: LifecycleCommentKind,
) -> Result<String, String> {
    let marker = match family {
        MarkerFamily::Shared => match kind {
            LifecycleCommentKind::Source | LifecycleCommentKind::Plan => format!(
                "<!-- issue-backed-plan:snapshot:v1 kind={} profile={} -->",
                kind.as_str(),
                profile.as_str()
            ),
            _ => format!(
                "<!-- issue-backed-plan:{}:v1 profile={} -->",
                kind.as_str(),
                profile.as_str()
            ),
        },
        MarkerFamily::Compat => compat_marker(profile, kind)?,
    };
    Ok(marker)
}

fn compat_marker(profile: RecordProfile, kind: LifecycleCommentKind) -> Result<String, String> {
    let marker = match (profile, kind) {
        (RecordProfile::Tracking, LifecycleCommentKind::Source)
        | (RecordProfile::Tracking, LifecycleCommentKind::Plan) => format!(
            "<!-- plan-tracking-issue:snapshot:v1 kind={} -->",
            kind.as_str()
        ),
        (RecordProfile::Tracking, LifecycleCommentKind::State) => {
            "<!-- execute-from-tracking-issue:state:v1 -->".to_string()
        }
        (RecordProfile::Tracking, LifecycleCommentKind::Session) => {
            "<!-- execute-from-tracking-issue:session:v1 -->".to_string()
        }
        (RecordProfile::Tracking, LifecycleCommentKind::Validation) => {
            "<!-- execute-from-tracking-issue:validation:v1 -->".to_string()
        }
        (RecordProfile::Tracking, LifecycleCommentKind::Closeout) => {
            "<!-- tracking-issue-closeout:v1 -->".to_string()
        }
        (RecordProfile::Tracking, LifecycleCommentKind::Review) => {
            return Err("tracking profile does not emit compat review markers".to_string());
        }
        (RecordProfile::Dispatch, LifecycleCommentKind::Source)
        | (RecordProfile::Dispatch, LifecycleCommentKind::Plan) => format!(
            "<!-- deliver-dispatch-plan:snapshot:v1 kind={} -->",
            kind.as_str()
        ),
        (RecordProfile::Dispatch, LifecycleCommentKind::State) => {
            "<!-- deliver-dispatch-plan:state:v1 -->".to_string()
        }
        (RecordProfile::Dispatch, LifecycleCommentKind::Session) => {
            "<!-- deliver-dispatch-plan:session:v1 -->".to_string()
        }
        (RecordProfile::Dispatch, LifecycleCommentKind::Validation) => {
            "<!-- deliver-dispatch-plan:validation:v1 -->".to_string()
        }
        (RecordProfile::Dispatch, LifecycleCommentKind::Review) => {
            "<!-- deliver-dispatch-plan:review:v1 -->".to_string()
        }
        (RecordProfile::Dispatch, LifecycleCommentKind::Closeout) => {
            "<!-- deliver-dispatch-plan:closeout:v1 -->".to_string()
        }
    };
    Ok(marker)
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

fn marker_hit(body: &str) -> Option<MarkerHit> {
    let marker = first_comment_marker(body)?;
    parse_marker_line(marker)
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

fn parse_marker_line(marker: &str) -> Option<MarkerHit> {
    let inner = marker.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let attrs = parse_attrs(inner);

    if inner.starts_with("issue-backed-plan:snapshot:v1") {
        let kind = attrs.get("kind")?;
        let profile = attrs
            .get("profile")
            .map(String::as_str)
            .unwrap_or("tracking");
        return Some(hit(format!("{kind}_snapshot"), profile, "shared"));
    }
    if let Some(rest) = inner.strip_prefix("issue-backed-plan:") {
        let kind = rest.split(':').next()?;
        let profile = attrs
            .get("profile")
            .map(String::as_str)
            .unwrap_or("tracking");
        return Some(hit(kind.to_string(), profile, "shared"));
    }

    if inner.starts_with("plan-tracking-issue:snapshot:v") {
        let kind = attrs.get("kind")?;
        return Some(hit(format!("{kind}_snapshot"), "tracking", "compat"));
    }
    if let Some(rest) = inner.strip_prefix("execute-from-tracking-issue:") {
        let kind = rest.split(':').next()?;
        return Some(hit(kind.to_string(), "tracking", "compat"));
    }
    if let Some(rest) = inner.strip_prefix("execute-plan-tracking-issue:") {
        let kind = rest.split(':').next()?;
        return Some(hit(kind.to_string(), "tracking", "compat"));
    }
    if inner.starts_with("tracking-issue-closeout:v")
        || inner.starts_with("plan-tracking-issue-closeout:v")
    {
        return Some(hit("closeout".to_string(), "tracking", "compat"));
    }

    if inner.starts_with("deliver-dispatch-plan:snapshot:v") {
        let kind = attrs.get("kind")?;
        return Some(hit(format!("{kind}_snapshot"), "dispatch", "compat"));
    }
    if let Some(rest) = inner.strip_prefix("deliver-dispatch-plan:") {
        let kind = rest.split(':').next()?;
        return Some(hit(kind.to_string(), "dispatch", "compat"));
    }
    if let Some(rest) = inner.strip_prefix("dispatch-plan:") {
        let kind = rest.split(':').next()?;
        return Some(hit(kind.to_string(), "dispatch", "compat"));
    }

    None
}

fn hit(role: String, profile: &str, family: &str) -> MarkerHit {
    MarkerHit {
        role,
        profile: profile.to_string(),
        family: family.to_string(),
        url: None,
        created_at: None,
        status: None,
    }
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

fn extract_status(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("- Status:") else {
            continue;
        };
        let status = rest.trim();
        if !status.is_empty() {
            return Some(status.to_string());
        }
    }
    None
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

fn push_marker_check(
    checks: &mut Vec<CloseoutCheck>,
    audit: &RecordAudit,
    role: &str,
    label: &str,
) {
    let (status, detail) = match audit.markers.get(role) {
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
    for hit in audit.markers.values() {
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
