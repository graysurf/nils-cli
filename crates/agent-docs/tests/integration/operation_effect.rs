use nils_test_support::cmd;

use super::common::run_cli;

#[test]
fn preflight_query_emits_a_bound_read_only_descriptor() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let project = temp.path().join("project");
    let docs_home = temp.path().join("docs-home");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::create_dir_all(&docs_home).expect("docs home");
    let project = project.to_str().expect("project UTF-8");
    let docs_home = docs_home.to_str().expect("docs home UTF-8");
    let output = run_cli(
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "--docs-home",
            docs_home,
            "--project-path",
            project,
            "preflight",
            "--intent",
            "project-dev",
            "--format",
            "json",
        ],
        &cmd::CmdOptions::default().with_cwd(temp.path()),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    let json = output.json();
    assert_eq!(json["schema_version"], "cli.agent-docs.operation-effect.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(
        json["data"]["schema_version"],
        "execution.operation-effect.v1"
    );
    assert_eq!(json["data"]["producer"]["tool"], "agent-docs");
    assert_eq!(
        json["data"]["producer"]["release"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(json["data"]["capability_class"], "tool_contract");
    assert_eq!(json["data"]["effect"], "read_only");
    assert_eq!(json["data"]["operation"], "preflight");
    assert!(
        json["data"]["binding"]["argv_digest"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:"))
    );
}

#[test]
fn session_prepare_is_never_described_as_read_only() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let output = run_cli(
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "session",
            "prepare",
            "--session-id",
            "session-1",
            "--product",
            "codex",
            "--state-home",
            temp.path().to_str().expect("temp UTF-8"),
            "--intent",
            "project-dev",
            "--format",
            "json",
        ],
        &cmd::CmdOptions::default().with_cwd(temp.path()),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    assert_ne!(output.json()["data"]["effect"], "read_only");
}

#[test]
fn unknown_inner_flags_produce_no_descriptor() {
    let output = run_cli(
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "preflight",
            "--intent",
            "project-dev",
            "--unknown-effect-flag",
        ],
        &cmd::CmdOptions::default(),
    );

    assert_eq!(output.code, 64);
    assert_eq!(output.json()["ok"], false);
}
