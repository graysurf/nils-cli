use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::bundle::{bundle_for_plan, markdown_field, normalize_repo_path, path_to_posix};

const SCHEMA_VERSION: &str = "plan-tooling.artifact-audit.v1";
const USAGE: &str = r#"Usage:
  plan-tooling artifact-audit --candidate <path>... [--repo <path>] [--format text|json] [--explain]
  plan-tooling artifact-audit --candidate-file <path> [--repo <path>] [--format text|json] [--explain]

Purpose:
  Classify durable coordination artifacts without deleting or moving files.

Options:
  --candidate <path>       Candidate artifact path (may be repeated)
  --candidate-file <path>  File containing candidate paths, one per line
  --repo <path>            Repository root for reference scans (defaults to detected repo)
  --format <fmt>           text (default) or json
  --explain                Include evidence details in text output
  -h, --help               Show help

Classifications:
  delete         Completed coordination artifact with no maintained references
  keep           Active, incomplete, blocked, or externally referenced artifact
  rehome         Useful retained content in an obsolete coordination location
  manual-review  Ambiguous policy, raw evidence, generated logs, or unsupported artifact

Exit:
  0: audit completed
  1: runtime error
  2: usage error
"#;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    Delete,
    Keep,
    Rehome,
    ManualReview,
}

#[derive(Debug, Serialize)]
struct AuditOutput {
    schema_version: &'static str,
    ok: bool,
    root: String,
    items: Vec<AuditItem>,
}

#[derive(Debug, Serialize)]
struct AuditItem {
    path: String,
    classification: Classification,
    reason: String,
    evidence: Vec<String>,
    blocking_references: Vec<String>,
}

#[derive(Debug)]
struct Config {
    candidates: Vec<String>,
    candidate_files: Vec<String>,
    repo: Option<String>,
    format: String,
    explain: bool,
}

pub fn run(args: &[String]) -> i32 {
    let config = match parse_args(args) {
        Ok(config) => config,
        Err(code) => return code,
    };

    let repo_root = match resolve_repo_root(config.repo.as_deref()) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("plan-tooling artifact-audit: {err}");
            return 1;
        }
    };

    let mut candidates = config.candidates;
    for candidate_file in &config.candidate_files {
        match read_candidate_file(&repo_root, candidate_file) {
            Ok(mut paths) => candidates.append(&mut paths),
            Err(err) => {
                eprintln!(
                    "plan-tooling artifact-audit: failed to read candidate file {}: {err}",
                    crate::repr::py_repr(candidate_file)
                );
                return 1;
            }
        }
    }

    if candidates.is_empty() {
        eprintln!("plan-tooling artifact-audit: missing --candidate or --candidate-file");
        return 2;
    }

    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| seen.insert(normalize_repo_path(candidate)));

    let items: Vec<AuditItem> = candidates
        .iter()
        .map(|candidate| audit_candidate(&repo_root, candidate))
        .collect();
    let output = AuditOutput {
        schema_version: SCHEMA_VERSION,
        ok: true,
        root: path_to_posix(&repo_root),
        items,
    };

    if config.format == "json" {
        match serde_json::to_string(&output) {
            Ok(s) => {
                println!("{s}");
                0
            }
            Err(err) => {
                eprintln!("plan-tooling artifact-audit: failed to encode JSON: {err}");
                1
            }
        }
    } else {
        print_text(&output, config.explain);
        0
    }
}

fn parse_args(args: &[String]) -> Result<Config, i32> {
    let mut candidates = Vec::new();
    let mut candidate_files = Vec::new();
    let mut repo = None;
    let mut format = "text".to_string();
    let mut explain = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--candidate" => {
                let Some(v) = args.get(i + 1) else {
                    return usage_error("--candidate requires a path");
                };
                candidates.push(v.to_string());
                i += 2;
            }
            "--candidate-file" => {
                let Some(v) = args.get(i + 1) else {
                    return usage_error("--candidate-file requires a path");
                };
                candidate_files.push(v.to_string());
                i += 2;
            }
            "--repo" => {
                let Some(v) = args.get(i + 1) else {
                    return usage_error("--repo requires a path");
                };
                repo = Some(v.to_string());
                i += 2;
            }
            "--format" => {
                let Some(v) = args.get(i + 1) else {
                    return usage_error("--format requires text|json");
                };
                format = v.to_string();
                i += 2;
            }
            "--explain" => {
                explain = true;
                i += 1;
            }
            "-h" | "--help" => {
                print_usage();
                return Err(0);
            }
            other => return usage_error(&format!("unknown argument: {other}")),
        }
    }

    if format != "text" && format != "json" {
        return usage_error(&format!("invalid --format (expected text|json): {format}"));
    }

    Ok(Config {
        candidates,
        candidate_files,
        repo,
        format,
        explain,
    })
}

