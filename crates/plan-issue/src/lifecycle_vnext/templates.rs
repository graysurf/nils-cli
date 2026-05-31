//! Deterministic template preview for lifecycle roles.
//!
//! Backs the `plan-issue record template` command. Source of truth for the
//! visible Markdown skeletons is
//! `crates/plan-issue/docs/specs/issue-backed-plan-record-contract-v2.md`;
//! payload JSON skeletons match the structured payload schemas declared in
//! the same document.
//!
//! Templates are previews of the comment shape. They never carry a real
//! hidden `plan-issue-record-payload:hex:<>` payload carrier. The hidden
//! line is rendered as a `<!-- ... -->` placeholder so agents see where the
//! payload would land without producing a record-grade comment.

use nils_markdown::Engine;
use serde::Serialize;

use crate::commands::record::RecordProfile;
use crate::lifecycle_record::PayloadRole;
use crate::lifecycle_vnext::registry::{self, RoleSpec};

const LIFECYCLE_VNEXT_TEMPLATE: &str = include_str!("../../templates/lifecycle_vnext.md.tera");
const LIFECYCLE_VNEXT_TEMPLATE_NAME: &str = "lifecycle_vnext";

#[derive(Debug, Clone, Serialize)]
struct LifecycleVnextView<'a> {
    role: &'a str,
    marker_role: &'a str,
    profile: &'a str,
    heading: &'a str,
}

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
    let view = LifecycleVnextView {
        role: spec.role.as_str(),
        marker_role: spec.marker_role,
        profile: profile.as_str(),
        heading: spec.default_heading,
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(LIFECYCLE_VNEXT_TEMPLATE_NAME, LIFECYCLE_VNEXT_TEMPLATE)
        .expect("lifecycle_vnext template registers");
    engine
        .render(LIFECYCLE_VNEXT_TEMPLATE_NAME, &view)
        .expect("lifecycle_vnext template renders")
}

fn render_json_skeleton(spec: &RoleSpec, profile: RecordProfile) -> String {
    // This preview reflects the active payload contract only. When the state
    // payload is replaced, update the skeleton instead of preserving a v2
    // compatibility preview.
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
                {"id": "1.1", "status": "pending|in-progress|done|deferred|blocked|waived", "title": "<task title>"},
                {"id": "1.2", "status": "pending|in-progress|done|deferred|blocked|waived", "title": "<accumulative: full per-task ledger>"}
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
