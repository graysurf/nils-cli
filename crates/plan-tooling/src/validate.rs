use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use nils_common::git as common_git;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
use serde::Serialize;

use crate::parse::{Plan, Sprint, Task, parse_plan_with_display};

const USAGE: &str = r#"Usage:
  validate_plans.sh [--file <path>]... [--format text|json] [--explain]

Purpose:
  Lint plan markdown files under docs/plans/ against Plan Format v1.

Options:
  --file <path>  Validate a specific plan file (may be repeated)
  --format <fmt> text (default) or json
  --explain      Append canonical accepted-shape examples per error class.
                 Output is independent of exit code (also prints on success).
  -h, --help     Show help

Defaults:
  With no --file args, validates tracked `docs/plans/*-plan.md` files.

Exit:
  0: all validated files are compliant
  1: validation errors found
  2: usage error
"#;

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn die(msg: &str) -> i32 {
    eprintln!("validate_plans: {msg}");
    2
}

#[derive(Debug, Serialize)]
struct ValidateOutput {
    ok: bool,
    files: Vec<String>,
    errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    explanations: Option<Vec<ExplainEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uncatalogued: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
struct ExplainEntry {
    class: &'static str,
    rule: &'static str,
    example: &'static str,
}

#[derive(Debug, Default)]
struct ExplainResult {
    matched: Vec<ExplainEntry>,
    uncatalogued: Vec<String>,
}

pub fn run(args: &[String]) -> i32 {
    let mut files: Vec<String> = Vec::new();
    let mut format = "text".to_string();
    let mut explain = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                if args.get(i + 1).is_none() {
                    return die("--file requires a path");
                }
                files.push(args[i + 1].to_string());
                i += 2;
            }
            "--format" => {
                if args.get(i + 1).is_none() {
                    return die("--format requires a value");
                }
                format = args[i + 1].to_string();
                i += 2;
            }
            "--explain" => {
                explain = true;
                i += 1;
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

    if format != "text" && format != "json" {
        return die(&format!("invalid --format (expected text|json): {format}"));
    }

    let repo_root = crate::repo_root::detect();

    let discovered = if files.is_empty() {
        discover_default_plan_files(&repo_root)
    } else {
        files
    };
    let discovered_for_output = discovered.clone();

    if discovered.is_empty() {
        if format == "json" {
            let output = ValidateOutput {
                ok: true,
                files: Vec::new(),
                errors: Vec::new(),
                explanations: if explain {
                    Some(all_explanations())
                } else {
                    None
                },
                uncatalogued: None,
            };
            return print_json_output(output, 0);
        }
        if explain {
            print_explanations_text(&ExplainResult {
                matched: all_explanations(),
                uncatalogued: Vec::new(),
            });
        }
        return 0;
    }

    let progress = if format == "text" {
        Some(Progress::new(
            discovered.len() as u64,
            ProgressOptions::default().with_finish(ProgressFinish::Clear),
        ))
    } else {
        None
    };

    let mut errors: Vec<String> = Vec::new();
    for (idx, display_path) in discovered.into_iter().enumerate() {
        if let Some(p) = progress.as_ref() {
            p.set_message(display_path.clone());
        }

        let read_path = resolve_repo_relative(&repo_root, Path::new(&display_path));
        if !read_path.is_file() {
            errors.push(format!("{display_path}: file not found"));
            if let Some(p) = progress.as_ref() {
                p.set_position((idx + 1) as u64);
            }
            continue;
        }
        errors.extend(validate_plan(&display_path, &read_path, &repo_root));

        if let Some(p) = progress.as_ref() {
            p.set_position((idx + 1) as u64);
        }
    }

    if let Some(p) = progress.as_ref() {
        p.finish_and_clear();
    }

    if format == "json" {
        let code = if errors.is_empty() { 0 } else { 1 };
        let (explanations, uncatalogued) = if explain {
            let result = explanations_for(&errors);
            (
                Some(result.matched),
                if result.uncatalogued.is_empty() {
                    None
                } else {
                    Some(result.uncatalogued)
                },
            )
        } else {
            (None, None)
        };
        let output = ValidateOutput {
            ok: errors.is_empty(),
            files: discovered_for_output,
            errors,
            explanations,
            uncatalogued,
        };
        return print_json_output(output, code);
    }

    if errors.is_empty() {
        if explain {
            print_explanations_text(&ExplainResult {
                matched: all_explanations(),
                uncatalogued: Vec::new(),
            });
        }
        return 0;
    }

    for err in &errors {
        eprintln!("error: {err}");
    }
    if explain {
        print_explanations_text(&explanations_for(&errors));
    }
    1
}

fn print_json_output(output: ValidateOutput, code: i32) -> i32 {
    match serde_json::to_string(&output) {
        Ok(s) => {
            println!("{s}");
            code
        }
        Err(err) => {
            eprintln!("error: failed to encode JSON: {err}");
            1
        }
    }
}

fn discover_default_plan_files(repo_root: &Path) -> Vec<String> {
    let mut files = git_ls_files(repo_root, "docs/plans/*-plan.md");
    if files.is_empty() {
        files = find_plan_files(repo_root);
    }
    files
}

fn git_ls_files(repo_root: &Path, pattern: &str) -> Vec<String> {
    let output = common_git::run_output_in(repo_root, &["ls-files", "--", pattern]);
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    files.sort();
    files
}

fn find_plan_files(repo_root: &Path) -> Vec<String> {
    let dir = repo_root.join("docs/plans");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with("-plan.md") {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(repo_root) {
            out.push(
                rel.to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        } else {
            out.push(path.to_string_lossy().to_string());
        }
    }
    out.sort();
    out
}

fn resolve_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    repo_root.join(path)
}

fn validate_plan(display_path: &str, read_path: &Path, repo_root: &Path) -> Vec<String> {
    let plan: Plan;
    let parse_errors: Vec<String>;
    match parse_plan_with_display(read_path, display_path) {
        Ok((p, errs)) => {
            plan = p;
            parse_errors = errs;
        }
        Err(err) => {
            return vec![format!("{display_path}: failed to parse plan: {err}")];
        }
    }

    if !parse_errors.is_empty() {
        return parse_errors
            .into_iter()
            .map(|e| format!("{display_path}: error: {e}"))
            .collect();
    }

    if plan.sprints.is_empty() {
        return vec![format!(
            "{display_path}: missing sprints (expected '## Sprint N: ...' headings)"
        )];
    }

    let mut tasks: Vec<&Task> = Vec::new();
    for sprint in &plan.sprints {
        tasks.extend(sprint.tasks.iter());
    }
    if tasks.is_empty() {
        return vec![format!(
            "{display_path}: no tasks found (expected '### Task N.M: ...' headings)"
        )];
    }

    let all_task_ids: HashSet<String> = tasks.iter().map(|t| t.id.trim().to_string()).collect();

    let mut errs: Vec<String> = validate_read_first(display_path, &plan, repo_root);
    errs.extend(validate_sprint_metadata(display_path, &plan.sprints));
    for task in tasks {
        errs.extend(validate_task(display_path, task, &all_task_ids));
    }
    errs.extend(crate::bundle::validate_plan_bundle(
        display_path,
        read_path,
        &plan,
        repo_root,
    ));
    errs
}

fn validate_read_first(plan_path: &str, plan: &Plan, repo_root: &Path) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(read_first) = plan.read_first.as_ref() else {
        return vec![format!(
            "{plan_path}: missing Read First section (expected Primary source, Source type, and Open questions carried into execution)"
        )];
    };

