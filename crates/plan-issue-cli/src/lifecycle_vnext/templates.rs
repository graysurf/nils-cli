//! Deterministic template preview for lifecycle roles.
//!
//! Backs the upcoming `plan-issue record template` command. Skeleton lives in
//! Task 1.1; the full Markdown + JSON skeleton output is implemented in
//! Task 3.1 against
//! `docs/source/plan-issue-redesign/plan-tracking-issue-cli-redesign-v1.md`
//! Workstream 3 and
//! `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`.
//!
//! The renderer must be a pure function over [`super::registry::RoleSpec`].
//! It never emits a real hidden payload carrier — templates are previews of
//! the comment shape, not lifecycle records.

use crate::commands::record::RecordProfile;
use crate::lifecycle_record::PayloadRole;
use crate::lifecycle_vnext::registry;

/// Output format requested by the template preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateFormat {
    /// Visible Markdown skeleton (headings + placeholder text).
    Markdown,
    /// Payload JSON data skeleton (no hidden carrier).
    Json,
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

/// Render the template preview for the supplied role/profile in the requested
/// format.
///
/// Implementation lands in Task 3.1; the Task 1.1 skeleton returns a stable
/// "not yet implemented" payload so consumers can wire CLI plumbing without
/// blocking on the visible/JSON skeletons.
pub fn render_template(
    profile: RecordProfile,
    role: PayloadRole,
    format: TemplateFormat,
) -> Result<String, TemplateError> {
    let _spec = registry::role(role);
    // Task 3.1 fills in Markdown + JSON skeletons; this stub keeps the
    // module path callable so the CLI surface can be added in parallel.
    let payload = serde_json::json!({
        "status": "not-implemented",
        "task": "3.1",
        "profile": profile.as_str(),
        "role": role.as_str(),
        "format": match format {
            TemplateFormat::Markdown => "markdown",
            TemplateFormat::Json => "json",
        },
    });
    Ok(payload.to_string())
}
