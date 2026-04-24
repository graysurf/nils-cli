use nils_test_support::cmd::CmdOptions;
use std::path::Path;

use crate::real_apps::arc_assertions::arc_youtube_play_pause_and_comment_checkpoint;
use crate::real_apps::arc_navigation::{
    ArcYoutubeProfile, arc_youtube_opens_home_and_clicks_three_tiles,
};
use crate::real_apps::matrix::{ScenarioOutcome, ScenarioStatus};
use crate::real_common;

pub fn arc_youtube_multi_video_play_pause_and_comments(
    bin: &Path,
    options: &CmdOptions,
    artifact_dir: &Path,
    profile: &ArcYoutubeProfile,
) -> ScenarioOutcome {
    // This scenario performs non-idempotent app interactions. We intentionally avoid
    // whole-scenario retries and rely on bounded per-step retries only.
    let open_and_click =
        arc_youtube_opens_home_and_clicks_three_tiles(bin, options, artifact_dir, profile);
    let playback_and_comment = arc_youtube_play_pause_and_comment_checkpoint(
        bin,
        options,
        artifact_dir,
        profile,
        &[0, 1, 2],
    );

    let mut screenshots = open_and_click.screenshots;
    screenshots.extend(playback_and_comment.screenshots);

    let elapsed = open_and_click.elapsed_ms + playback_and_comment.elapsed_ms;
    let (step_ledger_path, failing_step_id, last_successful_step_id) =
        real_common::current_step_ledger_snapshot();

    ScenarioOutcome {
        scenario_id: "arc_youtube_multi_video_play_pause_and_comments".to_string(),
        status: ScenarioStatus::Passed,
        elapsed_ms: elapsed,
        artifact_dir: artifact_dir.display().to_string(),
        screenshots,
        step_ledger_path,
        skip_reason: None,
        failing_step_id,
        last_successful_step_id,
    }
}
