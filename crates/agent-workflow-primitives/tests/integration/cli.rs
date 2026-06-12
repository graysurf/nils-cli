use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(bin: &str, dir: &Path, args: &[&str]) -> CmdOutput {
    let agent_home = dir.join(".agent-home");
    let agent_home_value = agent_home.to_string_lossy().to_string();
    run_resolved_in_dir(bin, dir, args, &[("AGENT_HOME", &agent_home_value)], None)
}

fn out_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[test]
fn all_binaries_export_zsh_completion() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    for bin in [
        "agent-run",
        "browser-session",
        "canary-check",
        "docs-impact",
        "heuristic-inbox",
        "model-cross-check",
        "repo-retro",
        "review-evidence",
        "review-specialists",
        "skill-usage",
        "test-first-evidence",
    ] {
        let output = run(bin, tmp.path(), &["completion", "zsh"]);
        assert_eq!(output.code, 0, "{bin} stderr={}", output.stderr_text());
        assert!(
            output.stdout_text().contains(&format!("#compdef {bin}")),
            "missing zsh header for {bin}: {}",
            output.stdout_text()
        );
    }
}

#[test]
fn repo_retro_help_mentions_report_contract() {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    let help = run("repo-retro", tmp.path(), &["--help"]);
    assert_eq!(help.code, 0, "stderr={}", help.stderr_text());
    assert!(help.stdout_text().contains("report"));
    assert!(help.stdout_text().contains("completion"));
    assert!(help.stdout_text().contains("-V"));

    let report_help = run("repo-retro", tmp.path(), &["report", "--help"]);
    assert_eq!(report_help.code, 0, "stderr={}", report_help.stderr_text());
    assert!(report_help.stdout_text().contains("--mode"));
    assert!(report_help.stdout_text().contains("--format"));
    assert!(report_help.stdout_text().contains("--since"));
    assert!(report_help.stdout_text().contains("--days"));
    assert!(report_help.stdout_text().contains("--from"));
    assert!(report_help.stdout_text().contains("--to"));
    assert!(report_help.stdout_text().contains("--timeline-jsonl"));
    assert!(report_help.stdout_text().contains("--learnings-jsonl"));
    assert!(report_help.stdout_text().contains("--validation-jsonl"));
    assert!(report_help.stdout_text().contains("--review-jsonl"));
    assert!(report_help.stdout_text().contains("--incidents-jsonl"));
    assert!(report_help.stdout_text().contains("--decisions-jsonl"));
    assert!(report_help.stdout_text().contains("--history-dir"));
    assert!(report_help.stdout_text().contains("--write"));
}

#[test]
fn repo_retro_reports_git_heuristic_analysis_and_sources() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = create_repo_retro_fixture(tmp.path());
    let repo_arg = out_arg(&repo);

    let output = run(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &repo_arg,
            "--from",
            "2026-05-12",
            "--to",
            "2026-05-16",
            "--mode",
            "team",
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.repo-retro.report.v2");
    assert_eq!(value["data"]["schema"], "repo-retro.report.v2");
    assert_eq!(value["data"]["mode"], "team");
    assert_eq!(value["data"]["repo"]["slug"], "repo");
    assert_eq!(value["data"]["window"]["mode"], "fixed");
    assert_eq!(value["data"]["git"]["summary"]["commitCount"], 5);
    assert_eq!(value["data"]["git"]["commitTypes"]["feat"], 1);
    assert_eq!(value["data"]["git"]["commitTypes"]["test"], 1);
    assert_eq!(value["data"]["git"]["commitTypes"]["fix"], 1);
    assert_eq!(
        value["data"]["git"]["testSignals"]["changedTestFileCount"],
        1
    );
    assert_eq!(
        value["data"]["heuristicSystem"]["activeInbox"]["byStatus"]["triaged"],
        1
    );
    assert_eq!(
        value["data"]["heuristicSystem"]["activeInbox"]["bySeverity"]["medium"],
        1
    );
    assert_eq!(
        value["data"]["heuristicSystem"]["errorInboxMovement"]["archived"]["count"],
        1
    );
    assert_eq!(
        value["data"]["heuristicSystem"]["operationRecords"]["changedCount"],
        1
    );
    assert_eq!(value["data"]["history"]["write"], false);
    assert!(
        value["data"]["analysis"]["themes"]
            .as_array()
            .expect("themes")
            .iter()
            .any(|item| item.as_str().unwrap_or("").contains("feat work"))
    );
    assert!(
        value["data"]["analysis"]["followUpQuestions"]
            .as_array()
            .expect("follow up")
            .iter()
            .any(|item| item.as_str().unwrap_or("").contains("HEURISTIC_SYSTEM"))
    );
    assert!(
        value["data"]["sources"]["commands"]
            .as_array()
            .expect("commands")
            .iter()
            .any(|item| item["command"].as_str().unwrap_or("").contains("git -C"))
    );
}

#[test]
fn repo_retro_discovers_core_policies_heuristic_root_with_nested_entries() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = create_repo_retro_core_policies_fixture(tmp.path());
    let repo_arg = out_arg(&repo);

    let output = run(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &repo_arg,
            "--from",
            "2026-05-12",
            "--to",
            "2026-05-16",
            "--mode",
            "team",
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let heuristic = &value["data"]["heuristicSystem"];
    assert_eq!(heuristic["state"], "present");
    // Nested `<slug>/ENTRY.md` entries are discovered; archived entries and the
    // README index are excluded from the active inbox.
    assert_eq!(heuristic["activeInbox"]["state"], "present");
    assert_eq!(heuristic["activeInbox"]["total"], 1);
    assert_eq!(heuristic["activeInbox"]["byStatus"]["triaged"], 1);
    assert_eq!(heuristic["activeInbox"]["bySeverity"]["medium"], 1);
    assert_eq!(heuristic["errorInboxMovement"]["archived"]["count"], 1);
    assert_eq!(heuristic["operationRecords"]["changedCount"], 1);
}

#[test]
fn repo_retro_heuristic_root_override_points_at_explicit_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = create_repo_retro_core_policies_fixture(tmp.path());
    let repo_arg = out_arg(&repo);

    let output = run(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &repo_arg,
            "--heuristic-root",
            "core/policies/heuristic-system",
            "--from",
            "2026-05-12",
            "--to",
            "2026-05-16",
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["data"]["heuristicSystem"]["state"], "present");
    assert_eq!(value["data"]["heuristicSystem"]["activeInbox"]["total"], 1);
}

