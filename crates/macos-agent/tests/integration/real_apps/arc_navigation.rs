use std::cmp::Ordering;
use std::path::Path;
use std::time::{Duration, Instant};

use nils_test_support::cmd::CmdOptions;
use serde_json::Value;

use crate::real_apps::matrix::{ScenarioOutcome, ScenarioStatus};
use crate::real_common::{self, UiPoint};

#[derive(Debug, Clone)]
pub struct ArcYoutubeProfile {
    pub app_name: String,
    pub youtube_home_url: String,
    pub video_tiles: Vec<UiPoint>,
}

pub(crate) struct AxLocateSpec<'a> {
    pub app_name: &'a str,
    pub role: &'a str,
    pub title_contains: Option<&'a str>,
    pub near: Option<UiPoint>,
    pub nth: Option<usize>,
}

#[derive(Debug, Clone)]
struct AxClickCandidate {
    node_id: String,
    center: UiPoint,
    area: f64,
    title: Option<String>,
}

const AX_LOCATE_MAX_DEPTH: &str = "14";
const AX_LOCATE_LIMIT: &str = "600";
const MIN_FRAME_EDGE: f64 = 2.0;
const MAX_ANCHOR_DRIFT_PX: i64 = 420;
const MAX_ANCHOR_DRIFT_SQUARED: i64 = MAX_ANCHOR_DRIFT_PX * MAX_ANCHOR_DRIFT_PX;

impl ArcYoutubeProfile {
    pub fn from_default_profile() -> Self {
        let profile = real_common::load_profile();
        Self {
            app_name: "Arc".to_string(),
            youtube_home_url: profile.arc.youtube_home_url,
            video_tiles: profile.arc.video_tiles,
        }
    }
}

