//! Registry-driven lifecycle comment renderer.
//!
//! Task 1.1 introduces the boundary; Tasks 3.2 and 6.3 migrate the real
//! Markdown assembly from [`crate::lifecycle_record`] onto this surface so
//! `record post`, `record open`, `record attach`, and `tracking checkpoint`
//! all share one rendering engine per role.

use crate::lifecycle_record::{self, CommentInput, PayloadRole};
use crate::lifecycle_vnext::registry::{self, RoleSpec};

/// Outcome of rendering one lifecycle comment.
#[derive(Debug, Clone)]
pub struct RenderedComment {
    pub role: PayloadRole,
    pub spec: &'static RoleSpec,
    pub body: String,
}

/// Render a lifecycle comment for the given [`CommentInput`].
///
/// Until Task 6.3 migrates the renderer wholesale, this adapter delegates to
/// the existing [`crate::lifecycle_record::render_comment`] implementation
/// while exposing the vNext-shaped result. Errors are surfaced verbatim so
/// upstream callers see the same stable strings.
pub fn render(input: CommentInput) -> Result<RenderedComment, String> {
    let role = derive_role(&input);
    let spec = registry::role(role);
    let body = lifecycle_record::render_comment(input)?;
    Ok(RenderedComment { role, spec, body })
}

fn derive_role(input: &CommentInput) -> PayloadRole {
    use crate::commands::record::LifecycleCommentKind;
    match input.kind {
        LifecycleCommentKind::Source => PayloadRole::Source,
        LifecycleCommentKind::Plan => PayloadRole::Plan,
        LifecycleCommentKind::State => PayloadRole::State,
        LifecycleCommentKind::Session => PayloadRole::Session,
        LifecycleCommentKind::Validation => PayloadRole::Validation,
        LifecycleCommentKind::Review => PayloadRole::Review,
        LifecycleCommentKind::Closeout => PayloadRole::Closeout,
    }
}
