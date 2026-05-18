use std::path::{Component, Path};

use crate::parse::Plan;

pub const DIRECT_SOURCE_DOC_EXECUTION_WAIVER_LABEL: &str = "Direct source-doc execution waiver";
pub const RECOMMENDED_EXECUTION_STATE_LABEL: &str = "Recommended execution state";
pub const RECOMMENDED_PLAN_LABEL: &str = "Recommended plan";
pub const SOURCE_DOCUMENT_LABEL: &str = "Source document";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBundle {
    pub slug: String,
    pub dir: String,
    pub plan_path: String,
    pub discussion_source_path: String,
    pub review_source_path: String,
    pub execution_state_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDocLinks {
    pub recommended_plan: Option<String>,
    pub recommended_execution_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStateLinks {
    pub source_document: Option<String>,
    pub direct_source_doc_execution_waiver: Option<String>,
}

pub fn validate_plan_bundle(
    display_path: &str,
    read_path: &Path,
    plan: &Plan,
    repo_root: &Path,
) -> Vec<String> {
    let Some(bundle) = bundle_for_plan(read_path, repo_root) else {
        return Vec::new();
    };

    let Some(read_first) = plan.read_first.as_ref() else {
        return Vec::new();
    };
    let Some(primary_source_raw) = read_first.primary_source.as_deref() else {
        return Vec::new();
    };
    let primary_source = clean_link_value(primary_source_raw);
    if primary_source.is_empty()
        || primary_source.starts_with("http://")
        || primary_source.starts_with("https://")
        || primary_source.starts_with('#')
        || primary_source.starts_with("plan-only waiver")
    {
        return Vec::new();
    }

    let primary_source = normalize_repo_path(&primary_source);
    let accepted_sources = [
        bundle.discussion_source_path.as_str(),
        bundle.review_source_path.as_str(),
    ];
    if !accepted_sources.contains(&primary_source.as_str()) {
        return vec![format!(
            "{display_path}: bundle Primary source must be an accepted sibling source doc (expected {} or {}, got {})",
            crate::repr::py_repr(&bundle.discussion_source_path),
            crate::repr::py_repr(&bundle.review_source_path),
            crate::repr::py_repr(&primary_source)
        )];
    }

    let mut errors = Vec::new();
    let source_path = repo_root.join(&primary_source);
    if source_path.is_file() {
        match read_source_doc_links(&source_path) {
            Ok(links) => {
                validate_source_links(display_path, &bundle, &primary_source, &links, &mut errors);
            }
            Err(err) => errors.push(format!(
                "{display_path}: failed to read source doc {}: {err}",
                crate::repr::py_repr(&primary_source)
            )),
        }
    }

    let execution_state_path = repo_root.join(&bundle.execution_state_path);
    if execution_state_path.is_file() {
        match read_execution_state_links(&execution_state_path) {
            Ok(links) => validate_execution_state_links(
                display_path,
                &bundle,
                &primary_source,
                &links,
                &mut errors,
            ),
            Err(err) => errors.push(format!(
                "{display_path}: failed to read execution state {}: {err}",
                crate::repr::py_repr(&bundle.execution_state_path)
            )),
        }
    }

    errors
}

pub fn bundle_for_plan(plan_path: &Path, repo_root: &Path) -> Option<PlanBundle> {
    let rel = repo_relative_posix(plan_path, repo_root);
    let path = Path::new(&rel);
    let file_name = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    let slug = parent.file_name()?.to_str()?;

    if !file_name.ends_with("-plan.md") || file_name != format!("{slug}-plan.md") {
        return None;
    }

    let mut components = parent.components();
    if !component_eq(components.next(), "docs")
        || !component_eq(components.next(), "plans")
        || components.next().is_none()
        || components.next().is_some()
    {
        return None;
    }

    let dir = parent.to_string_lossy().replace('\\', "/");
    Some(PlanBundle {
        slug: slug.to_string(),
        plan_path: format!("{dir}/{slug}-plan.md"),
        discussion_source_path: format!("{dir}/{slug}-discussion-source.md"),
        review_source_path: format!("{dir}/{slug}-review-source.md"),
        execution_state_path: format!("{dir}/{slug}-execution-state.md"),
        dir,
    })
}

pub fn read_source_doc_links(path: &Path) -> anyhow::Result<SourceDocLinks> {
    let text = std::fs::read_to_string(path)?;
    Ok(SourceDocLinks {
        recommended_plan: markdown_field(&text, RECOMMENDED_PLAN_LABEL)
            .map(|v| normalize_repo_path(&v)),
        recommended_execution_state: markdown_field(&text, RECOMMENDED_EXECUTION_STATE_LABEL)
            .map(|v| normalize_repo_path(&v)),
    })
}

pub fn read_execution_state_links(path: &Path) -> anyhow::Result<ExecutionStateLinks> {
    let text = std::fs::read_to_string(path)?;
    Ok(ExecutionStateLinks {
        source_document: markdown_field(&text, SOURCE_DOCUMENT_LABEL)
            .map(|v| normalize_repo_path(&v)),
        direct_source_doc_execution_waiver: markdown_field(
            &text,
            DIRECT_SOURCE_DOC_EXECUTION_WAIVER_LABEL,
        )
        .map(|v| v.trim().to_string()),
    })
}

pub(crate) fn repo_relative_posix(path: &Path, repo_root: &Path) -> String {
    let path_abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root_abs = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let rel = path_abs
        .strip_prefix(&root_abs)
        .map(Path::to_path_buf)
        .unwrap_or(path_abs);
    path_to_posix(&rel)
}

pub(crate) fn path_to_posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn normalize_repo_path(value: &str) -> String {
    let cleaned = clean_link_value(value);
    let normalized = cleaned.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                let _ = parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

pub(crate) fn clean_link_value(value: &str) -> String {
    let trimmed = value.trim().trim_matches('"').trim();
    let unwrapped = if trimmed.len() >= 2 && trimmed.starts_with('`') && trimmed.ends_with('`') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    unwrapped.trim().to_string()
}

pub(crate) fn markdown_field(text: &str, label: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let prefix = format!("{label}:");
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(item) = trimmed.strip_prefix("- ") else {
            continue;
        };
        let Some(rest) = item.strip_prefix(&prefix) else {
            continue;
        };
        let rest = rest.trim();
        if !rest.is_empty() {
            return Some(clean_link_value(rest));
        }
        for next in lines.iter().skip(idx + 1) {
            let candidate = next.trim();
            if candidate.is_empty() {
                continue;
            }
            if candidate.starts_with("- ") {
                break;
            }
            return Some(clean_link_value(candidate));
        }
        return Some(String::new());
    }
    None
}

fn validate_source_links(
    display_path: &str,
    bundle: &PlanBundle,
    primary_source: &str,
    links: &SourceDocLinks,
    errors: &mut Vec<String>,
) {
    match links.recommended_plan.as_deref() {
        Some(actual) if actual == bundle.plan_path => {}
        Some(actual) => errors.push(format!(
            "{display_path}: source doc {} recommends wrong plan (expected {}, got {})",
            crate::repr::py_repr(primary_source),
            crate::repr::py_repr(&bundle.plan_path),
            crate::repr::py_repr(actual)
        )),
        None => errors.push(format!(
            "{display_path}: source doc {} missing `{RECOMMENDED_PLAN_LABEL}`",
            crate::repr::py_repr(primary_source)
        )),
    }

    match links.recommended_execution_state.as_deref() {
        Some(actual) if actual == bundle.execution_state_path => {}
        Some(actual) => errors.push(format!(
            "{display_path}: source doc {} recommends wrong execution state (expected {}, got {})",
            crate::repr::py_repr(primary_source),
            crate::repr::py_repr(&bundle.execution_state_path),
            crate::repr::py_repr(actual)
        )),
        None => errors.push(format!(
            "{display_path}: source doc {} missing `{RECOMMENDED_EXECUTION_STATE_LABEL}`",
            crate::repr::py_repr(primary_source)
        )),
    }
}

fn validate_execution_state_links(
    display_path: &str,
    bundle: &PlanBundle,
    primary_source: &str,
    links: &ExecutionStateLinks,
    errors: &mut Vec<String>,
) {
    match links.source_document.as_deref() {
        Some(actual) if actual == bundle.plan_path => {}
        Some(actual) if actual == primary_source => {
            if !has_direct_source_doc_waiver(links.direct_source_doc_execution_waiver.as_deref()) {
                errors.push(format!(
                    "{display_path}: execution state {} points directly to source doc {} without `{DIRECT_SOURCE_DOC_EXECUTION_WAIVER_LABEL}`",
                    crate::repr::py_repr(&bundle.execution_state_path),
                    crate::repr::py_repr(primary_source)
                ));
            }
        }
        Some(actual) => errors.push(format!(
            "{display_path}: execution state {} points to wrong source document (expected {} or waived direct source doc {}, got {})",
            crate::repr::py_repr(&bundle.execution_state_path),
            crate::repr::py_repr(&bundle.plan_path),
            crate::repr::py_repr(primary_source),
            crate::repr::py_repr(actual)
        )),
        None => errors.push(format!(
            "{display_path}: execution state {} missing `{SOURCE_DOCUMENT_LABEL}`",
            crate::repr::py_repr(&bundle.execution_state_path)
        )),
    }
}

fn has_direct_source_doc_waiver(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return false;
    };
    let normalized = value.to_ascii_lowercase();
    !matches!(normalized.as_str(), "not applicable" | "n/a" | "none")
}

fn component_eq(component: Option<Component<'_>>, expected: &str) -> bool {
    matches!(component, Some(Component::Normal(v)) if v.to_str() == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::{
        DIRECT_SOURCE_DOC_EXECUTION_WAIVER_LABEL, bundle_for_plan, markdown_field,
        normalize_repo_path,
    };
    use std::path::Path;

    #[test]
    fn bundle_derives_repo_relative_sibling_paths() {
        let repo = Path::new("/tmp/repo");
        let bundle = bundle_for_plan(Path::new("/tmp/repo/docs/plans/demo/demo-plan.md"), repo)
            .expect("bundle");
        assert_eq!(bundle.slug, "demo");
        assert_eq!(bundle.plan_path, "docs/plans/demo/demo-plan.md");
        assert_eq!(
            bundle.discussion_source_path,
            "docs/plans/demo/demo-discussion-source.md"
        );
        assert_eq!(
            bundle.execution_state_path,
            "docs/plans/demo/demo-execution-state.md"
        );
    }

    #[test]
    fn bundle_rejects_non_sibling_plan_shape() {
        assert!(
            bundle_for_plan(
                Path::new("/tmp/repo/docs/plans/demo/custom-plan.md"),
                Path::new("/tmp/repo"),
            )
            .is_none()
        );
    }

    #[test]
    fn markdown_field_reads_continuation_value() {
        let text = format!(
            "- {DIRECT_SOURCE_DOC_EXECUTION_WAIVER_LABEL}:\n  `bounded direct execution`\n"
        );
        assert_eq!(
            markdown_field(&text, DIRECT_SOURCE_DOC_EXECUTION_WAIVER_LABEL),
            Some("bounded direct execution".to_string())
        );
    }

    #[test]
    fn normalize_repo_path_handles_unix_relative_segments() {
        assert_eq!(
            normalize_repo_path("`docs/plans/demo/../demo/demo-plan.md`"),
            "docs/plans/demo/demo-plan.md"
        );
    }
}