pub fn arc_youtube_opens_home_and_clicks_three_tiles(
    bin: &Path,
    options: &CmdOptions,
    artifact_dir: &Path,
    profile: &ArcYoutubeProfile,
) -> ScenarioOutcome {
    let started = Instant::now();
    let scenario_id = "arc_youtube_opens_home_and_clicks_three_tiles";
    let _ledger = real_common::begin_step_ledger(artifact_dir, scenario_id);
    let _cleanup_guard = real_common::ArcCleanupGuard::new();
    assert!(
        profile.video_tiles.len() >= 3,
        "arc profile requires at least 3 video tiles"
    );

    real_common::require_app_installed(&profile.app_name);
    let preflight = real_common::run_json_step(
        bin,
        options,
        &["--format", "json", "preflight"],
        "preflight",
    );
    real_common::require_preflight_ready(&preflight, &["accessibility", "automation"]);

    let mut screenshots = Vec::new();
    for (idx, point) in profile.video_tiles.iter().take(3).enumerate() {
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
            &format!("arc click youtube tile {}", idx + 1),
        );
        wait_for_arc(bin, options, &profile.app_name);
        let checkpoint = artifact_dir.join(format!("arc-youtube-tile-{}.png", idx + 1));
        capture_active_window(bin, options, &checkpoint);
        screenshots.push(checkpoint.to_string_lossy().to_string());
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

pub(crate) fn activate_arc(bin: &Path, options: &CmdOptions, app_name: &str) {
    let payload = real_common::activate_app_with_retry(
        bin,
        options,
        app_name,
        1800,
        8_000,
        2,
        Duration::from_millis(400),
    );
    assert_eq!(payload["command"], serde_json::json!("window.activate"));
    assert_eq!(
        payload["result"]["selected_app"]
            .as_str()
            .map(|value| value.eq_ignore_ascii_case(app_name)),
        Some(true)
    );
    let step = format!("ensure {app_name} ready for keyboard input after activate");
    real_common::ensure_app_ready_for_keyboard_input(bin, options, app_name, 7000, 60, &step);
}

pub(crate) fn open_youtube_home(bin: &Path, options: &CmdOptions, app_name: &str, url: &str) {
    let mut last_error = String::new();
    for attempt in 1..=3 {
        let preflight_step = format!("arc pre-navigation readiness attempt {attempt}");
        real_common::ensure_app_ready_for_keyboard_input(
            bin,
            options,
            app_name,
            7000,
            60,
            &preflight_step,
        );
        real_common::send_hotkey(
            bin,
            options,
            Some("cmd"),
            "l",
            "arc focus address bar for open youtube home",
        );
        real_common::replace_focused_text_with_clipboard(
            bin,
            options,
            url,
            "arc open youtube home url",
        );
        real_common::send_hotkey(bin, options, None, "return", "arc open youtube home");
        real_common::ensure_app_active_or_frontmost(
            bin,
            options,
            app_name,
            7000,
            60,
            &format!("wait arc active after opening youtube home attempt {attempt}"),
        );

        match verify_active_address_bar_url(bin, options) {
            Ok(()) => return,
            Err(message) => {
                last_error = message;
                if attempt < 3 {
                    let settle = real_common::run_json_step(
                        bin,
                        options,
                        &["--format", "json", "wait", "sleep", "--ms", "250"],
                        "wait before retrying arc youtube navigation",
                    );
                    assert_eq!(settle["command"], serde_json::json!("wait.sleep"));
                }
            }
        }
    }

    real_common::fail_step_with_checkpoint(bin, options, "verify arc address bar URL", &last_error);
}

pub(crate) fn click(bin: &Path, options: &CmdOptions, point: &UiPoint, step: &str) {
    let x = point.x.to_string();
    let y = point.y.to_string();
    let payload = real_common::run_json_step_with_retry(
        bin,
        options,
        &[
            "--format",
            "json",
            "input",
            "click",
            "--x",
            &x,
            "--y",
            &y,
            "--pre-wait-ms",
            "90",
            "--post-wait-ms",
            "150",
        ],
        step,
        2,
        std::time::Duration::from_millis(250),
    );
    assert_eq!(payload["command"], serde_json::json!("input.click"));
}

pub(crate) fn click_ax_or_coordinate(
    bin: &Path,
    options: &CmdOptions,
    ax_spec: AxLocateSpec<'_>,
    fallback_point: &UiPoint,
    step: &str,
) {
    let ready_step = format!("{step} (pre-click readiness)");
    real_common::ensure_app_ready_for_keyboard_input(
        bin,
        options,
        ax_spec.app_name,
        7000,
        60,
        &ready_step,
    );
    let locate_step = format!("{step} (ax-locate)");
    match resolve_ax_click_point(bin, options, &ax_spec, &locate_step) {
        Ok(point) => click(
            bin,
            options,
            &point,
            &format!("{step} (ax-located-input-click)"),
        ),
        Err(err) => {
            eprintln!("WARN[{step}]: {err}; fallback to profile coordinate click");
            click(
                bin,
                options,
                fallback_point,
                &format!("{step} (coordinate-fallback)"),
            );
        }
    }
}

pub(crate) fn wait_for_arc(bin: &Path, options: &CmdOptions, app_name: &str) {
    real_common::ensure_app_active_or_frontmost(
        bin,
        options,
        app_name,
        7000,
        60,
        &format!("wait app-active/frontmost {app_name}"),
    );
}

pub(crate) fn capture_active_window(bin: &Path, options: &CmdOptions, screenshot_path: &Path) {
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
        "observe screenshot",
    );
    assert_eq!(payload["command"], serde_json::json!("observe.screenshot"));
    assert!(screenshot_path.is_file(), "expected screenshot to exist");
}

fn verify_active_address_bar_url(bin: &Path, options: &CmdOptions) -> Result<(), String> {
    real_common::send_hotkey(
        bin,
        options,
        Some("cmd"),
        "l",
        "arc focus address bar for URL verification",
    );
    real_common::send_hotkey(bin, options, Some("cmd"), "c", "arc copy address bar URL");
    let settle = real_common::run_json_step(
        bin,
        options,
        &["--format", "json", "wait", "sleep", "--ms", "150"],
        "wait after copying arc address bar URL",
    );
    assert_eq!(settle["command"], serde_json::json!("wait.sleep"));

    let current_url = real_common::read_clipboard_text();
    let normalized = current_url.to_ascii_lowercase();
    let result = if !normalized.contains("youtube.com") {
        Err(format!(
            "expected Arc address bar to contain youtube.com, got `{current_url}`"
        ))
    } else if normalized.contains("google.com/search") {
        Err(format!(
            "expected direct YouTube URL, got Google search URL `{current_url}`"
        ))
    } else {
        Ok(())
    };

    real_common::send_hotkey(
        bin,
        options,
        None,
        "escape",
        "arc dismiss address bar focus",
    );

    result
}