#[test]
fn repo_retro_active_days_stay_within_committer_window() {
    // Active days must stay inside the requested window. Two date-field
    // mismatches used to leak a date outside it into activeDays:
    //   1. the window filters on committer date but activeDays collected the
    //      author date (%ad), so a commit authored before the window but
    //      committed inside it surfaced its out-of-window author day, and
    //   2. dates were rendered with --date=short (each commit's stored zone)
    //      while --since/--until parse the boundaries in local time, so a
    //      commit made just inside the window in local time but stored in a
    //      different zone rendered to the previous day.
    // TZ is pinned so both mechanisms are exercised deterministically.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Terry"]);
    git(&repo, &["config", "user.email", "terry@example.com"]);

    // Baseline commit squarely inside the window (Asia/Taipei 2026-05-13).
    fs::write(repo.join("a.txt"), "a\n").expect("write a");
    git(&repo, &["add", "a.txt"]);
    git_with_env(
        &repo,
        &["commit", "-q", "-m", "feat: baseline change"],
        &[
            ("GIT_AUTHOR_DATE", "2026-05-13T12:00:00+0000"),
            ("GIT_COMMITTER_DATE", "2026-05-13T12:00:00+0000"),
        ],
    );

    // Author/committer skew: authored before the window, committed inside it.
    // The committer date (2026-05-13) is in-window; the author date
    // (2026-05-11) must never reach activeDays.
    fs::write(repo.join("b.txt"), "b\n").expect("write b");
    git(&repo, &["add", "b.txt"]);
    git_with_env(
        &repo,
        &["commit", "-q", "-m", "fix: land older authored work"],
        &[
            ("GIT_AUTHOR_DATE", "2026-05-11T12:00:00+0000"),
            ("GIT_COMMITTER_DATE", "2026-05-13T04:00:00+0000"),
        ],
    );

    // Timezone boundary: committed 2026-05-11 16:30Z == 2026-05-12 00:30 in
    // Asia/Taipei, so the window includes it, but the stored +0000 zone renders
    // as 2026-05-11 under the previous --date=short behaviour.
    fs::write(repo.join("c.txt"), "c\n").expect("write c");
    git(&repo, &["add", "c.txt"]);
    git_with_env(
        &repo,
        &["commit", "-q", "-m", "chore: late commit near boundary"],
        &[
            ("GIT_AUTHOR_DATE", "2026-05-11T16:30:00+0000"),
            ("GIT_COMMITTER_DATE", "2026-05-11T16:30:00+0000"),
        ],
    );

    let repo_arg = out_arg(&repo);
    let agent_home = tmp.path().join(".agent-home");
    let agent_home_value = agent_home.to_string_lossy().to_string();
    let output = run_resolved_in_dir(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &repo_arg,
            "--from",
            "2026-05-12",
            "--to",
            "2026-05-16",
            "--format",
            "json",
        ],
        &[("AGENT_HOME", &agent_home_value), ("TZ", "Asia/Taipei")],
        None,
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let summary = &value["data"]["git"]["summary"];
    assert_eq!(
        summary["commitCount"], 3,
        "all three commits fall in window"
    );
    let active_days: Vec<String> = summary["activeDays"]
        .as_array()
        .expect("activeDays")
        .iter()
        .map(|day| day.as_str().expect("day string").to_string())
        .collect();
    assert!(
        !active_days.iter().any(|day| day.as_str() < "2026-05-12"),
        "no active day may precede the window start: {active_days:?}"
    );
    assert_eq!(
        active_days,
        vec!["2026-05-12".to_string(), "2026-05-13".to_string()],
        "active days use committer date rendered in local time"
    );
    assert_eq!(summary["firstCommitDate"], "2026-05-12");
    assert_eq!(summary["lastCommitDate"], "2026-05-13");
}

#[test]
fn repo_retro_loads_all_typed_jsonl_inputs_and_warns_on_malformed_lines() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = create_repo_retro_fixture(tmp.path());
    let repo_arg = out_arg(&repo);
    let timeline = tmp.path().join("timeline.jsonl");
    let learnings = tmp.path().join("learnings.jsonl");
    let validation = tmp.path().join("validation.jsonl");
    let review = tmp.path().join("review.jsonl");
    let incidents = tmp.path().join("incidents.jsonl");
    let decisions = tmp.path().join("decisions.jsonl");
    fs::write(
        &timeline,
        "{\"timestamp\":\"2026-05-13\",\"summary\":\"built retro\"}\nnot-json\n",
    )
    .expect("timeline");
    fs::write(&learnings, "{\"summary\":\"keep deterministic\"}\n").expect("learnings");
    fs::write(&validation, "{\"summary\":\"cargo test passed\"}\n").expect("validation");
    fs::write(&review, "{\"summary\":\"reviewed hotspots\"}\n").expect("review");
    fs::write(&incidents, "{\"summary\":\"no incidents\"}\n").expect("incidents");
    fs::write(&decisions, "{\"summary\":\"ship as repo-retro\"}\n").expect("decisions");

    let output = run(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &repo_arg,
            "--from",
            "2026-05-12",
            "--to",
            "2026-05-16",
            "--timeline-jsonl",
            &out_arg(&timeline),
            "--learnings-jsonl",
            &out_arg(&learnings),
            "--validation-jsonl",
            &out_arg(&validation),
            "--review-jsonl",
            &out_arg(&review),
            "--incidents-jsonl",
            &out_arg(&incidents),
            "--decisions-jsonl",
            &out_arg(&decisions),
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(
        value["data"]["optionalInputs"]["timeline"]["malformedLines"],
        1
    );
    assert_eq!(
        value["data"]["optionalInputs"]["validation"]["validLines"],
        1
    );
    assert_eq!(value["data"]["optionalInputs"]["review"]["validLines"], 1);
    assert_eq!(
        value["data"]["optionalInputs"]["incidents"]["validLines"],
        1
    );
    assert_eq!(
        value["data"]["optionalInputs"]["decisions"]["validLines"],
        1
    );
    assert!(
        value["data"]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|item| item == "timeline JSONL had 1 malformed line(s)")
    );
}

#[test]
fn repo_retro_history_dir_without_write_does_not_create_files() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = create_repo_retro_fixture(tmp.path());
    let history_dir = tmp.path().join("history");

    let output = run(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &out_arg(&repo),
            "--from",
            "2026-05-12",
            "--to",
            "2026-05-16",
            "--history-dir",
            &out_arg(&history_dir),
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["data"]["history"]["enabled"], true);
    assert_eq!(value["data"]["history"]["write"], false);
    assert!(
        value["data"]["history"]["intended"]["markdown"]
            .as_str()
            .expect("markdown path")
            .ends_with("retros/2026/2026-05-16-repo-repo-retro.md")
    );
    assert!(!history_dir.exists());
}

#[test]
fn repo_retro_history_write_creates_index_raw_and_markdown() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = create_repo_retro_fixture(tmp.path());
    let history_dir = tmp.path().join("history");

    let output = run(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &out_arg(&repo),
            "--from",
            "2026-05-12",
            "--to",
            "2026-05-16",
            "--history-dir",
            &out_arg(&history_dir),
            "--write",
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let markdown_path = Path::new(
        value["data"]["history"]["intended"]["markdown"]
            .as_str()
            .expect("markdown"),
    );
    let json_path = Path::new(
        value["data"]["history"]["intended"]["json"]
            .as_str()
            .expect("json"),
    );
    let index_path = Path::new(
        value["data"]["history"]["intended"]["index"]
            .as_str()
            .expect("index"),
    );
    assert!(markdown_path.is_file());
    assert!(json_path.is_file());
    assert!(index_path.is_file());
    assert!(
        fs::read_to_string(markdown_path)
            .expect("markdown")
            .contains("# Project Retro: repo")
    );
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(json_path).expect("raw"))
            .expect("raw json")["schema"],
        "repo-retro.report.v2"
    );
    let index_rows: Vec<Value> = fs::read_to_string(index_path)
        .expect("index")
        .lines()
        .map(|line| serde_json::from_str(line).expect("index row"))
        .collect();
    assert_eq!(index_rows.last().expect("row")["commitCount"], 5);
    assert_eq!(
        index_rows.last().expect("row")["rawPath"],
        "raw/2026/2026-05-16-repo-repo-retro.json"
    );
}

#[test]
fn repo_retro_invalid_window_uses_usage_exit_code() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let repo = create_repo_retro_fixture(tmp.path());

    let output = run(
        "repo-retro",
        tmp.path(),
        &[
            "report",
            "--repo",
            &out_arg(&repo),
            "--from",
            "2026-05-16",
            "--to",
            "2026-05-12",
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 64);
    assert!(
        output.stdout_text().contains("window start"),
        "stdout={}",
        output.stdout_text()
    );
}

