use std::path::Path;
use std::time::{Duration, Instant};

use nils_test_support::cmd::CmdOptions;

use crate::real_apps::matrix::{ScenarioOutcome, ScenarioStatus};
use crate::real_common;

pub const FINDER_SCENARIO_ID: &str = "finder_navigation_and_state_checks";

pub fn finder_navigation_and_state_checks(
    bin: &Path,
    options: &CmdOptions,
    artifact_dir: &Path,
) -> ScenarioOutcome {
    let started = Instant::now();
    let _ledger = real_common::begin_step_ledger(artifact_dir, FINDER_SCENARIO_ID);
    let mut cleanup_guard = real_common::FinderWindowCleanupGuard::new();
    let screenshot_path = artifact_dir.join("finder-active-window.png");
    let screenshot_path_text = screenshot_path.to_string_lossy().to_string();

    let preflight = real_common::run_json_step(
        bin,
        options,
        &["--format", "json", "preflight"],
        "preflight",
    );
    assert_eq!(preflight["command"], serde_json::json!("preflight"));
    real_common::require_preflight_ready(&preflight, &["accessibility", "automation"]);

    let activate = real_common::activate_app_with_retry(
        bin,
        options,
        "Finder",
        1800,
        8_000,
        2,
        Duration::from_millis(350),
    );
    assert_eq!(activate["command"], serde_json::json!("window.activate"));
    assert_eq!(
        activate["result"]["selected_app"],
        serde_json::json!("Finder")
    );

    let open_new_window = real_common::run_json_step(
        bin,
        options,
        &[
            "--format", "json", "input", "hotkey", "--mods", "cmd", "--key", "n",
        ],
        "input.hotkey cmd+n",
    );
    assert_eq!(
        open_new_window["command"],
        serde_json::json!("input.hotkey")
    );
    cleanup_guard.arm();

    let wait_present = real_common::run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "wait",
            "window-present",
            "--app",
            "Finder",
            "--timeout-ms",
            "7000",
            "--poll-ms",
            "50",
        ],
        "wait.window-present Finder",
    );
    assert_eq!(
        wait_present["command"],
        serde_json::json!("wait.window-present")
    );

    let wait_active = real_common::run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "wait",
            "app-active",
            "--app",
            "Finder",
            "--timeout-ms",
            "7000",
            "--poll-ms",
            "50",
        ],
        "wait.app-active Finder after opening window",
    );
    assert_eq!(wait_active["command"], serde_json::json!("wait.app-active"));

    let finder_profile = real_common::load_profile().finder;
    let focus_x = finder_profile.window_focus.x.to_string();
    let focus_y = finder_profile.window_focus.y.to_string();

    let click_focus = real_common::run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "input",
            "click",
            "--x",
            &focus_x,
            "--y",
            &focus_y,
            "--pre-wait-ms",
            "80",
            "--post-wait-ms",
            "120",
        ],
        "input.click Finder focus",
    );
    assert_eq!(click_focus["command"], serde_json::json!("input.click"));

    let go_home = real_common::run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "input",
            "hotkey",
            "--mods",
            "cmd,shift",
            "--key",
            "h",
        ],
        "input.hotkey cmd+shift+h",
    );
    assert_eq!(go_home["command"], serde_json::json!("input.hotkey"));

    let recheck_present = real_common::run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "wait",
            "window-present",
            "--app",
            "Finder",
            "--timeout-ms",
            "7000",
            "--poll-ms",
            "50",
        ],
        "wait.window-present Finder after actions",
    );
    assert_eq!(
        recheck_present["command"],
        serde_json::json!("wait.window-present")
    );

    let observe_args = vec![
        "--format",
        "json",
        "observe",
        "screenshot",
        "--active-window",
        "--path",
        &screenshot_path_text,
    ];
    let observe = real_common::run_json_step(
        bin,
        options,
        &observe_args,
        "observe.screenshot active Finder window",
    );
    assert_eq!(observe["command"], serde_json::json!("observe.screenshot"));
    assert_eq!(
        observe["result"]["path"],
        serde_json::json!(screenshot_path_text)
    );
    assert!(screenshot_path.is_file(), "screenshot should exist");

    let windows = real_common::run_json_step(
        bin,
        options,
        &["--format", "json", "windows", "list", "--app", "Finder"],
        "windows.list Finder",
    );
    assert_eq!(windows["command"], serde_json::json!("windows.list"));
    let rows = windows["result"]["windows"]
        .as_array()
        .expect("windows list should be an array");
    assert!(!rows.is_empty(), "Finder windows list should not be empty");

    let (step_ledger_path, failing_step_id, last_successful_step_id) =
        real_common::current_step_ledger_snapshot();

    ScenarioOutcome {
        scenario_id: FINDER_SCENARIO_ID.to_string(),
        status: ScenarioStatus::Passed,
        elapsed_ms: started.elapsed().as_millis() as u64,
        artifact_dir: artifact_dir.display().to_string(),
        screenshots: vec![screenshot_path_text],
        step_ledger_path,
        skip_reason: None,
        failing_step_id,
        last_successful_step_id,
    }
}
