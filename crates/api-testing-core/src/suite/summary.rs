use std::collections::BTreeMap;

use crate::suite::results::SuiteRunResults;

#[derive(Debug, Clone)]
pub struct SummaryOptions {
    pub slow_n: usize,
    pub hide_skipped: bool,
    pub max_failed: usize,
    pub max_skipped: usize,
}

impl Default for SummaryOptions {
    fn default() -> Self {
        Self {
            slow_n: 5,
            hide_skipped: false,
            max_failed: 50,
            max_skipped: 50,
        }
    }
}

fn sanitize_one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn md_escape_cell(value: &str) -> String {
    sanitize_one_line(value).replace('|', "\\|")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn md_code(value: &str) -> String {
    let s = md_escape_cell(value);
    if s.is_empty() {
        return String::new();
    }
    if !s.contains('`') {
        return format!("`{s}`");
    }
    format!("<code>{}</code>", html_escape(&s))
}

fn md_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    out.push_str("| ");
    out.push_str(&headers.join(" | "));
    out.push_str(" |\n| ");
    out.push_str(&vec!["---"; headers.len()].join(" | "));
    out.push_str(" |\n");
    for row in rows {
        let mut padded = row.clone();
        while padded.len() < headers.len() {
            padded.push(String::new());
        }
        out.push_str("| ");
        out.push_str(&padded[..headers.len()].join(" | "));
        out.push_str(" |\n");
    }
    out
}

fn dur_ms(case: &crate::suite::results::SuiteCaseResult) -> u64 {
    case.duration_ms
}

