use std::path::Path;
use std::time::Instant;

use nils_test_support::cmd::CmdOptions;

use crate::real_apps::arc_navigation::{
    ArcYoutubeProfile, AxLocateSpec, activate_arc, capture_active_window, click_ax_or_coordinate,
    open_youtube_home, wait_for_arc,
};
use crate::real_apps::matrix::{ScenarioOutcome, ScenarioStatus};
use crate::real_common;

pub fn arc_youtube_play_pause_and_comment_checkpoint(
    bin: &Path,
    options: &CmdOptions,
    artifact_dir: &Path,
    profile: &ArcYoutubeProfile,
    selected_video_indices: &[usize],
) -> ScenarioOutcome {
    let started = Instant::now();
    let scenario_id = "arc_youtube_play_pause_and_comment_checkpoint";
    let _ledger = real_common::begin_step_ledger(artifact_dir, scenario_id);
    let _cleanup_guard = real_common::ArcCleanupGuard::new();
    assert!(
        !selected_video_indices.is_empty(),
        "selected_video_indices cannot be empty"
    );

    real_common::require_app_installed(&profile.app_name);
    let mut screenshots = Vec::new();
    let common_profile = real_common::load_profile();
    let player_focus = common_profile.arc.player_focus;
    let comment_checkpoint = common_profile.arc.comment_checkpoint;

    for index in selected_video_indices {
        let point = profile
            .video_tiles
            .get(*index)
            .unwrap_or_else(|| panic!("selected video index {index} out of range"));
        activate_arc(bin, options, &profile.app_name);
        open_youtube_home(bin, options, &profile.app_name, &profile.youtube_home_url);
        click_ax_or_coordinate(
            bin,
            options,
            AxLocateSpec {
                app_name: &profile.app_name,
                role: "AXLink",
                title_contains: None,
                near: Some(*point),
                nth: None,
            },
            point,
            &format!("arc select video index {index}"),
        );
        wait_for_arc(bin, options, &profile.app_name);

        click_ax_or_coordinate(
            bin,
            options,
            AxLocateSpec {
                app_name: &profile.app_name,
                role: "AXWebArea",
                title_contains: None,
                near: Some(player_focus),
                nth: None,
            },
            &player_focus,
            "arc focus player for play/pause",
        );
        real_common::send_hotkey(bin, options, None, "space", "arc toggle play/pause #1");
        wait_for_arc(bin, options, &profile.app_name);

        real_common::send_hotkey(bin, options, None, "space", "arc toggle play/pause #2");
        wait_for_arc(bin, options, &profile.app_name);

        click_ax_or_coordinate(
            bin,
            options,
            AxLocateSpec {
                app_name: &profile.app_name,
                role: "AXStaticText",
                title_contains: None,
                near: Some(comment_checkpoint),
                nth: None,
            },
            &comment_checkpoint,
            "arc click comment checkpoint",
        );
        wait_for_arc(bin, options, &profile.app_name);

        let playback_screenshot = artifact_dir.join(format!("arc-video-{index}-playback.png"));
        capture_active_window(bin, options, &playback_screenshot);
        screenshots.push(playback_screenshot.to_string_lossy().to_string());

        let comment_screenshot = artifact_dir.join(format!("arc-video-{index}-comment.png"));
        capture_active_window(bin, options, &comment_screenshot);
        screenshots.push(comment_screenshot.to_string_lossy().to_string());
    }

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
