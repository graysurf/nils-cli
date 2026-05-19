use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::parse::{Plan, Sprint, parse_plan_with_display};

const USAGE: &str = r#"Usage:
  plan-tooling split-prs --file <plan.md> [options]

Purpose:
  Build task-to-PR split records from a Plan Format v1 file.

Required:
  --file <path>                    Plan file to parse

Options:
  --scope <plan|sprint>            Scope to split (default: sprint)
  --sprint <n>                     Sprint number when --scope sprint
  --pr-group <task=group>          Group pin; repeatable (group mode only)
                                   deterministic/group: required for every task
                                   auto/group lanes: optional pins + auto assignment for remaining tasks
  --pr-grouping <mode>             deterministic only: per-sprint | group
  --default-pr-grouping <mode>     auto fallback when sprint metadata omits grouping intent
  --strategy <deterministic|auto>  Split strategy (default: deterministic)
  --explain                        Include grouping rationale in JSON output
  --owner-prefix <text>            Owner prefix (default: subagent)
  --branch-prefix <text>           Branch prefix (default: issue)
  --worktree-prefix <text>         Worktree prefix (default: issue__)
  --format <json|tsv>              Output format (default: json)
  -h, --help                       Show help

Argument style:
  --key value and --key=value are both accepted for value options.

