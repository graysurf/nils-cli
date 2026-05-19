use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::parse::{Plan, Sprint, Task, parse_plan_with_display};

const USAGE: &str = r#"Usage:
  plan_batches.sh --file <plan.md> --sprint <n> [--format json|text]

Purpose:
  Compute dependency layers (parallel batches) for a sprint within a plan file.

Options:
  --file <path>     Plan file to parse (required)
  --sprint <n>      Sprint number to batch (required)
  --format <fmt>    json (default) or text
  -h, --help        Show help

Exit:
  0: success
  1: parse or cycle error
  2: usage error
"#;

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die(msg: &str) -> i32 {
    eprintln!("plan_batches: {msg}");
    2
}

#[derive(Debug, Serialize)]
struct ConflictRisk {
    batch: u32,
    overlap: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Output {
    file: String,
    sprint: i32,
    batches: Vec<Vec<String>>,
    blocked_by_external: BTreeMap<String, Vec<String>>,
    conflict_risk: Vec<ConflictRisk>,
}

pub fn run(args: &[String]) -> i32 {
    let mut file: Option<String> = None;
    let mut sprint: Option<String> = None;
    let mut format: String = "json".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                let Some(v) = args.get(i + 1) else {
                    return die("missing value for --file");
                };
                if v.is_empty() {
                    return die("missing value for --file");
                }
                file = Some(v.to_string());
                i += 2;
            }
            "--sprint" => {
                let Some(v) = args.get(i + 1) else {
                    return die("missing value for --sprint");
                };
                if v.is_empty() {
                    return die("missing value for --sprint");
                }
                sprint = Some(v.to_string());
                i += 2;
            }
            "--format" => {
                let Some(v) = args.get(i + 1) else {
                    return die("missing value for --format");
                };
                if v.is_empty() {
                    return die("missing value for --format");
                }
                format = v.to_string();
                i += 2;
            }
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            other => {
                return die(&format!("unknown argument: {other}"));
            }
        }
    }

    let Some(file_arg) = file else {
        print_usage();
        return 2;
    };
    let Some(sprint_raw) = sprint else {
        print_usage();
        return 2;
    };

    if format != "json" && format != "text" {
        return die(&format!("invalid --format (expected json|text): {format}"));
    }

    let sprint_num = match sprint_raw.parse::<i32>() {
        Ok(v) => v,
        Err(_) => {
            eprintln!(
                "error: invalid --sprint (expected int): {}",
                crate::repr::py_repr(&sprint_raw)
            );
            return 2;
        }
    };

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

    let output_file = path_to_posix(&maybe_relativize(&read_path, &repo_root));

    let Some(sprint) = find_sprint(&plan, sprint_num) else {
        eprintln!("error: {display_path}: sprint not found: {sprint_num}");
        return 1;
    };

    if sprint.tasks.is_empty() {
        eprintln!("error: {display_path}: sprint {sprint_num}: no tasks found");
        return 1;
    }

    let tasks_map: BTreeMap<String, Task> = sprint
        .tasks
        .iter()
        .filter(|t| !t.id.trim().is_empty())
        .map(|t| (t.id.trim().to_string(), t.clone()))
        .collect();

    if tasks_map.is_empty() {
        eprintln!("error: {display_path}: sprint {sprint_num}: no valid task IDs found");
        return 1;
    }

    let task_ids: Vec<String> = tasks_map.keys().cloned().collect();

    let mut internal_deps: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut external_deps: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for tid in &task_ids {
        let deps_list: Vec<String> = tasks_map
            .get(tid)
            .and_then(|t| t.dependencies.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.id)
            .filter(|d| !d.trim().is_empty())
            .collect();

        let mut in_sprint: Vec<String> = deps_list
            .iter()
            .filter(|d| tasks_map.contains_key(d.as_str()))
            .cloned()
            .collect();
        in_sprint.sort();

        let mut out_sprint: Vec<String> = deps_list
            .iter()
            .filter(|d| !tasks_map.contains_key(d.as_str()))
            .cloned()
            .collect();
        out_sprint.sort();

        internal_deps.insert(tid.clone(), in_sprint.into_iter().collect());
        if !out_sprint.is_empty() {
            external_deps.insert(tid.clone(), out_sprint);
        }
    }

    let batches = match topo_batches(&task_ids, &internal_deps) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("error: {display_path}: sprint {sprint_num}: {err}");
            return 1;
        }
    };

    let conflict_risk = compute_conflict_risk(&batches, &tasks_map);

    let result = Output {
        file: output_file,
        sprint: sprint_num,
        batches,
        blocked_by_external: external_deps,
        conflict_risk,
    };

    if format == "json" {
        match serde_json::to_string(&result) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(err) => {
                eprintln!("error: failed to encode JSON: {err}");
                1
            }
        }
    } else {
        print_text(&result);
        0
    }
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

