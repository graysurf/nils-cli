use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
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

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn write_min_runtime_roots(root: &Path) {
    write(
        &root.join("manifests/runtime-roots.yaml"),
        "schema_version: 1\nproducts: {}\n",
    );
}

fn make_codex_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_min_runtime_roots(root);
    write(
        &root.join("targets/codex/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.daily-brief.codex-skill-dir
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/daily-brief
    destination: skills/reporting/daily-brief
    recursive: false
  - id: reporting.topic-radar.codex-skill-dir
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/topic-radar
    destination: skills/reporting/topic-radar
    recursive: false
"#,
    );
    write(
        &root.join("build/codex/plugins/reporting/skills/daily-brief/SKILL.md"),
        "# daily-brief\n",
    );
    write(
        &root.join("build/codex/plugins/reporting/skills/topic-radar/SKILL.md"),
        "# topic-radar\n",
    );
    tmp
}

fn make_claude_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_min_runtime_roots(root);
    write(
        &root.join("targets/claude/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/claude/plugins/reporting/skills
    destination: plugins/reporting/skills
    recursive: true
"#,
    );
    write(
        &root.join("build/claude/plugins/reporting/skills/daily-brief/SKILL.md"),
        "# daily-brief\n",
    );
    write(
        &root.join("build/claude/plugins/reporting/skills/topic-radar/SKILL.md"),
        "# topic-radar\n",
    );
    tmp
}

fn make_hermes_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_min_runtime_roots(root);
    write(
        &root.join("targets/hermes/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.skills-tree
    kind: symlinked-file
    source: build/hermes/plugins/reporting/skills
    destination: skills/reporting
    recursive: true
"#,
    );
    write(
        &root.join("build/hermes/plugins/reporting/skills/daily-brief/SKILL.md"),
        "# daily-brief\n",
    );
    write(
        &root.join("build/hermes/plugins/reporting/skills/topic-radar/SKILL.md"),
        "# topic-radar\n",
    );
    tmp
}

fn write_v2_skills_manifest(root: &Path) {
    write(
        &root.join("manifests/skills.yaml"),
        r#"schema_version: 2
migration:
  owner: "https://github.com/graysurf/agent-runtime-kit/issues/562"
  pending_disposition:
    - reporting.topic-radar
skills:
  - id: reporting.daily-brief
    domain: reporting
    source: core/skills/reporting/daily-brief
    invocation:
      role: workflow
      intents: [daily-brief]
      example_request: "Prepare my daily brief"
      admission_rationale: "Produces a direct user-requested information brief."
    exposure:
      profile: default
    products: {}
    required_clis: {}
  - id: reporting.topic-radar
    domain: reporting
    source: core/skills/reporting/topic-radar
    products: {}
    required_clis: {}
"#,
    );
}

fn assert_v2_metadata(product: &str, tmp: &TempDir) {
    write_v2_skills_manifest(tmp.path());
    let output = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        product,
        "--format",
        "json",
    ]);
    assert_eq!(output.code, 0, "{product} stderr={}", output.stderr_text());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let skills = value["skills"].as_array().unwrap();

    assert_eq!(skills[0]["invocation"]["role"], "workflow");
    assert_eq!(skills[0]["invocation"]["intents"][0], "daily-brief");
    assert_eq!(skills[0]["exposure"]["profile"], "default");
    assert_eq!(skills[0]["pending_disposition"], false);

    assert!(skills[1]["invocation"].is_null());
    assert!(skills[1]["exposure"].is_null());
    assert_eq!(skills[1]["pending_disposition"], true);
}

fn make_codex_warning_fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_min_runtime_roots(root);
    write(
        &root.join("targets/codex/link-map.yaml"),
        r#"schema_version: 1
entries:
  - id: reporting.daily-brief.codex-skill-md
    kind: symlinked-file
    source: build/codex/plugins/reporting/skills/daily-brief/SKILL.md
    destination: skills/reporting/daily-brief/SKILL.md
    recursive: false
"#,
    );
    write(
        &root.join("build/codex/plugins/reporting/skills/daily-brief/SKILL.md"),
        "# daily-brief\n",
    );
    tmp
}