pub fn render_summary_markdown(results: &SuiteRunResults, options: &SummaryOptions) -> String {
    let mut out = String::new();
    let suite = if results.suite.trim().is_empty() {
        "suite"
    } else {
        results.suite.trim()
    };

    let failed_cases: Vec<&crate::suite::results::SuiteCaseResult> = results
        .cases
        .iter()
        .filter(|c| c.status == "failed")
        .collect();
    let skipped_cases: Vec<&crate::suite::results::SuiteCaseResult> = results
        .cases
        .iter()
        .filter(|c| c.status == "skipped")
        .collect();
    let executed_cases: Vec<&crate::suite::results::SuiteCaseResult> = results
        .cases
        .iter()
        .filter(|c| c.status == "passed" || c.status == "failed")
        .collect();

    let mut slow_cases: Vec<&crate::suite::results::SuiteCaseResult> = executed_cases.clone();
    slow_cases.sort_by_key(|c| std::cmp::Reverse(dur_ms(c)));
    if options.slow_n > 0 && slow_cases.len() > options.slow_n {
        slow_cases.truncate(options.slow_n);
    }

    out.push_str(&format!("## API test summary: {suite}\n\n"));

    out.push_str("### Totals\n");
    out.push_str(&md_table(
        &["total", "passed", "failed", "skipped"],
        &[vec![
            results.summary.total.to_string(),
            results.summary.passed.to_string(),
            results.summary.failed.to_string(),
            results.summary.skipped.to_string(),
        ]],
    ));
    out.push('\n');

    out.push_str("### Run info\n");
    let mut info_rows: Vec<Vec<String>> = Vec::new();
    if !results.run_id.trim().is_empty() {
        info_rows.push(vec!["runId".to_string(), md_code(&results.run_id)]);
    }
    if !results.started_at.trim().is_empty() {
        info_rows.push(vec!["startedAt".to_string(), md_code(&results.started_at)]);
    }
    if !results.finished_at.trim().is_empty() {
        info_rows.push(vec![
            "finishedAt".to_string(),
            md_code(&results.finished_at),
        ]);
    }
    if !results.suite_file.trim().is_empty() {
        info_rows.push(vec!["suiteFile".to_string(), md_code(&results.suite_file)]);
    }
    if !results.output_dir.trim().is_empty() {
        info_rows.push(vec!["outputDir".to_string(), md_code(&results.output_dir)]);
    }
    if info_rows.is_empty() {
        info_rows.push(vec!["(none)".to_string(), String::new()]);
    }
    out.push_str(&md_table(&["field", "value"], &info_rows));
    out.push('\n');

    let case_row_full = |c: &crate::suite::results::SuiteCaseResult| -> Vec<String> {
        vec![
            md_code(&c.id),
            md_escape_cell(&c.case_type),
            md_escape_cell(&c.status),
            dur_ms(c).to_string(),
            md_escape_cell(c.message.as_deref().unwrap_or("")),
            md_code(c.stdout_file.as_deref().unwrap_or("")),
            md_code(c.stderr_file.as_deref().unwrap_or("")),
        ]
    };

    out.push_str(&format!("### Failed ({})\n", failed_cases.len()));
    if failed_cases.is_empty() {
        out.push_str(&md_table(
            &[
                "id",
                "type",
                "status",
                "durationMs",
                "message",
                "stdout",
                "stderr",
            ],
            &[vec!["(none)".to_string()]],
        ));
    } else {
        let shown: Vec<&crate::suite::results::SuiteCaseResult> = if options.max_failed > 0 {
            failed_cases
                .iter()
                .take(options.max_failed)
                .copied()
                .collect()
        } else {
            failed_cases.clone()
        };
        let rows = shown.into_iter().map(case_row_full).collect::<Vec<_>>();
        out.push_str(&md_table(
            &[
                "id",
                "type",
                "status",
                "durationMs",
                "message",
                "stdout",
                "stderr",
            ],
            &rows,
        ));
        if options.max_failed > 0 && failed_cases.len() > options.max_failed {
            out.push_str(&format!(
                "\n_…and {} more failed cases_\n",
                failed_cases.len() - options.max_failed
            ));
        }
    }
    out.push('\n');

    out.push_str(&format!("### Slowest (Top {})\n", options.slow_n));
    if slow_cases.is_empty() {
        out.push_str(&md_table(
            &[
                "id",
                "type",
                "status",
                "durationMs",
                "message",
                "stdout",
                "stderr",
            ],
            &[vec!["(none)".to_string()]],
        ));
    } else {
        let rows = slow_cases
            .into_iter()
            .map(case_row_full)
            .collect::<Vec<_>>();
        out.push_str(&md_table(
            &[
                "id",
                "type",
                "status",
                "durationMs",
                "message",
                "stdout",
                "stderr",
            ],
            &rows,
        ));
    }
    out.push('\n');

    if !options.hide_skipped {
        out.push_str(&format!("### Skipped ({})\n", skipped_cases.len()));
        if skipped_cases.is_empty() {
            out.push_str(&md_table(
                &["id", "type", "message"],
                &[vec!["(none)".to_string()]],
            ));
        } else {
            let mut reasons: BTreeMap<String, u32> = BTreeMap::new();
            for c in &skipped_cases {
                let r = sanitize_one_line(c.message.as_deref().unwrap_or(""));
                let r = if r.is_empty() {
                    "(none)".to_string()
                } else {
                    r
                };
                *reasons.entry(r).or_default() += 1;
            }

            let hint_for = |reason: &str| -> &'static str {
                match reason {
                    "write_cases_disabled" => {
                        "Enable writes with API_TEST_ALLOW_WRITES_ENABLED=true (or --allow-writes) to run allowWrite cases."
                    }
                    "not_selected" => "Case not selected (check --only filter).",
                    "skipped_by_id" => "Case skipped by id (check --skip filter).",
                    "tag_mismatch" => "Case tags did not match selected --tag filters.",
                    _ => "",
                }
            };

            let mut reason_rows: Vec<Vec<String>> = Vec::new();
            for (reason, count) in reasons {
                reason_rows.push(vec![
                    md_code(&reason),
                    count.to_string(),
                    md_escape_cell(hint_for(&reason)),
                ]);
            }
            out.push_str(&md_table(&["reason", "count", "hint"], &reason_rows));
            out.push('\n');

            out.push_str(&format!(
                "#### Cases ({})\n",
                if options.max_skipped > 0 {
                    format!("max {}", options.max_skipped)
                } else {
                    "all".to_string()
                }
            ));
            let shown: Vec<&crate::suite::results::SuiteCaseResult> = if options.max_skipped > 0 {
                skipped_cases
                    .iter()
                    .take(options.max_skipped)
                    .copied()
                    .collect()
            } else {
                skipped_cases.clone()
            };
            let rows = shown
                .into_iter()
                .map(|c| {
                    vec![
                        md_code(&c.id),
                        md_escape_cell(&c.case_type),
                        md_escape_cell(c.message.as_deref().unwrap_or("")),
                    ]
                })
                .collect::<Vec<_>>();
            out.push_str(&md_table(&["id", "type", "message"], &rows));
            if options.max_skipped > 0 && skipped_cases.len() > options.max_skipped {
                out.push_str(&format!(
                    "\n_…and {} more skipped cases_\n",
                    skipped_cases.len() - options.max_skipped
                ));
            }
        }
        out.push('\n');
    }

    out.push_str(&format!("### Executed cases ({})\n", executed_cases.len()));
    if executed_cases.is_empty() {
        out.push_str(&md_table(
            &["id", "status", "durationMs"],
            &[vec!["(none)".to_string()]],
        ));
    } else {
        let rows = executed_cases
            .into_iter()
            .map(|c| {
                vec![
                    md_code(&c.id),
                    md_escape_cell(&c.status),
                    dur_ms(c).to_string(),
                ]
            })
            .collect::<Vec<_>>();
        out.push_str(&md_table(&["id", "status", "durationMs"], &rows));
    }

    out
}

