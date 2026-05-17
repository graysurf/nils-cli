use serde::Serialize;
use std::path::Path;

pub mod to_json;

#[derive(Debug, Clone, Default, Serialize)]
pub struct SprintMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_grouping_intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_width: Option<usize>,
}

fn sprint_metadata_is_empty(metadata: &SprintMetadata) -> bool {
    metadata.pr_grouping_intent.is_none()
        && metadata.execution_profile.is_none()
        && metadata.parallel_width.is_none()
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub title: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_first: Option<ReadFirst>,
    pub sprints: Vec<Sprint>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReadFirst {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_questions: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Sprint {
    pub number: i32,
    pub name: String,
    pub start_line: u32,
    pub tasks: Vec<Task>,
    #[serde(skip_serializing_if = "sprint_metadata_is_empty")]
    pub metadata: SprintMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub sprint: i32,
    pub start_line: u32,
    pub location: Vec<String>,
    pub description: Option<String>,
    pub dependencies: Option<Vec<String>>,
    pub complexity: Option<i32>,
    pub acceptance_criteria: Vec<String>,
    pub validation: Vec<String>,
}

pub fn parse_plan_with_display(
    path: &Path,
    display_path: &str,
) -> anyhow::Result<(Plan, Vec<String>)> {
    let raw = std::fs::read(path)?;
    let raw_text = String::from_utf8_lossy(&raw);
    let raw_lines: Vec<String> = raw_text.lines().map(|l| l.to_string()).collect();

    let mut plan_title = String::new();
    for line in &raw_lines {
        if let Some(rest) = line.strip_prefix("# ") {
            plan_title = rest.trim().to_string();
            break;
        }
    }

    let read_first = parse_read_first(&raw_lines);

    let mut errors: Vec<String> = Vec::new();

    let mut sprints: Vec<Sprint> = Vec::new();
    let mut current_sprint: Option<Sprint> = None;
    let mut current_task: Option<Task> = None;

    fn finish_task(
        current_task: &mut Option<Task>,
        current_sprint: &mut Option<Sprint>,
        errors: &mut Vec<String>,
        display_path: &str,
    ) {
        let Some(task) = current_task.take() else {
            return;
        };
        let Some(sprint) = current_sprint.as_mut() else {
            errors.push(format!(
                "{display_path}:{}: task outside of any sprint: {}",
                task.start_line, task.id
            ));
            return;
        };
        sprint.tasks.push(task);
    }

    fn finish_sprint(current_sprint: &mut Option<Sprint>, sprints: &mut Vec<Sprint>) {
        if let Some(s) = current_sprint.take() {
            sprints.push(s);
        }
    }

    let mut i: usize = 0;
    while i < raw_lines.len() {
        let line = raw_lines[i].as_str();

        if let Some((number, name)) = parse_sprint_heading(line) {
            finish_task(
                &mut current_task,
                &mut current_sprint,
                &mut errors,
                display_path,
            );
            finish_sprint(&mut current_sprint, &mut sprints);
            current_sprint = Some(Sprint {
                number,
                name,
                start_line: (i + 1) as u32,
                tasks: Vec::new(),
                metadata: SprintMetadata::default(),
            });
            i += 1;
            continue;
        }

        if let Some((sprint_num, seq_num, name)) = parse_task_heading(line) {
            finish_task(
                &mut current_task,
                &mut current_sprint,
                &mut errors,
                display_path,
            );
            current_task = Some(Task {
                id: normalize_task_id(sprint_num, seq_num),
                name,
                sprint: sprint_num,
                start_line: (i + 1) as u32,
                location: Vec::new(),
                description: None,
                dependencies: None,
                complexity: None,
                acceptance_criteria: Vec::new(),
                validation: Vec::new(),
            });
            i += 1;
            continue;
        }

        if current_task.is_none() {
            if let Some((_, field, rest)) = parse_any_field_line(line)
                && let Some(sprint) = current_sprint.as_mut()
            {
                let value = rest.unwrap_or_default();
                match field.as_str() {
                    "PR grouping intent" => {
                        sprint.metadata.pr_grouping_intent = parse_pr_grouping_intent(&value);
                        if sprint.metadata.pr_grouping_intent.is_none() && !value.trim().is_empty()
                        {
                            errors.push(format!(
                                "{display_path}:{}: invalid PR grouping intent (expected per-sprint|group): {}",
                                i + 1,
                                crate::repr::py_repr(value.trim())
                            ));
                        }
                    }
                    "Execution Profile" => {
                        sprint.metadata.execution_profile = parse_execution_profile(&value);
                        if sprint.metadata.execution_profile.is_none() && !value.trim().is_empty() {
                            errors.push(format!(
                                "{display_path}:{}: invalid Execution Profile (expected serial|parallel-xN): {}",
                                i + 1,
                                crate::repr::py_repr(value.trim())
                            ));
                        }
                        sprint.metadata.parallel_width = parse_parallel_width(
                            &value,
                            sprint.metadata.execution_profile.as_deref(),
                        );
                    }
                    _ => {
                        if let Some(expected) = canonical_metadata_field_name(&field) {
                            errors.push(format!(
                                "{display_path}:{}: invalid metadata field {}; use '{}'",
                                i + 1,
                                crate::repr::py_repr(&field),
                                expected
                            ));
                        }
                    }
                }
            }
            i += 1;
            continue;
        }

        let Some((base_indent, field, rest)) = parse_field_line(line) else {
            i += 1;
            continue;
        };

        match field.as_str() {
            "Description" => {
                let (v, next_idx) = parse_scalar_continuation(
                    &raw_lines,
                    i + 1,
                    base_indent,
                    &rest.unwrap_or_default(),
                );
                if let Some(task) = current_task.as_mut() {
                    task.description = Some(v);
                }
                i = next_idx;
            }
            "Complexity" => {
                let (v, next_idx) =
                    parse_complexity_value(&raw_lines, i + 1, base_indent, rest.as_deref());
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    errors.push(format!(
                        "{display_path}:{}: missing Complexity value; omit the field or set a 1-10 integer",
                        i + 1,
                    ));
                } else {
                    match trimmed.parse::<i32>() {
                        Ok(n) => {
                            if let Some(task) = current_task.as_mut() {
                                task.complexity = Some(n);
                            }
                        }
                        Err(_) => {
                            errors.push(format!(
                                "{display_path}:{}: invalid Complexity (expected int): {}",
                                i + 1,
                                crate::repr::py_repr(trimmed)
                            ));
                        }
                    }
                }
                i = next_idx;
            }
            "Location" | "Dependencies" | "Acceptance criteria" | "Validation" => {
                let (items, next_idx) = if let Some(r) = rest.clone() {
                    if !r.trim().is_empty() {
                        let (value, next_idx) =
                            parse_scalar_continuation(&raw_lines, i + 1, base_indent, &r);
                        (vec![value], next_idx)
                    } else {
                        parse_list_block(&raw_lines, i + 1, base_indent)
                    }
                } else {
                    parse_list_block(&raw_lines, i + 1, base_indent)
                };

                if let Some(task) = current_task.as_mut() {
                    let cleaned: Vec<String> =
                        items.into_iter().filter(|x| !x.trim().is_empty()).collect();
                    match field.as_str() {
                        "Location" => task.location.extend(cleaned),
                        "Dependencies" => task.dependencies = Some(cleaned),
                        "Acceptance criteria" => task.acceptance_criteria.extend(cleaned),
                        "Validation" => task.validation.extend(cleaned),
                        _ => {}
                    }
                }

                i = next_idx;
            }
            _ => {
                i += 1;
            }
        }
    }

    finish_task(
        &mut current_task,
        &mut current_sprint,
        &mut errors,
        display_path,
    );
    finish_sprint(&mut current_sprint, &mut sprints);

    for sprint in &mut sprints {
        for task in &mut sprint.tasks {
            let Some(deps) = task.dependencies.clone() else {
                continue;
            };

            let mut normalized: Vec<String> = Vec::new();
            let mut saw_value = false;
            for d in deps {
                let trimmed = d.trim();
                if trimmed.is_empty() {
                    continue;
                }
                saw_value = true;
                if trimmed.eq_ignore_ascii_case("none") {
                    continue;
                }
                for part in trimmed.split(',') {
                    let p = part.trim();
                    if !p.is_empty() {
                        normalized.push(p.to_string());
                    }
                }
            }
            if !saw_value {
                task.dependencies = None;
            } else {
                task.dependencies = Some(normalized);
            }
        }
    }

    Ok((
        Plan {
            title: plan_title,
            file: display_path.to_string(),
            read_first,
            sprints,
        },
        errors,
    ))
}

fn normalize_task_id(sprint: i32, seq: i32) -> String {
    format!("Task {sprint}.{seq}")
}

fn parse_sprint_heading(line: &str) -> Option<(i32, String)> {
    let rest = line.strip_prefix("## Sprint ")?;
    let (num_part, name_part) = rest.split_once(':')?;
    if num_part.is_empty() || !num_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number = num_part.parse::<i32>().ok()?;
    let name = name_part.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some((number, name))
}

fn parse_task_heading(line: &str) -> Option<(i32, i32, String)> {
    let rest = line.strip_prefix("### Task ")?;
    let (id_part, name_part) = rest.split_once(':')?;
    let (sprint_part, seq_part) = id_part.split_once('.')?;
    if sprint_part.is_empty() || !sprint_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if seq_part.is_empty() || !seq_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let sprint_num = sprint_part.parse::<i32>().ok()?;
    let seq_num = seq_part.parse::<i32>().ok()?;
    let name = name_part.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some((sprint_num, seq_num, name))
}

fn parse_field_line(line: &str) -> Option<(usize, String, Option<String>)> {
    let parsed = parse_any_field_line(line)?;
    match parsed.1.as_str() {
        "Location"
        | "Description"
        | "Dependencies"
        | "Complexity"
        | "Acceptance criteria"
        | "Validation"
        | "PR grouping intent"
        | "Execution Profile" => Some(parsed),
        _ => None,
    }
}

fn parse_any_field_line(line: &str) -> Option<(usize, String, Option<String>)> {
    let base_indent = line.chars().take_while(|c| *c == ' ').count();
    let trimmed = line.trim_start_matches(' ');
    let after_space = if let Some(after_dash) = trimmed.strip_prefix('-') {
        after_dash.trim_start()
    } else {
        trimmed
    };
    let after_star = after_space.strip_prefix("**")?;
    let (field, rest) = after_star.split_once("**:")?;
    let field = field.to_string();
    Some((base_indent, field, Some(rest.trim().to_string())))
}

fn parse_read_first(lines: &[String]) -> Option<ReadFirst> {
    let start = lines
        .iter()
        .position(|line| line.trim() == "## Read First")?;
    let mut end = lines.len();
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        if idx > start && line.starts_with("## ") {
            end = idx;
            break;
        }
    }

    let mut read_first = ReadFirst::default();
    let mut i = start + 1;
    while i < end {
        let Some((base_indent, field, rest)) = parse_read_first_field_line(&lines[i]) else {
            i += 1;
            continue;
        };

        let (value, next_idx) = parse_scalar_continuation(lines, i + 1, base_indent, &rest);
        match field.as_str() {
            "Primary source" => read_first.primary_source = Some(value),
            "Source type" => read_first.source_type = Some(value),
            "Open questions carried into execution" => read_first.open_questions = Some(value),
            _ => {}
        }
        i = next_idx.min(end);
    }

    Some(read_first)
}

