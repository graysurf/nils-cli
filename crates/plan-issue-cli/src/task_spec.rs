use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use nils_common::{fs as common_fs, git as common_git, markdown as common_markdown};
use plan_tooling::parse::{Sprint as ParsedSprint, parse_plan_with_display};
use plan_tooling::split_prs::{
    SplitPlanOptions, SplitPlanRecord, SplitPrGrouping, SplitPrStrategy, SplitScope,
    build_split_plan_records, resolve_pr_grouping_by_sprint, select_sprints_for_scope,
};

use crate::commands::{PrGroupMapping, PrGrouping, SplitStrategy};

pub const TASK_SPEC_HEADER: &str = "# task_id\tsummary\tbranch\tworktree\towner\tnotes\tpr_group";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSpecScope {
    Plan,
    Sprint(i32),
}

#[derive(Debug, Clone)]
pub struct TaskSpecBuildOptions {
    pub owner_prefix: String,
    pub branch_prefix: String,
    pub worktree_prefix: String,
    pub pr_grouping: Option<PrGrouping>,
    pub default_pr_grouping: Option<PrGrouping>,
    pub strategy: SplitStrategy,
    pub pr_group: Vec<PrGroupMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpecRow {
    pub task_id: String,
    pub summary: String,
    pub branch: String,
    pub worktree: String,
    pub owner: String,
    pub notes: String,
    pub pr_group: String,
    pub sprint: i32,
    pub grouping: PrGrouping,
}

#[derive(Debug, Clone)]
pub struct TaskSpecBuild {
    pub plan_title: String,
    pub display_plan_path: String,
    pub sprint_name: Option<String>,
    pub rows: Vec<TaskSpecRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLaneMetadata {
    pub execution_mode: String,
    pub owner: String,
    pub branch: String,
    pub worktree: String,
    pub notes: String,
}

pub fn build_task_spec(
    plan_file: &Path,
    scope: TaskSpecScope,
    options: &TaskSpecBuildOptions,
) -> Result<TaskSpecBuild, String> {
    let display_path = plan_file.to_string_lossy().to_string();
    let resolved_plan_path = resolve_plan_file(plan_file);
    if !resolved_plan_path.is_file() {
        return Err(format!("plan file not found: {display_path}"));
    }

    let (plan, parse_errors) = parse_plan_with_display(&resolved_plan_path, &display_path)
        .map_err(|err| format!("{display_path}: {err}"))?;
    if !parse_errors.is_empty() {
        return Err(format!("{display_path}: {}", parse_errors.join(" | ")));
    }

    let split_scope = match scope {
        TaskSpecScope::Plan => SplitScope::Plan,
        TaskSpecScope::Sprint(sprint) => SplitScope::Sprint(sprint),
    };

    let selected_sprints = select_sprints_for_scope(&plan, split_scope)?;
    let sprint_name = match scope {
        TaskSpecScope::Plan => None,
        TaskSpecScope::Sprint(_) => selected_sprints.first().map(|sprint| sprint.name.clone()),
    };

    let split_options = SplitPlanOptions {
        pr_grouping: options.pr_grouping.map(to_split_grouping),
        default_pr_grouping: options.default_pr_grouping.map(to_split_grouping),
        strategy: to_split_strategy(options.strategy),
        pr_group_entries: options
            .pr_group
            .iter()
            .map(|entry| format!("{}={}", entry.task, entry.group))
            .collect(),
        owner_prefix: options.owner_prefix.clone(),
        branch_prefix: options.branch_prefix.clone(),
        worktree_prefix: options.worktree_prefix.clone(),
    };
    let grouping_by_sprint =
        resolve_pr_grouping_by_sprint(&selected_sprints, &split_options).map(|resolved| {
            resolved
                .into_iter()
                .map(|(sprint, value)| (sprint, from_split_grouping(value.grouping)))
                .collect::<HashMap<_, _>>()
        })?;

    let split_records = build_split_plan_records(&selected_sprints, &split_options)?;
    let rows = RuntimeMetadataMaterializer::new(&selected_sprints, options, grouping_by_sprint)?
        .materialize_rows(&split_records)?;

    Ok(TaskSpecBuild {
        plan_title: plan.title,
        display_plan_path: display_path,
        sprint_name,
        rows,
    })
}

#[derive(Debug, Clone)]
struct RuntimeTaskSeed {
    sprint: i32,
    ordinal: usize,
    plan_task_id: String,
    dependencies: Vec<String>,
    first_validation: Option<String>,
}

#[derive(Debug, Clone)]
struct RuntimeMetadataMaterializer {
    owner_prefix: String,
    branch_prefix: String,
    worktree_prefix: String,
    grouping_by_sprint: HashMap<i32, PrGrouping>,
    strategy: SplitStrategy,
    task_seed_by_id: HashMap<String, RuntimeTaskSeed>,
}

impl RuntimeMetadataMaterializer {
    fn new(
        selected_sprints: &[ParsedSprint],
        options: &TaskSpecBuildOptions,
        grouping_by_sprint: HashMap<i32, PrGrouping>,
    ) -> Result<Self, String> {
        let mut task_seed_by_id: HashMap<String, RuntimeTaskSeed> = HashMap::new();

        for sprint in selected_sprints {
            for (idx, task) in sprint.tasks.iter().enumerate() {
                let ordinal = idx + 1;
                let task_id = format!("S{}T{ordinal}", sprint.number);
                let plan_task_id = if task.id.trim().is_empty() {
                    task_id.clone()
                } else {
                    task.id.trim().to_string()
                };
                let dependencies = task
                    .dependencies
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|dep| dep.id.trim().to_string())
                    .filter(|dep| !dep.is_empty())
                    .filter(|dep| !is_plan_placeholder(dep))
                    .collect::<Vec<_>>();
                let first_validation = task
                    .validation
                    .iter()
                    .map(|validation| validation.trim().to_string())
                    .find(|validation| !validation.is_empty() && !is_plan_placeholder(validation));

                let inserted = task_seed_by_id.insert(
                    task_id.clone(),
                    RuntimeTaskSeed {
                        sprint: sprint.number,
                        ordinal,
                        plan_task_id,
                        dependencies,
                        first_validation,
                    },
                );
                if inserted.is_some() {
                    return Err(format!(
                        "duplicate synthesized task id while materializing runtime metadata: {task_id}"
                    ));
                }
            }
        }

