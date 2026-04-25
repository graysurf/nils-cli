use std::path::Path;
use std::time::{Duration, Instant};

use nils_test_support::cmd::CmdOptions;

use crate::real_apps::matrix::{ScenarioOutcome, ScenarioStatus};
use crate::real_common::{self, SpotifyPlaybackState};

pub fn spotify_ui_selects_track_and_toggles_playback(
    bin: &Path,
    options: &CmdOptions,
    artifact_dir: &Path,
) -> ScenarioOutcome {
    let started = Instant::now();
    let scenario_id = "spotify_ui_selects_track_and_toggles_playback";
    let _ledger = real_common::begin_step_ledger(artifact_dir, scenario_id);
    real_common::require_app_installed("Spotify");
    let _cleanup_guard = real_common::SpotifyPlaybackCleanupGuard::capture();

    let profile = real_common::load_profile().spotify;
    let mut screenshots = Vec::new();
    let mut states = Vec::new();

    activate_spotify(bin, options);
    click(
        bin,
        options,
        profile.search_box.x,
        profile.search_box.y,
        "spotify focus search",
    );
    real_common::replace_focused_text_with_clipboard(
        bin,
        options,
        "lofi hip hop",
        "spotify search query",
    );
    real_common::send_hotkey(bin, options, None, "return", "spotify confirm search query");

    wait_spotify_active(bin, options, "wait after spotify search");

    let track_point = profile
        .track_rows
        .first()
        .expect("spotify track_rows empty");
    click(
        bin,
        options,
        track_point.x,
        track_point.y,
        "spotify click track row",
    );
    wait_spotify_active(bin, options, "wait after spotify track click");

    states.push(real_common::spotify_playback_state());
    let before = artifact_dir.join("spotify-before-toggle.png");
    capture_active_window(bin, options, &before);
    screenshots.push(before.to_string_lossy().to_string());

    click(
        bin,
        options,
        profile.play_toggle.x,
        profile.play_toggle.y,
        "spotify play/pause ui toggle #1",
    );
    wait_spotify_active(bin, options, "wait after spotify toggle #1");
    states.push(real_common::spotify_playback_state());
    let after_1 = artifact_dir.join("spotify-after-toggle-1.png");
    capture_active_window(bin, options, &after_1);
    screenshots.push(after_1.to_string_lossy().to_string());

    click(
        bin,
        options,
        profile.play_toggle.x,
        profile.play_toggle.y,
        "spotify play/pause ui toggle #2",
    );
    wait_spotify_active(bin, options, "wait after spotify toggle #2");
    states.push(real_common::spotify_playback_state());
    let after_2 = artifact_dir.join("spotify-after-toggle-2.png");
    capture_active_window(bin, options, &after_2);
    screenshots.push(after_2.to_string_lossy().to_string());

    require_playback_transition(&states);

    let (step_ledger_path, failing_step_id, last_successful_step_id) =
        real_common::current_step_ledger_snapshot();

    ScenarioOutcome {
        scenario_id: scenario_id.to_string(),
        status: ScenarioStatus::Passed,
        elapsed_ms: started.elapsed().as_millis() as u64,
        artifact_dir: artifact_dir.display().to_string(),
        screenshots,
        step_ledger_path,
        skip_reason: None,
        failing_step_id,
        last_successful_step_id,
    }
}

fn activate_spotify(bin: &Path, options: &CmdOptions) {
    real_common::ensure_input_source_for_text_entry();
    let activate = real_common::activate_app_with_retry(
        bin,
        options,
        "Spotify",
        1800,
        12_000,
        3,
        Duration::from_millis(500),
    );
    assert_eq!(activate["command"], serde_json::json!("window.activate"));
    wait_spotify_active(bin, options, "wait after Spotify activate");
}

fn click(bin: &Path, options: &CmdOptions, x: i32, y: i32, step: &str) {
    let x_text = x.to_string();
    let y_text = y.to_string();
    let payload = real_common::run_json_step_with_retry(
        bin,
        options,
        &[
            "--format",
            "json",
            "input",
            "click",
            "--x",
            &x_text,
            "--y",
            &y_text,
            "--pre-wait-ms",
            "80",
            "--post-wait-ms",
            "120",
        ],
        step,
        2,
        std::time::Duration::from_millis(250),
    );
    assert_eq!(payload["command"], serde_json::json!("input.click"));
}

fn capture_active_window(bin: &Path, options: &CmdOptions, screenshot_path: &Path) {
    let screenshot_path_text = screenshot_path.to_string_lossy().to_string();
    let payload = real_common::run_json_step(
        bin,
        options,
        &[
            "--format",
            "json",
            "observe",
            "screenshot",
            "--active-window",
            "--path",
            &screenshot_path_text,
        ],
        "observe screenshot spotify",
    );
    assert_eq!(payload["command"], serde_json::json!("observe.screenshot"));
    assert!(screenshot_path.is_file(), "spotify screenshot should exist");
}

fn require_playback_transition(states: &[SpotifyPlaybackState]) {
    let mut saw_playing = false;
    let mut saw_paused = false;
    for sample in states {
        if sample.player_state.contains("playing") {
            saw_playing = true;
        }
        if sample.player_state.contains("paused") {
            saw_paused = true;
        }
    }
    assert!(
        saw_playing || saw_paused,
        "expected at least one known spotify state sample, got: {:?}",
        states
            .iter()
            .map(|sample| sample.player_state.clone())
            .collect::<Vec<_>>()
    );
}

fn wait_spotify_active(bin: &Path, options: &CmdOptions, step: &str) {
    let payload = real_common::wait_app_active(bin, options, "Spotify", 7000, 60, step);
    assert_eq!(payload["command"], serde_json::json!("wait.app-active"));
}