    let primary_source = read_first
        .primary_source
        .as_deref()
        .map(clean_source_value)
        .unwrap_or_default();
    let source_type = read_first
        .source_type
        .as_deref()
        .map(clean_source_value)
        .unwrap_or_default();
    let open_questions = read_first
        .open_questions
        .as_deref()
        .map(clean_source_value)
        .unwrap_or_default();

    if primary_source.trim().is_empty() {
        errs.push(format!("{plan_path}: Read First missing Primary source"));
    }
    if source_type.trim().is_empty() {
        errs.push(format!("{plan_path}: Read First missing Source type"));
    } else if !is_allowed_source_type(&source_type) {
        errs.push(format!(
            "{plan_path}: invalid Read First Source type (expected discussion-to-implementation-doc|review-to-improvement-doc|existing issue/spec|plan-only waiver): {}",
            crate::repr::py_repr(&source_type)
        ));
    }
    if open_questions.trim().is_empty() {
        errs.push(format!(
            "{plan_path}: Read First missing Open questions carried into execution"
        ));
    }

    if source_type == "plan-only waiver" {
        if !primary_source.to_ascii_lowercase().contains("waiver") {
            errs.push(format!(
                "{plan_path}: plan-only waiver requires Primary source to state an explicit waiver"
            ));
        }
        return errs;
    }

    if primary_source.trim().is_empty()
        || primary_source.starts_with("http://")
        || primary_source.starts_with("https://")
        || primary_source.starts_with('#')
    {
        return errs;
    }
    if Path::new(&primary_source).is_absolute() {
        errs.push(format!(
            "{plan_path}: Primary source must be repo-relative or a URL: {}",
            crate::repr::py_repr(&primary_source)
        ));
        return errs;
    }

    let source_path = repo_root.join(&primary_source);
    if !source_path.is_file() {
        errs.push(format!(
            "{plan_path}: Primary source path not found: {}",
            crate::repr::py_repr(&primary_source)
        ));
    }

    errs
}

fn clean_source_value(value: &str) -> String {
    let trimmed = value.trim();
    let unwrapped = if trimmed.len() >= 2 && trimmed.starts_with('`') && trimmed.ends_with('`') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    unwrapped.trim().to_string()
}

fn is_allowed_source_type(value: &str) -> bool {
    matches!(
        value,
        "discussion-to-implementation-doc"
            | "review-to-improvement-doc"
            | "existing issue/spec"
            | "plan-only waiver"
    )
}

fn validate_sprint_metadata(plan_path: &str, sprints: &[Sprint]) -> Vec<String> {
    let mut errs = Vec::new();
    for sprint in sprints {
        let prefix = format!("{plan_path}:Sprint {}", sprint.number);
        let intent = sprint.metadata.pr_grouping_intent.as_deref();
        let profile = sprint.metadata.execution_profile.as_deref();

        if (intent.is_some() && profile.is_none()) || (intent.is_none() && profile.is_some()) {
            errs.push(format!(
                "{prefix}: sprint metadata must include both `PR grouping intent` and `Execution Profile`"
            ));
        }

        if intent == Some("per-sprint") {
            let width = sprint
                .metadata
                .parallel_width
                .or_else(|| profile.and_then(parse_parallel_width_from_execution_profile));
            if let Some(width) = width
                && width > 1
            {
                errs.push(format!(
                    "{prefix}: `PR grouping intent` is per-sprint but `Execution Profile` indicates parallel width {width}; use `PR grouping intent: group` for multi-lane execution"
                ));
            }
        }
    }
    errs
}

