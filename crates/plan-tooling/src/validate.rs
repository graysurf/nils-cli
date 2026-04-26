use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use nils_common::git as common_git;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
use serde::Serialize;

use crate::parse::{Plan, Sprint, Task, parse_plan_with_display};

const USAGE: &str = r#"Usage:
  validate_plans.sh [--file <path>]... [--format text|json]

Purpose:
  Lint plan markdown files under docs/plans/ against Plan Format v1.

Options:
  --file <path>  Validate a specific plan file (may be repeated)
  --format <fmt> text (default) or json
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
}

pub fn run(args: &[String]) -> i32 {
    let mut files: Vec<String> = Vec::new();
    let mut format = "text".to_string();

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
            };
            return print_json_output(output, 0);
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
        errors.extend(validate_plan(&display_path, &read_path));

        if let Some(p) = progress.as_ref() {
            p.set_position((idx + 1) as u64);
        }
    }

    if let Some(p) = progress.as_ref() {
        p.finish_and_clear();
    }

    if format == "json" {
        let code = if errors.is_empty() { 0 } else { 1 };
        let output = ValidateOutput {
            ok: errors.is_empty(),
            files: discovered_for_output,
            errors,
        };
        return print_json_output(output, code);
    }

    if errors.is_empty() {
        return 0;
    }

    for err in errors {
        eprintln!("error: {err}");
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

fn validate_plan(display_path: &str, read_path: &Path) -> Vec<String> {
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

    let mut errs: Vec<String> = validate_sprint_metadata(display_path, &plan.sprints);
    for task in tasks {
        errs.extend(validate_task(display_path, task, &all_task_ids));
    }
    errs
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
        contains_angle_placeholder, contains_word_case_insensitive, has_placeholder,
        is_non_empty_list, is_task_id, strip_backtick_spans, validate_task,
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
}