#[test]
fn docs_impact_scans_docs_and_non_docs_changes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    git(tmp.path(), &["init"]);
    fs::create_dir_all(tmp.path().join("src")).expect("src dir");
    fs::create_dir_all(tmp.path().join("docs")).expect("docs dir");
    fs::write(
        tmp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("src write");
    fs::write(tmp.path().join("docs/runbook.md"), "# Runbook\n").expect("docs write");

    let output = run(
        "docs-impact",
        tmp.path(),
        &["scan", "--include-untracked", "--format", "json"],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.docs-impact.scan.v1");
    assert_eq!(value["data"]["docs_changed"], true);
    assert_eq!(value["data"]["non_docs_changed"], true);
    assert!(
        value["data"]["docs_files"]
            .as_array()
            .expect("docs array")
            .iter()
            .any(|path| path == "docs/runbook.md")
    );
}

#[test]
fn canary_check_records_passing_command() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("canary");
    let out = out_arg(&out_dir);

    let run_output = run(
        "canary-check",
        tmp.path(),
        &[
            "run",
            "--out",
            &out,
            "--name",
            "smoke",
            "--command",
            "printf ok",
            "--format",
            "json",
        ],
    );
    assert_eq!(run_output.code, 0, "stderr={}", run_output.stderr_text());
    let run_json = run_output.stdout_json();
    assert_eq!(run_json["schema_version"], "cli.canary-check.run.v1");
    assert_eq!(run_json["data"]["record"]["last_run"]["status"], "pass");

    let verify = run(
        "canary-check",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(
        verify.stdout_json()["data"]["last_run"]["stdout_preview"],
        "ok"
    );
}

#[test]
fn review_evidence_requires_no_open_medium_or_high_findings() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("review");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "review-evidence",
            tmp.path(),
            &["init", "--out", &out, "--subject", "PR #1"]
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "review-evidence",
            tmp.path(),
            &[
                "record-finding",
                "--out",
                &out,
                "--severity",
                "medium",
                "--path",
                "src/lib.rs",
                "--summary",
                "needs guard",
                "--status",
                "fixed",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "review-evidence",
            tmp.path(),
            &[
                "record-validation",
                "--out",
                &out,
                "--command",
                "cargo test",
                "--status",
                "pass",
            ],
        )
        .code,
        0
    );

    let verify = run(
        "review-evidence",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(verify.stdout_json()["data"]["complete"], true);
}

#[test]
fn skill_usage_records_successful_skill_invocation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("skill");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--skill",
                "skills/tools/devex/review-evidence",
                "--intent",
                "record review evidence",
                "--user-request-summary",
                "Review PR #12",
                "--referenced-file",
                "docs/runbook.md",
                "--format",
                "json",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "link-record",
                "--out",
                &out,
                "--type",
                "review-evidence",
                "--path",
                "review-evidence.json",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "record-validation",
                "--out",
                &out,
                "--command",
                "scripts/check.sh --docs",
                "--status",
                "pass",
                "--summary",
                "docs freshness passed",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "record-outcome",
                "--out",
                &out,
                "--status",
                "pass",
                "--summary",
                "skill completed",
                "--artifact",
                "docs/runbook.md",
            ],
        )
        .code,
        0
    );

    let verify = run(
        "skill-usage",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    let value = verify.stdout_json();
    assert_eq!(value["schema_version"], "cli.skill-usage.verify.v1");
    assert_eq!(value["data"]["complete"], true);
    assert_eq!(value["data"]["record"]["schema"], "skill-usage.record.v1");
    assert_eq!(value["data"]["record"]["outcome"]["status"], "pass");
    assert_eq!(
        value["data"]["record"]["linked_records"][0]["type"],
        "review-evidence"
    );
}

#[test]
fn skill_usage_serializes_concurrent_record_mutations() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("skill-concurrent");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--skill",
                "skills/tools/devex/skill-usage",
                "--intent",
                "record skill usage",
                "--user-request-summary",
                "record this workflow",
            ],
        )
        .code,
        0
    );

    let dir = tmp.path().to_path_buf();
    let handles: Vec<_> = (0..16)
        .map(|idx| {
            let dir = dir.clone();
            let out = out.clone();
            thread::spawn(move || {
                let command = format!("check-{idx:02}");
                let summary = format!("validation {idx:02}");
                run(
                    "skill-usage",
                    &dir,
                    &[
                        "record-validation",
                        "--out",
                        &out,
                        "--command",
                        &command,
                        "--status",
                        "pass",
                        "--summary",
                        &summary,
                    ],
                )
            })
        })
        .collect();

    let outputs: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("validation thread should not panic"))
        .collect();
    for (idx, output) in outputs.iter().enumerate() {
        assert_eq!(
            output.code,
            0,
            "worker {idx} failed: stdout={} stderr={}",
            output.stdout_text(),
            output.stderr_text()
        );
    }

    let show = run(
        "skill-usage",
        tmp.path(),
        &["show", "--out", &out, "--format", "json"],
    );
    assert_eq!(show.code, 0, "stderr={}", show.stderr_text());
    let value = show.stdout_json();
    let validation = value["data"]["record"]["validation"]
        .as_array()
        .expect("validation array");
    let mut commands: Vec<String> = validation
        .iter()
        .map(|item| {
            item["command"]
                .as_str()
                .expect("validation command")
                .to_string()
        })
        .collect();
    commands.sort();
    let expected: Vec<String> = (0..16).map(|idx| format!("check-{idx:02}")).collect();

    assert_eq!(commands, expected);
}

#[test]
fn skill_usage_requires_failure_record_for_blocked_outcome() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("skill-blocked");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--skill",
                "skills/tools/devex/skill-usage",
                "--intent",
                "record skill usage",
                "--user-request-summary",
                "record this workflow",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "record-validation",
                "--out",
                &out,
                "--command",
                "scripts/check.sh --docs",
                "--status",
                "pass",
                "--summary",
                "docs freshness passed",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "record-outcome",
                "--out",
                &out,
                "--status",
                "blocked",
                "--summary",
                "missing dependency",
            ],
        )
        .code,
        0
    );

    let verify = run(
        "skill-usage",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 1);
    assert!(
        verify.stdout_text().contains("missing_failure_record"),
        "stdout={}",
        verify.stdout_text()
    );
}

#[test]
fn skill_usage_accepts_validation_waiver_and_redacts_secrets() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("skill-waiver");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--skill",
                "skills/workflows/prompts/parallel-first",
                "--intent",
                "Authorization: Bearer secret-token",
                "--user-request-summary",
                "Enable parallel-first",
                "--validation-waiver",
                "prompt-only mode enablement",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "skill-usage",
            tmp.path(),
            &[
                "record-outcome",
                "--out",
                &out,
                "--status",
                "pass",
                "--summary",
                "mode enabled",
            ],
        )
        .code,
        0
    );

    let verify = run(
        "skill-usage",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    let record =
        fs::read_to_string(out_dir.join("skill-usage.record.json")).expect("skill usage record");
    assert!(record.contains("[REDACTED]"));
    assert!(!record.contains("secret-token"));
}

#[test]
fn browser_session_records_steps_and_verifies() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("browser");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "browser-session",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--target",
                "http://localhost:3000",
                "--goal",
                "verify checkout",
            ],
        )
        .code,
        0
    );
    assert_eq!(
        run(
            "browser-session",
            tmp.path(),
            &[
                "record-step",
                "--out",
                &out,
                "--action",
                "opened checkout page",
                "--status",
                "pass",
            ],
        )
        .code,
        0
    );

    let verify = run(
        "browser-session",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(verify.stdout_json()["data"]["complete"], true);
}

