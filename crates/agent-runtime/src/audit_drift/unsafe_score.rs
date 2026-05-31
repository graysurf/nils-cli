//! Composite unsafe-score drift class.
//!
//! The score is intentionally small and explainable:
//!
//! - sensitive path match: 0.4
//! - keyword prefix with a nearby value-shaped token: 0.4
//! - high-entropy value run: 0.4
//!
//! Scores `>= 0.8` block, scores `>= 0.4` warn, and lower scores are
//! suppressed. Suppressed findings stay in the in-memory report so
//! `audit-drift --verbose` can explain why a keyword-only line did not
//! become a warning.

use crate::audit_drift::walk;
use crate::audit_drift::{DriftReport, Finding, PRODUCTS, Severity};
use crate::render::manifest::SourceRoot;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub const CLASS: &str = "unsafe";

const SIGNAL_WEIGHT: f64 = 0.4;
const BLOCK_THRESHOLD: f64 = 0.8;
const WARN_THRESHOLD: f64 = 0.4;
const ENTROPY_THRESHOLD: f64 = 4.0;
const MIN_VALUE_RUN: usize = 24;

const KEYWORDS: &[&str] = &[
    "token",
    "api_key",
    "password",
    "bearer",
    "secret",
    "private_key",
];

pub fn check(root: &SourceRoot, report: &mut DriftReport) -> Result<()> {
    for product in PRODUCTS {
        let build_dir = root.path().join("build").join(product);
        scan_tree(root, &build_dir, Some((*product).to_string()), report)?;
    }
    for sub in ["core", "targets", "manifests", "tests/drift"] {
        let dir = root.path().join(sub);
        scan_tree(root, &dir, None, report)?;
    }
    Ok(())
}

fn scan_tree(
    root: &SourceRoot,
    dir: &Path,
    product: Option<String>,
    report: &mut DriftReport,
) -> Result<()> {
    for path in walk::collect_files_under(dir, root.path()) {
        scan_file(root, &path, product.as_deref(), report)?;
    }
    Ok(())
}

fn scan_file(
    root: &SourceRoot,
    path: &Path,
    product: Option<&str>,
    report: &mut DriftReport,
) -> Result<()> {
    let bytes =
        fs::read(path).with_context(|| format!("audit-drift unsafe read {}", path.display()))?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };
    let rel = path.strip_prefix(root.path()).unwrap_or(path).to_path_buf();
    let analysis = analyze_file(&rel, text);
    if analysis.should_report() {
        report.push(Finding {
            class: CLASS,
            severity: analysis.severity(),
            product: product.map(str::to_string),
            path: rel,
            message: analysis.message(),
        });
    }
    Ok(())
}

#[derive(Debug, Default)]
struct UnsafeAnalysis {
    path_match: bool,
    keyword_prefix: Option<usize>,
    entropy_above_threshold: Option<usize>,
    keyword_without_value: Option<usize>,
}

impl UnsafeAnalysis {
    fn score(&self) -> f64 {
        let mut score = 0.0;
        if self.path_match {
            score += SIGNAL_WEIGHT;
        }
        if self.keyword_prefix.is_some() {
            score += SIGNAL_WEIGHT;
        }
        if self.entropy_above_threshold.is_some() {
            score += SIGNAL_WEIGHT;
        }
        score
    }

    fn severity(&self) -> Severity {
        let score = self.score();
        if score >= BLOCK_THRESHOLD {
            Severity::Block
        } else if score >= WARN_THRESHOLD {
            Severity::Warn
        } else {
            Severity::Suppressed
        }
    }

    fn should_report(&self) -> bool {
        self.score() >= WARN_THRESHOLD || self.keyword_without_value.is_some()
    }

    fn message(&self) -> String {
        let mut signals = Vec::new();
        if self.path_match {
            signals.push("path_match".to_string());
        }
        if let Some(line) = self.keyword_prefix {
            signals.push(format!("keyword_prefix(line {line})"));
        }
        if let Some(line) = self.entropy_above_threshold {
            signals.push(format!("entropy_above_threshold(line {line})"));
        }
        if signals.is_empty()
            && let Some(line) = self.keyword_without_value
        {
            signals.push(format!("keyword_without_value(line {line})"));
        }
        format!(
            "score={score:.1} signals={signals}; thresholds: >=0.8 block, >=0.4 warn, <0.4 suppressed",
            score = self.score(),
            signals = signals.join(","),
        )
    }
}

