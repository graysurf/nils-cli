use std::fs;
use std::path::Path;
use std::process::Command;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(bin: &str, dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir(bin, dir, args, &[], None)
}

fn json_stdout(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stdout_text()).expect("stdout should be json")
}

fn out_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[test]
fn all_binaries_export_zsh_completion() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    for bin in [
        "browser-session",
        "canary-check",
        "docs-impact",
        "model-cross-check",
        "repo-retro",
        "review-evidence",
        "skill-usage",
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
    let value = json_stdout(&output);
    assert_eq!(value["schema_version"], "cli.repo-retro.report.v1");
    assert_eq!(value["command"], "repo-retro report");
    assert_eq!(value["result"]["schema"], "repo-retro.report.v1");
    assert_eq!(value["result"]["mode"], "team");
    assert_eq!(value["result"]["repo"]["slug"], "repo");
    assert_eq!(value["result"]["window"]["mode"], "fixed");
    assert_eq!(value["result"]["git"]["summary"]["commitCount"], 5);
    assert_eq!(value["result"]["git"]["commitTypes"]["feat"], 1);
    assert_eq!(value["result"]["git"]["commitTypes"]["test"], 1);
    assert_eq!(value["result"]["git"]["commitTypes"]["fix"], 1);
    assert_eq!(
        value["result"]["git"]["testSignals"]["changedTestFileCount"],
        1
    );
    assert_eq!(
        value["result"]["heuristicSystem"]["activeInbox"]["byStatus"]["triaged"],
        1
    );
    assert_eq!(
        value["result"]["heuristicSystem"]["activeInbox"]["bySeverity"]["medium"],
        1
    );
    assert_eq!(
        value["result"]["heuristicSystem"]["errorInboxMovement"]["archived"]["count"],
        1
    );
    assert_eq!(
        value["result"]["heuristicSystem"]["operationRecords"]["changedCount"],
        1
    );
    assert_eq!(value["result"]["history"]["write"], false);
    assert!(
        value["result"]["analysis"]["themes"]
            .as_array()
            .expect("themes")
            .iter()
            .any(|item| item.as_str().unwrap_or("").contains("feat work"))
    );
    assert!(
        value["result"]["analysis"]["followUpQuestions"]
            .as_array()
            .expect("follow up")
            .iter()
            .any(|item| item.as_str().unwrap_or("").contains("HEURISTIC_SYSTEM"))
    );
    assert!(
        value["result"]["sources"]["commands"]
            .as_array()
            .expect("commands")
            .iter()
            .any(|item| item["command"].as_str().unwrap_or("").contains("git -C"))
    );
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
    let value = json_stdout(&output);
    assert_eq!(
        value["result"]["optionalInputs"]["timeline"]["malformedLines"],
        1
    );
    assert_eq!(
        value["result"]["optionalInputs"]["validation"]["validLines"],
        1
    );
    assert_eq!(value["result"]["optionalInputs"]["review"]["validLines"], 1);
    assert_eq!(
        value["result"]["optionalInputs"]["incidents"]["validLines"],
        1
    );
    assert_eq!(
        value["result"]["optionalInputs"]["decisions"]["validLines"],
        1
    );
    assert!(
        value["result"]["warnings"]
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
    let value = json_stdout(&output);
    assert_eq!(value["result"]["history"]["enabled"], true);
    assert_eq!(value["result"]["history"]["write"], false);
    assert!(
        value["result"]["history"]["intended"]["markdown"]
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
    let value = json_stdout(&output);
    let markdown_path = Path::new(
        value["result"]["history"]["intended"]["markdown"]
            .as_str()
            .expect("markdown"),
    );
    let json_path = Path::new(
        value["result"]["history"]["intended"]["json"]
            .as_str()
            .expect("json"),
    );
    let index_path = Path::new(
        value["result"]["history"]["intended"]["index"]
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
        "repo-retro.report.v1"
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
    let value = json_stdout(&output);
    assert_eq!(value["schema_version"], "cli.docs-impact.scan.v1");
    assert_eq!(value["result"]["docs_changed"], true);
    assert_eq!(value["result"]["non_docs_changed"], true);
    assert!(
        value["result"]["docs_files"]
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
    let run_json = json_stdout(&run_output);
    assert_eq!(run_json["schema_version"], "cli.canary-check.run.v1");
    assert_eq!(run_json["result"]["record"]["last_run"]["status"], "pass");

    let verify = run(
        "canary-check",
        tmp.path(),
        &["verify", "--out", &out, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert_eq!(
        json_stdout(&verify)["result"]["last_run"]["stdout_preview"],
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
    assert_eq!(json_stdout(&verify)["result"]["complete"], true);
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
    let value = json_stdout(&verify);
    assert_eq!(value["schema_version"], "cli.skill-usage.verify.v1");
    assert_eq!(value["result"]["complete"], true);
    assert_eq!(value["result"]["record"]["schema"], "skill-usage.record.v1");
    assert_eq!(value["result"]["record"]["outcome"]["status"], "pass");
    assert_eq!(
        value["result"]["record"]["linked_records"][0]["type"],
        "review-evidence"
    );
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
    assert_eq!(json_stdout(&verify)["result"]["complete"], true);
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
    assert_eq!(json_stdout(&verify)["result"]["complete"], true);
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
