use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run(args: &[&str]) -> CmdOutput {
    let bin = agent_runtime_bin();
    cmd::run(&bin, args, &[], None)
}

struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();
        Self { _tmp: tmp, root }
    }

    fn write(&self, name: &str, body: &str) -> String {
        let path = self.root.join(name);
        fs::write(&path, body).expect("write fixture file");
        path.to_string_lossy().to_string()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

#[test]
fn pr_body_render_feature_writes_forge_compatible_body_to_stdout() {
    let fixture = Fixture::new();
    let summary = fixture.write("summary.md", "Adds the runtime PR body renderer.\n");
    let changes = fixture.write("changes.md", "- Add `agent-runtime pr-body render`.\n");
    let test_first = fixture.write(
        "test-first.md",
        "- Change classification: feature\n- Failing test before fix: integration test failed\n- Final validation: pending\n- Waiver reason: N/A\n",
    );
    let test_plan = fixture.write(
        "test-plan.md",
        "- cargo test -p nils-agent-runtime pr_body (pass)\n",
    );

    let output = run(&[
        "pr-body",
        "render",
        "--kind",
        "feature",
        "--summary-file",
        &summary,
        "--changes-file",
        &changes,
        "--test-first-file",
        &test_first,
        "--test-plan-file",
        &test_plan,
    ]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("## Summary"), "{stdout}");
    assert!(
        stdout.contains("Adds the runtime PR body renderer."),
        "{stdout}"
    );
    assert!(stdout.contains("## Changes"), "{stdout}");
    assert!(stdout.contains("## Test-First Evidence"), "{stdout}");
    assert!(stdout.contains("## Test plan"), "{stdout}");
    assert!(!stdout.contains("## Testing"), "{stdout}");
    assert!(stdout.contains("## Risk / Notes"), "{stdout}");
}

#[test]
fn pr_body_render_bug_writes_kind_specific_body_to_file() {
    let fixture = Fixture::new();
    let out = fixture.path("pr-body.md");
    let summary = fixture.write("summary.md", "Fixes PR body rejection before creation.\n");
    let problem = fixture.write(
        "problem.md",
        "- Expected: generated bodies pass forge-cli validation\n- Actual: missing Test plan section\n- Impact: PR creation fails late\n",
    );
    let reproduction = fixture.write(
        "reproduction.md",
        "1. Render the previous bug PR body.\n2. Run `forge-cli pr create --body-file`.\n",
    );
    let issues = fixture.write(
        "issues.md",
        "| ID | Severity | Confidence | Area | Summary | Evidence | Status |\n| --- | --- | --- | --- | --- | --- | --- |\n| PR-BODY-01 | high | high | pr/create | Missing Test plan | forge-cli error | fixed |\n",
    );
    let fix_approach = fixture.write("fix-approach.md", "- Render `## Test plan` from the CLI.\n");
    let test_first = fixture.write(
        "test-first.md",
        "- Change classification: bug fix\n- Failing test before fix: integration test failed\n- Final validation: pending\n- Waiver reason: N/A\n",
    );
    let test_plan = fixture.write(
        "test-plan.md",
        "- cargo test -p nils-agent-runtime pr_body (pass)\n",
    );
    let out_string = out.to_string_lossy().to_string();

    let output = run(&[
        "pr-body",
        "render",
        "--kind",
        "bug",
        "--summary-file",
        &summary,
        "--problem-file",
        &problem,
        "--reproduction-file",
        &reproduction,
        "--issues-file",
        &issues,
        "--fix-approach-file",
        &fix_approach,
        "--test-first-file",
        &test_first,
        "--test-plan-file",
        &test_plan,
        "--out",
        &out_string,
    ]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let body = fs::read_to_string(out).expect("read output body");
    assert!(body.contains("## Problem"), "{body}");
    assert!(body.contains("## Reproduction"), "{body}");
    assert!(body.contains("## Issues Found"), "{body}");
    assert!(body.contains("## Fix Approach"), "{body}");
    assert!(body.contains("## Test plan"), "{body}");
    assert!(!body.contains("## Testing"), "{body}");
}

#[test]
fn pr_body_render_rejects_missing_required_content_file() {
    let fixture = Fixture::new();
    let summary = fixture.write("summary.md", "Adds the runtime PR body renderer.\n");
    let changes = fixture.write("changes.md", "- Add renderer.\n");
    let test_plan = fixture.write("test-plan.md", "- cargo test (pass)\n");
    let missing = fixture.path("missing.md");
    assert!(!Path::new(&missing).exists());
    let missing_string = missing.to_string_lossy().to_string();

    let output = run(&[
        "pr-body",
        "render",
        "--kind",
        "feature",
        "--summary-file",
        &summary,
        "--changes-file",
        &changes,
        "--test-first-file",
        &missing_string,
        "--test-plan-file",
        &test_plan,
    ]);

    assert_eq!(output.code, 2);
    let stderr = output.stderr_text();
    assert!(stderr.contains("agent-runtime pr-body"), "{stderr}");
    assert!(stderr.contains("test-first"), "{stderr}");
}

#[test]
fn pr_body_render_generic_kinds_emit_forge_compatible_body() {
    // The four non-feature/bug kinds match `forge-cli pr deliver --kind` and
    // render a generic Summary / Test-First / Test plan / Risk skeleton with
    // no feature- or bug-specific sections and no extra required files.
    for kind in ["chore", "docs", "ci", "refactor"] {
        let fixture = Fixture::new();
        let summary = fixture.write("summary.md", "Bumps the pinned toolchain.\n");
        let test_first = fixture.write(
            "test-first.md",
            "- Change classification: chore\n- Waiver reason: mechanical change\n",
        );
        let test_plan = fixture.write(
            "test-plan.md",
            "- cargo test -p nils-agent-runtime (pass)\n",
        );

        let output = run(&[
            "pr-body",
            "render",
            "--kind",
            kind,
            "--summary-file",
            &summary,
            "--test-first-file",
            &test_first,
            "--test-plan-file",
            &test_plan,
        ]);

        assert_eq!(
            output.code,
            0,
            "kind={kind} stderr={}",
            output.stderr_text()
        );
        let stdout = output.stdout_text();
        assert!(stdout.contains("## Summary"), "kind={kind}: {stdout}");
        assert!(stdout.contains("## Test plan"), "kind={kind}: {stdout}");
        assert!(
            stdout.contains("## Test-First Evidence"),
            "kind={kind}: {stdout}"
        );
        assert!(stdout.contains("## Risk / Notes"), "kind={kind}: {stdout}");
        // Generic kinds omit the feature/bug-specific sections.
        assert!(!stdout.contains("## Changes"), "kind={kind}: {stdout}");
        assert!(!stdout.contains("## Problem"), "kind={kind}: {stdout}");
    }
}