        Ok(Self {
            owner_prefix: normalize_owner_prefix(&options.owner_prefix),
            branch_prefix: normalize_branch_prefix(&options.branch_prefix),
            worktree_prefix: normalize_worktree_prefix(&options.worktree_prefix),
            grouping_by_sprint,
            strategy: options.strategy,
            task_seed_by_id,
        })
    }

    fn materialize_rows(
        &self,
        split_records: &[SplitPlanRecord],
    ) -> Result<Vec<TaskSpecRow>, String> {
        let mut group_sizes: HashMap<(i32, String), usize> = HashMap::new();
        let mut anchor_by_lane: HashMap<(i32, String), String> = HashMap::new();
        for record in split_records {
            let lane_key = (record.sprint, record.pr_group.clone());
            *group_sizes.entry(lane_key.clone()).or_insert(0) += 1;
            anchor_by_lane
                .entry(lane_key)
                .or_insert_with(|| record.task_id.clone());
        }

        let mut rows = Vec::with_capacity(split_records.len());
        for record in split_records {
            let seed = self
                .task_seed_by_id
                .get(&record.task_id)
                .ok_or_else(|| format!("{}: missing parsed plan task metadata", record.task_id))?;
            let lane_key = (record.sprint, record.pr_group.clone());
            let shared_anchor = if group_sizes.get(&lane_key).copied().unwrap_or(0) > 1 {
                anchor_by_lane.get(&lane_key).cloned()
            } else {
                None
            };

            let slug_fallback = format!("task-{}", seed.ordinal);
            let slug = normalize_token(&record.summary, &slug_fallback, 48);
            let grouping = *self.grouping_by_sprint.get(&record.sprint).ok_or_else(|| {
                format!(
                    "{}: missing resolved grouping for sprint {}",
                    record.task_id, record.sprint
                )
            })?;
            let notes =
                synthesize_notes(seed, grouping, &record.pr_group, shared_anchor.as_deref());

            rows.push(TaskSpecRow {
                task_id: record.task_id.clone(),
                summary: record.summary.clone(),
                branch: format!(
                    "{}/s{}-t{}-{}",
                    self.branch_prefix, seed.sprint, seed.ordinal, slug
                ),
                worktree: format!(
                    "{}-s{}-t{}",
                    self.worktree_prefix, seed.sprint, seed.ordinal
                ),
                owner: format!("{}-s{}-t{}", self.owner_prefix, seed.sprint, seed.ordinal),
                notes,
                pr_group: record.pr_group.clone(),
                sprint: record.sprint,
                grouping,
            });
        }

        let runtime_lane_metadata = runtime_lane_metadata_by_task(&rows, self.strategy);
        for row in &mut rows {
            let metadata = runtime_lane_metadata.get(&row.task_id).ok_or_else(|| {
                format!(
                    "{}: missing runtime lane metadata after materialization",
                    row.task_id
                )
            })?;
            row.owner = metadata.owner.clone();
            row.branch = metadata.branch.clone();
            row.worktree = metadata.worktree.clone();
            row.notes = metadata.notes.clone();
        }

        Ok(rows)
    }
}

fn synthesize_notes(
    seed: &RuntimeTaskSeed,
    grouping: PrGrouping,
    pr_group: &str,
    shared_anchor: Option<&str>,
) -> String {
    let mut notes = vec![
        format!("sprint=S{}", seed.sprint),
        format!("plan-task:{}", seed.plan_task_id),
    ];
    if !seed.dependencies.is_empty() {
        notes.push(format!("deps={}", seed.dependencies.join(",")));
    }
    if let Some(first_validation) = &seed.first_validation {
        notes.push(format!("validate={first_validation}"));
    }
    notes.push(format!("pr-grouping={}", pr_grouping_label(grouping)));
    notes.push(format!("pr-group={pr_group}"));
    if let Some(anchor) = shared_anchor {
        notes.push(format!("shared-pr-anchor={anchor}"));
    }

    common_markdown::canonicalize_table_cell(&notes.join("; "))
}

fn pr_grouping_label(grouping: PrGrouping) -> &'static str {
    match grouping {
        PrGrouping::PerSprint => "per-sprint",
        PrGrouping::Group => "group",
    }
}