#[test]
fn help_documents_every_flag() {
    let output = run(&["list-skills", "--help"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    for flag in [
        "--source-root",
        "--product",
        "--live-home",
        "--format",
        "--include-warnings",
    ] {
        assert!(
            stdout.contains(flag),
            "list-skills --help should document `{flag}`: {stdout}"
        );
    }
}

#[test]
fn unknown_product_exits_two_with_usage_error() {
    let output = run(&["list-skills", "--source-root", ".", "--product", "vscode"]);
    assert_eq!(output.code, 2);
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("unknown --product"),
        "stderr should explain unknown product: {stderr}"
    );
}

#[test]
fn relative_live_home_exits_two() {
    let tmp = make_codex_fixture();
    let output = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "codex",
        "--live-home",
        "relative/path",
    ]);
    assert_eq!(output.code, 2);
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("--live-home must be absolute"),
        "stderr should explain absolute requirement: {stderr}"
    );
}

#[test]
fn codex_text_output_is_one_line_per_skill_sorted_by_id() {
    let tmp = make_codex_fixture();
    let output = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "codex",
    ]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 skills, got: {stdout:?}");
    assert!(lines[0].starts_with("reporting.daily-brief\t"));
    assert!(lines[1].starts_with("reporting.topic-radar\t"));
    assert!(lines[0].contains("directory"));
    assert!(lines[0].contains("skills/reporting/daily-brief"));
}

#[test]
fn codex_json_output_carries_schema_and_sorted_skills() {
    let tmp = make_codex_fixture();
    let output = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "codex",
        "--format",
        "json",
    ]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "cli.agent-runtime.list-skills.v1");
    assert_eq!(value["product"], "codex");
    assert!(value["live_home"].is_null());
    let skills = value["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 2);
    let ids: Vec<&str> = skills.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["reporting.daily-brief", "reporting.topic-radar"]);
    assert_eq!(skills[0]["link_mode"], "directory");
    assert_eq!(skills[0]["discoverable"], true);
    assert!(skills[0]["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn codex_json_output_reports_v2_invocation_exposure_and_pending_state() {
    let tmp = make_codex_fixture();
    assert_v2_metadata("codex", &tmp);
}

#[test]
fn claude_json_output_reports_v2_invocation_exposure_and_pending_state() {
    let tmp = make_claude_fixture();
    assert_v2_metadata("claude", &tmp);
}

#[test]
fn hermes_json_output_reports_v2_invocation_exposure_and_pending_state() {
    let tmp = make_hermes_fixture();
    assert_v2_metadata("hermes", &tmp);
}

#[test]
fn claude_recursive_expansion_yields_one_record_per_skill() {
    let tmp = make_claude_fixture();
    let output = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["product"], "claude");
    let skills = value["skills"].as_array().unwrap();
    let ids: Vec<&str> = skills.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["reporting.daily-brief", "reporting.topic-radar"]);
    // Non-codex products omit the `discoverable` field entirely.
    assert!(skills[0].get("discoverable").is_none());
}

#[test]
fn codex_warning_class_surfaces_in_json_and_text_with_flag() {
    let tmp = make_codex_warning_fixture();
    let json_out = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "codex",
        "--format",
        "json",
    ]);
    assert_eq!(json_out.code, 0, "stderr={}", json_out.stderr_text());
    let value: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    let warnings = value["skills"][0]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["code"], "codex.active-skill.file-symlink");

    let text_out = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "codex",
        "--include-warnings",
    ]);
    assert_eq!(text_out.code, 0, "stderr={}", text_out.stderr_text());
    let stdout = text_out.stdout_text();
    assert!(
        stdout.contains("codex.active-skill.file-symlink"),
        "text output with --include-warnings should surface the warning code: {stdout}"
    );

    let text_no_flag = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "codex",
    ]);
    assert_eq!(text_no_flag.code, 0);
    let stdout = text_no_flag.stdout_text();
    assert!(
        !stdout.contains("codex.active-skill.file-symlink"),
        "text output without --include-warnings should NOT surface warnings: {stdout}"
    );
}

#[test]
fn json_output_is_byte_deterministic_across_runs() {
    let tmp = make_codex_fixture();
    let args = [
        "list-skills",
        "--source-root",
        &*tmp.path().to_string_lossy(),
        "--product",
        "codex",
        "--format",
        "json",
    ];
    let first = run(&args);
    let second = run(&args);
    assert_eq!(first.code, 0);
    assert_eq!(second.code, 0);
    assert_eq!(
        first.stdout, second.stdout,
        "JSON output must be deterministic"
    );
}

#[test]
fn live_home_is_accepted_for_parity() {
    let tmp = make_codex_fixture();
    let live_home = TempDir::new().unwrap();
    let output = run(&[
        "list-skills",
        "--source-root",
        &tmp.path().to_string_lossy(),
        "--product",
        "codex",
        "--live-home",
        &live_home.path().to_string_lossy(),
        "--format",
        "json",
    ]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["live_home"],
        live_home.path().to_string_lossy().as_ref()
    );
}