fn validate_task(plan_path: &str, task: &Task, all_task_ids: &HashSet<String>) -> Vec<String> {
    let mut errs: Vec<String> = Vec::new();

    let task_id = task.id.trim();
    let prefix = if task_id.is_empty() {
        format!("{plan_path}:<unknown task>")
    } else {
        format!("{plan_path}:{task_id}")
    };

    if task_id.is_empty() || !is_task_id(task_id) {
        errs.push(format!("{prefix}: invalid or missing task id"));
    }

    if !is_non_empty_list(&task.location) {
        errs.push(format!(
            "{prefix}: missing Location (must be a non-empty list)"
        ));
    } else {
        for loc in &task.location {
            if loc.trim().is_empty() {
                continue;
            }
            if loc.starts_with('/') {
                errs.push(format!(
                    "{prefix}: Location must be repo-relative (no leading '/'): {}",
                    crate::repr::py_repr(loc)
                ));
            }
            if loc.ends_with('/') {
                errs.push(format!(
                    "{prefix}: Location must be a file path (not a directory): {}",
                    crate::repr::py_repr(loc)
                ));
            }
            if ["*", "?", "{", "}"].iter().any(|ch| loc.contains(ch)) {
                errs.push(format!(
                    "{prefix}: Location must not use globs/braces: {}",
                    crate::repr::py_repr(loc)
                ));
            }
            if has_placeholder(loc) {
                errs.push(format!(
                    "{prefix}: Location contains placeholder: {}",
                    crate::repr::py_repr(loc)
                ));
            }
        }
    }

    match task.description.as_deref() {
        None => errs.push(format!("{prefix}: missing Description")),
        Some(desc) => {
            if desc.trim().is_empty() {
                errs.push(format!("{prefix}: missing Description"));
            } else if has_placeholder(desc) {
                errs.push(format!(
                    "{prefix}: Description contains placeholder: {}",
                    crate::repr::py_repr(desc)
                ));
            }
        }
    }

    match task.dependencies.as_ref() {
        None => errs.push(format!(
            "{prefix}: missing Dependencies (use 'none' or list task IDs)"
        )),
        Some(deps) => {
            let mut invalid: Vec<String> = Vec::new();
            let mut unknown: Vec<String> = Vec::new();
            for dep in deps {
                let d = dep.trim();
                if d.is_empty() {
                    continue;
                }
                if !is_task_id(d) {
                    invalid.push(crate::repr::py_repr(dep));
                } else if !all_task_ids.contains(d) {
                    unknown.push(crate::repr::py_repr(d));
                }
            }
            if !invalid.is_empty() {
                errs.push(format!(
                    "{prefix}: line {line}: invalid dependency (expected 'Task N.M', e.g. 'Task 1.2'): {values}",
                    line = task.start_line,
                    values = invalid.join(", ")
                ));
            }
            if !unknown.is_empty() {
                errs.push(format!(
                    "{prefix}: line {line}: unknown dependency (not found in plan): {values}",
                    line = task.start_line,
                    values = unknown.join(", ")
                ));
            }
        }
    }

    if let Some(c) = task.complexity
        && !(1..=10).contains(&c)
    {
        errs.push(format!("{prefix}: Complexity out of range (1-10): {c}"));
    }

    if !is_non_empty_list(&task.acceptance_criteria) {
        errs.push(format!(
            "{prefix}: missing Acceptance criteria (must be a non-empty list)"
        ));
    } else {
        for item in &task.acceptance_criteria {
            if has_placeholder(item) {
                errs.push(format!(
                    "{prefix}: Acceptance criteria contains placeholder: {}",
                    crate::repr::py_repr(item)
                ));
            }
        }
    }

    if !is_non_empty_list(&task.validation) {
        errs.push(format!(
            "{prefix}: missing Validation (must be a non-empty list)"
        ));
    } else {
        for cmd in &task.validation {
            if has_placeholder(cmd) {
                errs.push(format!(
                    "{prefix}: Validation contains placeholder: {}",
                    crate::repr::py_repr(cmd)
                ));
            }
        }
    }

    errs
}

fn has_placeholder(value: &str) -> bool {
    let scan = strip_backtick_spans(value);
    if contains_angle_placeholder(&scan) {
        return true;
    }

    contains_word_case_insensitive(&scan, "TBD") || contains_word_case_insensitive(&scan, "TODO")
}

/// Drop the contents of paired backtick spans so placeholder checks only fire
/// on prose. An unpaired trailing backtick is kept verbatim (treated as
/// literal text), matching how Markdown renders it.
fn strip_backtick_spans(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(rel_end) = chars[i + 1..].iter().position(|c| *c == '`') {
                i = i + 1 + rel_end + 1;
                continue;
            }
            out.push('`');
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn contains_angle_placeholder(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let start = i + 1;
            if start < bytes.len()
                && let Some(end) = bytes[start..].iter().position(|b| *b == b'>')
            {
                if end >= 1 {
                    let inner = &value[start..start + end];
                    // Treat only tight `<...>` tokens as placeholders. This avoids
                    // false positives on shell redirects like `cat < in > out`.
                    if inner.trim() == inner {
                        return true;
                    }
                }
                i = start + end;
            }
        }
        i += 1;
    }
    false
}

fn contains_word_case_insensitive(haystack: &str, needle: &str) -> bool {
    let h = haystack.to_ascii_uppercase();
    let n = needle.to_ascii_uppercase();
    let hb = h.as_bytes();
    let nb = n.as_bytes();
    if nb.is_empty() || hb.len() < nb.len() {
        return false;
    }

    for i in 0..=(hb.len() - nb.len()) {
        if &hb[i..i + nb.len()] != nb {
            continue;
        }
        let left_ok = i == 0 || !is_word_byte(hb[i - 1]);
        let right_ok = i + nb.len() == hb.len() || !is_word_byte(hb[i + nb.len()]);
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
}

fn is_non_empty_list(items: &[String]) -> bool {
    items.iter().any(|x| !x.trim().is_empty())
}

fn is_task_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("Task ") else {
        return false;
    };
    let Some((a, b)) = rest.split_once('.') else {
        return false;
    };
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit())
}

struct ExplainCatalogEntry {
    /// Substring used to detect that this error class fired. Must appear in
    /// the matching error message and be unique across the catalog.
    pattern: &'static str,
    explain: ExplainEntry,
}