fn resolve_ax_click_point(
    bin: &Path,
    options: &CmdOptions,
    spec: &AxLocateSpec<'_>,
    step: &str,
) -> Result<UiPoint, String> {
    let nodes = run_ax_list_nodes(bin, options, spec, step, true)?;
    let mut candidates = collect_click_candidates(&nodes);
    if candidates.is_empty() {
        let relaxed_nodes =
            run_ax_list_nodes(bin, options, spec, &format!("{step} (role-relaxed)"), false)?;
        candidates = collect_click_candidates(&relaxed_nodes);
        if candidates.is_empty() {
            return Err(format!(
                "ax.list returned no frame-bearing candidates for role `{}` (including relaxed retry)",
                spec.role
            ));
        }
    }

    let selected = select_ax_candidate(candidates, spec).ok_or_else(|| {
        format!(
            "no selectable AX candidate for role `{}` after filters",
            spec.role
        )
    })?;
    if let Some(anchor) = spec.near {
        let drift_sq = squared_distance(anchor, selected.center);
        if drift_sq > MAX_ANCHOR_DRIFT_SQUARED {
            return Err(format!(
                "selected AX node `{}` is too far from anchor (drift={}px > {}px)",
                selected.node_id,
                (drift_sq as f64).sqrt().round(),
                MAX_ANCHOR_DRIFT_PX
            ));
        }
    }

    Ok(selected.center)
}

fn run_ax_list_nodes(
    bin: &Path,
    options: &CmdOptions,
    spec: &AxLocateSpec<'_>,
    step: &str,
    include_role_filter: bool,
) -> Result<Vec<Value>, String> {
    let mut args = vec![
        "--format".to_string(),
        "json".to_string(),
        "ax".to_string(),
        "list".to_string(),
        "--app".to_string(),
        spec.app_name.to_string(),
        "--max-depth".to_string(),
        AX_LOCATE_MAX_DEPTH.to_string(),
        "--limit".to_string(),
        AX_LOCATE_LIMIT.to_string(),
    ];
    if include_role_filter {
        args.push("--role".to_string());
        args.push(spec.role.to_string());
    }
    if let Some(title_contains) = spec.title_contains {
        args.push("--title-contains".to_string());
        args.push(title_contains.to_string());
    }

    let args_ref = args.iter().map(String::as_str).collect::<Vec<_>>();
    let payload = real_common::try_run_json_step(bin, options, &args_ref, step)?;
    if payload["command"] != serde_json::json!("ax.list") {
        return Err(format!(
            "expected ax.list command in locate step, got `{}`",
            payload["command"]
        ));
    }
    let nodes = payload["result"]["nodes"]
        .as_array()
        .ok_or_else(|| "ax.list response missing result.nodes array".to_string())?;
    Ok(nodes.to_vec())
}

fn collect_click_candidates(nodes: &[Value]) -> Vec<AxClickCandidate> {
    let mut out = Vec::new();
    for (idx, node) in nodes.iter().enumerate() {
        let Some((center, area)) = parse_frame_center(node) else {
            continue;
        };
        let node_id = node
            .get("node_id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("node-{idx}"));
        let title = node
            .get("title")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        out.push(AxClickCandidate {
            node_id,
            center,
            area,
            title,
        });
    }
    out
}

fn parse_frame_center(node: &Value) -> Option<(UiPoint, f64)> {
    let frame = node.get("frame")?.as_object()?;
    let x = frame.get("x")?.as_f64()?;
    let y = frame.get("y")?.as_f64()?;
    let width = frame.get("width")?.as_f64()?;
    let height = frame.get("height")?.as_f64()?;
    if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
        return None;
    }
    if width < MIN_FRAME_EDGE || height < MIN_FRAME_EDGE {
        return None;
    }

    let center_x = x + (width / 2.0);
    let center_y = y + (height / 2.0);
    Some((
        UiPoint {
            x: round_to_i32(center_x)?,
            y: round_to_i32(center_y)?,
        },
        width * height,
    ))
}

fn round_to_i32(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round();
    if rounded < i32::MIN as f64 || rounded > i32::MAX as f64 {
        return None;
    }
    Some(rounded as i32)
}

