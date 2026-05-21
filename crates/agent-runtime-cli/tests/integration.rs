// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/audit_drift_classes.rs"]
mod audit_drift_classes;
#[path = "integration/cli.rs"]
mod cli;
#[path = "integration/determinism_gate.rs"]
mod determinism_gate;
#[path = "integration/doctor_filesystem.rs"]
mod doctor_filesystem;
#[path = "integration/doctor_upgrade.rs"]
mod doctor_upgrade;
#[path = "integration/doctor_version.rs"]
mod doctor_version;
#[path = "integration/gc_backups.rs"]
mod gc_backups;
#[path = "integration/install_flags.rs"]
mod install_flags;
#[path = "integration/install_pipeline.rs"]
mod install_pipeline;
#[path = "integration/managed_block.rs"]
mod managed_block;
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
