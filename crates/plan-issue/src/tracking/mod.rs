//! vNext tracking controller boundary.
//!
//! Design references:
//!
//! - `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`
//!   (run-state schema, event journal, FSM, reconciliation, command surface)
//! - `docs/source/plan-issue-redesign/plan-tracking-issue-workflow-v1.md`
//!   (workflow states and timing rules)
//!
//! Module map:
//!
//! - [`run_state`] — typed `plan-issue.execution-run.v1` schema and on-disk
//!   helpers (`run-state.json`).
//! - [`events`] — append-only `plan-issue.execution-event.v1` journal
//!   (`events.jsonl`).
//! - [`fsm`] — deterministic state machine over the lifecycle evidence.
//! - [`reconcile`] — combines provider evidence, plan bundle, run state, and
//!   events into a reconciled view that the FSM evaluates.
//! - [`checkpoint`] — dry-run and live checkpoint behavior built on top of
//!   the lifecycle renderer + visible-lint surfaces.
//! - [`close_ready`] — non-mutating close-readiness probe that mirrors the
//!   `record close` strict gate.
//!
//! The boundary is intentionally narrow: low-level provider IO and
//! Markdown rendering are reused from [`crate::lifecycle_record`] and
//! [`crate::lifecycle_vnext`]; this module owns local execution state,
//! reconciliation, and the high-level commands.

pub mod checkpoint;
pub mod close_ready;
pub mod events;
pub mod fsm;
pub mod reconcile;
pub mod run_state;