fn usage_error(msg: &str) -> Result<Config, i32> {
    eprintln!("plan-tooling artifact-audit: {msg}");
    print_usage();
    Err(2)
}

fn print_usage() {
    let _ = std::io::stderr().write_all(USAGE.as_bytes());
}

fn resolve_repo_root(repo: Option<&str>) -> anyhow::Result<PathBuf> {
    match repo {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                Ok(path)
            } else {
                Ok(std::env::current_dir()?.join(path))
            }
        }
        None => Ok(crate::repo_root::detect()),
    }
}

fn read_candidate_file(repo_root: &Path, candidate_file: &str) -> anyhow::Result<Vec<String>> {
    let path = resolve_repo_relative(repo_root, Path::new(candidate_file));
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToString::to_string)
        .collect())
}

fn audit_candidate(repo_root: &Path, candidate: &str) -> AuditItem {
    let rel = normalize_repo_path(candidate);
    let path = resolve_repo_relative(repo_root, Path::new(&rel));
    if !path.is_file() {
        return item(
            rel,
            Classification::ManualReview,
            "candidate file is missing",
            vec!["missing candidate cannot be classified safely".to_string()],
            Vec::new(),
        );
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            return item(
                rel,
                Classification::ManualReview,
                "candidate is not readable text",
                vec![format!("read error: {err}")],
                Vec::new(),
            );
        }
    };

    if is_manual_review_path(&rel) {
        return item(
            rel,
            Classification::ManualReview,
            "artifact path requires manual retention policy review",
            vec!["HEURISTIC_SYSTEM records, raw evidence, generated logs, and run outputs are never auto-classified".to_string()],
            Vec::new(),
        );
    }

    if has_retained_content_marker(&content) {
        return item(
            rel,
            Classification::Rehome,
            "useful retained content should move out of obsolete coordination location",
            vec!["retained-content marker found".to_string()],
            Vec::new(),
        );
    }

    let blocking_references = external_references(repo_root, &rel);
    if !blocking_references.is_empty() {
        return item(
            rel,
            Classification::Keep,
            "candidate still has maintained references",
            vec!["external reference scan found active links".to_string()],
            blocking_references,
        );
    }

    let status = status_marker(&content);
    if matches!(
        status.as_deref(),
        Some("active") | Some("blocked") | Some("in progress") | Some("in-progress")
    ) {
        return item(
            rel,
            Classification::Keep,
            "candidate status is active, blocked, or in progress",
            vec![format!("status marker: {}", status.unwrap_or_default())],
            Vec::new(),
        );
    }

    if is_completed_bundle_member(repo_root, &path, &content) {
        return item(
            rel,
            Classification::Delete,
            "completed sibling bundle with no maintained references",
            vec![
                "execution-state status complete".to_string(),
                "no external references".to_string(),
            ],
            Vec::new(),
        );
    }

    item(
        rel,
        Classification::ManualReview,
        "artifact status or retention policy is ambiguous",
        vec!["no completed bundle evidence found".to_string()],
        Vec::new(),
    )
}

fn item(
    path: String,
    classification: Classification,
    reason: &str,
    evidence: Vec<String>,
    blocking_references: Vec<String>,
) -> AuditItem {
    AuditItem {
        path,
        classification,
        reason: reason.to_string(),
        evidence,
        blocking_references,
    }
}

fn resolve_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    repo_root.join(path)
}

fn is_manual_review_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("heuristic_system")
        || lower.contains("/runs/")
        || lower.starts_with("runs/")
        || lower.contains("/out/")
        || lower.starts_with("out/")
        || lower.ends_with(".log")
        || lower.ends_with(".wav")
        || lower.ends_with(".mp3")
        || lower.ends_with(".jsonl")
}