fn normalize_branch_prefix(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        "issue".to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_worktree_prefix(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches(['-', '_']);
    if trimmed.is_empty() {
        "issue".to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_owner_prefix(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "subagent".to_string()
    } else if trimmed.to_ascii_lowercase().contains("subagent") {
        trimmed.to_string()
    } else {
        format!("subagent-{trimmed}")
    }
}

fn normalize_token(value: &str, fallback: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let normalized = out.trim_matches('-').to_string();
    let mut final_token = if normalized.is_empty() {
        fallback.to_string()
    } else {
        normalized
    };
    if final_token.len() > max_len {
        final_token.truncate(max_len);
        final_token = final_token.trim_matches('-').to_string();
    }
    final_token
}

fn is_plan_placeholder(value: &str) -> bool {
    let token = value.trim().to_ascii_lowercase();
    if matches!(token.as_str(), "" | "-" | "none" | "n/a" | "na" | "...") {
        return true;
    }
    if token.starts_with('<') && token.ends_with('>') {
        return true;
    }
    token.contains("task ids")
}

pub fn render_tsv(rows: &[TaskSpecRow]) -> String {
    let mut out = String::new();
    out.push_str(TASK_SPEC_HEADER);
    out.push('\n');
    for row in rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.task_id.replace('\t', " "),
            row.summary.replace('\t', " "),
            row.branch.replace('\t', " "),
            row.worktree.replace('\t', " "),
            row.owner.replace('\t', " "),
            row.notes.replace('\t', " "),
            row.pr_group.replace('\t', " "),
        ));
    }
    out
}

pub fn write_tsv(path: &Path, rows: &[TaskSpecRow]) -> Result<(), String> {
    common_fs::write_text(path, &render_tsv(rows)).map_err(|err| match err {
        common_fs::WriteTextError::CreateParentDir { path, source } => {
            format!(
                "failed to create output directory {}: {source}",
                path.display()
            )
        }
        common_fs::WriteTextError::WriteFile { source, .. } => {
            format!("failed to write task-spec {}: {source}", path.display())
        }
    })
}

pub fn execution_mode_by_task(
    rows: &[TaskSpecRow],
    strategy: SplitStrategy,
) -> HashMap<String, String> {
    runtime_lane_metadata_by_task(rows, strategy)
        .into_iter()
        .map(|(task_id, lane)| (task_id, lane.execution_mode))
        .collect()
}