const EXPLAIN_CATALOG: &[ExplainCatalogEntry] = &[
    ExplainCatalogEntry {
        pattern: "missing Read First",
        explain: ExplainEntry {
            class: "read-first-missing",
            rule: "Plans must start with a Read First section that names the primary source artifact.",
            example: "## Read First\n\n- Primary source: docs/runbooks/example-source.md\n- Source type: review-to-improvement-doc\n- Open questions carried into execution: none",
        },
    },
    ExplainCatalogEntry {
        pattern: "Read First missing Primary source",
        explain: ExplainEntry {
            class: "read-first-primary-source-missing",
            rule: "Read First must declare a `Primary source` value.",
            example: "- Primary source: docs/runbooks/example-source.md",
        },
    },
    ExplainCatalogEntry {
        pattern: "Read First missing Source type",
        explain: ExplainEntry {
            class: "read-first-source-type-missing",
            rule: "Read First must declare a `Source type` value.",
            example: "- Source type: review-to-improvement-doc",
        },
    },
    ExplainCatalogEntry {
        pattern: "Read First missing Open questions carried into execution",
        explain: ExplainEntry {
            class: "read-first-open-questions-missing",
            rule: "Read First must declare `Open questions carried into execution` (use 'none' when empty).",
            example: "- Open questions carried into execution: none",
        },
    },
    ExplainCatalogEntry {
        pattern: "plan-only waiver requires Primary source to state an explicit waiver",
        explain: ExplainEntry {
            class: "read-first-plan-only-waiver-implicit",
            rule: "When `Source type: plan-only waiver`, the `Primary source` must say so explicitly.",
            example: "- Primary source: plan-only waiver: bounded follow-up to issue #123\n- Source type: plan-only waiver",
        },
    },
    ExplainCatalogEntry {
        pattern: "Primary source must be repo-relative or a URL",
        explain: ExplainEntry {
            class: "read-first-primary-source-absolute",
            rule: "Primary source must be a repo-relative path, an `http(s)://` URL, an anchor (`#...`), or an explicit `plan-only waiver:` string.",
            example: "- Primary source: docs/runbooks/example-source.md",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing sprints",
        explain: ExplainEntry {
            class: "plan-missing-sprints",
            rule: "Plans must contain at least one `## Sprint N: name` heading.",
            example: "## Sprint 1: Bootstrap\n\n### Task 1.1: ...",
        },
    },
    ExplainCatalogEntry {
        pattern: "no tasks found",
        explain: ExplainEntry {
            class: "plan-missing-tasks",
            rule: "Every plan must contain at least one `### Task N.M: name` heading under a sprint.",
            example: "### Task 1.1: Validate sprint metadata",
        },
    },
    ExplainCatalogEntry {
        pattern: "task outside of any sprint",
        explain: ExplainEntry {
            class: "task-orphaned",
            rule: "Tasks must appear under a preceding `## Sprint N:` heading.",
            example: "## Sprint 1: First sprint\n\n### Task 1.1: Do thing",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing Location",
        explain: ExplainEntry {
            class: "location-missing",
            rule: "Each task must declare a non-empty `Location` list.",
            example: "- **Location**:\n  - `crates/foo/src/bar.rs`",
        },
    },
    ExplainCatalogEntry {
        pattern: "invalid PR grouping intent",
        explain: ExplainEntry {
            class: "sprint-metadata-pr-grouping-invalid",
            rule: "Sprint `PR grouping intent` accepts `per-sprint` or `group`.",
            example: "**PR grouping intent**: `group`",
        },
    },
    ExplainCatalogEntry {
        pattern: "invalid Execution Profile",
        explain: ExplainEntry {
            class: "sprint-metadata-execution-profile-invalid",
            rule: "Sprint `Execution Profile` accepts `serial` or `parallel-xN` (where N is a positive integer).",
            example: "**Execution Profile**: `parallel-x2`",
        },
    },
    ExplainCatalogEntry {
        pattern: "invalid metadata field",
        explain: ExplainEntry {
            class: "sprint-metadata-field-typo",
            rule: "Sprint metadata field name must use the exact canonical spelling (`PR grouping intent`, `Execution Profile`).",
            example: "**PR grouping intent**: `group`\n**Execution Profile**: `parallel-x2`",
        },
    },
    ExplainCatalogEntry {
        pattern: "Primary source path not found",
        explain: ExplainEntry {
            class: "read-first-source-missing",
            rule: "Repo-local Primary source paths must exist; use a URL or explicit plan-only waiver when no repo file exists.",
            example: "- Primary source: docs/runbooks/example-source.md\n- Source type: existing issue/spec",
        },
    },
    ExplainCatalogEntry {
        pattern: "invalid Read First Source type",
        explain: ExplainEntry {
            class: "read-first-source-type",
            rule: "Source type must be one of the canonical plan source categories.",
            example: "- Source type: discussion-to-implementation-doc",
        },
    },
    ExplainCatalogEntry {
        pattern: "Location must be repo-relative",
        explain: ExplainEntry {
            class: "location-absolute",
            rule: "Location entries must be repo-relative paths (no leading '/').",
            example: "- **Location**:\n  - `crates/foo/src/bar.rs`",
        },
    },
    ExplainCatalogEntry {
        pattern: "Location must be a file path",
        explain: ExplainEntry {
            class: "location-directory",
            rule: "Location entries must point at files, not directories.",
            example: "- **Location**:\n  - `crates/foo/src/lib.rs`",
        },
    },
    ExplainCatalogEntry {
        pattern: "must not use globs",
        explain: ExplainEntry {
            class: "location-glob",
            rule: "Enumerate every touched file; globs and braces are rejected.",
            example: "- **Location**:\n  - `crates/foo/src/a.rs`\n  - `crates/foo/src/b.rs`",
        },
    },
    ExplainCatalogEntry {
        pattern: "Location contains placeholder",
        explain: ExplainEntry {
            class: "location-placeholder",
            rule: "Replace `<...>` / TODO / TBD placeholders with real paths.",
            example: "- **Location**:\n  - `crates/plan-tooling/src/validate.rs`",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing Description",
        explain: ExplainEntry {
            class: "description-missing",
            rule: "Description is required and must be non-empty prose.",
            example: "- **Description**: Validate task dependencies against the plan's task IDs.",
        },
    },
    ExplainCatalogEntry {
        pattern: "Description contains placeholder",
        explain: ExplainEntry {
            class: "description-placeholder",
            rule: "Description must not contain `<...>` / TODO / TBD outside backticks. \
                   Wrap usage slots in backticks (e.g. `<arg>`) when documenting CLI shapes.",
            example: "- **Description**: Invoke `<arg>` to wire the slot.",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing Dependencies",
        explain: ExplainEntry {
            class: "dependencies-missing",
            rule: "Dependencies field is required: use 'none' or list `Task N.M` IDs.",
            example: "- **Dependencies**:\n  - none",
        },
    },
    ExplainCatalogEntry {
        pattern: "invalid dependency",
        explain: ExplainEntry {
            class: "dependency-invalid",
            rule: "Dependency entries must use the canonical `Task N.M` form.",
            example: "- **Dependencies**:\n  - Task 1.2\n  - Task 2.1",
        },
    },
    ExplainCatalogEntry {
        pattern: "unknown dependency",
        explain: ExplainEntry {
            class: "dependency-unknown",
            rule: "Dependency must reference a `### Task N.M` heading present in the plan.",
            example: "- **Dependencies**:\n  - Task 1.1     # must match an actual task heading",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing Complexity value",
        explain: ExplainEntry {
            class: "complexity-missing-value",
            rule: "If the Complexity field is present, set a 1-10 integer or omit the field entirely.",
            example: "- **Complexity**: 5",
        },
    },
    ExplainCatalogEntry {
        pattern: "Complexity out of range",
        explain: ExplainEntry {
            class: "complexity-range",
            rule: "Complexity is a 1-10 integer.",
            example: "- **Complexity**: 5",
        },
    },
    ExplainCatalogEntry {
        pattern: "invalid Complexity",
        explain: ExplainEntry {
            class: "complexity-non-integer",
            rule: "Complexity must be an integer; omit the field entirely when no estimate exists.",
            example: "- **Complexity**: 5",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing Acceptance criteria",
        explain: ExplainEntry {
            class: "acceptance-missing",
            rule: "Acceptance criteria is a non-empty list of testable outcomes.",
            example: "- **Acceptance criteria**:\n  - All callers of foo() return Result<()>.",
        },
    },
    ExplainCatalogEntry {
        pattern: "Acceptance criteria contains placeholder",
        explain: ExplainEntry {
            class: "acceptance-placeholder",
            rule: "Acceptance criteria entries must be concrete; no `<...>` / TODO / TBD placeholders.",
            example: "- **Acceptance criteria**:\n  - validate() rejects empty input with exit 2.",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing Validation",
        explain: ExplainEntry {
            class: "validation-missing",
            rule: "Validation is a non-empty list of runnable commands or steps.",
            example: "- **Validation**:\n  - cargo test -p plan-tooling",
        },
    },
    ExplainCatalogEntry {
        pattern: "Validation contains placeholder",
        explain: ExplainEntry {
            class: "validation-placeholder",
            rule: "Validation entries must be concrete commands; no `<...>` / TODO / TBD placeholders.",
            example: "- **Validation**:\n  - cargo nextest run --profile ci -p plan-tooling",
        },
    },
    ExplainCatalogEntry {
        pattern: "must include both `PR grouping intent` and `Execution Profile`",
        explain: ExplainEntry {
            class: "sprint-metadata-partial",
            rule: "Sprint metadata must declare both `PR grouping intent` and `Execution Profile`.",
            example: "**PR grouping intent**: `per-sprint`\n**Execution Profile**: `serial`",
        },
    },
    ExplainCatalogEntry {
        pattern: "is per-sprint but `Execution Profile` indicates parallel width",
        explain: ExplainEntry {
            class: "sprint-metadata-mismatch",
            rule: "`per-sprint` grouping cannot run with parallel execution; use `group` for parallel-x{N}.",
            example: "**PR grouping intent**: `group`\n**Execution Profile**: `parallel-x2`",
        },
    },
    ExplainCatalogEntry {
        pattern: "invalid or missing task id",
        explain: ExplainEntry {
            class: "task-id-invalid",
            rule: "Task headings must use `### Task N.M: name` (numeric N and M).",
            example: "### Task 2.1: Validate sprint metadata",
        },
    },
    ExplainCatalogEntry {
        pattern: "bundle Primary source must be an accepted sibling source doc",
        explain: ExplainEntry {
            class: "bundle-primary-source-mismatch",
            rule: "Plan `Primary source` must point at the sibling `-discussion-source.md` \
                   or `-review-source.md` doc under the same `docs/plans/<slug>/` directory.",
            example: "- Primary source: docs/plans/demo/demo-discussion-source.md\n  # or\n- Primary source: docs/plans/demo/demo-review-source.md",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing `Recommended plan`",
        explain: ExplainEntry {
            class: "bundle-source-doc-missing-plan-label",
            rule: "Source doc paired with a plan must declare the canonical `Recommended plan` \
                   pointer so plan-tooling can verify the bundle.",
            example: "## Execution\n\n- Recommended plan: docs/plans/demo/demo-plan.md\n- Recommended execution state: docs/plans/demo/demo-execution-state.md",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing `Recommended execution state`",
        explain: ExplainEntry {
            class: "bundle-source-doc-missing-execution-state-label",
            rule: "Source doc paired with a plan must declare the canonical \
                   `Recommended execution state` pointer so plan-tooling can verify the bundle.",
            example: "## Execution\n\n- Recommended plan: docs/plans/demo/demo-plan.md\n- Recommended execution state: docs/plans/demo/demo-execution-state.md",
        },
    },
    ExplainCatalogEntry {
        pattern: "recommends wrong plan",
        explain: ExplainEntry {
            class: "bundle-source-doc-wrong-plan",
            rule: "The source doc's `Recommended plan` value must equal the sibling `*-plan.md` path.",
            example: "- Recommended plan: docs/plans/demo/demo-plan.md",
        },
    },
    ExplainCatalogEntry {
        pattern: "recommends wrong execution state",
        explain: ExplainEntry {
            class: "bundle-source-doc-wrong-execution-state",
            rule: "The source doc's `Recommended execution state` value must equal the sibling `*-execution-state.md` path.",
            example: "- Recommended execution state: docs/plans/demo/demo-execution-state.md",
        },
    },
    ExplainCatalogEntry {
        pattern: "missing `Source document`",
        explain: ExplainEntry {
            class: "bundle-execution-state-missing-source-document",
            rule: "Execution state must declare a `Source document` pointer back to the plan (or to the source doc when waived).",
            example: "## Current State\n\n- Source document: `docs/plans/demo/demo-plan.md`",
        },
    },
    ExplainCatalogEntry {
        pattern: "points to wrong source document",
        explain: ExplainEntry {
            class: "bundle-execution-state-wrong-source-document",
            rule: "Execution state `Source document` must point at the plan, or at the source doc with an explicit `Direct source-doc execution waiver`.",
            example: "- Source document: `docs/plans/demo/demo-plan.md`\n- Direct source-doc execution waiver: not applicable",
        },
    },
    ExplainCatalogEntry {
        pattern: "without `Direct source-doc execution waiver`",
        explain: ExplainEntry {
            class: "bundle-direct-source-execution-waiver-missing",
            rule: "When execution state points at the source doc instead of the plan, set an explicit \
                   `Direct source-doc execution waiver` value (e.g. `bounded single-step source execution`).",
            example: "- Source document: `docs/plans/demo/demo-discussion-source.md`\n- Direct source-doc execution waiver: bounded single-step source execution",
        },
    },
];

/// Error fragments that are intentionally not paired with an `EXPLAIN_CATALOG` entry.
///
/// These cover I/O / parse failures and similar conditions where a canonical
/// authoring example would not be actionable. A new emitted error must either
/// match an `EXPLAIN_CATALOG.pattern` or appear here.
const KNOWN_UNCATALOGUED: &[&str] = &[
    "file not found",
    "failed to parse plan",
    "failed to read source doc",
    "failed to read execution state",
];

/// Static registry of every literal substring guaranteed to appear in an error
/// emitted by this crate's validators (and by parse.rs in `error:` form). The
/// completeness test asserts each fragment is either in `EXPLAIN_CATALOG` or
/// in `KNOWN_UNCATALOGUED` — so adding a new emitter without updating one of
/// those two arrays will fail CI.
#[cfg(test)]
const ALL_EMITTED_ERROR_PATTERNS: &[&str] = &[
    // I/O and parse
    "file not found",
    "failed to parse plan",
    // Top-level structure (validate.rs)
    "missing sprints",
    "no tasks found",
    // Read First
    "missing Read First section",
    "Read First missing Primary source",
    "Read First missing Source type",
    "invalid Read First Source type",
    "Read First missing Open questions carried into execution",
    "plan-only waiver requires Primary source to state an explicit waiver",
    "Primary source must be repo-relative or a URL",
    "Primary source path not found",
    // Sprint metadata
    "must include both `PR grouping intent` and `Execution Profile`",
    "is per-sprint but `Execution Profile` indicates parallel width",
    // Task fields
    "invalid or missing task id",
    "missing Location",
    "Location must be repo-relative",
    "Location must be a file path",
    "must not use globs",
    "Location contains placeholder",
    "missing Description",
    "Description contains placeholder",
    "missing Dependencies",
    "invalid dependency",
    "unknown dependency",
    "Complexity out of range",
    "missing Acceptance criteria",
    "Acceptance criteria contains placeholder",
    "missing Validation",
    "Validation contains placeholder",
    // parse.rs errors (rendered as `error: <msg>`)
    "missing Complexity value",
    "invalid Complexity",
    "invalid PR grouping intent",
    "invalid Execution Profile",
    "invalid metadata field",
    "task outside of any sprint",
    // bundle.rs errors
    "bundle Primary source must be an accepted sibling source doc",
    "failed to read source doc",
    "recommends wrong plan",
    "missing `Recommended plan`",
    "recommends wrong execution state",
    "missing `Recommended execution state`",
    "failed to read execution state",
    "points to wrong source document",
    "missing `Source document`",
    "without `Direct source-doc execution waiver`",
];

fn all_explanations() -> Vec<ExplainEntry> {
    EXPLAIN_CATALOG.iter().map(|e| e.explain.clone()).collect()
}

fn explanations_for(errors: &[String]) -> ExplainResult {
    let mut matched: Vec<ExplainEntry> = Vec::new();
    let mut seen_classes: HashSet<&'static str> = HashSet::new();
    let mut uncatalogued: Vec<String> = Vec::new();
    let mut uncatalogued_seen: HashSet<String> = HashSet::new();

    for err in errors {
        let mut matched_any = false;
        for entry in EXPLAIN_CATALOG {
            if err.contains(entry.pattern) {
                matched_any = true;
                if seen_classes.insert(entry.explain.class) {
                    matched.push(entry.explain.clone());
                }
                break;
            }
        }
        if matched_any {
            continue;
        }
        if KNOWN_UNCATALOGUED.iter().any(|frag| err.contains(*frag)) {
            continue;
        }
        // Strip the leading `<display_path>: ` (or `<display_path>:<task>: `) prefix so the
        // surfaced note focuses on the message body, which is what authors and LLM agents
        // need to recognize. Falls back to the raw error if no `:` is present.
        let body = err
            .split_once(':')
            .map(|(_, rest)| rest.trim().to_string())
            .unwrap_or_else(|| err.clone());
        if uncatalogued_seen.insert(body.clone()) {
            uncatalogued.push(body);
        }
    }

    ExplainResult { matched, uncatalogued }
}

fn print_explanations_text(result: &ExplainResult) {
    if result.matched.is_empty() && result.uncatalogued.is_empty() {
        return;
    }
    eprintln!();
    eprintln!("Examples:");
    for entry in &result.matched {
        eprintln!("  [{}] {}", entry.class, entry.rule);
        for line in entry.example.lines() {
            eprintln!("      {line}");
        }
    }
    for note in &result.uncatalogued {
        eprintln!("  note: no canonical example registered for error class: {note}");
    }
}

fn parse_parallel_width_from_execution_profile(profile: &str) -> Option<usize> {
    let digits = profile
        .to_ascii_lowercase()
        .strip_prefix("parallel-x")?
        .to_string();
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::parse::Task;

    use super::{
        ALL_EMITTED_ERROR_PATTERNS, EXPLAIN_CATALOG, KNOWN_UNCATALOGUED, clean_source_value,
        contains_angle_placeholder, contains_word_case_insensitive, explanations_for,
        has_placeholder, is_allowed_source_type, is_non_empty_list, is_task_id,
        strip_backtick_spans, validate_task,
    };

    #[test]
    fn angle_placeholder_detects_tight_token() {
        assert!(contains_angle_placeholder("needs <TBD>"));
    }

    #[test]
    fn angle_placeholder_ignores_shell_redirect_spacing() {
        assert!(!contains_angle_placeholder("cat < input.txt > output.txt"));
    }

    #[test]
    fn contains_word_case_insensitive_respects_word_boundaries() {
        assert!(contains_word_case_insensitive("TODO: fix this", "todo"));
        assert!(contains_word_case_insensitive("set tbd value", "tbd"));
        assert!(!contains_word_case_insensitive("methodology", "todo"));
        assert!(!contains_word_case_insensitive("set_tbd_value", "tbd"));
        assert!(!contains_word_case_insensitive("tbdvalue", "tbd"));
    }

    #[test]
    fn has_placeholder_detects_todo_and_tbd() {
        assert!(has_placeholder("still TODO"));
        assert!(has_placeholder("mark as tBd"));
        assert!(!has_placeholder("cat < input > output"));
        assert!(!has_placeholder("all good"));
    }

    #[test]
    fn has_placeholder_ignores_tokens_inside_backtick_spans() {
        // Legitimate usage docs that wrap argument slots in backticks.
        assert!(!has_placeholder("invoke `<arg>` with the path"));
        assert!(!has_placeholder("plan-issue resolve-approval `<TBD>`"));
        assert!(!has_placeholder("write `TODO: hook entry` then return"));
        // Bare placeholders outside backticks still fail.
        assert!(has_placeholder("invoke <arg> with the path"));
        assert!(has_placeholder("plan-issue resolve-approval <TBD>"));
        assert!(has_placeholder("write TODO: hook entry then return"));
    }

    #[test]
    fn strip_backtick_spans_handles_pairs_and_dangling() {
        assert_eq!(strip_backtick_spans("hi `code` bye"), "hi  bye");
        assert_eq!(strip_backtick_spans("a `b` c `d` e"), "a  c  e");
        // Unpaired backtick is preserved as literal text.
        assert_eq!(strip_backtick_spans("a `b c"), "a `b c");
        // Empty span drops nothing visible (just two backticks).
        assert_eq!(strip_backtick_spans("a `` b"), "a  b");
    }

    #[test]
    fn is_task_id_accepts_expected_shape_only() {
        assert!(is_task_id("Task 1.1"));
        assert!(is_task_id("Task 10.42"));
        assert!(!is_task_id("task 1.1"));
        assert!(!is_task_id("Task 1"));
        assert!(!is_task_id("Task 1.a"));
    }

    #[test]
    fn is_non_empty_list_checks_trimmed_values() {
        assert!(is_non_empty_list(&["x".to_string()]));
        assert!(is_non_empty_list(&["   ".to_string(), "x".to_string()]));
        assert!(!is_non_empty_list(&[]));
        assert!(!is_non_empty_list(&["  ".to_string(), "\t".to_string()]));
    }

    #[test]
    fn validate_task_reports_location_and_dependency_violations() {
        let task = Task {
            id: "Task 1.1".to_string(),
            name: "demo".to_string(),
            sprint: 1,
            start_line: 42,
            location: vec![
                "/abs/path.rs".to_string(),
                "dir/".to_string(),
                "src/*/x.rs".to_string(),
                "src/<name>.rs".to_string(),
            ],
            description: Some("TODO".to_string()),
            dependencies: Some(vec!["Task x.y".to_string(), "Task 9.9".to_string()]),
            complexity: Some(11),
            acceptance_criteria: vec!["<TBD>".to_string()],
            validation: vec!["TBD".to_string()],
        };
        let all_ids = HashSet::from(["Task 1.1".to_string()]);
        let errs = validate_task("plan.md", &task, &all_ids);
        assert!(errs.iter().any(|e| e.contains("repo-relative")));
        assert!(errs.iter().any(|e| e.contains("not a directory")));
        assert!(errs.iter().any(|e| e.contains("must not use globs")));
        assert!(errs.iter().any(|e| e.contains("contains placeholder")));
        assert!(errs.iter().any(|e| e.contains("invalid dependency")));
        assert!(errs.iter().any(|e| e.contains("unknown dependency")));
        assert!(errs.iter().any(|e| e.contains("Complexity out of range")));
        assert!(
            errs.iter()
                .any(|e| e.contains("line 42") && e.contains("e.g. 'Task 1.2'")),
            "expected dep error to carry task line + canonical example, got: {errs:?}",
        );
    }

    #[test]
    fn validate_task_groups_invalid_deps_into_single_error() {
        let task = Task {
            id: "Task 2.5".to_string(),
            name: "multi-bad-deps".to_string(),
            sprint: 2,
            start_line: 87,
            location: vec!["src/lib.rs".to_string()],
            description: Some("Ship feature".to_string()),
            dependencies: Some(vec![
                "Task x.y".to_string(),
                "1.1".to_string(),
                "Task 1".to_string(),
            ]),
            complexity: Some(3),
            acceptance_criteria: vec!["Done".to_string()],
            validation: vec!["cargo test".to_string()],
        };
        let all_ids = HashSet::from(["Task 2.5".to_string()]);
        let errs = validate_task("plan.md", &task, &all_ids);
        let invalid: Vec<&String> = errs
            .iter()
            .filter(|e| e.contains("invalid dependency"))
            .collect();
        assert_eq!(
            invalid.len(),
            1,
            "expected a single grouped invalid-dep error, got: {errs:?}",
        );
        let msg = invalid[0];
        assert!(msg.contains("line 87"), "missing line ref: {msg}");
        assert!(msg.contains("e.g. 'Task 1.2'"), "missing example: {msg}");
        assert!(msg.contains("'Task x.y'"), "missing first bad value: {msg}");
        assert!(msg.contains("'1.1'"), "missing second bad value: {msg}");
        assert!(msg.contains("'Task 1'"), "missing third bad value: {msg}");
    }

    #[test]
    fn validate_task_accepts_well_formed_task() {
        let task = Task {
            id: "Task 2.3".to_string(),
            name: "good".to_string(),
            sprint: 2,
            start_line: 10,
            location: vec!["src/lib.rs".to_string()],
            description: Some("Ship feature".to_string()),
            dependencies: Some(vec!["Task 2.1".to_string()]),
            complexity: Some(5),
            acceptance_criteria: vec!["Done".to_string()],
            validation: vec!["cargo test".to_string()],
        };
        let all_ids = HashSet::from(["Task 2.1".to_string(), "Task 2.3".to_string()]);
        let errs = validate_task("plan.md", &task, &all_ids);
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn clean_source_value_unwraps_backticks() {
        assert_eq!(clean_source_value("`docs/source.md`"), "docs/source.md");
        assert_eq!(clean_source_value(" docs/source.md "), "docs/source.md");
    }

    #[test]
    fn allowed_source_types_are_exact() {
        assert!(is_allowed_source_type("discussion-to-implementation-doc"));
        assert!(is_allowed_source_type("review-to-improvement-doc"));
        assert!(is_allowed_source_type("existing issue/spec"));
        assert!(is_allowed_source_type("plan-only waiver"));
        assert!(!is_allowed_source_type("review"));
    }

    #[test]
    fn every_emitted_pattern_is_catalogued_or_explicitly_opted_out() {
        for pattern in ALL_EMITTED_ERROR_PATTERNS {
            let matched_catalog = EXPLAIN_CATALOG
                .iter()
                .any(|entry| pattern.contains(entry.pattern));
            let matched_optout = KNOWN_UNCATALOGUED.iter().any(|frag| pattern.contains(*frag));
            assert!(
                matched_catalog || matched_optout,
                "emitted error pattern is neither catalogued nor opted out: {pattern}",
            );
        }
    }

    #[test]
    fn known_uncatalogued_does_not_shadow_catalog_patterns() {
        for entry in EXPLAIN_CATALOG {
            for opt in KNOWN_UNCATALOGUED {
                assert!(
                    !entry.pattern.contains(opt),
                    "EXPLAIN_CATALOG pattern '{}' overlaps KNOWN_UNCATALOGUED fragment '{}'",
                    entry.pattern,
                    opt,
                );
            }
        }
    }

    #[test]
    fn explanations_for_returns_matched_and_uncatalogued() {
        let errors = vec![
            "plan.md: Location must be repo-relative (no leading '/'): '/abs'".to_string(),
            "plan.md: failed to read source doc 'docs/x.md': io error".to_string(),
            "plan.md: something brand new that nobody indexed".to_string(),
        ];
        let result = explanations_for(&errors);
        assert!(
            result.matched.iter().any(|e| e.class == "location-absolute"),
            "expected location-absolute, got: {:?}",
            result.matched,
        );
        // KNOWN_UNCATALOGUED fragment "failed to read source doc" silences this one.
        assert!(
            result
                .uncatalogued
                .iter()
                .all(|note| !note.contains("failed to read source doc")),
            "KNOWN_UNCATALOGUED entry leaked into uncatalogued: {:?}",
            result.uncatalogued,
        );
        // The brand-new error must surface as an uncatalogued note.
        assert!(
            result
                .uncatalogued
                .iter()
                .any(|note| note.contains("brand new that nobody indexed")),
            "uncatalogued: {:?}",
            result.uncatalogued,
        );
    }

    #[test]
    fn explain_catalog_contains_bundle_source_doc_classes() {
        let classes: Vec<&str> = EXPLAIN_CATALOG.iter().map(|e| e.explain.class).collect();
        for required in [
            "bundle-primary-source-mismatch",
            "bundle-source-doc-missing-plan-label",
            "bundle-source-doc-missing-execution-state-label",
        ] {
            assert!(
                classes.contains(&required),
                "missing required catalog class: {required}",
            );
        }
    }
}