fn parse_read_first_field_line(line: &str) -> Option<(usize, String, String)> {
    let base_indent = line.chars().take_while(|c| *c == ' ').count();
    let trimmed = line.trim_start_matches(' ');
    let after_dash = trimmed.strip_prefix('-')?.trim_start();
    let (field, rest) = after_dash.split_once(':')?;
    let field = field.trim().trim_matches('*').to_string();
    Some((base_indent, field, rest.trim().to_string()))
}

fn canonical_metadata_field_name(field: &str) -> Option<&'static str> {
    if field.eq_ignore_ascii_case("PR grouping intent") && field != "PR grouping intent" {
        return Some("PR grouping intent");
    }
    if field.eq_ignore_ascii_case("Execution Profile") && field != "Execution Profile" {
        return Some("Execution Profile");
    }
    None
}

fn parse_pr_grouping_intent(text: &str) -> Option<String> {
    let token = extract_primary_token(text);
    if token.is_empty() {
        return None;
    }
    match token.to_ascii_lowercase().as_str() {
        "per-sprint" | "persprint" | "per_sprint" => Some("per-sprint".to_string()),
        "group" => Some("group".to_string()),
        _ => None,
    }
}

fn parse_execution_profile(text: &str) -> Option<String> {
    let token = extract_primary_token(text);
    if token.is_empty() {
        return None;
    }
    let normalized = token.to_ascii_lowercase();
    if normalized == "serial" {
        return Some(normalized);
    }
    let width = parse_parallel_width_from_profile_token(&normalized)?;
    Some(format!("parallel-x{width}"))
}