Exit:
  0: success
  1: runtime or validation error
  2: usage error
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitScope {
    Plan,
    Sprint(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitPrGrouping {
    PerSprint,
    Group,
}

impl SplitPrGrouping {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerSprint => "per-sprint",
            Self::Group => "group",
        }
    }

    fn from_cli(value: &str) -> Option<Self> {
        match value {
            "per-sprint" | "per-spring" => Some(Self::PerSprint),
            "group" => Some(Self::Group),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitPrStrategy {
    Deterministic,
    Auto,
}

impl SplitPrStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::Auto => "auto",
        }
    }

    fn from_cli(value: &str) -> Option<Self> {
        match value {
            "deterministic" => Some(Self::Deterministic),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPlanOptions {
    pub pr_grouping: Option<SplitPrGrouping>,
    pub default_pr_grouping: Option<SplitPrGrouping>,
    pub strategy: SplitPrStrategy,
    pub pr_group_entries: Vec<String>,
    pub owner_prefix: String,
    pub branch_prefix: String,
    pub worktree_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPlanRecord {
    pub task_id: String,
    pub sprint: i32,
    pub summary: String,
    pub pr_group: String,
}

#[derive(Debug, Clone)]
struct Record {
    task_id: String,
    plan_task_id: String,
    sprint: i32,
    summary: String,
    complexity: i32,
    location_paths: Vec<String>,
    dependency_keys: Vec<String>,
    pr_group: String,
}

#[derive(Debug, Clone, Default)]
struct AutoSprintHint {
    pr_grouping_intent: Option<SplitPrGrouping>,
    execution_profile: Option<String>,
    target_parallel_width: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedPrGroupingSource {
    CommandPrGrouping,
    PlanMetadata,
    DefaultPrGrouping,
}

impl ResolvedPrGroupingSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::CommandPrGrouping => "command-pr-grouping",
            Self::PlanMetadata => "plan-metadata",
            Self::DefaultPrGrouping => "default-pr-grouping",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPrGrouping {
    pub grouping: SplitPrGrouping,
    pub source: ResolvedPrGroupingSource,
}

#[derive(Debug, Serialize)]
struct Output {
    file: String,
    scope: String,
    sprint: Option<i32>,
    pr_grouping: String,
    strategy: String,
    records: Vec<OutputRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explain: Option<Vec<ExplainSprint>>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct OutputRecord {
    task_id: String,
    summary: String,
    pr_group: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ExplainSprint {
    sprint: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_parallel_width: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_grouping_intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_grouping_intent_source: Option<String>,
    groups: Vec<ExplainGroup>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ExplainGroup {
    pr_group: String,
    task_ids: Vec<String>,
    anchor: String,
}

pub fn run(args: &[String]) -> i32 {
    let mut file: Option<String> = None;
    let mut scope = String::from("sprint");
    let mut sprint: Option<String> = None;
    let mut pr_grouping: Option<String> = None;
    let mut default_pr_grouping: Option<String> = None;
    let mut pr_group_entries: Vec<String> = Vec::new();
    let mut strategy = String::from("deterministic");
    let mut explain = false;
    let mut owner_prefix = String::from("subagent");
    let mut branch_prefix = String::from("issue");
    let mut worktree_prefix = String::from("issue__");
    let mut format = String::from("json");

    let mut i = 0usize;
    while i < args.len() {
        let raw_arg = args[i].as_str();
        let (flag, inline_value) = split_value_arg(raw_arg);
        match flag {
            "--file" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--file") else {
                    return die("missing value for --file");
                };
                file = Some(v);
                i = next_i;
            }
            "--scope" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--scope") else {
                    return die("missing value for --scope");
                };
                scope = v;
                i = next_i;
            }
            "--sprint" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--sprint")
                else {
                    return die("missing value for --sprint");
                };
                sprint = Some(v);
                i = next_i;
            }
            "--pr-grouping" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--pr-grouping")
                else {
                    return die("missing value for --pr-grouping");
                };
                pr_grouping = Some(v);
                i = next_i;
            }
            "--default-pr-grouping" => {
                let Ok((v, next_i)) =
                    consume_option_value(args, i, inline_value, "--default-pr-grouping")
                else {
                    return die("missing value for --default-pr-grouping");
                };
                default_pr_grouping = Some(v);
                i = next_i;
            }
            "--pr-group" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--pr-group")
                else {
                    return die("missing value for --pr-group");
                };
                pr_group_entries.push(v);
                i = next_i;
            }
            "--strategy" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--strategy")
                else {
                    return die("missing value for --strategy");
                };
                strategy = v;
                i = next_i;
            }
            "--explain" => {
                if inline_value.is_some() {
                    return die("unexpected value for --explain");
                }
                explain = true;
                i += 1;
            }
            "--owner-prefix" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--owner-prefix")
                else {
                    return die("missing value for --owner-prefix");
                };
                owner_prefix = v;
                i = next_i;
            }
            "--branch-prefix" => {
                let Ok((v, next_i)) =
                    consume_option_value(args, i, inline_value, "--branch-prefix")
                else {
                    return die("missing value for --branch-prefix");
                };
                branch_prefix = v;
                i = next_i;
            }
            "--worktree-prefix" => {
                let Ok((v, next_i)) =
                    consume_option_value(args, i, inline_value, "--worktree-prefix")
                else {
                    return die("missing value for --worktree-prefix");
                };
                worktree_prefix = v;
                i = next_i;
            }
            "--format" => {
                let Ok((v, next_i)) = consume_option_value(args, i, inline_value, "--format")
                else {
                    return die("missing value for --format");
                };
                format = v;
                i = next_i;
            }
            "-h" | "--help" => {
                if inline_value.is_some() {
                    return die(&format!("unknown argument: {raw_arg}"));
                }
                print_usage();
                return 0;
            }
            _ => {
                return die(&format!("unknown argument: {raw_arg}"));
            }
        }
    }

    let Some(file_arg) = file else {
        print_usage();
        return 2;
    };
    if scope != "plan" && scope != "sprint" {
        return die(&format!(
            "invalid --scope (expected plan|sprint): {}",
            crate::repr::py_repr(&scope)
        ));
    }
    if strategy != "deterministic" && strategy != "auto" {
        return die(&format!(
            "invalid --strategy (expected deterministic|auto): {}",
            crate::repr::py_repr(&strategy)
        ));
    }
    if format != "json" && format != "tsv" {
        return die(&format!(
            "invalid --format (expected json|tsv): {}",
            crate::repr::py_repr(&format)
        ));
    }

    let sprint_num = if scope == "sprint" {
        let Some(raw) = sprint.as_deref() else {
            return die("--sprint is required when --scope sprint");
        };
        match raw.parse::<i32>() {
            Ok(v) if v > 0 => Some(v),
            _ => {
                eprintln!(
                    "error: invalid --sprint (expected positive int): {}",
                    crate::repr::py_repr(raw)
                );
                return 2;
            }
        }
    } else {
        None
    };

    if let Some(value) = pr_grouping.as_deref()
        && SplitPrGrouping::from_cli(value).is_none()
    {
        return die(&format!(
            "invalid --pr-grouping (expected per-sprint|group): {}",
            crate::repr::py_repr(value)
        ));
    }
    if let Some(value) = default_pr_grouping.as_deref()
        && SplitPrGrouping::from_cli(value).is_none()
    {
        return die(&format!(
            "invalid --default-pr-grouping (expected per-sprint|group): {}",
            crate::repr::py_repr(value)
        ));
    }

    if strategy == "deterministic" {
        let Some(grouping) = pr_grouping.as_deref() else {
            return die("--strategy deterministic requires --pr-grouping <per-sprint|group>");
        };
        if default_pr_grouping.is_some() {
            return die("--default-pr-grouping is only valid when --strategy auto");
        }
        if grouping == "group" && pr_group_entries.is_empty() {
            return die(
                "--pr-grouping group requires at least one --pr-group <task-or-plan-id>=<group> entry",
            );
        }
        if grouping != "group" && !pr_group_entries.is_empty() {
            return die("--pr-group can only be used when --pr-grouping group");
        }
    } else if pr_grouping.is_some() {
        return die(
            "--pr-grouping cannot be used with --strategy auto; use sprint metadata or --default-pr-grouping",
        );
    }

    let repo_root = crate::repo_root::detect();
    let display_path = file_arg.clone();
    let read_path = resolve_repo_relative(&repo_root, Path::new(&file_arg));
    if !read_path.is_file() {
        eprintln!("error: plan file not found: {display_path}");
        return 1;
    }

    let plan: Plan;
    let parse_errors: Vec<String>;
    match parse_plan_with_display(&read_path, &display_path) {
        Ok((p, errs)) => {
            plan = p;
            parse_errors = errs;
        }
        Err(err) => {
            eprintln!("error: {display_path}: {err}");
            return 1;
        }
    }
    if !parse_errors.is_empty() {
        for err in parse_errors {
            eprintln!("error: {display_path}: error: {err}");
        }
        return 1;
    }

    let split_scope = match scope.as_str() {
        "plan" => SplitScope::Plan,
        "sprint" => {
            let Some(want) = sprint_num else {
                return die("internal error: missing sprint number");
            };
            SplitScope::Sprint(want)
        }
        _ => return die("internal error: invalid scope"),
    };
    let Some(strategy_mode) = SplitPrStrategy::from_cli(&strategy) else {
        return die("internal error: invalid strategy");
    };

    let selected_sprints = match select_sprints_for_scope(&plan, split_scope) {
        Ok(sprints) => sprints,
        Err(err) => {
            eprintln!("error: {display_path}: {err}");
            return 1;
        }
    };
    let sprint_hints = sprint_hints(&selected_sprints);

    let options = SplitPlanOptions {
        pr_grouping: pr_grouping.as_deref().and_then(SplitPrGrouping::from_cli),
        default_pr_grouping: default_pr_grouping
            .as_deref()
            .and_then(SplitPrGrouping::from_cli),
        strategy: strategy_mode,
        pr_group_entries,
        owner_prefix,
        branch_prefix,
        worktree_prefix,
    };
    let resolved_grouping = match resolve_pr_grouping_by_sprint(&selected_sprints, &options) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };
    let split_records = match build_split_plan_records(&selected_sprints, &options) {
        Ok(records) => records,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };
    let explain_payload = if explain {
        Some(build_explain_payload(
            &split_records,
            &sprint_hints,
            &resolved_grouping,
        ))
    } else {
        None
    };

    let out_records: Vec<OutputRecord> = split_records
        .iter()
        .map(OutputRecord::from_split_record)
        .collect();

    if format == "tsv" {
        print_tsv(&out_records);
        return 0;
    }

    let output = Output {
        file: path_to_posix(&maybe_relativize(&read_path, &repo_root)),
        scope: scope.clone(),
        sprint: sprint_num,
        pr_grouping: summarize_resolved_grouping(&resolved_grouping),
        strategy,
        records: out_records,
        explain: explain_payload,
    };
    match serde_json::to_string(&output) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(err) => {
            eprintln!("error: failed to encode JSON: {err}");
            1
        }
    }
}

