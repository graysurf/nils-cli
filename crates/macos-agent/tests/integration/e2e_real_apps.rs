use std::path::Path;

use nils_test_support::bin::resolve;
use nils_test_support::{EnvGuard, GlobalStateLock};
use tempfile::TempDir;

use crate::real_apps;
use crate::real_common;
use real_apps::matrix::{
    ScenarioOutcome, ScenarioStatus, artifact_index_has_required_fields, classify_base_vs_extended,
    selected_apps_from_env, subset_selection_matches, summarize_soak,
    summary_has_base_extended_separation, write_artifact_index,
};
use real_apps::{
    ArcYoutubeProfile, FINDER_SCENARIO_ID, arc_youtube_multi_video_play_pause_and_comments,
    arc_youtube_opens_home_and_clicks_three_tiles, arc_youtube_play_pause_and_comment_checkpoint,
    cross_app_arc_spotify_focus_and_state_recovery, finder_navigation_and_state_checks,
    spotify_player_state_transitions_are_observable, spotify_ui_selects_track_and_toggles_playback,
};

#[test]
fn real_common_profile_loader_and_artifact_paths_are_deterministic() {
    let profile = real_common::load_profile();
    assert_eq!(profile.profile_name, real_common::selected_profile_name());
    assert!(profile.arc.video_tiles.len() >= 3);
    assert!(!profile.spotify.track_rows.is_empty());

    let first = real_common::create_artifact_dir("real-common-determinism");
    let second = real_common::create_artifact_dir("real-common-determinism");
    assert!(first.exists(), "first artifact dir should exist");
    assert!(second.exists(), "second artifact dir should exist");
    assert_ne!(first, second, "artifact dirs should be unique per call");
    assert!(
        first.to_string_lossy().contains("macos-agent-e2e"),
        "artifact path should include macos-agent-e2e namespace"
    );
}

#[test]
fn real_e2e_contract_enforces_skip_vs_fail_policy() {
    let lock = GlobalStateLock::new();

    let _remove_apps = EnvGuard::remove(&lock, "MACOS_AGENT_REAL_E2E_APPS");
    assert!(real_common::app_selected("arc"));
    assert!(real_common::app_selected("spotify"));
    assert!(real_common::app_selected("finder"));

    let _apps = EnvGuard::set(&lock, "MACOS_AGENT_REAL_E2E_APPS", "arc,finder");
    assert!(real_common::app_selected("arc"));
    assert!(real_common::app_selected("finder"));
    assert!(!real_common::app_selected("spotify"));

    let invalid = real_common::validate_selected_apps_raw("arc,mail");
    assert!(
        invalid.is_err(),
        "unsupported app selections should fail with actionable diagnostics"
    );
}