#[test]
fn model_cross_check_requires_primary_and_checker_observations() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("model");
    let out = out_arg(&out_dir);

    assert_eq!(
        run(
            "model-cross-check",
            tmp.path(),
            &[
                "init",
                "--out",
                &out,
                "--prompt",
                "review patch",
                "--primary-model",
                "gpt-5.5",
                "--checker-model",
                "gemini-2.5-pro",
            ],
        )
        .code,
        0
    );
    for (role, model) in [("primary", "gpt-5.5"), ("checker", "gemini-2.5-pro")] {
        let output = run(
            "model-cross-check",
            tmp.path(),
            &[
                "record-observation",
                "--out",
                &out,
                "--role",
                role,
                "--model",
                model,
                "--verdict",
                "pass",
                "--summary",
                "no blocker",
            ],
        );
        assert_eq!(output.code, 0, "{role} stderr={}", output.stderr_text());
    }

    let verify = run(
        "model-cross-check",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(verify.stdout_json()["data"]["complete"], true);
}

#[test]
fn secret_like_inputs_are_redacted() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("canary-secret");
    let out = out_arg(&out_dir);

    let output = run(
        "canary-check",
        tmp.path(),
        &[
            "run",
            "--out",
            &out,
            "--name",
            "secret",
            "--command",
            "printf OPENAI_API_KEY=sk-proj-supersecret",
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let combined = format!(
        "{}\n{}",
        output.stdout_text(),
        fs::read_to_string(out_dir.join("canary-check.json")).expect("record")
    );
    assert!(combined.contains("[REDACTED]"));
    assert!(!combined.contains("sk-proj-supersecret"));
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_with_env(dir: &Path, args: &[&str], envs: &[(&str, &str)]) {
    let mut command = Command::new("git");
    command.args(args).current_dir(dir);
    for (key, value) in envs {
        command.env(key, value);
    }
    let status = command.status().expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn commit_retro_file(repo: &Path, rel: &str, text: &str, message: &str, commit_date: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, text).expect("write fixture file");
    git(repo, &["add", rel]);
    let author_date = format!("{commit_date}T12:00:00+0000");
    git_with_env(
        repo,
        &["commit", "-q", "-m", message],
        &[
            ("GIT_AUTHOR_DATE", &author_date),
            ("GIT_COMMITTER_DATE", &author_date),
        ],
    );
}

fn create_repo_retro_fixture(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Terry"]);
    git(&repo, &["config", "user.email", "terry@example.com"]);
    commit_retro_file(
        &repo,
        "src/retro.rs",
        "pub fn retro() {}\n",
        "feat: add repo retro helper",
        "2026-05-12",
    );
    commit_retro_file(
        &repo,
        "tests/repo_retro_test.rs",
        "#[test]\nfn repo_retro() { assert!(true); }\n",
        "test: cover repo retro helper",
        "2026-05-13",
    );
    commit_retro_file(
        &repo,
        "heuristic-system/error-inbox/runtime-gap.md",
        "# Runtime Gap\n\n## Status\n\n- Status: open\n- First observed: 2026-05-14\n- Area: repo-retro\n- Severity: high\n",
        "fix: retain heuristic inbox record",
        "2026-05-14",
    );
    commit_retro_file(
        &repo,
        "heuristic-system/operation-records/gating.md",
        "# Gating\n",
        "docs: add operation record",
        "2026-05-14",
    );
    fs::create_dir_all(repo.join("heuristic-system/error-inbox/archive/2026"))
        .expect("archive dir");
    git(
        &repo,
        &[
            "mv",
            "heuristic-system/error-inbox/runtime-gap.md",
            "heuristic-system/error-inbox/archive/2026/runtime-gap.md",
        ],
    );
    fs::write(
        repo.join("heuristic-system/error-inbox/current-gap.md"),
        "# Current Gap\n\n## Status\n\n- Status: triaged\n- First observed: 2026-05-15\n- Area: repo-retro\n- Severity: medium\n",
    )
    .expect("current gap");
    git(
        &repo,
        &["add", "heuristic-system/error-inbox/current-gap.md"],
    );
    git_with_env(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            "docs: archive resolved heuristic record",
        ],
        &[
            ("GIT_AUTHOR_DATE", "2026-05-15T12:00:00+0000"),
            ("GIT_COMMITTER_DATE", "2026-05-15T12:00:00+0000"),
        ],
    );
    repo
}

/// Fixture mirroring the real agent-runtime-kit layout: the heuristic-system
/// root lives under `core/policies/` and each inbox case is a `<slug>/ENTRY.md`
/// directory rather than a flat Markdown file.
fn create_repo_retro_core_policies_fixture(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Terry"]);
    git(&repo, &["config", "user.email", "terry@example.com"]);
    commit_retro_file(
        &repo,
        "src/retro.rs",
        "pub fn retro() {}\n",
        "feat: add repo retro helper",
        "2026-05-12",
    );
    // A README index in the inbox root must never count as an active entry.
    commit_retro_file(
        &repo,
        "core/policies/heuristic-system/error-inbox/README.md",
        "# Error inbox\n",
        "docs: seed error inbox index",
        "2026-05-13",
    );
    commit_retro_file(
        &repo,
        "core/policies/heuristic-system/error-inbox/runtime-gap/ENTRY.md",
        "# Runtime Gap\n\n## Status\n\n- Status: open\n- First observed: 2026-05-14\n- Area: repo-retro\n- Severity: high\n",
        "fix: retain heuristic inbox record",
        "2026-05-14",
    );
    // Evidence files alongside an ENTRY.md must not be miscounted as entries.
    commit_retro_file(
        &repo,
        "core/policies/heuristic-system/error-inbox/runtime-gap/evidence/note.md",
        "# Evidence\n",
        "docs: attach inbox evidence",
        "2026-05-14",
    );
    commit_retro_file(
        &repo,
        "core/policies/heuristic-system/operation-records/gating/ENTRY.md",
        "# Gating\n",
        "docs: add operation record",
        "2026-05-14",
    );
    fs::create_dir_all(
        repo.join("core/policies/heuristic-system/error-inbox/archive/2026/runtime-gap"),
    )
    .expect("archive dir");
    git(
        &repo,
        &[
            "mv",
            "core/policies/heuristic-system/error-inbox/runtime-gap/ENTRY.md",
            "core/policies/heuristic-system/error-inbox/archive/2026/runtime-gap/ENTRY.md",
        ],
    );
    fs::create_dir_all(repo.join("core/policies/heuristic-system/error-inbox/current-gap"))
        .expect("current gap dir");
    fs::write(
        repo.join("core/policies/heuristic-system/error-inbox/current-gap/ENTRY.md"),
        "# Current Gap\n\n## Status\n\n- Status: triaged\n- First observed: 2026-05-15\n- Area: repo-retro\n- Severity: medium\n",
    )
    .expect("current gap");
    git(
        &repo,
        &[
            "add",
            "core/policies/heuristic-system/error-inbox/current-gap/ENTRY.md",
        ],
    );
    git_with_env(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            "docs: archive resolved heuristic record",
        ],
        &[
            ("GIT_AUTHOR_DATE", "2026-05-15T12:00:00+0000"),
            ("GIT_COMMITTER_DATE", "2026-05-15T12:00:00+0000"),
        ],
    );
    repo
}

// ---------------------------------------------------------------------------
// heuristic-inbox integration tests (parity with the original reference helper).
// ---------------------------------------------------------------------------

