use std::collections::HashSet;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SUPPORTED_APPS: [&str; 3] = ["arc", "spotify", "finder"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioOutcome {
    pub scenario_id: String,
    pub status: ScenarioStatus,
    pub elapsed_ms: u64,
    pub artifact_dir: String,
    #[serde(default)]
    pub screenshots: Vec<String>,
    #[serde(default)]
    pub step_ledger_path: Option<String>,
    #[serde(default)]
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub failing_step_id: Option<String>,
    #[serde(default)]
    pub last_successful_step_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioBucketSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    #[serde(default)]
    pub scenario_ids: Vec<String>,
}

impl ScenarioBucketSummary {
    fn push(&mut self, scenario: &ScenarioOutcome) {
        self.total += 1;
        match scenario.status {
            ScenarioStatus::Passed => self.passed += 1,
            ScenarioStatus::Failed => self.failed += 1,
            ScenarioStatus::Skipped => self.skipped += 1,
        }
        self.scenario_ids.push(scenario.scenario_id.clone());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixSummary {
    pub base: ScenarioBucketSummary,
    pub extended: ScenarioBucketSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactIndex {
    #[serde(default)]
    pub scenarios: Vec<ScenarioOutcome>,
    pub summary: MatrixSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SoakSummary {
    pub iterations: usize,
    pub total_runs: usize,
    pub passed_runs: usize,
    pub failed_runs: usize,
    pub skipped_runs: usize,
    pub pass_rate_percent: f64,
    #[serde(default)]
    pub top_failing_step_ids: Vec<String>,
}

pub fn selected_apps_from_env(raw: Option<&str>) -> Vec<&'static str> {
    let Some(raw) = raw else {
        return SUPPORTED_APPS.to_vec();
    };

    let selected: HashSet<String> = raw
        .split(',')
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect();

    if selected.is_empty() {
        return SUPPORTED_APPS.to_vec();
    }

    SUPPORTED_APPS
        .iter()
        .copied()
        .filter(|app| selected.contains(*app))
        .collect()
}

pub fn write_artifact_index(
    path: &Path,
    scenarios: &[ScenarioOutcome],
) -> io::Result<ArtifactIndex> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let index = ArtifactIndex {
        scenarios: scenarios.to_vec(),
        summary: classify_base_vs_extended(scenarios),
    };
    let payload = serde_json::to_vec_pretty(&index)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    std::fs::write(path, payload)?;
    Ok(index)
}

pub fn classify_base_vs_extended(scenarios: &[ScenarioOutcome]) -> MatrixSummary {
    let mut summary = MatrixSummary::default();
    for scenario in scenarios {
        if is_extended_scenario_id(&scenario.scenario_id) {
            summary.extended.push(scenario);
        } else {
            summary.base.push(scenario);
        }
    }
    summary
}

pub fn subset_selection_matches(raw: Option<&str>, expected: &[&str]) -> bool {
    selected_apps_from_env(raw) == expected
}

pub fn artifact_index_has_required_fields(index: &ArtifactIndex) -> bool {
    let Ok(value) = serde_json::to_value(index) else {
        return false;
    };
    let Some(rows) = value.get("scenarios").and_then(serde_json::Value::as_array) else {
        return false;
    };

    rows.iter().all(|row| {
        row.get("scenario_id")
            .and_then(serde_json::Value::as_str)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
            && row
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some()
            && row
                .get("elapsed_ms")
                .and_then(serde_json::Value::as_u64)
                .is_some()
            && row
                .get("artifact_dir")
                .and_then(serde_json::Value::as_str)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            && row
                .get("screenshots")
                .and_then(serde_json::Value::as_array)
                .is_some()
            && row
                .get("step_ledger_path")
                .map(|value| value.is_string() || value.is_null())
                .unwrap_or(false)
    })
}

pub fn summary_has_base_extended_separation(summary: &MatrixSummary) -> bool {
    let base_ids: HashSet<&str> = summary
        .base
        .scenario_ids
        .iter()
        .map(String::as_str)
        .collect();
    let extended_ids: HashSet<&str> = summary
        .extended
        .scenario_ids
        .iter()
        .map(String::as_str)
        .collect();

    base_ids.is_disjoint(&extended_ids)
        && summary.base.total == summary.base.scenario_ids.len()
        && summary.extended.total == summary.extended.scenario_ids.len()
}

pub fn summarize_soak(outcomes: &[ScenarioOutcome], iterations: usize) -> SoakSummary {
    let total_runs = outcomes.len();
    let passed_runs = outcomes
        .iter()
        .filter(|outcome| outcome.status == ScenarioStatus::Passed)
        .count();
    let failed_runs = outcomes
        .iter()
        .filter(|outcome| outcome.status == ScenarioStatus::Failed)
        .count();
    let skipped_runs = outcomes
        .iter()
        .filter(|outcome| outcome.status == ScenarioStatus::Skipped)
        .count();

    let considered = total_runs.saturating_sub(skipped_runs);
    let pass_rate_percent = if considered == 0 {
        0.0
    } else {
        (passed_runs as f64 / considered as f64) * 100.0
    };

    let mut failing_steps = outcomes
        .iter()
        .filter_map(|outcome| outcome.failing_step_id.clone())
        .collect::<Vec<_>>();
    failing_steps.sort();
    failing_steps.dedup();

    SoakSummary {
        iterations,
        total_runs,
        passed_runs,
        failed_runs,
        skipped_runs,
        pass_rate_percent,
        top_failing_step_ids: failing_steps,
    }
}

fn is_extended_scenario_id(scenario_id: &str) -> bool {
    let normalized = scenario_id.trim().to_ascii_lowercase();
    normalized.starts_with("cross_app_")
        || normalized.contains("_extended")
        || normalized.contains("extended_")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        SUPPORTED_APPS, ScenarioOutcome, ScenarioStatus, artifact_index_has_required_fields,
        classify_base_vs_extended, selected_apps_from_env, subset_selection_matches,
        summarize_soak, summary_has_base_extended_separation, write_artifact_index,
    };

    #[test]
    fn app_subset_selection_defaults_to_all() {
        assert_eq!(selected_apps_from_env(None), SUPPORTED_APPS);
        assert_eq!(selected_apps_from_env(Some("  ")), SUPPORTED_APPS);
    }

    #[test]
    fn app_subset_selection_is_deterministic() {
        let selected = selected_apps_from_env(Some("finder,arc,arc,spotify"));
        assert_eq!(selected, vec!["arc", "spotify", "finder"]);
        assert!(subset_selection_matches(
            Some("spotify,finder,arc"),
            &["arc", "spotify", "finder"]
        ));
    }

    #[test]
    fn app_subset_selection_filters_unsupported_apps() {
        let selected = selected_apps_from_env(Some("mail,spotify,notes"));
        assert_eq!(selected, vec!["spotify"]);
    }

    #[test]
    fn write_artifact_index_includes_required_fields() {
        let dir = tempdir().expect("tempdir");
        let index_path = dir.path().join("artifacts/index.json");
        let scenarios = vec![ScenarioOutcome {
            scenario_id: "finder_navigation_and_state_checks".to_string(),
            status: ScenarioStatus::Passed,
            elapsed_ms: 1250,
            artifact_dir: "/tmp/finder".to_string(),
            screenshots: vec!["/tmp/finder/step-1.png".to_string()],
            step_ledger_path: Some("/tmp/finder/steps.jsonl".to_string()),
            skip_reason: None,
            failing_step_id: None,
            last_successful_step_id: Some("finder-3".to_string()),
        }];

        let index = write_artifact_index(&index_path, &scenarios).expect("write index");
        assert!(index_path.is_file());
        assert!(artifact_index_has_required_fields(&index));

        let persisted = fs::read_to_string(&index_path).expect("read index");
        let json: serde_json::Value = serde_json::from_str(&persisted).expect("json parse");
        assert!(json["scenarios"].is_array());
        assert_eq!(
            json["scenarios"][0]["scenario_id"],
            "finder_navigation_and_state_checks"
        );
        assert_eq!(json["scenarios"][0]["status"], "passed");
    }

    #[test]
    fn classify_base_vs_extended_separates_summary_buckets() {
        let scenarios = vec![
            ScenarioOutcome {
                scenario_id: "finder_navigation_and_state_checks".to_string(),
                status: ScenarioStatus::Passed,
                elapsed_ms: 1200,
                artifact_dir: "/tmp/finder".to_string(),
                screenshots: vec![],
                step_ledger_path: Some("/tmp/finder/steps.jsonl".to_string()),
                skip_reason: None,
                failing_step_id: None,
                last_successful_step_id: Some("finder-9".to_string()),
            },
            ScenarioOutcome {
                scenario_id: "cross_app_arc_spotify_focus_and_state_recovery".to_string(),
                status: ScenarioStatus::Failed,
                elapsed_ms: 2400,
                artifact_dir: "/tmp/cross-app".to_string(),
                screenshots: vec![],
                step_ledger_path: Some("/tmp/cross-app/steps.jsonl".to_string()),
                skip_reason: None,
                failing_step_id: Some("cross-2".to_string()),
                last_successful_step_id: Some("cross-1".to_string()),
            },
        ];

        let summary = classify_base_vs_extended(&scenarios);
        assert_eq!(summary.base.total, 1);
        assert_eq!(summary.extended.total, 1);
        assert_eq!(summary.base.passed, 1);
        assert_eq!(summary.extended.failed, 1);
        assert!(summary_has_base_extended_separation(&summary));
    }

    #[test]
    fn summarize_soak_reports_pass_rate_and_failing_steps() {
        let outcomes = vec![
            ScenarioOutcome {
                scenario_id: "finder_navigation_and_state_checks".to_string(),
                status: ScenarioStatus::Passed,
                elapsed_ms: 1000,
                artifact_dir: "/tmp/finder".to_string(),
                screenshots: vec![],
                step_ledger_path: Some("/tmp/finder/steps.jsonl".to_string()),
                skip_reason: None,
                failing_step_id: None,
                last_successful_step_id: Some("finder-2".to_string()),
            },
            ScenarioOutcome {
                scenario_id: "cross_app_arc_spotify_focus_and_state_recovery".to_string(),
                status: ScenarioStatus::Failed,
                elapsed_ms: 1000,
                artifact_dir: "/tmp/cross".to_string(),
                screenshots: vec![],
                step_ledger_path: Some("/tmp/cross/steps.jsonl".to_string()),
                skip_reason: None,
                failing_step_id: Some("cross-4".to_string()),
                last_successful_step_id: Some("cross-3".to_string()),
            },
        ];
        let summary = summarize_soak(&outcomes, 2);
        assert_eq!(summary.iterations, 2);
        assert_eq!(summary.total_runs, 2);
        assert_eq!(summary.failed_runs, 1);
        assert!(summary.pass_rate_percent > 0.0);
        assert_eq!(summary.top_failing_step_ids, vec!["cross-4".to_string()]);
    }
}
