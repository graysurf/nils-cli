// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/audit_drift_allowlist.rs"]
mod audit_drift_allowlist;
#[path = "integration/audit_drift_classes.rs"]
mod audit_drift_classes;
#[path = "integration/audit_drift_extra_intentional.rs"]
mod audit_drift_extra_intentional;
#[path = "integration/audit_drift_unsafe_score.rs"]
mod audit_drift_unsafe_score;
#[path = "integration/cli.rs"]
mod cli;
#[path = "integration/determinism_gate.rs"]
mod determinism_gate;
#[path = "integration/doctor_filesystem.rs"]
mod doctor_filesystem;
#[path = "integration/doctor_skill_surface.rs"]
mod doctor_skill_surface;
#[path = "integration/doctor_upgrade.rs"]
mod doctor_upgrade;
#[path = "integration/doctor_version.rs"]
mod doctor_version;
#[path = "integration/doctor_version_alignment.rs"]
mod doctor_version_alignment;
#[path = "integration/gc_backups.rs"]
mod gc_backups;
#[path = "integration/install_flags.rs"]
mod install_flags;
#[path = "integration/install_pipeline.rs"]
mod install_pipeline;
#[path = "integration/list_skills.rs"]
mod list_skills;
#[path = "integration/managed_block.rs"]
mod managed_block;
#[path = "integration/pr_body.rs"]
mod pr_body;
#[path = "integration/prune_stale.rs"]
mod prune_stale;
#[path = "integration/purge_state.rs"]
mod purge_state;
#[path = "integration/render.rs"]
mod render;
#[path = "integration/render_determinism.rs"]
mod render_determinism;
#[path = "integration/restore_backups.rs"]
mod restore_backups;
#[path = "integration/uninstall.rs"]
mod uninstall;
