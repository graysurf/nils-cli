//! Deterministic template preview for lifecycle roles.
//!
//! Backs the `plan-issue record template` command. Source of truth for the
//! visible Markdown skeletons is
//! `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`;
//! payload JSON skeletons match the structured payload schemas declared in
//! the same document.
//!
//! Templates are previews of the comment shape. They never carry a real
//! hidden `plan-issue-record-payload:hex:<>` payload carrier. The hidden
//! line is rendered as a `<!-- ... -->` placeholder so agents see where the
//! payload would land without producing a record-grade comment.

use crate::commands::record::RecordProfile;
use crate::lifecycle_record::PayloadRole;
use crate::lifecycle_vnext::registry::{self, RoleSpec};

/// Output format requested by the template preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateFormat {
    /// Visible Markdown skeleton (headings + placeholder text).
    Markdown,
    /// Payload JSON data skeleton (no hidden carrier).
    Json,
}

impl TemplateFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

/// Error emitted by [`render_template`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// Asked for a role/profile combination that the registry forbids.
    UnsupportedRoleProfile {
        role: PayloadRole,
        profile: RecordProfile,
    },
}

impl TemplateError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedRoleProfile { .. } => "record-template-unsupported-role-profile",
        }
    }
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedRoleProfile { role, profile } => {
                write!(
                    f,
                    "lifecycle role `{}` is not supported for profile `{}`",
                    role.as_str(),
                    profile.as_str()
                )
            }
        }
    }
}

impl std::error::Error for TemplateError {}

/// Render the template preview for the supplied role/profile in the
/// requested format.
pub fn render_template(
    profile: RecordProfile,
    role: PayloadRole,
    format: TemplateFormat,
) -> Result<String, TemplateError> {
    // Both lifecycle profiles share the same role inventory in v1; reject
    // role/profile combos that have not been declared compatible. Today
    // every role is allowed in both profiles, but the check is explicit so
    // future profile-only roles surface here rather than silently rendering.
    if !registry::all_roles().iter().any(|spec| spec.role == role) {
        return Err(TemplateError::UnsupportedRoleProfile { role, profile });
    }

    let spec = registry::role(role);
    let output = match format {
        TemplateFormat::Markdown => render_markdown(spec, profile),
        TemplateFormat::Json => render_json_skeleton(spec, profile),
    };
    Ok(output)
}

fn render_markdown(spec: &RoleSpec, profile: RecordProfile) -> String {
    let marker = format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->",
        role = spec.marker_role,
        profile = profile.as_str()
    );
    let payload_placeholder = format!(
        "<!-- plan-issue-record-payload:hex:<{role}.v1 payload hex bytes> -->",
        role = spec.marker_role
    );
    let body = role_visible_skeleton(spec.role);
    format!(
        "{marker}\n\n{heading}\n\n- Profile: {profile}\n{body}\n\n{payload_placeholder}\n",
        heading = spec.default_heading,
        profile = profile.as_str(),
    )
}

fn role_visible_skeleton(role: PayloadRole) -> &'static str {
    match role {
        PayloadRole::Source => {
            "- Path: `docs/plans/<slug>/<slug>-discussion-source.md`\n\
             - Commit: `<full-sha>`\n\
             - Summary: <one-line source summary>\n\
             - Snapshot mode: local committed Markdown\n\n\
             <details>\n<summary>Source snapshot</summary>\n\n\
             <verbatim source document content>\n\n</details>"
        }
        PayloadRole::Plan => {
            "- Path: `docs/plans/<slug>/<slug>-plan.md`\n\
             - Commit: `<full-sha>`\n\
             - Summary: <one-line plan summary>\n\
             - Snapshot mode: local committed Markdown\n\n\
             <details>\n<summary>Plan snapshot</summary>\n\n\
             <verbatim plan content>\n\n</details>"
        }
        PayloadRole::State => {
            "- Status: in-progress\n\
             - Target scope: <issue-backed scope>\n\
             - Current task: <task id or description>\n\
             - Next task: <next action>\n\
             - Last updated: YYYY-MM-DD\n\
             - Branch: <branch>\n\
             - PR: <owner/repo#number or pending>\n\
             - Source document: docs/plans/<slug>/<source-file>\n\
             - Plan document: docs/plans/<slug>/<slug>-plan.md\n\n\
             ## Task Ledger\n\n\
             <details>\n<summary>Show task ledger</summary>\n\n\
             | ID | Status | Task | Notes |\n\
             | --- | --- | --- | --- |\n\
             | 1.1 | in-progress | <task title> | <short note> |\n\
             | 1.2 | pending | <task title> |  |\n\n\
             </details>\n\n\
             ## Blockers\n\n\
             - <blocker or `None`>\n\n\
             ## Validation\n\n\
             | Command | Status | Evidence |\n\
             | --- | --- | --- |\n\
             | `<command>` | pass|fail|skipped | <path or URL> |"
        }
        PayloadRole::Session => {
            "- Summary: <one-line summary of work done in this session>\n\n\
             ### Highlights\n\n\
             - <meaningful implementation, investigation, or handoff note>\n\
             - <branch, PR, or issue-visible decision>\n\n\
             ### Links\n\n\
             - State: <latest state comment URL>\n\
             - PR: <PR URL or pending>\n\
             - Artifacts: <validation or evidence path>\n\n\
             ### Session Fields\n\n\
             - branch: <branch>\n\
             - pr: <owner/repo#number or pending>\n\
             - selected_task: <task id>"
        }
        PayloadRole::Validation => {
            "- Overall: pass|partial|fail\n\n\
             | Command | Status | Evidence |\n\
             | --- | --- | --- |\n\
             | `<exact command>` | pass|fail|skipped | <artifact path, URL, or short reason> |\n\n\
             ### Waivers\n\n\
             - `<command>`: <why it was not run or why failure is accepted>"
        }
        PayloadRole::Review => {
            "- Decision: approve|request-changes|comments-only\n\
             - Lenses: testing, maintainability\n\
             - Outcome comment: <provider comment URL or retained evidence path>\n\n\
             | ID | Severity | Disposition | Summary |\n\
             | --- | --- | --- | --- |\n\
             | F1 | major | fixed|residual|follow-up|deferred|no-action | <finding summary> |"
        }
        PayloadRole::Closeout => {
            "- Final status: complete\n\
             - Approver: <login or source>\n\
             - Approval: <comment URL or approval text>\n\
             - Final validation: <validation evidence URL>\n\
             - Notes: <optional closeout note>\n\n\
             | PR | Merge SHA | Checks | Required | Non-required failures |\n\
             | --- | --- | --- | --- | --- |\n\
             | <PR URL or ref> | <sha> | pass|fail|none | pass|fail|none (<count>) | none |"
        }
    }
}

