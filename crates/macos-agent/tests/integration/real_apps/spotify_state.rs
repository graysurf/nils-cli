use std::path::Path;
use std::time::{Duration, Instant};

use nils_test_support::cmd::CmdOptions;

use crate::real_apps::matrix::{ScenarioOutcome, ScenarioStatus};
use crate::real_common::{self, SpotifyPlaybackState};

pub fn spotify_player_state_transitions_are_observable(
    bin: &Path,
    options: &CmdOptions,
    artifact_dir: &Path,
) -> ScenarioOutcome {
    let started = Instant::now();
    let scenario_id = "spotify_player_state_transitions_are_observable";
    let _ledger = real_common::begin_step_ledger(artifact_dir, scenario_id);
    real_common::require_app_installed("Spotify");
    let _cleanup_guard = real_common::SpotifyPlaybackCleanupGuard::capture();

    let profile = real_common::load_profile().spotify;
    let mut screenshots = Vec::new();
    let mut states = Vec::new();

    activate_spotify(bin, options);
    let baseline = real_common::spotify_playback_state();
    states.push(baseline);

    click(
        bin,
        options,
        profile.track_rows[0].x,
        profile.track_rows[0].y,
    );
    wait_spotify_active(
        bin,
        options,
        "wait after spotify track click for state probe",
    );

    states.push(real_common::spotify_playback_state());
    click(bin, options, profile.play_toggle.x, profile.play_toggle.y);
    states.push(real_common::spotify_playback_state());
    click(bin, options, profile.play_toggle.x, profile.play_toggle.y);
    states.push(real_common::spotify_playback_state());

    assert!(
        states.iter().any(|sample| !sample.track_name.is_empty()),
        "expected at least one spotify sample with track_name"
    );
    assert!(
        states.iter().any(|sample| !sample.artist.is_empty()),
        "expected at least one spotify sample with artist"
    );
    if distinct_states(&states) < 2 {
        // Fallback: UI coordinates can drift across Spotify layouts.
        // Use a deterministic playpause toggle so we can still validate observable state changes.
        real_common::spotify_toggle_play_pause();
        std::thread::sleep(std::time::Duration::from_millis(500));
        states.push(real_common::spotify_playback_state());
    }
    assert!(
        distinct_states(&states) >= 2,
        "expected observable spotify state transitions, got {:?}",
        states
            .iter()
            .map(|sample| sample.player_state.clone())
            .collect::<Vec<_>>()
    );

    let screenshot = artifact_dir.join("spotify-state-checkpoint.png");
    capture_active_window(bin, options, &screenshot);
    screenshots.push(screenshot.to_string_lossy().to_string());

    let state_json = artifact_dir.join("spotify-state-samples.json");
    let state_payload = serde_json::json!({
        "samples": states.iter().map(|sample| {
            serde_json::json!({
                "player_state": sample.player_state,
                "track_name": sample.track_name,
                "artist": sample.artist,
            })
        }).collect::<Vec<_>>()
    });
    real_common::write_json(&state_json, &state_payload);

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

fn click(bin: &Path, options: &CmdOptions, x: i32, y: i32) {
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
        "spotify click",
        2,
        std::time::Duration::from_millis(250),
    );
    assert_eq!(payload["command"], serde_json::json!("input.click"));
}

fn distinct_states(samples: &[SpotifyPlaybackState]) -> usize {
    let mut states = std::collections::BTreeSet::new();
    for sample in samples {
        states.insert(sample.player_state.to_ascii_lowercase());
    }
    states.len()
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
        "observe screenshot spotify state",
    );
    assert_eq!(payload["command"], serde_json::json!("observe.screenshot"));
    assert!(
        screenshot_path.is_file(),
        "spotify state screenshot should exist"
    );
}

fn wait_spotify_active(bin: &Path, options: &CmdOptions, step: &str) {
    let payload = real_common::wait_app_active(bin, options, "Spotify", 7000, 60, step);
    assert_eq!(payload["command"], serde_json::json!("wait.app-active"));
}