pub fn runtime_lane_metadata_by_task(
    rows: &[TaskSpecRow],
    strategy: SplitStrategy,
) -> HashMap<String, RuntimeLaneMetadata> {
    let execution_modes = execution_mode_from_rows(rows, strategy);
    let row_by_task: HashMap<&str, &TaskSpecRow> =
        rows.iter().map(|row| (row.task_id.as_str(), row)).collect();

    let mut anchor_by_lane: HashMap<(i32, String), String> = HashMap::new();
    for row in rows {
        anchor_by_lane
            .entry((row.sprint, row.pr_group.clone()))
            .or_insert_with(|| {
                canonical_lane_anchor_task_id(rows, row.sprint, &row.pr_group)
                    .unwrap_or_else(|| row.task_id.clone())
            });
    }

    let mut out = HashMap::new();
    for row in rows {
        let execution_mode = execution_modes
            .get(&row.task_id)
            .cloned()
            .unwrap_or_else(|| "pr-isolated".to_string());
        let lane_key = (row.sprint, row.pr_group.clone());
        let anchor_row = if execution_mode == "pr-isolated" {
            row
        } else {
            anchor_by_lane
                .get(&lane_key)
                .and_then(|task_id| row_by_task.get(task_id.as_str()))
                .copied()
                .unwrap_or(row)
        };

        out.insert(
            row.task_id.clone(),
            RuntimeLaneMetadata {
                execution_mode,
                owner: anchor_row.owner.clone(),
                branch: anchor_row.branch.clone(),
                worktree: anchor_row.worktree.clone(),
                notes: common_markdown::canonicalize_table_cell(&row.notes),
            },
        );
    }

    out
}

fn execution_mode_from_rows(
    rows: &[TaskSpecRow],
    _strategy: SplitStrategy,
) -> HashMap<String, String> {
    let mut sprint_group_set: HashMap<i32, BTreeSet<String>> = HashMap::new();
    let mut sprint_group_sizes: HashMap<(i32, String), usize> = HashMap::new();
    for row in rows {
        sprint_group_set
            .entry(row.sprint)
            .or_default()
            .insert(row.pr_group.clone());
        *sprint_group_sizes
            .entry((row.sprint, row.pr_group.clone()))
            .or_insert(0) += 1;
    }

    let mut out = HashMap::new();
    for row in rows {
        let sprint_group_count = sprint_group_set
            .get(&row.sprint)
            .map(BTreeSet::len)
            .unwrap_or(0);
        let group_size = sprint_group_sizes
            .get(&(row.sprint, row.pr_group.clone()))
            .copied()
            .unwrap_or(0);

        let mode = if row.grouping == PrGrouping::PerSprint {
            "per-sprint"
        } else if sprint_group_count == 1 && group_size > 1 {
            // Group mode (auto or deterministic) can converge to a single shared PR lane.
            // Expose that as per-sprint so downstream execution semantics match explicit
            // per-sprint mode.
            "per-sprint"
        } else if group_size > 1 {
            "pr-shared"
        } else {
            "pr-isolated"
        };
        out.insert(row.task_id.clone(), mode.to_string());
    }

    out
}

fn canonical_lane_anchor_task_id(
    rows: &[TaskSpecRow],
    sprint: i32,
    pr_group: &str,
) -> Option<String> {
    let mut lane_rows = rows
        .iter()
        .filter(|row| row.sprint == sprint && row.pr_group == pr_group)
        .collect::<Vec<_>>();
    if lane_rows.is_empty() {
        return None;
    }

    lane_rows.sort_unstable_by(|a, b| a.task_id.cmp(&b.task_id));
    lane_rows.first().map(|row| row.task_id.clone())
}

pub fn default_plan_task_spec_path(plan_file: &Path) -> PathBuf {
    let plan_stem = plan_file
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("plan")
        .to_string();

    state_dir()
        .join("out")
        .join("plan-issue-delivery")
        .join(format!("{plan_stem}-plan-tasks.tsv"))
}

pub fn default_sprint_task_spec_path(plan_file: &Path, sprint: i32) -> PathBuf {
    let plan_stem = plan_file
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("plan")
        .to_string();

    state_dir()
        .join("out")
        .join("plan-issue-delivery")
        .join(format!("{plan_stem}-sprint-{sprint}-tasks.tsv"))
}

pub fn state_dir() -> PathBuf {
    crate::state::state_dir()
}