#[test]
fn real_e2e_foundation_reports_preflight_and_skip_reasons() {
    if let Some(reason) = real_common::app_gate_reason("finder", false) {
        eprintln!("SKIP[real_e2e_foundation_reports_preflight_and_skip_reasons]: {reason}");
        return;
    }

    if !real_common::real_e2e_enabled() {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let preflight = real_common::run_json_step(
        &bin,
        &options,
        &["--format", "json", "preflight"],
        "preflight",
    );
    assert_eq!(preflight["command"], serde_json::json!("preflight"));
    let checks = preflight["result"]["checks"]
        .as_array()
        .expect("preflight checks should be array");
    for id in ["accessibility", "automation"] {
        let check = checks
            .iter()
            .find(|check| check["id"] == serde_json::json!(id))
            .unwrap_or_else(|| panic!("missing preflight check `{id}`"));
        let status = check["status"].as_str().unwrap_or("");
        assert!(["ok", "warn", "fail"].contains(&status));
    }
}

#[test]
fn real_e2e_foundation_collects_artifacts() {
    if !should_run("finder") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("foundation-finder");
    let outcome = finder_navigation_and_state_checks(&bin, &options, &artifact_dir);
    assert_eq!(outcome.status, ScenarioStatus::Passed);
    assert!(
        !outcome.screenshots.is_empty(),
        "foundation finder flow should create screenshots"
    );
}

#[test]
fn finder_navigation_and_state_checks_test() {
    if !should_run("finder") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("finder-navigation");
    let outcome = finder_navigation_and_state_checks(&bin, &options, &artifact_dir);
    assert_eq!(outcome.scenario_id, FINDER_SCENARIO_ID);
    assert_eq!(outcome.status, ScenarioStatus::Passed);
    assert!(
        !outcome.screenshots.is_empty(),
        "finder scenario should produce screenshots"
    );
}

#[test]
fn arc_youtube_opens_home_and_clicks_three_tiles_test() {
    if !should_run("arc") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("arc-open-and-click");
    let profile = ArcYoutubeProfile::from_default_profile();
    let outcome =
        arc_youtube_opens_home_and_clicks_three_tiles(&bin, &options, &artifact_dir, &profile);
    assert_eq!(
        outcome.scenario_id,
        "arc_youtube_opens_home_and_clicks_three_tiles"
    );
    assert_eq!(outcome.status, ScenarioStatus::Passed);
    assert!(
        outcome.screenshots.len() >= 3,
        "expected at least 3 screenshot checkpoints"
    );
}

#[test]
fn arc_youtube_play_pause_and_comment_checkpoint_test() {
    if !should_run("arc") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("arc-play-pause-comment");
    let profile = ArcYoutubeProfile::from_default_profile();
    let outcome = arc_youtube_play_pause_and_comment_checkpoint(
        &bin,
        &options,
        &artifact_dir,
        &profile,
        &[0, 1],
    );
    assert_eq!(
        outcome.scenario_id,
        "arc_youtube_play_pause_and_comment_checkpoint"
    );
    assert_eq!(outcome.status, ScenarioStatus::Passed);
    assert!(
        outcome.screenshots.len() >= 2,
        "expected playback and comment checkpoints"
    );
}

#[test]
fn arc_youtube_multi_video_play_pause_and_comments_test() {
    if !should_run("arc") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("arc-reliability");
    let profile = ArcYoutubeProfile::from_default_profile();
    let outcome =
        arc_youtube_multi_video_play_pause_and_comments(&bin, &options, &artifact_dir, &profile);
    assert_eq!(
        outcome.scenario_id,
        "arc_youtube_multi_video_play_pause_and_comments"
    );
    assert_eq!(outcome.status, ScenarioStatus::Passed);
    assert!(
        outcome.screenshots.len() >= 6,
        "expected multi-video screenshot evidence"
    );
}

#[test]
fn spotify_ui_selects_track_and_toggles_playback_test() {
    if !should_run("spotify") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("spotify-ui");
    let outcome = spotify_ui_selects_track_and_toggles_playback(&bin, &options, &artifact_dir);
    assert_eq!(
        outcome.scenario_id,
        "spotify_ui_selects_track_and_toggles_playback"
    );
    assert_eq!(outcome.status, ScenarioStatus::Passed);
}

#[test]
fn spotify_player_state_transitions_are_observable_test() {
    if !should_run("spotify") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("spotify-state");
    let outcome = spotify_player_state_transitions_are_observable(&bin, &options, &artifact_dir);
    assert_eq!(
        outcome.scenario_id,
        "spotify_player_state_transitions_are_observable"
    );
    assert_eq!(outcome.status, ScenarioStatus::Passed);
}

#[test]
fn cross_app_arc_spotify_focus_and_state_recovery_test() {
    if !should_run("arc") || !should_run("spotify") {
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let artifact_dir = real_common::create_artifact_dir("cross-app-arc-spotify");
    let outcome = cross_app_arc_spotify_focus_and_state_recovery(&bin, &options, &artifact_dir);
    assert_eq!(
        outcome.scenario_id,
        "cross_app_arc_spotify_focus_and_state_recovery"
    );
    assert_eq!(outcome.status, ScenarioStatus::Passed);
}

#[test]
fn matrix_runner_supports_app_subset_selection() {
    assert!(subset_selection_matches(
        None,
        &["arc", "spotify", "finder"]
    ));
    assert!(subset_selection_matches(
        Some("spotify,finder,arc"),
        &["arc", "spotify", "finder"]
    ));
    assert!(subset_selection_matches(Some("spotify"), &["spotify"]));
}

#[test]
fn matrix_runner_emits_artifact_index_with_required_fields() {
    let root = TempDir::new().expect("tempdir");
    let index_path = root.path().join("artifact-index.json");
    let scenarios = vec![
        sample_outcome("finder_navigation_and_state_checks"),
        sample_outcome("cross_app_arc_spotify_focus_and_state_recovery"),
    ];
    let index = write_artifact_index(&index_path, &scenarios).expect("write artifact index");
    assert!(index_path.is_file(), "artifact index should be persisted");
    assert!(artifact_index_has_required_fields(&index));
}

#[test]
fn matrix_runner_reports_base_and_extended_scenarios_separately() {
    let scenarios = vec![
        sample_outcome("finder_navigation_and_state_checks"),
        sample_outcome("cross_app_arc_spotify_focus_and_state_recovery"),
    ];
    let summary = classify_base_vs_extended(&scenarios);
    assert_eq!(summary.base.total, 1);
    assert_eq!(summary.extended.total, 1);
    assert!(summary_has_base_extended_separation(&summary));
}

#[test]
fn matrix_runner_supports_app_subset_selection_real() {
    if let Some(reason) = real_common::app_gate_reason("finder", true) {
        eprintln!("SKIP[matrix_runner_supports_app_subset_selection_real]: {reason}");
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = real_common::base_options(cwd.path());
    let root = real_common::create_artifact_dir("matrix-run");

    let selected =
        selected_apps_from_env(std::env::var("MACOS_AGENT_REAL_E2E_APPS").ok().as_deref());
    let iterations = std::env::var("MACOS_AGENT_REAL_E2E_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);

    let mut outcomes = Vec::new();
    for _ in 0..iterations {
        outcomes.extend(run_selected_real_scenarios(
            &bin, &options, &root, &selected,
        ));
    }
    assert!(
        !outcomes.is_empty(),
        "expected at least one selected scenario"
    );

    let index_path = root.join("artifact-index.json");
    let index = write_artifact_index(&index_path, &outcomes).expect("write real artifact index");
    assert!(artifact_index_has_required_fields(&index));
    assert!(summary_has_base_extended_separation(&index.summary));

    let soak_summary = summarize_soak(&outcomes, iterations);
    let soak_summary_path = root.join("soak-summary.json");
    real_common::write_json(&soak_summary_path, &serde_json::json!(soak_summary));
    assert!(
        soak_summary_path.is_file(),
        "soak summary should be written"
    );
}

fn should_run(app: &str) -> bool {
    if let Some(reason) = real_common::app_gate_reason(app, true) {
        eprintln!("SKIP[{app}]: {reason}");
        return false;
    }
    true
}

fn sample_outcome(id: &str) -> ScenarioOutcome {
    ScenarioOutcome {
        scenario_id: id.to_string(),
        status: ScenarioStatus::Passed,
        elapsed_ms: 100,
        artifact_dir: "/tmp/macos-agent-e2e".to_string(),
        screenshots: vec!["/tmp/macos-agent-e2e/checkpoint.png".to_string()],
        step_ledger_path: Some("/tmp/macos-agent-e2e/steps.jsonl".to_string()),
        skip_reason: None,
        failing_step_id: None,
        last_successful_step_id: Some("sample-1".to_string()),
    }
}

fn run_selected_real_scenarios(
    bin: &Path,
    options: &nils_test_support::cmd::CmdOptions,
    root: &Path,
    selected_apps: &[&str],
) -> Vec<ScenarioOutcome> {
    let mut outcomes = Vec::new();

    if selected_apps.contains(&"finder") {
        let dir = root.join("finder_navigation_and_state_checks");
        outcomes.push(finder_navigation_and_state_checks(bin, options, &dir));
    } else {
        outcomes.push(skipped_outcome(
            "finder_navigation_and_state_checks",
            &root.join("finder_navigation_and_state_checks"),
            "finder not selected by MACOS_AGENT_REAL_E2E_APPS",
        ));
    }

    if selected_apps.contains(&"arc") {
        let arc_profile = ArcYoutubeProfile::from_default_profile();
        let dir = root.join("arc_youtube_multi_video_play_pause_and_comments");
        outcomes.push(arc_youtube_multi_video_play_pause_and_comments(
            bin,
            options,
            &dir,
            &arc_profile,
        ));
    } else {
        outcomes.push(skipped_outcome(
            "arc_youtube_multi_video_play_pause_and_comments",
            &root.join("arc_youtube_multi_video_play_pause_and_comments"),
            "arc not selected by MACOS_AGENT_REAL_E2E_APPS",
        ));
    }

    if selected_apps.contains(&"spotify") {
        let dir = root.join("spotify_player_state_transitions_are_observable");
        outcomes.push(spotify_player_state_transitions_are_observable(
            bin, options, &dir,
        ));
    } else {
        outcomes.push(skipped_outcome(
            "spotify_player_state_transitions_are_observable",
            &root.join("spotify_player_state_transitions_are_observable"),
            "spotify not selected by MACOS_AGENT_REAL_E2E_APPS",
        ));
    }

    if selected_apps.contains(&"arc") && selected_apps.contains(&"spotify") {
        let dir = root.join("cross_app_arc_spotify_focus_and_state_recovery");
        outcomes.push(cross_app_arc_spotify_focus_and_state_recovery(
            bin, options, &dir,
        ));
    } else {
        outcomes.push(skipped_outcome(
            "cross_app_arc_spotify_focus_and_state_recovery",
            &root.join("cross_app_arc_spotify_focus_and_state_recovery"),
            "cross-app scenario requires both arc and spotify selections",
        ));
    }

    outcomes
}

fn skipped_outcome(id: &str, artifact_dir: &Path, reason: &str) -> ScenarioOutcome {
    ScenarioOutcome {
        scenario_id: id.to_string(),
        status: ScenarioStatus::Skipped,
        elapsed_ms: 0,
        artifact_dir: artifact_dir.display().to_string(),
        screenshots: vec![],
        step_ledger_path: Some(real_common::step_ledger_path_for(artifact_dir)),
        skip_reason: Some(reason.to_string()),
        failing_step_id: None,
        last_successful_step_id: None,
    }
}
