//! Adapter opt-outs for the canonical init-prompt snapshot mechanism.
//!
//! Runtime adapters (such as Claude Code) that ship their own
//! role/protocol prompts can set `PLAN_ISSUE_SKIP_INIT_SNAPSHOT=1` to
//! make the binary skip both the existence check on
//! `<AGENT_HOME>/prompts/plan-issue-delivery-{main,subagent}-init.md`
//! and the matching `.snapshot.md` copy in per-sprint workspaces.
//!
//! The codex / opencode adapters keep their current behaviour and
//! must not set this var.
use nils_common::env as common_env;

pub const PLAN_ISSUE_SKIP_INIT_SNAPSHOT_ENV: &str = "PLAN_ISSUE_SKIP_INIT_SNAPSHOT";

/// Returns `true` when `PLAN_ISSUE_SKIP_INIT_SNAPSHOT` is set to a
/// non-empty value, signalling that the caller wants the binary to
/// skip the init-prompt snapshot mechanism end-to-end (no existence
/// check on the source, no copy into the runtime workspace).
///
/// This is the single read of the env var inside the crate; other
/// modules must call this helper instead of touching the env directly.
pub fn should_skip_init_snapshot() -> bool {
    common_env::env_non_empty(PLAN_ISSUE_SKIP_INIT_SNAPSHOT_ENV).is_some()
}