pub fn resolve_plan_file(plan_file: &Path) -> PathBuf {
    let repo_root = detect_repo_root();
    resolve_repo_relative(&repo_root, plan_file)
}

fn detect_repo_root() -> PathBuf {
    common_git::repo_root_or_cwd()
}

fn resolve_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    repo_root.join(path)
}

fn to_split_grouping(grouping: PrGrouping) -> SplitPrGrouping {
    match grouping {
        PrGrouping::PerSprint => SplitPrGrouping::PerSprint,
        PrGrouping::Group => SplitPrGrouping::Group,
    }
}

fn from_split_grouping(grouping: SplitPrGrouping) -> PrGrouping {
    match grouping {
        SplitPrGrouping::PerSprint => PrGrouping::PerSprint,
        SplitPrGrouping::Group => PrGrouping::Group,
    }
}

fn to_split_strategy(strategy: SplitStrategy) -> SplitPrStrategy {
    match strategy {
        SplitStrategy::Deterministic => SplitPrStrategy::Deterministic,
        SplitStrategy::Auto => SplitPrStrategy::Auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn spec_row(
        task_id: &str,
        sprint: i32,
        pr_group: &str,
        grouping: PrGrouping,
        owner: &str,
        branch: &str,
        worktree: &str,
        notes: &str,
    ) -> TaskSpecRow {
        TaskSpecRow {
            task_id: task_id.to_string(),
            summary: format!("Summary for {task_id}"),
            branch: branch.to_string(),
            worktree: worktree.to_string(),
            owner: owner.to_string(),
            notes: notes.to_string(),
            pr_group: pr_group.to_string(),
            sprint,
            grouping,
        }
    }

    #[test]
    fn execution_mode_by_task_auto_single_lane_uses_per_sprint() {
        let rows = vec![
            spec_row(
                "S1T1",
                1,
                "s1-auto-g1",
                PrGrouping::Group,
                "subagent-s1-t1",
                "issue/s1-t1",
                "wt-1",
                "sprint=S1; plan-task:Task 1.1; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
            spec_row(
                "S1T2",
                1,
                "s1-auto-g1",
                PrGrouping::Group,
                "subagent-s1-t2",
                "issue/s1-t2",
                "wt-2",
                "sprint=S1; plan-task:Task 1.2; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
        ];

        let modes = execution_mode_by_task(&rows, SplitStrategy::Auto);
        assert_eq!(modes.get("S1T1").map(String::as_str), Some("per-sprint"));
        assert_eq!(modes.get("S1T2").map(String::as_str), Some("per-sprint"));
    }

    #[test]
    fn execution_mode_by_task_deterministic_single_lane_uses_per_sprint() {
        let rows = vec![
            spec_row(
                "S1T1",
                1,
                "s1-serial",
                PrGrouping::Group,
                "subagent-s1-t1",
                "issue/s1-t1",
                "wt-1",
                "sprint=S1; plan-task:Task 1.1; pr-group=s1-serial; shared-pr-anchor=S1T1",
            ),
            spec_row(
                "S1T2",
                1,
                "s1-serial",
                PrGrouping::Group,
                "subagent-s1-t1",
                "issue/s1-t1",
                "wt-1",
                "sprint=S1; plan-task:Task 1.2; pr-group=s1-serial; shared-pr-anchor=S1T1",
            ),
        ];

        let modes = execution_mode_by_task(&rows, SplitStrategy::Deterministic);
        assert_eq!(modes.get("S1T1").map(String::as_str), Some("per-sprint"));
        assert_eq!(modes.get("S1T2").map(String::as_str), Some("per-sprint"));
    }

    #[test]
    fn execution_mode_by_task_auto_multi_group_keeps_group_modes() {
        let rows = vec![
            spec_row(
                "S2T1",
                2,
                "s2-auto-g1",
                PrGrouping::Group,
                "subagent-s2-t1",
                "issue/s2-t1",
                "wt-1",
                "sprint=S2; plan-task:Task 2.1; pr-group=s2-auto-g1",
            ),
            spec_row(
                "S2T2",
                2,
                "s2-auto-g1",
                PrGrouping::Group,
                "subagent-s2-t2",
                "issue/s2-t2",
                "wt-2",
                "sprint=S2; plan-task:Task 2.2; pr-group=s2-auto-g1",
            ),
            spec_row(
                "S2T3",
                2,
                "s2-auto-g2",
                PrGrouping::Group,
                "subagent-s2-t3",
                "issue/s2-t3",
                "wt-3",
                "sprint=S2; plan-task:Task 2.3; pr-group=s2-auto-g2",
            ),
        ];

        let modes = execution_mode_by_task(&rows, SplitStrategy::Auto);
        assert_eq!(modes.get("S2T1").map(String::as_str), Some("pr-shared"));
        assert_eq!(modes.get("S2T2").map(String::as_str), Some("pr-shared"));
        assert_eq!(modes.get("S2T3").map(String::as_str), Some("pr-isolated"));
    }

    #[test]
    fn canonical_lane_anchor_uses_stable_task_order_even_when_notes_disagree() {
        let rows = vec![
            spec_row(
                "S1T1",
                1,
                "s1-auto-g1",
                PrGrouping::Group,
                "subagent-s1-t1",
                "issue/s1-t1",
                "wt-1",
                "sprint=S1; plan-task:Task 1.1; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
            spec_row(
                "S1T2",
                1,
                "s1-auto-g1",
                PrGrouping::Group,
                "subagent-s1-t2",
                "issue/s1-t2",
                "wt-2",
                "sprint=S1; plan-task:Task 1.2; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
        ];

        assert_eq!(
            canonical_lane_anchor_task_id(&rows, 1, "s1-auto-g1"),
            Some("S1T1".to_string())
        );
    }

    #[test]
    fn canonical_lane_anchor_uses_deterministic_task_id_fallback_when_note_absent() {
        let rows = vec![
            spec_row(
                "S4T3",
                4,
                "s4-auto-g2",
                PrGrouping::Group,
                "subagent-s4-t3",
                "issue/s4-t3",
                "wt-3",
                "sprint=S4; plan-task:Task 4.3; pr-group=s4-auto-g2",
            ),
            spec_row(
                "S4T1",
                4,
                "s4-auto-g2",
                PrGrouping::Group,
                "subagent-s4-t1",
                "issue/s4-t1",
                "wt-1",
                "sprint=S4; plan-task:Task 4.1; pr-group=s4-auto-g2",
            ),
            spec_row(
                "S4T2",
                4,
                "s4-auto-g2",
                PrGrouping::Group,
                "subagent-s4-t2",
                "issue/s4-t2",
                "wt-2",
                "sprint=S4; plan-task:Task 4.2; pr-group=s4-auto-g2",
            ),
        ];

        assert_eq!(
            canonical_lane_anchor_task_id(&rows, 4, "s4-auto-g2"),
            Some("S4T1".to_string())
        );
    }

    #[test]
    fn runtime_lane_canonicalization_uses_shared_anchor_metadata() {
        let rows = vec![
            spec_row(
                "S1T1",
                1,
                "s1-auto-g1",
                PrGrouping::Group,
                "subagent-s1-t1",
                "issue/s1-t1",
                "wt-1",
                "sprint=S1; plan-task:Task 1.1; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
            spec_row(
                "S1T2",
                1,
                "s1-auto-g1",
                PrGrouping::Group,
                "subagent-s1-t2",
                "issue/s1-t2",
                "wt-2",
                "sprint=S1; plan-task:Task 1.2; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
        ];

        let runtime_by_task = runtime_lane_metadata_by_task(&rows, SplitStrategy::Auto);
        let expected_anchor = runtime_by_task
            .get("S1T2")
            .expect("anchor runtime lane metadata")
            .clone();

        for row in rows
            .iter()
            .filter(|row| row.sprint == 1 && row.pr_group == "s1-auto-g1")
        {
            let lane = runtime_by_task
                .get(&row.task_id)
                .expect("runtime lane metadata");
            assert_eq!(lane.execution_mode, "per-sprint");
            assert_eq!(
                lane.owner, expected_anchor.owner,
                "task {} owner should match anchor",
                row.task_id
            );
            assert_eq!(
                lane.branch, expected_anchor.branch,
                "task {} branch should match anchor",
                row.task_id
            );
            assert_eq!(
                lane.worktree, expected_anchor.worktree,
                "task {} worktree should match anchor",
                row.task_id
            );
        }

        let rerun = runtime_lane_metadata_by_task(&rows, SplitStrategy::Auto);
        assert_eq!(runtime_by_task, rerun);
    }
}
