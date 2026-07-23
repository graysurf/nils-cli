use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions};
use nils_test_support::fs as test_fs;
use serde_json::{Value, json};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

fn write_capsule(root: &Path, workspace: &Path, access: &str) -> PathBuf {
    let capsule = root.join("capsule");
    fs::create_dir_all(&capsule).expect("capsule dir");
    set_mode(&capsule, 0o700);

    let script = b"#!/bin/sh\nset -eu\nprintf 'capsule ran\\n'\n";
    fs::write(capsule.join("run.sh"), script).expect("script");
    set_mode(&capsule.join("run.sh"), 0o700);

    let manifest = json!({
        "schema_version": "execution-capsule.v1",
        "task": "Apply the prepared change and verify it.",
        "cwd": workspace,
        "entrypoint": "run.sh",
        "entrypoint_sha256": "sha256:d2f796d097b2a96a1cbd188ac53606244a465aa14afa449c2b0d734e145897ed",
        "access": access,
        "allowed_paths": [workspace],
        "validation": []
    });
    test_fs::write_json(&capsule.join("manifest.json"), &manifest);
    set_mode(&capsule.join("manifest.json"), 0o600);
    capsule
}

fn write_codex_stub(root: &Path) -> (PathBuf, PathBuf) {
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let argv_log = root.join("codex.argv");
    test_fs::write_executable(
        &bin_dir.join("codex"),
        r#"#!/bin/sh
set -eu
: > "$CODEX_CAPSULE_ARGV_LOG"
previous=''
output=''
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$CODEX_CAPSULE_ARGV_LOG"
  if [ "$previous" = '--output-last-message' ]; then output="$arg"; fi
  previous="$arg"
done
test -n "$output"
printf '%s\n' '{"status":"succeeded","summary":"capsule completed","actions":["ran run.sh"],"validation":[],"errors":[],"recommendations":[]}' > "$output"
printf '%s\n' '{"type":"thread.started","thread_id":"test-thread"}'
"#,
    );
    (bin_dir, argv_log)
}

#[test]
fn workspace_capsule_preserves_governance_and_writes_private_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            ),
    );

    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let response: Value = output.stdout_json();
    assert_eq!(
        response["schema_version"],
        "cli.codex-cli.execution-capsule.receipt.v1"
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["data"]["access"], "workspace");
    assert_eq!(response["data"]["final"]["status"], "succeeded");

    let argv = fs::read_to_string(argv_log).expect("argv log");
    assert!(argv.contains("--ask-for-approval\nnever\nexec\n"));
    assert!(argv.contains("--sandbox\nworkspace-write\n"));
    assert!(argv.contains("--json\n"));
    assert!(argv.contains("--output-schema\n"));
    assert!(argv.contains("--output-last-message\n"));
    assert!(argv.contains(workspace.to_str().expect("workspace path")));
    assert!(!argv.contains("--ignore-user-config"));
    assert!(!argv.contains("--ignore-rules"));
    assert!(!argv.contains("--dangerously-bypass-approvals-and-sandbox"));

    for artifact in [
        "events.jsonl",
        "final.json",
        "receipt.json",
        "result.schema.json",
    ] {
        let metadata = fs::metadata(capsule.join(artifact)).expect("artifact metadata");
        assert_eq!(metadata.mode() & 0o077, 0, "{artifact} must be owner-only");
    }
}

#[test]
fn wrapper_reruns_declared_validation_and_records_the_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["validation"] = json!([{
        "name": "marker",
        "argv": ["sh", "-c", "printf checked > validation-marker"]
    }]);
    test_fs::write_json(&manifest_path, &manifest);
    set_mode(&manifest_path, 0o600);
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            ),
    );

    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    assert_eq!(
        fs::read_to_string(workspace.join("validation-marker")).expect("validation marker"),
        "checked"
    );
    let response: Value = output.stdout_json();
    assert_eq!(response["data"]["validation"][0]["name"], "marker");
    assert_eq!(response["data"]["validation"][0]["passed"], true);
    assert_eq!(response["data"]["validation"][0]["exit_code"], 0);
}

#[test]
fn validation_launch_failure_is_reported_in_the_receipt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["validation"] = json!([{
        "name": "missing-validator",
        "argv": ["execution-capsule-validator-that-does-not-exist"]
    }]);
    test_fs::write_json(&manifest_path, &manifest);
    set_mode(&manifest_path, 0o600);
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            ),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_eq!(response["ok"], false);
    assert_eq!(response["data"]["validation"][0]["passed"], false);
    assert_eq!(response["data"]["validation"][0]["exit_code"], 127);
    assert!(
        response["data"]["validation"][0]["error"]
            .as_str()
            .expect("error")
            .contains("failed to run validation")
    );
    assert!(capsule.join("receipt.json").is_file());
}

#[test]
fn group_writable_capsule_is_rejected_before_codex_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    set_mode(&capsule, 0o775);
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            ),
    );

    assert_eq!(output.code, 65);
    let response: Value = output.stderr_json();
    assert_eq!(response["error"]["code"], "capsule-not-private-directory");
    assert!(!argv_log.exists(), "Codex must not run for a 0775 capsule");
}

#[test]
fn host_capsule_requires_explicit_operator_acknowledgement() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "host");
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            ),
    );

    assert_eq!(output.code, 65);
    let response: Value = output.stderr_json();
    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"]["code"],
        "host-access-acknowledgement-required"
    );
    assert!(
        !argv_log.exists(),
        "Codex must not run before acknowledgement"
    );
}

#[test]
fn host_capsule_uses_danger_full_access_only_after_acknowledgement() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "host");
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--allow-host-access",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            ),
    );

    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let argv = fs::read_to_string(argv_log).expect("argv log");
    assert!(argv.contains("--sandbox\ndanger-full-access\n"));
    assert!(!argv.contains("--dangerously-bypass-approvals-and-sandbox"));
}

#[test]
fn tampered_entrypoint_is_rejected_before_codex_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    fs::write(capsule.join("run.sh"), "#!/bin/sh\nexit 7\n").expect("tamper");
    set_mode(&capsule.join("run.sh"), 0o700);
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            ),
    );

    assert_eq!(output.code, 65);
    let response: Value = output.stderr_json();
    assert_eq!(response["error"]["code"], "entrypoint-digest-mismatch");
    assert!(
        !argv_log.exists(),
        "Codex must not run for a tampered capsule"
    );
}