pub fn render_summary_from_json_str(
    raw: &str,
    input_label: Option<&str>,
    options: &SummaryOptions,
) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return format!(
            "## API test summary\n\n- {}\n",
            if let Some(label) = input_label {
                format!("results file not found or empty: `{label}`")
            } else {
                "no input provided (stdin is empty)".to_string()
            }
        );
    }

    // Accept both the raw `SuiteRunResults` shape (pre-Sprint-3.2 callers) and
    // the new `cli.api-test.run.v1` envelope-wrapped shape. Try envelope first
    // so post-migration callers parse cleanly without a fallback fail.
    #[derive(serde::Deserialize)]
    struct EnvelopeShape {
        data: Option<SuiteRunResults>,
    }

    let results: SuiteRunResults = match serde_json::from_str::<EnvelopeShape>(raw) {
        Ok(envelope) if envelope.data.is_some() => envelope.data.expect("checked above"),
        _ => match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                return format!(
                    "## API test summary\n\n- {}\n",
                    if let Some(label) = input_label {
                        format!("invalid JSON in: `{label}`")
                    } else {
                        "invalid JSON from stdin".to_string()
                    }
                );
            }
        },
    };

    render_summary_markdown(&results, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::results::{SuiteCaseResult, SuiteRunResults, SuiteRunSummary};

    fn base_results(summary: SuiteRunSummary, cases: Vec<SuiteCaseResult>) -> SuiteRunResults {
        SuiteRunResults {
            version: 1,
            suite: "sample".to_string(),
            suite_file: "tests/api/suites/sample.suite.json".to_string(),
            run_id: "run-1".to_string(),
            started_at: "2026-02-02T00:00:00Z".to_string(),
            finished_at: "2026-02-02T00:00:10Z".to_string(),
            output_dir: "out/api-test-runner/run-1".to_string(),
            summary,
            cases,
        }
    }

    #[test]
    fn render_summary_markdown_handles_successful_runs() {
        let summary = SuiteRunSummary {
            total: 1,
            passed: 1,
            failed: 0,
            skipped: 0,
        };
        let cases = vec![SuiteCaseResult {
            id: "rest.health".to_string(),
            case_type: "rest".to_string(),
            status: "passed".to_string(),
            duration_ms: 12,
            tags: Vec::new(),
            command: None,
            message: None,
            assertions: None,
            stdout_file: Some("out/run-1/rest.health.response.json".to_string()),
            stderr_file: Some("out/run-1/rest.health.stderr.log".to_string()),
        }];
        let results = base_results(summary, cases);
        let markdown = render_summary_markdown(&results, &SummaryOptions::default());

        assert!(markdown.contains("## API test summary: sample"));
        assert!(markdown.contains("### Failed (0)"));
        assert!(markdown.contains("(none)"));
    }

    #[test]
    fn render_summary_markdown_includes_failed_cases() {
        let summary = SuiteRunSummary {
            total: 1,
            passed: 0,
            failed: 1,
            skipped: 0,
        };
        let cases = vec![SuiteCaseResult {
            id: "gql.health".to_string(),
            case_type: "graphql".to_string(),
            status: "failed".to_string(),
            duration_ms: 120,
            tags: Vec::new(),
            command: None,
            message: Some("boom".to_string()),
            assertions: None,
            stdout_file: Some("out/run-1/gql.health.response.json".to_string()),
            stderr_file: Some("out/run-1/gql.health.stderr.log".to_string()),
        }];
        let results = base_results(summary, cases);
        let markdown = render_summary_markdown(&results, &SummaryOptions::default());

        assert!(markdown.contains("### Failed (1)"));
        assert!(markdown.contains("boom"));
    }

    #[test]
    fn summary_renders_totals_table() {
        let results = SuiteRunResults {
            version: 1,
            suite: "smoke".to_string(),
            suite_file: "tests/api/suites/smoke.suite.json".to_string(),
            run_id: "20260131-000000Z".to_string(),
            started_at: "2026-01-31T00:00:00Z".to_string(),
            finished_at: "2026-01-31T00:00:01Z".to_string(),
            output_dir: "out/api-test-runner/20260131-000000Z".to_string(),
            summary: crate::suite::results::SuiteRunSummary {
                total: 3,
                passed: 2,
                failed: 1,
                skipped: 0,
            },
            cases: vec![],
        };

        let md = render_summary_markdown(&results, &SummaryOptions::default());
        assert!(md.contains("### Totals"));
        assert!(md.contains("| total | passed | failed | skipped |"));
    }

    #[test]
    fn summary_renders_failed_skipped_and_slowest_with_limits() {
        let results = SuiteRunResults {
            version: 1,
            suite: "smoke".to_string(),
            suite_file: "tests/api/suites/smoke.suite.json".to_string(),
            run_id: "run`id & <tag>".to_string(),
            started_at: "2026-01-31T00:00:00Z".to_string(),
            finished_at: "2026-01-31T00:00:05Z".to_string(),
            output_dir: "out/api-test-runner/20260131-000000Z".to_string(),
            summary: crate::suite::results::SuiteRunSummary {
                total: 6,
                passed: 1,
                failed: 3,
                skipped: 2,
            },
            cases: vec![
                crate::suite::results::SuiteCaseResult {
                    id: "fail.1".to_string(),
                    case_type: "rest".to_string(),
                    status: "failed".to_string(),
                    duration_ms: 50,
                    tags: vec![],
                    command: None,
                    message: Some("bad | pipe".to_string()),
                    assertions: None,
                    stdout_file: Some("out/stdout-1.txt".to_string()),
                    stderr_file: Some("out/stderr-1.txt".to_string()),
                },
                crate::suite::results::SuiteCaseResult {
                    id: "fail.2".to_string(),
                    case_type: "graphql".to_string(),
                    status: "failed".to_string(),
                    duration_ms: 150,
                    tags: vec![],
                    command: None,
                    message: Some("write_cases_disabled".to_string()),
                    assertions: None,
                    stdout_file: None,
                    stderr_file: None,
                },
                crate::suite::results::SuiteCaseResult {
                    id: "fail.3".to_string(),
                    case_type: "rest".to_string(),
                    status: "failed".to_string(),
                    duration_ms: 20,
                    tags: vec![],
                    command: None,
                    message: Some("skipped_by_id".to_string()),
                    assertions: None,
                    stdout_file: None,
                    stderr_file: None,
                },
                crate::suite::results::SuiteCaseResult {
                    id: "skip.1".to_string(),
                    case_type: "rest".to_string(),
                    status: "skipped".to_string(),
                    duration_ms: 5,
                    tags: vec![],
                    command: None,
                    message: Some("write_cases_disabled".to_string()),
                    assertions: None,
                    stdout_file: None,
                    stderr_file: None,
                },
                crate::suite::results::SuiteCaseResult {
                    id: "skip.2".to_string(),
                    case_type: "graphql".to_string(),
                    status: "skipped".to_string(),
                    duration_ms: 7,
                    tags: vec![],
                    command: None,
                    message: Some("not_selected".to_string()),
                    assertions: None,
                    stdout_file: None,
                    stderr_file: None,
                },
                crate::suite::results::SuiteCaseResult {
                    id: "pass.1".to_string(),
                    case_type: "rest".to_string(),
                    status: "passed".to_string(),
                    duration_ms: 10,
                    tags: vec![],
                    command: None,
                    message: None,
                    assertions: None,
                    stdout_file: None,
                    stderr_file: None,
                },
            ],
        };

        let options = SummaryOptions {
            max_failed: 1,
            max_skipped: 1,
            slow_n: 1,
            ..SummaryOptions::default()
        };

        let md = render_summary_markdown(&results, &options);
        assert!(md.contains("### Failed (3)"));
        assert!(md.contains("…and 2 more failed cases"));
        assert!(md.contains("### Skipped (2)"));
        assert!(md.contains("…and 1 more skipped cases"));
        assert!(md.contains("### Slowest (Top 1)"));
        assert!(md.contains("<code>run`id &amp; &lt;tag&gt;</code>"));
        assert!(md.contains("bad \\| pipe"));
    }

    #[test]
    fn summary_render_handles_empty_and_invalid_json_input() {
        let empty =
            render_summary_from_json_str("", Some("missing.json"), &SummaryOptions::default());
        assert!(empty.contains("results file not found or empty"));

        let invalid = render_summary_from_json_str("{not-json", None, &SummaryOptions::default());
        assert!(invalid.contains("invalid JSON from stdin"));
    }
}
