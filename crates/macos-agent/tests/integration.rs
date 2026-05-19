// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/ax_extended.rs"]
mod ax_extended;
#[path = "integration/cli_smoke.rs"]
mod cli_smoke;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/contracts.rs"]
mod contracts;
#[path = "integration/e2e_real_apps.rs"]
mod e2e_real_apps;
#[path = "integration/e2e_real_macos.rs"]
mod e2e_real_macos;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
#[path = "integration/input_click.rs"]
mod input_click;
#[path = "integration/input_keyboard.rs"]
mod input_keyboard;
#[path = "integration/list_commands.rs"]
mod list_commands;
#[path = "integration/observe_screenshot.rs"]
mod observe_screenshot;
#[path = "integration/preflight.rs"]
mod preflight;
#[path = "integration/preflight_probes.rs"]
mod preflight_probes;
#[path = "integration/profile.rs"]
mod profile;
#[path = "integration/real_apps/mod.rs"]
pub mod real_apps;
#[path = "integration/real_common.rs"]
pub mod real_common;
#[path = "integration/retry.rs"]
mod retry;
#[path = "integration/scenario_chain.rs"]
mod scenario_chain;
#[path = "integration/wait.rs"]
mod wait;
#[path = "integration/window_activate.rs"]
mod window_activate;