fn has_retained_content_marker(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    lower.contains("retained content:")
        || lower.contains("retain as canonical:")
        || lower.contains("reusable guidance:")
}

fn status_marker(content: &str) -> Option<String> {
    markdown_field(content, "Status").map(|value| value.trim().to_ascii_lowercase())
}

fn is_completed_bundle_member(
    repo_root: &Path,
    candidate_path: &Path,
    candidate_content: &str,
) -> bool {
    let Some(bundle) = bundle_from_candidate(repo_root, candidate_path) else {
        return false;
    };

    if status_marker(candidate_content).as_deref() == Some("complete") {
        return true;
    }

    let state_path = repo_root.join(bundle.execution_state_path);
    let Ok(state) = std::fs::read_to_string(state_path) else {
        return false;
    };
    status_marker(&state)
        .as_deref()
        .is_some_and(|status| matches!(status, "complete" | "completed"))
}

fn bundle_from_candidate(
    repo_root: &Path,
    candidate_path: &Path,
) -> Option<crate::bundle::PlanBundle> {
    if let Some(bundle) = bundle_for_plan(candidate_path, repo_root) {
        return Some(bundle);
    }

    let rel = crate::bundle::repo_relative_posix(candidate_path, repo_root);
    let path = Path::new(&rel);
    let file = path.file_name()?.to_str()?;
    let parent = path.parent()?;
    let slug = parent.file_name()?.to_str()?;
    let plan_path = repo_root.join(parent).join(format!("{slug}-plan.md"));
    if !file.starts_with(slug) {
        return None;
    }
    bundle_for_plan(&plan_path, repo_root)
}

fn external_references(repo_root: &Path, candidate_rel: &str) -> Vec<String> {
    let mut references = Vec::new();
    let candidate_file = Path::new(candidate_rel)
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or(candidate_rel);
    let candidate_dir = Path::new(candidate_rel)
        .parent()
        .map(path_to_posix)
        .unwrap_or_default();

    for file in repo_text_files(repo_root) {
        let rel = crate::bundle::repo_relative_posix(&file, repo_root);
        if rel == candidate_rel || same_bundle_dir(&rel, &candidate_dir) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        if content.contains(candidate_rel) || content.contains(candidate_file) {
            references.push(rel);
        }
    }
    references.sort();
    references.dedup();
    references
}

fn same_bundle_dir(path: &str, candidate_dir: &str) -> bool {
    !candidate_dir.is_empty()
        && Path::new(path)
            .parent()
            .map(path_to_posix)
            .is_some_and(|dir| dir == candidate_dir)
}

fn repo_text_files(repo_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_text_files(repo_root, &mut out);
    out
}

fn collect_text_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_text_files(&path, out);
        } else if is_text_artifact(&path) {
            out.push(path);
        }
    }
}

fn is_text_artifact(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|v| v.to_str()),
        Some("md")
            | Some("txt")
            | Some("toml")
            | Some("rs")
            | Some("sh")
            | Some("zsh")
            | Some("bash")
            | Some("py")
            | Some("json")
            | Some("yaml")
            | Some("yml")
    )
}

fn print_text(output: &AuditOutput, explain: bool) {
    for item in &output.items {
        println!(
            "{}\t{}\t{}",
            item.path,
            classification_name(&item.classification),
            item.reason
        );
        if explain {
            for evidence in &item.evidence {
                println!("  evidence: {evidence}");
            }
            for reference in &item.blocking_references {
                println!("  blocking-reference: {reference}");
            }
        }
    }
}

fn classification_name(classification: &Classification) -> &'static str {
    match classification {
        Classification::Delete => "delete",
        Classification::Keep => "keep",
        Classification::Rehome => "rehome",
        Classification::ManualReview => "manual-review",
    }
}

#[cfg(test)]
mod tests {
    use super::{Classification, has_retained_content_marker, is_manual_review_path};

    #[test]
    fn classification_serializes_as_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Classification::ManualReview).expect("json"),
            "\"manual-review\""
        );
    }

    #[test]
    fn retained_content_marker_drives_rehome() {
        assert!(has_retained_content_marker("Retained content: keep this"));
    }

    #[test]
    fn manual_review_path_catches_generated_evidence() {
        assert!(is_manual_review_path("out/tests/run.log"));
        assert!(is_manual_review_path("docs/HEURISTIC_SYSTEM/record.md"));
    }
}
