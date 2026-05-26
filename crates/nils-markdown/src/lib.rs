//! Shared Markdown template layer for the nils-cli workspace.
//!
//! Markdown artifacts emitted by workspace CLIs (issue bodies, lifecycle
//! comments, dashboards, retros, heuristic-system entries) are produced
//! by `.md.tera` templates rendered against flat
//! [`serde::Serialize`] view structs. Consumers prepare the view in Rust
//! and let the template own layout; templates never reach into the
//! consumer's domain.
//!
//! Sprint 1 of `docs/plans/markdown-render-template-layer/` populates
//! this crate with the engine builder, the `md_cell` Tera filter, the
//! `nils_common::markdown` re-export bridge, the `register_helper`
//! extension point that consumer crates plug their own domain helpers
//! into, and a byte-equality `assert_render` test harness. Sprint 3
//! lands the `md-render` binary that drives the same engine over JSON
//! input under the `bin-cli` Cargo feature.