fn parse_parallel_width(text: &str, execution_profile: Option<&str>) -> Option<usize> {
    parse_width_after_marker(text, "parallel width")
        .or_else(|| parse_width_after_marker(text, "intended width"))
        .or_else(|| execution_profile.and_then(parse_parallel_width_from_profile_token))
}

fn parse_width_after_marker(text: &str, marker: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let pos = lower.find(marker)?;
    let tail = &lower[pos + marker.len()..];
    let mut digits = String::new();
    let mut reading = false;
    for ch in tail.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            reading = true;
            continue;
        }
        if reading {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse::<usize>().ok().filter(|v| *v > 0)
    }
}

fn parse_parallel_width_from_profile_token(token: &str) -> Option<usize> {
    let digits = token.strip_prefix("parallel-x")?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok().filter(|v| *v > 0)
}

fn extract_primary_token(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(start) = trimmed.find('`')
        && let Some(end_rel) = trimmed[start + 1..].find('`')
    {
        let token = trimmed[start + 1..start + 1 + end_rel].trim();
        if !token.is_empty() {
            return token.to_string();
        }
    }
    trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
}

fn strip_inline_code(text: &str) -> String {
    let t = text.trim();
    if t.len() >= 2 && t.starts_with('`') && t.ends_with('`') {
        return t[1..t.len() - 1].trim().to_string();
    }
    t.to_string()
}

