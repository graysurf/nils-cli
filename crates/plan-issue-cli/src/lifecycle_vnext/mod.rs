//! vNext lifecycle implementation boundary for the plan-issue record
//! workflow.
//!
//! Design references:
//!
//! - `docs/source/plan-issue-redesign/plan-tracking-issue-cli-redesign-v1.md`
//!   (Workstreams 0–6 and the rewrite boundary)
//! - `docs/source/plan-issue-redesign/plan-tracking-issue-comment-taxonomy-v1.md`
//!   (canonical lifecycle role list, visible templates, posting policies)
//!
//! Module map:
//!
//! - [`registry`] enumerates the seven lifecycle roles and the metadata each
//!   role carries (heading, payload schema, direct-post permission, closeout
//!   ownership).
//! - [`templates`] renders the visible Markdown and JSON skeletons used by the
//!   `record template` preview command.
//! - [`visible_lint`] enforces visible-completeness rules for rendered
//!   lifecycle comments (Task Ledger heading, validation status,
//!   review decision, session summary, closeout approval, …).
//! - [`payloads`] exposes typed payload helpers built on top of the legacy
//!   [`crate::lifecycle_record`] data types until the catch-all
//!   `lifecycle_record.rs` is fully migrated.
//! - [`render`] is the registry-driven rendering surface used by `record post`
//!   and the upcoming `tracking checkpoint` macro.
//!
//! The module deliberately keeps the public binary shell (`plan-issue` /
//! `plan-issue-local`), the global output envelope, exit-code conventions,
//! provider abstraction, and the existing `record` commands behaving the same
//! way during the rewrite. Old internals continue to live in
//! [`crate::lifecycle_record`] until Task 6.3 migrates them onto this
//! registry.

pub mod payloads;
pub mod registry;
pub mod render;
pub mod templates;
pub mod visible_lint;