fn select_ax_candidate(
    mut candidates: Vec<AxClickCandidate>,
    spec: &AxLocateSpec<'_>,
) -> Option<AxClickCandidate> {
    if let Some(title_contains) = spec.title_contains {
        let needle = title_contains.to_ascii_lowercase();
        candidates.retain(|candidate| {
            candidate
                .title
                .as_ref()
                .map(|title| title.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }
    if candidates.is_empty() {
        return None;
    }

    if let Some(anchor) = spec.near {
        candidates.sort_by(|lhs, rhs| {
            let lhs_dist = squared_distance(lhs.center, anchor);
            let rhs_dist = squared_distance(rhs.center, anchor);
            lhs_dist
                .cmp(&rhs_dist)
                .then_with(|| match rhs.area.partial_cmp(&lhs.area) {
                    Some(order) => order,
                    None => Ordering::Equal,
                })
                .then_with(|| lhs.node_id.cmp(&rhs.node_id))
        });
    }

    let index = spec.nth.unwrap_or(1).saturating_sub(1);
    candidates.into_iter().nth(index)
}

fn squared_distance(lhs: UiPoint, rhs: UiPoint) -> i64 {
    let dx = i64::from(lhs.x) - i64::from(rhs.x);
    let dy = i64::from(lhs.y) - i64::from(rhs.y);
    (dx * dx) + (dy * dy)
}

#[cfg(test)]
mod tests {
    use super::{AxLocateSpec, collect_click_candidates, select_ax_candidate};
    use crate::real_common::UiPoint;
    use serde_json::json;

    fn spec_with_anchor(anchor: UiPoint) -> AxLocateSpec<'static> {
        AxLocateSpec {
            app_name: "Arc",
            role: "AXLink",
            title_contains: None,
            near: Some(anchor),
            nth: None,
        }
    }

    #[test]
    fn collect_click_candidates_ignores_invalid_frames() {
        let nodes = vec![
            json!({"node_id":"missing-frame"}),
            json!({"node_id":"tiny","frame":{"x":10.0,"y":10.0,"width":1.0,"height":1.0}}),
            json!({"node_id":"ok","title":"Tile","frame":{"x":100.0,"y":60.0,"width":50.0,"height":20.0}}),
        ];
        let candidates = collect_click_candidates(&nodes);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].node_id, "ok");
        assert_eq!(candidates[0].center.x, 125);
        assert_eq!(candidates[0].center.y, 70);
    }

    #[test]
    fn select_ax_candidate_prefers_nearest_anchor() {
        let nodes = vec![
            json!({"node_id":"far","frame":{"x":700.0,"y":700.0,"width":40.0,"height":40.0}}),
            json!({"node_id":"near","frame":{"x":95.0,"y":95.0,"width":20.0,"height":20.0}}),
            json!({"node_id":"mid","frame":{"x":250.0,"y":250.0,"width":30.0,"height":30.0}}),
        ];
        let candidates = collect_click_candidates(&nodes);
        let picked = select_ax_candidate(candidates, &spec_with_anchor(UiPoint { x: 100, y: 100 }))
            .expect("candidate should be selected");
        assert_eq!(picked.node_id, "near");
    }

    #[test]
    fn select_ax_candidate_respects_nth_after_anchor_sort() {
        let nodes = vec![
            json!({"node_id":"c1","frame":{"x":100.0,"y":100.0,"width":20.0,"height":20.0}}),
            json!({"node_id":"c2","frame":{"x":180.0,"y":180.0,"width":20.0,"height":20.0}}),
            json!({"node_id":"c3","frame":{"x":260.0,"y":260.0,"width":20.0,"height":20.0}}),
        ];
        let mut spec = spec_with_anchor(UiPoint { x: 100, y: 100 });
        spec.nth = Some(2);
        let candidates = collect_click_candidates(&nodes);
        let picked = select_ax_candidate(candidates, &spec).expect("second candidate should exist");
        assert_eq!(picked.node_id, "c2");
    }

    #[test]
    fn select_ax_candidate_filters_title_case_insensitively() {
        let nodes = vec![
            json!({"node_id":"ignore","title":"Top Stories","frame":{"x":200.0,"y":200.0,"width":60.0,"height":20.0}}),
            json!({"node_id":"match","title":"COMMENTS","frame":{"x":100.0,"y":100.0,"width":60.0,"height":20.0}}),
        ];
        let mut spec = spec_with_anchor(UiPoint { x: 110, y: 110 });
        spec.title_contains = Some("comments");
        let candidates = collect_click_candidates(&nodes);
        let picked = select_ax_candidate(candidates, &spec).expect("title-filtered candidate");
        assert_eq!(picked.node_id, "match");
    }
}