fn analyze_file(path: &Path, text: &str) -> UnsafeAnalysis {
    let mut analysis = UnsafeAnalysis {
        path_match: matches_sensitive_path(path),
        ..UnsafeAnalysis::default()
    };

    for (line_idx, line) in text.lines().enumerate() {
        let line_number = line_idx + 1;
        if analysis.keyword_prefix.is_none() && line_has_keyword_value(line) {
            analysis.keyword_prefix = Some(line_number);
        } else if analysis.keyword_without_value.is_none() && line_has_keyword(line) {
            analysis.keyword_without_value = Some(line_number);
        }

        if analysis.entropy_above_threshold.is_none() && line_has_high_entropy_run(line) {
            analysis.entropy_above_threshold = Some(line_number);
        }

        if analysis.keyword_prefix.is_some() && analysis.entropy_above_threshold.is_some() {
            break;
        }
    }

    analysis
}

fn matches_sensitive_path(path: &Path) -> bool {
    let normalized = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if normalized == "auth.json" || normalized.ends_with("/auth.json") {
        return true;
    }
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".credentials"))
    {
        return true;
    }
    normalized.starts_with("sessions/") || normalized.contains("/sessions/")
}

fn line_has_keyword(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    KEYWORDS.iter().any(|keyword| lower.contains(keyword))
}

fn line_has_keyword_value(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    for keyword in KEYWORDS {
        let mut search_from = 0usize;
        while let Some(idx) = lower[search_from..].find(keyword) {
            let after_idx = search_from + idx + keyword.len();
            let after = &line[after_idx..];
            if value_runs(after).any(|run| is_value_shaped(run, 8)) {
                return true;
            }
            search_from = after_idx;
        }
    }
    false
}

fn line_has_high_entropy_run(line: &str) -> bool {
    value_runs(line).any(|run| {
        is_entropy_candidate(run)
            && run.len() >= MIN_VALUE_RUN
            && shannon_entropy(run) >= ENTROPY_THRESHOLD
    })
}

fn value_runs(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !is_value_char(c))
        .filter(|run| !run.is_empty())
}

fn is_value_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '=')
}

fn is_value_shaped(run: &str, min_len: usize) -> bool {
    if run.len() < min_len {
        return false;
    }
    let has_digit = run.chars().any(|c| c.is_ascii_digit());
    let has_symbol = run
        .chars()
        .any(|c| matches!(c, '_' | '-' | '.' | '+' | '/' | '='));
    let has_upper = run.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = run.chars().any(|c| c.is_ascii_lowercase());
    has_digit || has_symbol || (has_upper && has_lower)
}

fn is_entropy_candidate(run: &str) -> bool {
    // Long path/template references can have high character variety
    // while being ordinary source text. Requiring at least one digit
    // keeps the entropy signal focused on token-like values and lets
    // keyword/path signals handle lower-entropy secret names.
    run.chars().any(|c| c.is_ascii_digit())
}

fn shannon_entropy(value: &str) -> f64 {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for byte in bytes {
        counts[*byte as usize] += 1;
    }
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / len;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn sensitive_path_patterns_match_documented_set() {
        assert!(matches_sensitive_path(Path::new("core/auth.json")));
        assert!(matches_sensitive_path(Path::new("core/.credentials.local")));
        assert!(matches_sensitive_path(Path::new(
            "build/codex/sessions/abc.json"
        )));
        assert!(!matches_sensitive_path(Path::new("core/skills/SKILL.md")));
    }

    #[test]
    fn keyword_without_value_is_suppressed() {
        let analysis = analyze_file(Path::new("core/notes.md"), "password rotation only\n");
        assert_eq!(analysis.score(), 0.0);
        assert_eq!(analysis.severity(), Severity::Suppressed);
        assert!(analysis.should_report());
    }

    #[test]
    fn path_keyword_entropy_scores_full_weight() {
        let analysis = analyze_file(
            Path::new("core/auth.json"),
            "token: 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
        );
        assert!((analysis.score() - 1.2).abs() < 0.000_001);
        assert_eq!(analysis.severity(), Severity::Block);
    }

    #[test]
    fn entropy_threshold_uses_bits_per_byte() {
        assert!(shannon_entropy("4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd") >= ENTROPY_THRESHOLD);
        assert!(shannon_entropy("aaaaaaaaaaaaaaaaaaaaaaaaaaaa") < ENTROPY_THRESHOLD);
    }

    #[test]
    fn entropy_signal_ignores_long_path_like_runs_without_digits() {
        assert!(!line_has_high_entropy_run(
            r#"script(path="core/skills/codex_only/SKILL.md.tera")"#
        ));
    }

    #[test]
    fn signal_names_are_deduped_in_message_order() {
        let analysis = analyze_file(
            Path::new("core/auth.json"),
            "token: 4fK9zQm2Lp8sVx7Tn3Bc6Rj0WaYd\n",
        );
        let message = analysis.message();
        let signals = message
            .split("signals=")
            .nth(1)
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let unique: BTreeSet<_> = signals.split(',').collect();
        assert_eq!(signals.split(',').count(), unique.len());
    }
}