impl OutputRecord {
    fn from_split_record(record: &SplitPlanRecord) -> Self {
        Self {
            task_id: record.task_id.clone(),
            summary: record.summary.clone(),
            pr_group: record.pr_group.clone(),
        }
    }
}

pub fn select_sprints_for_scope(plan: &Plan, scope: SplitScope) -> Result<Vec<Sprint>, String> {
    let selected = match scope {
        SplitScope::Plan => plan
            .sprints
            .iter()
            .filter(|s| !s.tasks.is_empty())
            .cloned()
            .collect::<Vec<_>>(),
        SplitScope::Sprint(want) => match plan.sprints.iter().find(|s| s.number == want) {
            Some(sprint) if !sprint.tasks.is_empty() => vec![sprint.clone()],
            Some(_) => return Err(format!("sprint {want} has no tasks")),
            None => return Err(format!("sprint not found: {want}")),
        },
    };
    if selected.is_empty() {
        return Err("selected scope has no tasks".to_string());
    }
    Ok(selected)
}

pub fn build_split_plan_records(
    selected_sprints: &[Sprint],
    options: &SplitPlanOptions,
) -> Result<Vec<SplitPlanRecord>, String> {
    if selected_sprints.is_empty() {
        return Err("selected scope has no tasks".to_string());
    }

    let sprint_hints = sprint_hints(selected_sprints);
    let resolved_grouping = resolve_pr_grouping_by_sprint(selected_sprints, options)?;

    let mut records: Vec<Record> = Vec::new();
    for sprint in selected_sprints {
        for (idx, task) in sprint.tasks.iter().enumerate() {
            let ordinal = idx + 1;
            let task_id = format!("S{}T{ordinal}", sprint.number);
            let plan_task_id = task.id.trim().to_string();
            let summary = normalize_spaces(if task.name.trim().is_empty() {
                if plan_task_id.is_empty() {
                    format!("sprint-{}-task-{ordinal}", sprint.number)
                } else {
                    plan_task_id.clone()
                }
            } else {
                task.name.trim().to_string()
            });
            let deps: Vec<String> = task
                .dependencies
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.id.trim().to_string())
                .filter(|d| !d.is_empty())
                .filter(|d| !is_placeholder(d))
                .collect();
            let location_paths: Vec<String> = task
                .location
                .iter()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .filter(|p| !is_placeholder(p))
                .collect();
            let complexity = match task.complexity {
                Some(value) if value > 0 => value,
                _ => 5,
            };

            records.push(Record {
                task_id,
                plan_task_id,
                sprint: sprint.number,
                summary,
                complexity,
                location_paths,
                dependency_keys: deps,
                pr_group: String::new(),
            });
        }
    }

    if records.is_empty() {
        return Err("selected scope has no tasks".to_string());
    }

    let mut group_assignments: HashMap<String, String> = HashMap::new();
    let mut assignment_sources: Vec<String> = Vec::new();
    for entry in &options.pr_group_entries {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((raw_key, raw_group)) = trimmed.split_once('=') else {
            return Err("--pr-group must use <task-or-plan-id>=<group> format".to_string());
        };
        let key = raw_key.trim();
        let group = normalize_token(raw_group.trim(), "", 48);
        if key.is_empty() || group.is_empty() {
            return Err("--pr-group must include both task key and group".to_string());
        }
        assignment_sources.push(key.to_string());
        group_assignments.insert(key.to_ascii_lowercase(), group);
    }

    if !assignment_sources.is_empty() {
        let mut known: HashMap<String, bool> = HashMap::new();
        for rec in &records {
            known.insert(rec.task_id.to_ascii_lowercase(), true);
            if !rec.plan_task_id.is_empty() {
                known.insert(rec.plan_task_id.to_ascii_lowercase(), true);
            }
        }

        let unknown: Vec<String> = assignment_sources
            .iter()
            .filter(|key| !known.contains_key(&key.to_ascii_lowercase()))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "--pr-group references unknown task keys: {}",
                unknown
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    let mut missing: Vec<String> = Vec::new();
    let mut invalid_pin_targets: Vec<String> = Vec::new();
    for rec in &mut records {
        let grouping = resolved_grouping
            .get(&rec.sprint)
            .map(|value| value.grouping)
            .ok_or_else(|| format!("missing resolved grouping for sprint {}", rec.sprint))?;

        let mut pinned_group: Option<String> = None;
        for key in [&rec.task_id, &rec.plan_task_id] {
            if key.is_empty() {
                continue;
            }
            if let Some(v) = group_assignments.get(&key.to_ascii_lowercase()) {
                pinned_group = Some(v.to_string());
                break;
            }
        }

        match grouping {
            SplitPrGrouping::PerSprint => {
                if pinned_group.is_some() {
                    invalid_pin_targets.push(rec.task_id.clone());
                }
                rec.pr_group =
                    normalize_token(&format!("s{}", rec.sprint), &format!("s{}", rec.sprint), 48);
            }
            SplitPrGrouping::Group => {
                rec.pr_group = pinned_group.unwrap_or_default();
                if rec.pr_group.is_empty() {
                    missing.push(rec.task_id.clone());
                }
            }
        }
    }

    if !invalid_pin_targets.is_empty() {
        return Err(format!(
            "--pr-group cannot target auto lanes resolved as per-sprint; offending tasks: {}",
            invalid_pin_targets
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if options.strategy == SplitPrStrategy::Deterministic {
        if !missing.is_empty() {
            return Err(format!(
                "--pr-grouping group requires explicit mapping for every task; missing: {}",
                missing
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    } else if !missing.is_empty() {
        assign_auto_groups(&mut records, &sprint_hints);
    }

    let mut out: Vec<SplitPlanRecord> = Vec::new();
    for rec in records {
        out.push(SplitPlanRecord {
            task_id: rec.task_id,
            sprint: rec.sprint,
            summary: rec.summary,
            pr_group: rec.pr_group,
        });
    }

    Ok(out)
}

pub fn resolve_pr_grouping_by_sprint(
    selected_sprints: &[Sprint],
    options: &SplitPlanOptions,
) -> Result<HashMap<i32, ResolvedPrGrouping>, String> {
    if selected_sprints.is_empty() {
        return Err("selected scope has no tasks".to_string());
    }

    match options.strategy {
        SplitPrStrategy::Deterministic => {
            let Some(grouping) = options.pr_grouping else {
                return Err(
                    "--strategy deterministic requires --pr-grouping <per-sprint|group>"
                        .to_string(),
                );
            };
            if options.default_pr_grouping.is_some() {
                return Err("--default-pr-grouping is only valid when --strategy auto".to_string());
            }

            let mut mismatches: Vec<String> = Vec::new();
            let mut out: HashMap<i32, ResolvedPrGrouping> = HashMap::new();
            for sprint in selected_sprints {
                if let Some(intent) = sprint.metadata.pr_grouping_intent.as_deref()
                    && intent != grouping.as_str()
                {
                    mismatches.push(format!(
                        "S{} metadata `PR grouping intent={intent}` conflicts with `--pr-grouping {}`",
                        sprint.number,
                        grouping.as_str()
                    ));
                }
                out.insert(
                    sprint.number,
                    ResolvedPrGrouping {
                        grouping,
                        source: ResolvedPrGroupingSource::CommandPrGrouping,
                    },
                );
            }

            if mismatches.is_empty() {
                Ok(out)
            } else {
                Err(format!(
                    "plan metadata/CLI grouping mismatch: {}",
                    mismatches.join(" | ")
                ))
            }
        }
        SplitPrStrategy::Auto => {
            if options.pr_grouping.is_some() {
                return Err(
                    "--pr-grouping cannot be used with --strategy auto; use sprint metadata or --default-pr-grouping"
                        .to_string(),
                );
            }

            let mut out: HashMap<i32, ResolvedPrGrouping> = HashMap::new();
            let mut missing: Vec<String> = Vec::new();
            for sprint in selected_sprints {
                let resolved = if let Some(intent) = sprint
                    .metadata
                    .pr_grouping_intent
                    .as_deref()
                    .and_then(SplitPrGrouping::from_cli)
                {
                    Some(ResolvedPrGrouping {
                        grouping: intent,
                        source: ResolvedPrGroupingSource::PlanMetadata,
                    })
                } else {
                    options
                        .default_pr_grouping
                        .map(|grouping| ResolvedPrGrouping {
                            grouping,
                            source: ResolvedPrGroupingSource::DefaultPrGrouping,
                        })
                };

                if let Some(value) = resolved {
                    out.insert(sprint.number, value);
                } else {
                    missing.push(format!("S{}", sprint.number));
                }
            }

            if missing.is_empty() {
                Ok(out)
            } else {
                Err(format!(
                    "auto grouping requires `PR grouping intent` metadata for every selected sprint or --default-pr-grouping <per-sprint|group>; missing: {}",
                    missing.join(", ")
                ))
            }
        }
    }
}

#[derive(Debug)]
struct AutoMergeCandidate {
    i: usize,
    j: usize,
    score_key: i64,
    key_a: String,
    key_b: String,
}

#[derive(Debug)]
struct ForcedMergeCandidate {
    i: usize,
    j: usize,
    span: usize,
    complexity: i32,
    key_a: String,
    key_b: String,
}

fn assign_auto_groups(records: &mut [Record], hints: &HashMap<i32, AutoSprintHint>) {
    let mut sprint_to_indices: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (idx, rec) in records.iter().enumerate() {
        if rec.pr_group.is_empty() {
            sprint_to_indices.entry(rec.sprint).or_default().push(idx);
        }
    }

    for (sprint, indices) in sprint_to_indices {
        let hint = hints.get(&sprint).cloned().unwrap_or_default();
        let assignments = auto_groups_for_sprint(records, sprint, &indices, &hint);
        for (idx, group) in assignments {
            if let Some(rec) = records.get_mut(idx)
                && rec.pr_group.is_empty()
            {
                rec.pr_group = group;
            }
        }
    }
}

fn auto_groups_for_sprint(
    records: &[Record],
    sprint: i32,
    indices: &[usize],
    hint: &AutoSprintHint,
) -> BTreeMap<usize, String> {
    let mut lookup: HashMap<String, usize> = HashMap::new();
    for idx in indices {
        let rec = &records[*idx];
        lookup.insert(rec.task_id.to_ascii_lowercase(), *idx);
        if !rec.plan_task_id.is_empty() {
            lookup.insert(rec.plan_task_id.to_ascii_lowercase(), *idx);
        }
    }

    let mut deps: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut paths: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for idx in indices {
        let rec = &records[*idx];
        let mut resolved_deps: BTreeSet<usize> = BTreeSet::new();
        for dep in &rec.dependency_keys {
            let dep_key = dep.trim().to_ascii_lowercase();
            if dep_key.is_empty() {
                continue;
            }
            if let Some(dep_idx) = lookup.get(&dep_key)
                && dep_idx != idx
            {
                resolved_deps.insert(*dep_idx);
            }
        }
        deps.insert(*idx, resolved_deps);

        let normalized_paths: BTreeSet<String> = rec
            .location_paths
            .iter()
            .map(|path| normalize_location_path(path))
            .filter(|path| !path.is_empty())
            .collect();
        paths.insert(*idx, normalized_paths);
    }

    let batch_by_idx = compute_batch_index(records, indices, &deps);
    let mut parent: HashMap<usize, usize> = indices.iter().copied().map(|idx| (idx, idx)).collect();

    let mut by_batch: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for idx in indices {
        let batch = batch_by_idx.get(idx).copied().unwrap_or(0);
        by_batch.entry(batch).or_default().push(*idx);
    }

    for members in by_batch.values_mut() {
        members.sort_by_key(|idx| task_sort_key(records, *idx));

        let mut path_to_members: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for idx in members {
            for path in paths.get(idx).into_iter().flatten() {
                path_to_members.entry(path.clone()).or_default().push(*idx);
            }
        }
        for overlap_members in path_to_members.values() {
            if overlap_members.len() < 2 {
                continue;
            }
            let first = overlap_members[0];
            for other in overlap_members.iter().skip(1) {
                uf_union(&mut parent, first, *other);
            }
        }
    }

    let mut grouped: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for idx in indices {
        let root = uf_find(&mut parent, *idx);
        grouped.entry(root).or_default().insert(*idx);
    }
    let mut groups: Vec<BTreeSet<usize>> = grouped.into_values().collect();
    let target_group_count = desired_auto_group_count(indices.len(), hint);

    loop {
        if let Some(target) = target_group_count
            && groups.len() <= target
        {
            break;
        }

        let mut candidates: Vec<AutoMergeCandidate> = Vec::new();
        for i in 0..groups.len() {
            for j in (i + 1)..groups.len() {
                let merged_complexity =
                    group_complexity(records, &groups[i]) + group_complexity(records, &groups[j]);
                if merged_complexity > 20 {
                    continue;
                }

                let dep_cross = dependency_cross_edges(&deps, &groups[i], &groups[j]);
                let overlap_paths = overlap_path_count(&paths, &groups[i], &groups[j]);
                let min_group_size = groups[i].len().min(groups[j].len()).max(1) as f64;
                let dep_affinity = ((dep_cross as f64) / min_group_size).min(1.0);
                let ovl_affinity = ((overlap_paths as f64) / 2.0).min(1.0);
                let size_fit = (1.0 - ((merged_complexity as f64 - 12.0).abs() / 12.0)).max(0.0);
                let span = group_span(&batch_by_idx, &groups[i], &groups[j]);
                let serial_penalty = ((span as f64 - 1.0).max(0.0)) / 3.0;
                let oversize_penalty = ((merged_complexity as f64 - 20.0).max(0.0)) / 20.0;

                let score = (0.45 * dep_affinity) + (0.35 * ovl_affinity) + (0.20 * size_fit)
                    - (0.25 * serial_penalty)
                    - (0.45 * oversize_penalty);
                if score < 0.30 {
                    continue;
                }

                let mut key_a = group_min_task_key(records, &groups[i]);
                let mut key_b = group_min_task_key(records, &groups[j]);
                if key_b < key_a {
                    std::mem::swap(&mut key_a, &mut key_b);
                }
                candidates.push(AutoMergeCandidate {
                    i,
                    j,
                    score_key: (score * 1_000_000.0).round() as i64,
                    key_a,
                    key_b,
                });
            }
        }

        if candidates.is_empty() {
            if let Some(target) = target_group_count
                && groups.len() > target
                && let Some(chosen) = pick_forced_merge(records, &batch_by_idx, &groups)
            {
                let mut merged = groups[chosen.i].clone();
                merged.extend(groups[chosen.j].iter().copied());
                groups[chosen.i] = merged;
                groups.remove(chosen.j);
                continue;
            }
            break;
        }

        candidates.sort_by(|a, b| {
            b.score_key
                .cmp(&a.score_key)
                .then_with(|| a.key_a.cmp(&b.key_a))
                .then_with(|| a.key_b.cmp(&b.key_b))
                .then_with(|| a.i.cmp(&b.i))
                .then_with(|| a.j.cmp(&b.j))
        });
        let chosen = &candidates[0];

        let mut merged = groups[chosen.i].clone();
        merged.extend(groups[chosen.j].iter().copied());
        groups[chosen.i] = merged;
        groups.remove(chosen.j);
    }

    groups.sort_by(|a, b| {
        group_min_batch(&batch_by_idx, a)
            .cmp(&group_min_batch(&batch_by_idx, b))
            .then_with(|| group_min_task_key(records, a).cmp(&group_min_task_key(records, b)))
    });

    let mut out: BTreeMap<usize, String> = BTreeMap::new();
    for (idx, group) in groups.iter().enumerate() {
        let fallback = format!("s{sprint}-auto-g{}", idx + 1);
        let group_key = normalize_token(&fallback, &fallback, 48);
        for member in group {
            out.insert(*member, group_key.clone());
        }
    }
    out
}

fn compute_batch_index(
    records: &[Record],
    indices: &[usize],
    deps: &BTreeMap<usize, BTreeSet<usize>>,
) -> BTreeMap<usize, usize> {
    let mut in_deg: HashMap<usize, usize> = indices.iter().copied().map(|idx| (idx, 0)).collect();
    let mut reverse: HashMap<usize, BTreeSet<usize>> = indices
        .iter()
        .copied()
        .map(|idx| (idx, BTreeSet::new()))
        .collect();

    for idx in indices {
        for dep in deps.get(idx).cloned().unwrap_or_default() {
            if !in_deg.contains_key(&dep) {
                continue;
            }
            if let Some(value) = in_deg.get_mut(idx) {
                *value += 1;
            }
            if let Some(children) = reverse.get_mut(&dep) {
                children.insert(*idx);
            }
        }
    }

    let mut remaining: BTreeSet<usize> = indices.iter().copied().collect();
    let mut batch_by_idx: BTreeMap<usize, usize> = BTreeMap::new();
    let mut layer = 0usize;
    let mut ready: VecDeque<usize> = {
        let mut start: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|idx| in_deg.get(idx).copied().unwrap_or(0) == 0)
            .collect();
        start.sort_by_key(|idx| task_sort_key(records, *idx));
        start.into_iter().collect()
    };

    while !remaining.is_empty() {
        let mut batch_members: Vec<usize> = ready.drain(..).collect();
        batch_members.sort_by_key(|idx| task_sort_key(records, *idx));

        if batch_members.is_empty() {
            let mut cycle_members: Vec<usize> = remaining.iter().copied().collect();
            cycle_members.sort_by_key(|idx| task_sort_key(records, *idx));
            for idx in cycle_members {
                remaining.remove(&idx);
                batch_by_idx.insert(idx, layer);
            }
            break;
        }

        for idx in &batch_members {
            remaining.remove(idx);
            batch_by_idx.insert(*idx, layer);
        }

        let mut next: Vec<usize> = Vec::new();
        for idx in batch_members {
            for child in reverse.get(&idx).cloned().unwrap_or_default() {
                if let Some(value) = in_deg.get_mut(&child) {
                    *value = value.saturating_sub(1);
                    if *value == 0 && remaining.contains(&child) {
                        next.push(child);
                    }
                }
            }
        }
        next.sort_by_key(|idx| task_sort_key(records, *idx));
        next.dedup();
        ready.extend(next);
        layer += 1;
    }

    for idx in indices {
        batch_by_idx.entry(*idx).or_insert(0);
    }
    batch_by_idx
}

fn task_sort_key(records: &[Record], idx: usize) -> (String, String) {
    let rec = &records[idx];
    let primary = if rec.plan_task_id.trim().is_empty() {
        rec.task_id.to_ascii_lowercase()
    } else {
        rec.plan_task_id.to_ascii_lowercase()
    };
    (primary, rec.task_id.to_ascii_lowercase())
}

fn normalize_location_path(path: &str) -> String {
    path.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn group_complexity(records: &[Record], group: &BTreeSet<usize>) -> i32 {
    group
        .iter()
        .map(|idx| records[*idx].complexity.max(1))
        .sum::<i32>()
}

fn group_min_task_key(records: &[Record], group: &BTreeSet<usize>) -> String {
    group
        .iter()
        .map(|idx| task_sort_key(records, *idx).0)
        .min()
        .unwrap_or_default()
}

fn group_min_batch(batch_by_idx: &BTreeMap<usize, usize>, group: &BTreeSet<usize>) -> usize {
    group
        .iter()
        .filter_map(|idx| batch_by_idx.get(idx).copied())
        .min()
        .unwrap_or(0)
}

fn group_span(
    batch_by_idx: &BTreeMap<usize, usize>,
    left: &BTreeSet<usize>,
    right: &BTreeSet<usize>,
) -> usize {
    let mut min_batch = usize::MAX;
    let mut max_batch = 0usize;
    for idx in left.union(right) {
        let batch = batch_by_idx.get(idx).copied().unwrap_or(0);
        min_batch = min_batch.min(batch);
        max_batch = max_batch.max(batch);
    }
    if min_batch == usize::MAX {
        0
    } else {
        max_batch.saturating_sub(min_batch)
    }
}

fn dependency_cross_edges(
    deps: &BTreeMap<usize, BTreeSet<usize>>,
    left: &BTreeSet<usize>,
    right: &BTreeSet<usize>,
) -> usize {
    let mut count = 0usize;
    for src in left {
        if let Some(edges) = deps.get(src) {
            count += edges.iter().filter(|dep| right.contains(dep)).count();
        }
    }
    for src in right {
        if let Some(edges) = deps.get(src) {
            count += edges.iter().filter(|dep| left.contains(dep)).count();
        }
    }
    count
}

fn overlap_path_count(
    paths: &BTreeMap<usize, BTreeSet<String>>,
    left: &BTreeSet<usize>,
    right: &BTreeSet<usize>,
) -> usize {
    let mut left_paths: BTreeSet<String> = BTreeSet::new();
    let mut right_paths: BTreeSet<String> = BTreeSet::new();
    for idx in left {
        for path in paths.get(idx).into_iter().flatten() {
            left_paths.insert(path.clone());
        }
    }
    for idx in right {
        for path in paths.get(idx).into_iter().flatten() {
            right_paths.insert(path.clone());
        }
    }
    left_paths.intersection(&right_paths).count()
}

fn desired_auto_group_count(max_groups: usize, hint: &AutoSprintHint) -> Option<usize> {
    if max_groups == 0 {
        return None;
    }
    let preferred = hint
        .target_parallel_width
        .or_else(|| {
            if hint.execution_profile.as_deref() == Some("serial") {
                Some(1usize)
            } else {
                None
            }
        })
        .or_else(|| {
            if hint.pr_grouping_intent == Some(SplitPrGrouping::PerSprint) {
                Some(1usize)
            } else {
                None
            }
        })?;
    Some(preferred.clamp(1, max_groups))
}

fn pick_forced_merge(
    records: &[Record],
    batch_by_idx: &BTreeMap<usize, usize>,
    groups: &[BTreeSet<usize>],
) -> Option<ForcedMergeCandidate> {
    let mut chosen: Option<ForcedMergeCandidate> = None;
    for i in 0..groups.len() {
        for j in (i + 1)..groups.len() {
            let mut key_a = group_min_task_key(records, &groups[i]);
            let mut key_b = group_min_task_key(records, &groups[j]);
            if key_b < key_a {
                std::mem::swap(&mut key_a, &mut key_b);
            }
            let candidate = ForcedMergeCandidate {
                i,
                j,
                span: group_span(batch_by_idx, &groups[i], &groups[j]),
                complexity: group_complexity(records, &groups[i])
                    + group_complexity(records, &groups[j]),
                key_a,
                key_b,
            };
            let replace = match &chosen {
                None => true,
                Some(best) => {
                    (
                        candidate.span,
                        candidate.complexity,
                        &candidate.key_a,
                        &candidate.key_b,
                        candidate.i,
                        candidate.j,
                    ) < (
                        best.span,
                        best.complexity,
                        &best.key_a,
                        &best.key_b,
                        best.i,
                        best.j,
                    )
                }
            };
            if replace {
                chosen = Some(candidate);
            }
        }
    }
    chosen
}

fn sprint_hints(selected_sprints: &[Sprint]) -> HashMap<i32, AutoSprintHint> {
    let mut hints: HashMap<i32, AutoSprintHint> = HashMap::new();
    for sprint in selected_sprints {
        let pr_grouping_intent = sprint
            .metadata
            .pr_grouping_intent
            .as_deref()
            .and_then(SplitPrGrouping::from_cli);
        let execution_profile = sprint.metadata.execution_profile.clone();
        let target_parallel_width = sprint.metadata.parallel_width;
        hints.insert(
            sprint.number,
            AutoSprintHint {
                pr_grouping_intent,
                execution_profile,
                target_parallel_width,
            },
        );
    }
    hints
}

fn build_explain_payload(
    records: &[SplitPlanRecord],
    hints: &HashMap<i32, AutoSprintHint>,
    resolved_grouping: &HashMap<i32, ResolvedPrGrouping>,
) -> Vec<ExplainSprint> {
    let mut grouped: BTreeMap<i32, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for record in records {
        grouped
            .entry(record.sprint)
            .or_default()
            .entry(record.pr_group.clone())
            .or_default()
            .push(record.task_id.clone());
    }

    let mut out: Vec<ExplainSprint> = Vec::new();
    for (sprint, per_group) in grouped {
        let hint = hints.get(&sprint).cloned().unwrap_or_default();
        let groups = per_group
            .into_iter()
            .map(|(pr_group, task_ids)| {
                let anchor = task_ids.first().cloned().unwrap_or_default();
                ExplainGroup {
                    pr_group,
                    task_ids,
                    anchor,
                }
            })
            .collect::<Vec<_>>();
        out.push(ExplainSprint {
            sprint,
            target_parallel_width: hint.target_parallel_width,
            execution_profile: hint.execution_profile,
            pr_grouping_intent: resolved_grouping
                .get(&sprint)
                .map(|value| value.grouping.as_str().to_string()),
            pr_grouping_intent_source: resolved_grouping
                .get(&sprint)
                .map(|value| value.source.as_str().to_string()),
            groups,
        });
    }
    out
}

fn summarize_resolved_grouping(resolved_grouping: &HashMap<i32, ResolvedPrGrouping>) -> String {
    let unique = resolved_grouping
        .values()
        .map(|value| value.grouping.as_str())
        .collect::<BTreeSet<_>>();
    if unique.len() == 1 {
        unique
            .iter()
            .next()
            .map(|value| (*value).to_string())
            .unwrap_or_else(|| "mixed".to_string())
    } else {
        "mixed".to_string()
    }
}

fn split_value_arg(raw: &str) -> (&str, Option<&str>) {
    if raw.starts_with("--")
        && let Some((flag, value)) = raw.split_once('=')
        && !flag.is_empty()
    {
        return (flag, Some(value));
    }
    (raw, None)
}

fn consume_option_value(
    args: &[String],
    idx: usize,
    inline_value: Option<&str>,
    _flag: &str,
) -> Result<(String, usize), ()> {
    match inline_value {
        Some(value) => {
            if value.is_empty() {
                Err(())
            } else {
                Ok((value.to_string(), idx + 1))
            }
        }
        None => {
            let Some(value) = args.get(idx + 1) else {
                return Err(());
            };
            if value.is_empty() {
                Err(())
            } else {
                Ok((value.to_string(), idx + 2))
            }
        }
    }
}

fn uf_find(parent: &mut HashMap<usize, usize>, node: usize) -> usize {
    let parent_node = parent.get(&node).copied().unwrap_or(node);
    if parent_node == node {
        return node;
    }
    let root = uf_find(parent, parent_node);
    parent.insert(node, root);
    root
}

fn uf_union(parent: &mut HashMap<usize, usize>, left: usize, right: usize) {
    let left_root = uf_find(parent, left);
    let right_root = uf_find(parent, right);
    if left_root == right_root {
        return;
    }
    if left_root < right_root {
        parent.insert(right_root, left_root);
    } else {
        parent.insert(left_root, right_root);
    }
}

fn print_tsv(records: &[OutputRecord]) {
    println!("# task_id\tsummary\tpr_group");
    for rec in records {
        println!(
            "{}\t{}\t{}",
            rec.task_id.replace('\t', " "),
            rec.summary.replace('\t', " "),
            rec.pr_group.replace('\t', " "),
        );
    }
}

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die(msg: &str) -> i32 {
    eprintln!("split-prs: {msg}");
    2
}

fn resolve_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    repo_root.join(path)
}

fn maybe_relativize(path: &Path, repo_root: &Path) -> PathBuf {
    let Ok(path_abs) = path.canonicalize() else {
        return path.to_path_buf();
    };
    let Ok(root_abs) = repo_root.canonicalize() else {
        return path_abs;
    };
    match path_abs.strip_prefix(&root_abs) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => path_abs,
    }
}

fn path_to_posix(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn normalize_spaces(value: String) -> String {
    let joined = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        String::from("task")
    } else {
        joined
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

fn is_placeholder(value: &str) -> bool {
    let token = value.trim().to_ascii_lowercase();
    if matches!(token.as_str(), "" | "-" | "none" | "n/a" | "na" | "...") {
        return true;
    }
    if token.starts_with('<') && token.ends_with('>') {
        return true;
    }
    token.contains("task ids")
}

#[cfg(test)]
mod tests {
    use super::{is_placeholder, normalize_token};
    use pretty_assertions::assert_eq;

    #[test]
    fn normalize_token_collapses_non_alnum_and_limits_length() {
        assert_eq!(
            normalize_token("Sprint 2 :: Shared Pair", "fallback", 20),
            "sprint-2-shared-pair"
        );
        assert_eq!(normalize_token("!!!!", "fallback-value", 8), "fallback");
    }

    #[test]
    fn placeholder_rules_cover_common_plan_values() {
        assert!(is_placeholder("none"));
        assert!(is_placeholder("<task ids>"));
        assert!(is_placeholder("Task IDs here"));
        assert!(!is_placeholder("Task 1.1"));
    }
}