fn parse_scalar_continuation(
    lines: &[String],
    start_idx: usize,
    base_indent: usize,
    initial: &str,
) -> (String, usize) {
    let mut parts: Vec<String> = Vec::new();
    let first = strip_inline_code(initial);
    if !first.trim().is_empty() {
        parts.push(first);
    }

    let mut i = start_idx;
    while i < lines.len() {
        let raw = lines[i].as_str();
        if raw.trim().is_empty() {
            i += 1;
            continue;
        }

        let indent = raw.chars().take_while(|c| *c == ' ').count();
        if indent <= base_indent {
            break;
        }
        if parse_any_field_line(raw).is_some() {
            break;
        }

        parts.push(strip_inline_code(raw.trim_start_matches(' ')));
        i += 1;
    }

    (join_continuation_parts(parts), i)
}

fn parse_complexity_value(
    lines: &[String],
    start_idx: usize,
    base_indent: usize,
    rest: Option<&str>,
) -> (String, usize) {
    let Some(rest) = rest else {
        return (String::new(), start_idx);
    };
    if !rest.trim().is_empty() {
        return parse_scalar_continuation(lines, start_idx, base_indent, rest);
    }

    let (items, next_idx) = parse_list_block(lines, start_idx, base_indent);
    if items.len() == 1 {
        (items[0].clone(), next_idx)
    } else if items.is_empty() {
        (String::new(), next_idx)
    } else {
        (items.join(" "), next_idx)
    }
}

fn join_continuation_parts(parts: Vec<String>) -> String {
    parts
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<String>>()
        .join(" ")
}

fn parse_list_item_line(line: &str) -> Option<(usize, String)> {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    let trimmed = line.trim_start_matches(' ');
    let after_dash = trimmed.strip_prefix('-')?;
    if after_dash.is_empty() || !after_dash.chars().next().unwrap_or('x').is_whitespace() {
        return None;
    }
    Some((
        indent,
        strip_inline_code(after_dash.trim_start().trim_end()),
    ))
}

fn parse_list_block(
    lines: &[String],
    start_idx: usize,
    base_indent: usize,
) -> (Vec<String>, usize) {
    let mut items: Vec<String> = Vec::new();
    let mut i = start_idx;
    while i < lines.len() {
        let raw = lines[i].as_str();
        if raw.trim().is_empty() {
            i += 1;
            continue;
        }

        let Some((indent, text)) = parse_list_item_line(raw) else {
            break;
        };
        if indent <= base_indent {
            break;
        }

        let mut parts = vec![text];
        i += 1;
        while i < lines.len() {
            let raw = lines[i].as_str();
            if raw.trim().is_empty() {
                i += 1;
                continue;
            }

            let next_indent = raw.chars().take_while(|c| *c == ' ').count();
            if next_indent <= base_indent {
                break;
            }
            if let Some((item_indent, _)) = parse_list_item_line(raw)
                && item_indent <= indent
            {
                break;
            }
            if parse_any_field_line(raw).is_some() {
                break;
            }

            parts.push(strip_inline_code(raw.trim_start_matches(' ')));
            i += 1;
        }
        items.push(join_continuation_parts(parts));
    }

    (items, i)
}