mod heuristic_inbox {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    const ENTRY_TEMPLATE: &str = "# {title}\n\n\
## Status\n\n\
- Status: {status}\n\
- First observed: 2026-05-18\n\
- Area: fixture skill\n\
- Severity: {severity}\n\n\
## Signal\n\n\
The fixture workflow gap was observed and needs triage.\n\n\
## Evidence\n\n\
- Raw record: `{raw_record}`\n\
- Summary: fixture evidence summary\n\
{evidence_extra}\
## Impact\n\n\
Future agents need a retained tracker for this gap.\n\n\
## Current Workaround\n\n\
Use the documented manual workaround.\n\n\
## Promotion Criteria\n\n\
Promote after a durable fix and validation are linked.\n\n\
## Next Action\n\n\
{next_action}\n";

    struct EntryOpts<'a> {
        title: &'a str,
        status: &'a str,
        severity: &'a str,
        raw_record: &'a str,
        evidence_extra: &'a str,
        next_action: &'a str,
        create_evidence_dir: bool,
    }

    impl<'a> Default for EntryOpts<'a> {
        fn default() -> Self {
            Self {
                title: "Fixture Gap",
                status: "open",
                severity: "medium",
                raw_record: "out/projects/example/skill-usage.record.json",
                evidence_extra: "- Durable fix: `docs/fix.md`\n",
                next_action: "Create a focused implementation plan.",
                create_evidence_dir: true,
            }
        }
    }

    fn write_entry(folder: &Path, opts: EntryOpts<'_>) -> PathBuf {
        fs::create_dir_all(folder).expect("entry folder");
        if opts.create_evidence_dir {
            fs::create_dir_all(folder.join("evidence")).expect("evidence dir");
        }
        let entry = folder.join("ENTRY.md");
        let body = ENTRY_TEMPLATE
            .replace("{title}", opts.title)
            .replace("{status}", opts.status)
            .replace("{severity}", opts.severity)
            .replace("{raw_record}", opts.raw_record)
            .replace("{evidence_extra}", opts.evidence_extra)
            .replace("{next_action}", opts.next_action);
        fs::write(&entry, body).expect("write entry");
        entry
    }

    fn write_skill_usage_record(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir).expect("record dir");
        let path = dir.join("skill-usage.record.json");
        let body = serde_json::json!({
            "schema": "skill-usage.record.v1",
            "skill": "skills/workflows/mr/gitlab/deliver-gitlab-mr",
            "started_at": "2026-05-18T07:00:00Z",
            "cwd": "/tmp/project",
            "trigger": "user_explicit",
            "intent": "deliver MR",
            "inputs": {
                "user_request_summary": "Deliver a GitLab MR",
                "referenced_files": [],
                "external_sources": []
            },
            "outcome": {
                "status": "fail",
                "summary": "Pipeline status parsing failed."
            },
            "failures": [{
                "phase": "validation",
                "classification": "script_bug",
                "symptom": "Pipeline status parsing failed. SECRET_TOKEN_SHOULD_NOT_COPY",
                "diagnosis": "The script did not read pipeline.status.",
                "handling": "Recorded an inbox entry.",
                "result": "blocked"
            }],
            "artifacts": [],
            "linked_records": [],
            "validation": [],
            "follow_up": []
        });
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&body).unwrap()),
        )
        .expect("write record");
        path
    }

    fn inbox_root(tmp: &Path) -> PathBuf {
        let inbox = tmp.join("heuristic-system").join("error-inbox");
        fs::create_dir_all(&inbox).expect("inbox dir");
        inbox
    }

    fn collect_files_named(root: &Path, name: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(current) = stack.pop() {
            let Ok(entries) = fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().and_then(|s| s.to_str()) == Some(name) {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn help_lists_canonical_subcommands() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let out = run("heuristic-inbox", tmp.path(), &["--help"]);
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let stdout = out.stdout_text();
        for keyword in [
            "list",
            "verify",
            "new",
            "set-status",
            "archive",
            "ingest-evidence",
            "deliver",
        ] {
            assert!(
                stdout.contains(keyword),
                "missing subcommand '{keyword}' in help: {stdout}"
            );
        }
    }

    fn init_repo_with_record(dir: &Path) {
        nils_test_support::git::init_repo_at_with(
            dir,
            nils_test_support::git::InitRepoOptions::new().with_branch("main"),
        );
        let rec = dir.join("core/policies/heuristic-system/error-inbox/foo/ENTRY.md");
        fs::create_dir_all(rec.parent().unwrap()).expect("mkdir record");
        fs::write(&rec, "# record\n").expect("write record");
    }

    #[test]
    fn deliver_dry_run_renders_plan_without_side_effects() {
        let repo = tempfile::TempDir::new().expect("tempdir");
        init_repo_with_record(repo.path());

        let out = run(
            "heuristic-inbox",
            repo.path(),
            &["deliver", "--dry-run", "--format", "json"],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["data"]["dry_run"], true);
        assert!(payload["data"]["pr_url"].is_null());
        assert!(
            payload["data"]["branch"]
                .as_str()
                .expect("branch")
                .starts_with("docs/")
        );
        let paths = payload["data"]["committed_paths"]
            .as_array()
            .expect("committed_paths array");
        assert_eq!(paths.len(), 1);
        assert!(
            paths[0]
                .as_str()
                .expect("path")
                .ends_with("error-inbox/foo/ENTRY.md")
        );
        // The plan must include the forge-cli pr create step but nothing ran.
        let plan = payload["data"]["plan"].as_array().expect("plan array");
        assert_eq!(plan.len(), 6);
    }

    #[test]
    fn deliver_kind_feature_uses_feat_branch_prefix() {
        let repo = tempfile::TempDir::new().expect("tempdir");
        init_repo_with_record(repo.path());

        let out = run(
            "heuristic-inbox",
            repo.path(),
            &[
                "deliver",
                "--dry-run",
                "--kind",
                "feature",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert!(
            payload["data"]["branch"]
                .as_str()
                .expect("branch")
                .starts_with("feat/")
        );
        assert_eq!(payload["data"]["kind"], "feature");
    }

    #[test]
    fn deliver_without_records_errors_nothing_to_deliver() {
        let repo = tempfile::TempDir::new().expect("tempdir");
        nils_test_support::git::init_repo_at_with(
            repo.path(),
            nils_test_support::git::InitRepoOptions::new().with_branch("main"),
        );

        let out = run(
            "heuristic-inbox",
            repo.path(),
            &["deliver", "--dry-run", "--format", "json"],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["error"]["code"], "nothing-to-deliver");
    }

    #[test]
    fn verify_valid_entry_returns_ok() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("fixture-gap"), EntryOpts::default());
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert_eq!(payload["data"]["ok"], true);
        assert_eq!(payload["data"]["kind"], "inbox");
        assert_eq!(payload["data"]["fields"]["status"], "open");
    }

    #[test]
    fn verify_rejects_invalid_status() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("bad-status"),
            EntryOpts {
                status: "done",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .expect("violations array");
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("invalid status")
        }));
    }

    #[test]
    fn verify_rejects_missing_raw_evidence() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("missing-evidence"), EntryOpts::default());
        let text = fs::read_to_string(&entry).unwrap();
        let stripped = text.replace(
            "- Raw record: `out/projects/example/skill-usage.record.json`\n",
            "",
        );
        fs::write(&entry, stripped).unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .expect("violations array");
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("missing raw evidence pointer")
        }));
    }

    #[test]
    fn verify_detects_duplicate_entries() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let first = write_entry(
            &inbox.join("fixture-gap"),
            EntryOpts {
                title: "Duplicate Gap",
                raw_record: "out/projects/shared/skill-usage.record.json",
                ..EntryOpts::default()
            },
        );
        write_entry(
            &inbox.join("fixture-gap-copy"),
            EntryOpts {
                title: "Duplicate Gap",
                raw_record: "out/projects/shared/skill-usage.record.json",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                first.to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let duplicates = payload["error"]["details"]["duplicates"]
            .as_array()
            .expect("duplicates array");
        assert!(!duplicates.is_empty());
    }

    #[test]
    fn list_outputs_active_entries_in_json() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        write_entry(
            &inbox.join("fixture-gap"),
            EntryOpts {
                severity: "high",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "list",
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let entries = payload["data"]["entries"]
            .as_array()
            .expect("entries array");
        assert_eq!(entries[0]["status"], "open");
        assert_eq!(entries[0]["severity"], "high");
        assert_eq!(entries[0]["archived"], false);
        let path = entries[0]["path"].as_str().unwrap();
        assert!(
            path.ends_with("fixture-gap/ENTRY.md"),
            "unexpected path: {path}"
        );
    }

    #[test]
    fn list_reads_retired_planned_status() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        write_entry(
            &inbox.join("retired-gap"),
            EntryOpts {
                status: "planned",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "list",
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--status",
                "planned",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let entries = payload["data"]["entries"].as_array().unwrap();
        assert_eq!(entries[0]["status"], "planned");
    }

    #[test]
    fn verify_reads_retired_triaged_status() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("retired-gap"),
            EntryOpts {
                status: "triaged",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert_eq!(payload["data"]["ok"], true);
        assert_eq!(payload["data"]["fields"]["status"], "triaged");
    }

    #[test]
    fn list_excludes_archived_by_default() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        write_entry(&inbox.join("active-gap"), EntryOpts::default());
        let archive_dir = inbox.join("archive").join("2026");
        write_entry(
            &archive_dir.join("archived-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "None. Done.",
                ..EntryOpts::default()
            },
        );
        let active = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "list",
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        let with_archive = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "list",
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--include-archived",
                "--format",
                "json",
            ],
        );
        let active_paths: Vec<String> = active.stdout_json()["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap().to_string())
            .collect();
        let archive_paths: Vec<String> = with_archive.stdout_json()["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(active_paths.len(), 1);
        assert!(active_paths[0].contains("active-gap"));
        assert!(archive_paths.iter().any(|p| p.contains("archived-gap")));
    }

    #[test]
    fn new_from_skill_usage_redacts_summary() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let record_dir = tmp.path().join("out").join("skill-usage");
        write_skill_usage_record(&record_dir);
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "new",
                "--from-skill-usage",
                record_dir.to_str().unwrap(),
                "--slug",
                "pipeline-status-gap",
                "--out-dir",
                inbox.to_str().unwrap(),
                "--severity",
                "high",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let entry = PathBuf::from(payload["data"]["path"].as_str().unwrap());
        let entry_text = fs::read_to_string(&entry).unwrap();
        assert!(entry_text.contains("- Status: open"));
        assert!(entry_text.contains("- Severity: high"));
        assert!(entry_text.contains("skill-usage.record.json"));
        assert!(
            !entry_text.contains("SECRET_TOKEN_SHOULD_NOT_COPY"),
            "raw secret leaked into ENTRY.md"
        );
    }

    #[test]
    fn new_from_evidence_redacts_and_passes_verify() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let evidence = tmp.path().join("diagnosis.md");
        fs::write(
            &evidence,
            "# Worktree signing diagnosis\n\nRan validation under /Users/example/Project/x; signing failed.\n",
        )
        .expect("write evidence");
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "new",
                "--from-evidence",
                evidence.to_str().unwrap(),
                "--slug",
                "worktree-signing-gap",
                "--out-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let entry = PathBuf::from(payload["data"]["path"].as_str().unwrap());
        let folder = entry.parent().unwrap();
        let entry_text = fs::read_to_string(&entry).unwrap();
        assert!(entry_text.contains("- Raw record: `evidence/diagnosis.md`"));
        let evidence_copy =
            fs::read_to_string(folder.join("evidence").join("diagnosis.md")).unwrap();
        assert!(
            evidence_copy.contains("<workspace>/Project/x"),
            "absolute home path not redacted: {evidence_copy}"
        );

        let verify = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                folder.to_str().unwrap(),
                "--strict",
                "--format",
                "json",
            ],
        );
        assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
        assert_eq!(verify.stdout_json()["data"]["ok"], true);
    }

    #[test]
    fn new_manual_passes_verify_with_uncaptured_pointer() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "new",
                "--manual",
                "--slug",
                "live-diagnosis-gap",
                "--out-dir",
                inbox.to_str().unwrap(),
                "--area",
                "cli",
                "--severity",
                "high",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let entry = PathBuf::from(payload["data"]["path"].as_str().unwrap());
        let folder = entry.parent().unwrap();
        let entry_text = fs::read_to_string(&entry).unwrap();
        assert!(entry_text.contains("- Area: cli"));
        assert!(entry_text.contains("- Raw record: not captured (manual diagnosis,"));

        let verify = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                folder.to_str().unwrap(),
                "--strict",
                "--format",
                "json",
            ],
        );
        assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
        assert_eq!(verify.stdout_json()["data"]["ok"], true);
    }

    #[test]
    fn new_requires_exactly_one_source() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());

        // No source provided.
        let none = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "new",
                "--slug",
                "no-source",
                "--out-dir",
                inbox.to_str().unwrap(),
            ],
        );
        assert_ne!(none.code, 0, "missing source should fail");

        // Two sources provided.
        let record_dir = tmp.path().join("out").join("skill-usage");
        write_skill_usage_record(&record_dir);
        let both = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "new",
                "--manual",
                "--from-skill-usage",
                record_dir.to_str().unwrap(),
                "--slug",
                "two-sources",
                "--out-dir",
                inbox.to_str().unwrap(),
            ],
        );
        assert_ne!(both.code, 0, "conflicting sources should fail");
    }

    #[test]
    fn set_status_updates_status_and_link() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("fixture-gap"), EntryOpts::default());
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "set-status",
                entry.parent().unwrap().to_str().unwrap(),
                "--status",
                "promoted",
                "--link",
                "docs/plans/example/example-plan.md",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert_eq!(payload["data"]["status"], "promoted");
        let text = fs::read_to_string(&entry).unwrap();
        assert!(text.contains("- Status: promoted"));
        assert!(text.contains("docs/plans/example/example-plan.md"));
    }

    #[test]
    fn set_status_rejects_retired_value() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("fixture-gap"), EntryOpts::default());
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "set-status",
                entry.to_str().unwrap(),
                "--status",
                "planned",
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let msg = payload["error"]["message"].as_str().unwrap_or("");
        assert!(msg.contains("retired lifecycle status"));
        assert!(msg.contains("open|promoted|wontfix"));
        let after = fs::read_to_string(&entry).unwrap();
        assert!(after.contains("- Status: open"));
    }

    #[test]
    fn archive_dry_run_reports_destination() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("fixture-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "None. Fixed and validated.",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--date",
                "2026-05-18",
                "--dry-run",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let destination = payload["data"]["destination"].as_str().unwrap();
        assert!(destination.ends_with("archive/2026/fixture-gap/ENTRY.md"));
        assert_eq!(payload["data"]["dry_run"], true);
        assert!(entry.exists());
    }

    #[test]
    fn archive_requires_yes_in_non_interactive_mode() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("fixture-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "None. Fixed and validated.",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--date",
                "2026-05-18",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 64, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert_eq!(payload["error"]["code"], "archive-confirmation-required");
        assert!(entry.exists());
    }

    #[test]
    fn archive_moves_promoted_entry_with_evidence() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("fixture-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "None. Fixed and validated.",
                ..EntryOpts::default()
            },
        );
        fs::write(
            entry
                .parent()
                .unwrap()
                .join("evidence")
                .join("validation.md"),
            "Local validation: `scripts/check.sh --all` pass.\n",
        )
        .unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--date",
                "2026-05-18",
                "--reason",
                "Fixed and compressed into docs.",
                "--yes",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let destination = PathBuf::from(payload["data"]["destination"].as_str().unwrap());
        assert!(!entry.exists());
        assert!(destination.exists());
        assert!(
            destination
                .parent()
                .unwrap()
                .join("evidence/validation.md")
                .exists()
        );
        let text = fs::read_to_string(&destination).unwrap();
        assert!(text.contains("## Archive"));
        assert!(text.contains("- Archived: 2026-05-18"));
        assert!(text.contains("- Reason: Fixed and compressed into docs."));
    }

    #[test]
    fn archive_without_inbox_dir_resolves_destination_from_case_path() {
        // Regression for #739: with no --inbox-dir, the destination must derive
        // from the absolute case path's own inbox, not the (unrelated) cwd.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let foreign_cwd = tmp.path().join("checkout-a");
        fs::create_dir_all(&foreign_cwd).expect("foreign cwd");
        let inbox = inbox_root(&tmp.path().join("checkout-b"));
        let entry = write_entry(
            &inbox.join("fixture-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "None. Fixed and validated.",
                ..EntryOpts::default()
            },
        );
        let case_folder = entry.parent().unwrap().to_path_buf();
        // Run from the unrelated checkout-a cwd, archiving a case that lives in
        // checkout-b, with no --inbox-dir / --archive-root override.
        let out = run(
            "heuristic-inbox",
            &foreign_cwd,
            &[
                "archive",
                case_folder.to_str().unwrap(),
                "--date",
                "2026-05-18",
                "--yes",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let destination = PathBuf::from(payload["data"]["destination"].as_str().unwrap());
        // The reported destination resolved inside the case's own inbox tree.
        assert!(
            destination.ends_with(
                "checkout-b/heuristic-system/error-inbox/archive/2026/fixture-gap/ENTRY.md"
            ),
            "destination resolved outside the case's own inbox: {destination:?}"
        );
        assert!(
            destination.exists(),
            "destination should exist: {destination:?}"
        );
        // Filesystem truth (independent of path display): the case moved into
        // its own inbox archive, and no stray tree was created under the cwd.
        assert!(!case_folder.exists());
        assert!(
            inbox
                .join("archive")
                .join("2026")
                .join("fixture-gap")
                .join("ENTRY.md")
                .exists()
        );
        assert!(
            !foreign_cwd.join("heuristic-system").exists(),
            "archive must not create a stray tree under the cwd"
        );
    }

    #[test]
    fn archive_accepts_wontfix_with_durable_link() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("accepted-risk"),
            EntryOpts {
                status: "wontfix",
                evidence_extra: "",
                next_action: "None. Risk accepted.",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--date",
                "2026-05-18",
                "--link",
                "docs/accepted-risk.md",
                "--yes",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let destination = PathBuf::from(payload["data"]["destination"].as_str().unwrap());
        let text = fs::read_to_string(&destination).unwrap();
        assert!(text.contains("- Durable link: `docs/accepted-risk.md`"));
    }

    #[test]
    fn archive_rejects_active_status() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("active-gap"), EntryOpts::default());
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("closed status")
        }));
        assert!(entry.exists());
    }

    #[test]
    fn archive_rejects_actionable_next_action() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("actionable-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "Create a follow-up plan.",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(
            violations
                .iter()
                .any(|v| v["message"].as_str().unwrap_or("").contains("Next Action"))
        );
    }

    #[test]
    fn archive_rejects_missing_durable_link() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("missing-link"),
            EntryOpts {
                status: "promoted",
                evidence_extra: "",
                next_action: "None. Fixed.",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("durable outcome link")
        }));
    }

    #[test]
    fn archive_rejects_existing_destination() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("fixture-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "None. Fixed.",
                ..EntryOpts::default()
            },
        );
        write_entry(
            &inbox.join("archive").join("2026").join("fixture-gap"),
            EntryOpts {
                status: "promoted",
                next_action: "None. Already archived.",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "archive",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--date",
                "2026-05-18",
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("archive target already exists")
        }));
    }

    #[test]
    fn verify_rejects_raw_skill_usage_evidence() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("raw-evidence"), EntryOpts::default());
        let raw = entry
            .parent()
            .unwrap()
            .join("evidence")
            .join("skill-usage.record.json");
        fs::write(
            &raw,
            r#"{"schema":"skill-usage.record.v1","outcome":{"status":"ok"}}"#,
        )
        .unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("raw skill-usage record")
        }));
    }

    #[test]
    fn verify_rejects_bearer_token_in_evidence() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("token-evidence"), EntryOpts::default());
        let leaky = entry.parent().unwrap().join("evidence").join("auth.md");
        fs::write(&leaky, "Bearer abcdefghijklmnopqrstuvwxyz1234567890\n").unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("redacted-secret pattern")
        }));
    }

    #[test]
    fn verify_rejects_oversize_evidence() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("huge-evidence"), EntryOpts::default());
        let huge = entry.parent().unwrap().join("evidence").join("huge.md");
        fs::write(&huge, "x".repeat(64 * 1024 + 8)).unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(
            violations
                .iter()
                .any(|v| v["message"].as_str().unwrap_or("").contains("limit is"))
        );
    }

    #[test]
    fn verify_warns_on_body_home_path_without_strict() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("body-home-path"),
            EntryOpts {
                raw_record: "/Users/example/project/out/skill-usage.record.json",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert_eq!(payload["data"]["ok"], true);
        assert_eq!(payload["data"]["strict"], false);
        let body_violations = payload["data"]["body_violations"]
            .as_array()
            .expect("body_violations array");
        assert!(
            body_violations
                .iter()
                .any(|v| { v["kind"].as_str().unwrap_or("") == "body_absolute_home_path" })
        );
        let warnings = payload["data"]["warnings"]
            .as_array()
            .expect("warnings array");
        assert!(warnings.iter().any(|w| {
            w.as_str()
                .unwrap_or("")
                .contains("body warning: body contains absolute home path")
        }));
    }

    #[test]
    fn verify_strict_fails_on_body_home_path() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("body-home-path-strict"),
            EntryOpts {
                raw_record: "/Users/example/project/out/skill-usage.record.json",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--strict",
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        assert_eq!(payload["error"]["details"]["strict"], true);
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .expect("violations array");
        assert!(
            violations
                .iter()
                .any(|v| { v["kind"].as_str().unwrap_or("") == "body_absolute_home_path" })
        );
        let body_violations = payload["error"]["details"]["body_violations"]
            .as_array()
            .expect("body_violations array");
        assert!(
            body_violations
                .iter()
                .any(|v| { v["kind"].as_str().unwrap_or("") == "body_absolute_home_path" })
        );
    }

    #[test]
    fn verify_strict_fails_on_body_token_pattern() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("body-token-strict"),
            EntryOpts {
                evidence_extra: "- Auth: Bearer abcdefghijklmnopqrstuvwxyz1234567890\n",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--strict",
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .expect("violations array");
        assert!(
            violations
                .iter()
                .any(|v| { v["kind"].as_str().unwrap_or("") == "body_token_pattern" })
        );
    }

    #[test]
    fn verify_strict_fails_on_body_raw_skill_usage_schema() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(
            &inbox.join("body-raw-schema-strict"),
            EntryOpts {
                evidence_extra: "- Inline raw JSON: `{\"schema\":\"skill-usage.record.v1\"}`\n",
                ..EntryOpts::default()
            },
        );
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "verify",
                entry.parent().unwrap().to_str().unwrap(),
                "--inbox-dir",
                inbox.to_str().unwrap(),
                "--strict",
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .expect("violations array");
        assert!(
            violations
                .iter()
                .any(|v| { v["kind"].as_str().unwrap_or("") == "body_raw_skill_usage" })
        );
    }

    #[test]
    fn ingest_evidence_rejects_raw_skill_usage_record() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("ingest-raw"), EntryOpts::default());
        let record_dir = tmp.path().join("out").join("skill-usage");
        let raw_record = write_skill_usage_record(&record_dir);
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "ingest-evidence",
                entry.parent().unwrap().to_str().unwrap(),
                "--from",
                raw_record.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("raw skill-usage record")
        }));
    }

    #[test]
    fn ingest_evidence_rejects_token_pattern_source() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("ingest-token"), EntryOpts::default());
        let source = tmp.path().join("out").join("leaky.md");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "Bearer abcdefghijklmnopqrstuvwxyz1234567890\n").unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "ingest-evidence",
                entry.parent().unwrap().to_str().unwrap(),
                "--from",
                source.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);
        let payload = out.stdout_json();
        let violations = payload["error"]["details"]["violations"]
            .as_array()
            .unwrap();
        assert!(violations.iter().any(|v| {
            v["message"]
                .as_str()
                .unwrap_or("")
                .contains("redacted-secret pattern")
        }));
    }

    #[test]
    fn ingest_evidence_accepts_redacted_excerpt() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("ingest-clean"), EntryOpts::default());
        let source = tmp.path().join("out").join("validation.md");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            &source,
            "Validation summary: `scripts/check.sh --all` pass.\n",
        )
        .unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "ingest-evidence",
                entry.parent().unwrap().to_str().unwrap(),
                "--from",
                source.to_str().unwrap(),
                "--label",
                "validation-summary",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let target = PathBuf::from(payload["data"]["path"].as_str().unwrap());
        assert_eq!(target.file_name().unwrap(), "validation-summary.md");
        let text = fs::read_to_string(&target).unwrap();
        assert!(text.contains("scripts/check.sh --all"));
    }

    #[test]
    fn ingest_evidence_normalizes_home_paths() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("ingest-paths"), EntryOpts::default());
        let source = tmp.path().join("out").join("paths.md");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            &source,
            "Run from /Users/example/project succeeded; log at /home/example/build.log.\n",
        )
        .unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "ingest-evidence",
                entry.parent().unwrap().to_str().unwrap(),
                "--from",
                source.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        let target = PathBuf::from(payload["data"]["path"].as_str().unwrap());
        let text = fs::read_to_string(&target).unwrap();
        assert!(!text.contains("/Users/example"));
        assert!(!text.contains("/home/example"));
        assert!(text.contains("<workspace>"));
    }

    #[test]
    fn verify_accepts_operation_record_folder() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let record_root = tmp
            .path()
            .join("heuristic-system")
            .join("operation-records")
            .join("sample-record");
        fs::create_dir_all(record_root.join("evidence")).unwrap();
        fs::write(
            record_root.join("RECORD.md"),
            "# Sample Operation Record\n\n\
## Status\n\n\
- Date: 2026-05-19\n\
- Status: implemented and validated\n\
- System area: sample area\n\n\
## Signal\n\n\
A real workflow exercised the system.\n\n\
## Evidence\n\n\
- Local validation: see `evidence/validation.md`.\n\n\
## Diagnosis\n\n\
Root cause identified.\n\n\
## Promotion Decision\n\n\
Promoted for cross-skill audit value.\n\n\
## Durable Fix\n\n\
Fix landed in maintained code.\n\n\
## Validation\n\n\
All gates green.\n",
        )
        .unwrap();
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &["verify", record_root.to_str().unwrap(), "--format", "json"],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let payload = out.stdout_json();
        assert_eq!(payload["data"]["ok"], true);
        assert_eq!(payload["data"]["kind"], "record");
    }

    #[test]
    fn write_op_emits_invocation_log_when_log_dir_set() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("fixture-gap"), EntryOpts::default());
        let log_dir = tmp.path().join("logs");
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "set-status",
                entry.parent().unwrap().to_str().unwrap(),
                "--status",
                "promoted",
                "--link",
                "docs/plans/example.md",
                "--log-dir",
                log_dir.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let invocation = log_dir.join("invocation.json");
        assert!(invocation.exists(), "invocation.json not written");
        assert!(
            log_dir.join("before.json").exists(),
            "before.json not written"
        );
        assert!(
            log_dir.join("after.json").exists(),
            "after.json not written"
        );
        let log = read_json(&invocation);
        assert_eq!(log["exit_code"], 0);
        assert!(log["argv"].as_array().unwrap().len() >= 3);
        let before = read_json(&log_dir.join("before.json"));
        let after = read_json(&log_dir.join("after.json"));
        assert_eq!(before["targets"][0]["case"]["fields"]["status"], "open");
        assert_eq!(after["targets"][0]["case"]["fields"]["status"], "promoted");
    }

    #[test]
    fn write_op_auto_logs_to_agent_out_topic() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("fixture-gap"), EntryOpts::default());
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "set-status",
                entry.parent().unwrap().to_str().unwrap(),
                "--status",
                "promoted",
                "--link",
                "docs/plans/example.md",
                "--format",
                "json",
            ],
        );
        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

        let agent_home = tmp.path().join(".agent-home");
        let invocations =
            collect_files_named(&agent_home.join("out").join("projects"), "invocation.json");
        assert_eq!(
            invocations.len(),
            1,
            "expected one auto log: {invocations:?}"
        );
        let log_dir = invocations[0].parent().unwrap();
        let run_id = log_dir.file_name().unwrap().to_string_lossy();
        assert!(
            run_id.ends_with("-heuristic-inbox"),
            "unexpected run id: {run_id}"
        );
        assert!(
            log_dir.join("before.json").exists(),
            "before.json not written"
        );
        assert!(
            log_dir.join("after.json").exists(),
            "after.json not written"
        );
        let log = read_json(&invocations[0]);
        assert_eq!(log["exit_code"], 0);
        let before = read_json(&log_dir.join("before.json"));
        let after = read_json(&log_dir.join("after.json"));
        assert_eq!(before["targets"][0]["case"]["fields"]["status"], "open");
        assert_eq!(after["targets"][0]["case"]["fields"]["status"], "promoted");
    }

    #[test]
    fn write_op_logs_failure_to_agent_out_topic() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let inbox = inbox_root(tmp.path());
        let entry = write_entry(&inbox.join("fixture-gap"), EntryOpts::default());
        let out = run(
            "heuristic-inbox",
            tmp.path(),
            &[
                "set-status",
                entry.parent().unwrap().to_str().unwrap(),
                "--status",
                "planned",
                "--format",
                "json",
            ],
        );
        assert_ne!(out.code, 0);

        let agent_home = tmp.path().join(".agent-home");
        let invocations =
            collect_files_named(&agent_home.join("out").join("projects"), "invocation.json");
        assert_eq!(
            invocations.len(),
            1,
            "expected one failure log: {invocations:?}"
        );
        let log_dir = invocations[0].parent().unwrap();
        let log = read_json(&invocations[0]);
        assert_ne!(log["exit_code"], 0);
        let before = read_json(&log_dir.join("before.json"));
        let after = read_json(&log_dir.join("after.json"));
        assert_eq!(before["targets"][0]["case"]["fields"]["status"], "open");
        assert_eq!(after["targets"][0]["case"]["fields"]["status"], "open");
    }
}