fn render_json_skeleton(spec: &RoleSpec, profile: RecordProfile) -> String {
    let data = role_payload_skeleton(spec.role);
    let envelope = serde_json::json!({
        "schema": "plan-issue-record.payload.v2",
        "role": spec.marker_role,
        "profile": profile.as_str(),
        "data": data,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| envelope.to_string())
}

fn role_payload_skeleton(role: PayloadRole) -> serde_json::Value {
    use serde_json::json;
    match role {
        PayloadRole::Source | PayloadRole::Plan => json!({
            "path": "docs/plans/<slug>/<slug>-<source|plan>.md",
            "commit": "<full-sha>",
            "title": "<optional title>",
            "summary": "<optional summary>"
        }),
        PayloadRole::State => json!({
            "status": "in-progress|complete|blocked",
            "target_scope": "<issue-backed scope>",
            "current": "<current task or state>",
            "next_action": "<next task or unblock action>",
            "tasks": [
                {"id": "1.1", "status": "pending|in-progress|done|deferred", "title": "<task title>"}
            ],
            "prs": [
                {"ref": "owner/repo#123", "url": "<url>", "status": "open|merged|closed"}
            ],
            "blockers": ["<blocking fact>"],
            "links": {
                "source": "<url>",
                "plan": "<url>",
                "previous_state": "<url>"
            }
        }),
        PayloadRole::Session => json!({
            "summary": "<one-line summary>",
            "highlights": ["<short bullet>"],
            "links": {"state": "<url>", "pr": "<url>"}
        }),
        PayloadRole::Validation => json!({
            "overall": "pass|partial|fail",
            "commands": [
                {"command": "<exact command>", "status": "pass|fail|skipped", "evidence": "<optional path or URL>"}
            ],
            "waivers": [
                {"command": "<command>", "reason": "<reason>"}
            ]
        }),
        PayloadRole::Review => json!({
            "decision": "approve|request-changes|comments-only",
            "lenses": ["testing", "maintainability"],
            "findings": [
                {
                    "id": "F1",
                    "severity": "blocker|major|minor|nit",
                    "disposition": "fixed|residual|follow-up|deferred|no-action",
                    "summary": "<finding summary>"
                }
            ],
            "outcome_comment_url": "<optional URL>"
        }),
        PayloadRole::Closeout => json!({
            "final_status": "complete",
            "approval": {"comment_url": "<optional URL>", "approver": "<optional login>"},
            "linked_prs": [
                {
                    "ref": "owner/repo#123",
                    "url": "<optional URL>",
                    "merge_sha": "<sha>",
                    "checks": "pass|fail|none",
                    "required_state": "pass|fail|none",
                    "required_count": 0,
                    "non_required_failures": []
                }
            ],
            "non_required_check_override": {"reason": "<reason>", "observed_non_required_failures": []},
            "final_validation_url": "<optional URL>",
            "notes": "<optional note>"
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_template_includes_role_heading_and_marker() {
        for spec in registry::all_roles() {
            let out = render_template(RecordProfile::Tracking, spec.role, TemplateFormat::Markdown)
                .expect("render");
            assert!(
                out.contains("<!-- plan-issue-record:v2"),
                "role {:?} missing marker: {out}",
                spec.role
            );
            assert!(
                out.contains(spec.default_heading),
                "role {:?} missing heading {}: {out}",
                spec.role,
                spec.default_heading
            );
            // Payload placeholder must be a comment, not a fenced code block.
            assert!(
                !out.contains("```plan-issue-record-payload"),
                "role {:?} must not render a real payload carrier: {out}",
                spec.role
            );
            assert!(
                out.contains("<!-- plan-issue-record-payload:hex:"),
                "role {:?} missing payload placeholder: {out}",
                spec.role
            );
        }
    }

    #[test]
    fn json_template_includes_envelope_schema_and_role() {
        for spec in registry::all_roles() {
            let out = render_template(RecordProfile::Tracking, spec.role, TemplateFormat::Json)
                .expect("render");
            let value: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
            assert_eq!(value["schema"], "plan-issue-record.payload.v2");
            assert_eq!(value["role"], spec.marker_role);
            assert_eq!(value["profile"], "tracking");
            assert!(value["data"].is_object());
        }
    }
}