fn find_sprint(plan: &Plan, sprint_num: i32) -> Option<&Sprint> {
    plan.sprints.iter().find(|s| s.number == sprint_num)
}

fn topo_batches(
    nodes: &[String],
    edges: &HashMap<String, BTreeSet<String>>,
) -> Result<Vec<Vec<String>>, String> {
    let mut in_deg: HashMap<String, usize> = nodes.iter().map(|n| (n.clone(), 0)).collect();
    let mut rev: HashMap<String, BTreeSet<String>> =
        nodes.iter().map(|n| (n.clone(), BTreeSet::new())).collect();

    for n in nodes {
        for dep in edges.get(n).cloned().unwrap_or_default() {
            if !in_deg.contains_key(&dep) {
                continue;
            }
            *in_deg.get_mut(n).unwrap() += 1;
            rev.get_mut(&dep).unwrap().insert(n.clone());
        }
    }

    let mut q: VecDeque<String> = nodes
        .iter()
        .filter(|n| *in_deg.get(*n).unwrap_or(&0) == 0)
        .cloned()
        .collect();
    let mut q_sorted: Vec<String> = q.drain(..).collect();
    q_sorted.sort();
    q.extend(q_sorted);

    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut remaining: BTreeSet<String> = nodes.iter().cloned().collect();

    while !remaining.is_empty() {
        let mut batch: Vec<String> = q.drain(..).collect();
        batch.sort();
        if batch.is_empty() {
            let cycle_hint: Vec<String> = remaining.iter().take(10).cloned().collect();
            return Err(format!(
                "dependency cycle detected (remaining: {})",
                crate::repr::py_list_repr(&cycle_hint)
            ));
        }

        batches.push(batch.clone());

        for n in batch {
            remaining.remove(&n);
            let affected: Vec<String> = rev
                .get(&n)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            for m in affected {
                if let Some(v) = in_deg.get_mut(&m) {
                    *v = v.saturating_sub(1);
                    if *v == 0 {
                        q.push_back(m);
                    }
                }
            }
        }
    }

    Ok(batches)
}

fn compute_conflict_risk(
    batches: &[Vec<String>],
    tasks: &BTreeMap<String, Task>,
) -> Vec<ConflictRisk> {
    let mut out: Vec<ConflictRisk> = Vec::new();

    for (idx, batch) in batches.iter().enumerate() {
        let mut path_to_tasks: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for tid in batch {
            let Some(task) = tasks.get(tid) else {
                continue;
            };
            for p in &task.location {
                let p = p.trim();
                if p.is_empty() {
                    continue;
                }
                path_to_tasks
                    .entry(p.to_string())
                    .or_default()
                    .push(tid.to_string());
            }
        }

        let overlaps: Vec<String> = path_to_tasks
            .iter()
            .filter_map(|(path, owners)| {
                let unique: BTreeSet<&String> = owners.iter().collect();
                (unique.len() > 1).then(|| path.to_string())
            })
            .collect();

        if !overlaps.is_empty() {
            out.push(ConflictRisk {
                batch: (idx + 1) as u32,
                overlap: overlaps,
            });
        }
    }

    out
}

fn print_text(result: &Output) {
    println!("Plan: {}", result.file);
    println!("Sprint: {}", result.sprint);
    for (i, batch) in result.batches.iter().enumerate() {
        println!();
        println!("Batch {}:", i + 1);
        for tid in batch {
            println!("- {tid}");
        }
    }

    if !result.blocked_by_external.is_empty() {
        println!();
        println!("External blockers:");
        for (tid, deps) in &result.blocked_by_external {
            println!("- {tid}: {}", deps.join(", "));
        }
    }

    if !result.conflict_risk.is_empty() {
        println!();
        println!("Conflict risk (overlapping Location paths):");
        for item in &result.conflict_risk {
            println!("- Batch {}: {}", item.batch, item.overlap.join(", "));
        }
    }
}
