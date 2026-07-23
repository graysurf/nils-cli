use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions};
use nils_test_support::fs as test_fs;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

fn assert_service_envelope(response: &Value, ok: bool) {
    assert!(response["schema_version"].is_string());
    assert_eq!(response["command"], "agent run");
    assert_eq!(response["ok"], ok);
    if ok {
        assert!(response["result"].is_object());
        assert!(response.get("error").is_none());
    } else {
        assert!(response["error"]["code"].is_string());
        assert!(response["error"]["message"].is_string());
    }
    let schema: Value =
        if response["schema_version"] == "cli.codex-cli.execution-capsule.receipt.v1" {
            serde_json::from_str(include_str!(
                "../../docs/specs/execution-capsule-receipt-v1.schema.json"
            ))
            .expect("receipt schema")
        } else {
            serde_json::from_str(include_str!(
                "../../docs/specs/execution-capsule-error-v1.schema.json"
            ))
            .expect("error schema")
        };
    let validator = jsonschema::draft202012::new(&schema).expect("compile published schema");
    assert!(
        validator.is_valid(response),
        "emitted response must validate against {}",
        schema["$id"]
    );
}

fn assert_receipt_schema_rejects(response: &Value, case: &str) {
    let schema: Value = serde_json::from_str(include_str!(
        "../../docs/specs/execution-capsule-receipt-v1.schema.json"
    ))
    .expect("receipt schema");
    let validator = jsonschema::draft202012::new(&schema).expect("compile receipt schema");
    assert!(
        !validator.is_valid(response),
        "receipt schema must reject {case}"
    );
}

#[test]
fn published_execution_capsule_schemas_are_checked_json_contracts() {
    let receipt: Value = serde_json::from_str(include_str!(
        "../../docs/specs/execution-capsule-receipt-v1.schema.json"
    ))
    .expect("receipt schema json");
    let error: Value = serde_json::from_str(include_str!(
        "../../docs/specs/execution-capsule-error-v1.schema.json"
    ))
    .expect("error schema json");
    assert_eq!(receipt["$id"], "cli.codex-cli.execution-capsule.receipt.v1");
    assert_eq!(error["$id"], "cli.codex-cli.execution-capsule.error.v1");
    assert!(jsonschema::draft202012::meta::is_valid(&receipt));
    assert!(jsonschema::draft202012::meta::is_valid(&error));
    for required in ["schema_version", "command", "ok", "result"] {
        assert!(
            receipt["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == required)
        );
    }
    for required in ["schema_version", "command", "ok", "error"] {
        assert!(
            error["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == required)
        );
    }
}

fn write_capsule(root: &Path, workspace: &Path, access: &str) -> PathBuf {
    let capsule = root.join("capsule");
    fs::create_dir_all(&capsule).expect("capsule dir");
    set_mode(&capsule, 0o700);

    let script =
        b"#!/usr/bin/env bash\nset -euo pipefail\nprintf 'capsule ran\\n' > execution-marker\n";
    fs::write(capsule.join("run.sh"), script).expect("script");
    set_mode(&capsule.join("run.sh"), 0o700);
    let digest = format!(
        "sha256:{}",
        Sha256::digest(script)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );

    let manifest = json!({
        "schema_version": "execution-capsule.v1",
        "task": "Apply the prepared change and verify it.",
        "cwd": workspace,
        "entrypoint": "run.sh",
        "entrypoint_sha256": digest,
        "access": access,
        "allowed_paths": [workspace],
        "validation": []
    });
    test_fs::write_json(&capsule.join("manifest.json"), &manifest);
    set_mode(&capsule.join("manifest.json"), 0o600);
    capsule
}

fn replace_script(capsule: &Path, script: &[u8]) {
    fs::write(capsule.join("run.sh"), script).expect("script");
    set_mode(&capsule.join("run.sh"), 0o700);
    let digest = format!(
        "sha256:{}",
        Sha256::digest(script)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["entrypoint_sha256"] = json!(digest);
    test_fs::write_json(&manifest_path, &manifest);
    set_mode(&manifest_path, 0o600);
}

fn write_codex_stub(root: &Path) -> (PathBuf, PathBuf) {
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let argv_log = root.join("codex.argv");
    test_fs::write_executable(
        &bin_dir.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail
: > "$CODEX_CAPSULE_ARGV_LOG"
previous=''
output=''
last=''
for arg in "$@"; do
  printf '%s\0' "$arg" >> "$CODEX_CAPSULE_ARGV_LOG"
  if [ "$previous" = '--output-last-message' ]; then output="$arg"; fi
  previous="$arg"
  last="$arg"
done
test -n "$output"

json_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  printf '%s' "$value"
}

LAST_STATUS=127
emit_command_event() {
  local command="$1"
  local combined command_json output_json
  set +e
  combined="$(eval "$command" 2>&1)"
  LAST_STATUS=$?
  set -e
  command_json="$(json_escape "$command")"
  output_json="$(json_escape "$combined")"
  printf '{"type":"item.completed","item":{"id":"item-test","type":"command_execution","command":"/usr/bin/zsh -lc %s","aggregated_output":"%s","exit_code":%d,"status":"completed"}}\n' \
    "$command_json" "$output_json" "$LAST_STATUS"
}

script_command="$(printf '%s\n' "$last" | sed -n 's/^Exact script command: //p' | head -1)"
test -n "$script_command"
if [ -n "${CODEX_CAPSULE_REPLACE_HELPER_PATH:-}" ]; then
  mv "$CODEX_CAPSULE_REPLACE_HELPER_PATH" "$CODEX_CAPSULE_REPLACE_HELPER_PATH.original"
  printf '%s\n' '#!/usr/bin/env bash' 'exit 0' > "$CODEX_CAPSULE_REPLACE_HELPER_PATH"
  chmod 700 "$CODEX_CAPSULE_REPLACE_HELPER_PATH"
fi
if [ "${CODEX_CAPSULE_SKIP_HELPERS:-0}" != 1 ]; then
  emit_command_event "$script_command"
  if [ "$LAST_STATUS" -eq 0 ] && [ "${CODEX_CAPSULE_REPEAT_SCRIPT:-0}" = 1 ]; then
    emit_command_event "$script_command"
  fi
  if [ "$LAST_STATUS" -ne 0 ] && [ -n "${CODEX_CAPSULE_FIX_PATH:-}" ]; then
    : > "$CODEX_CAPSULE_FIX_PATH"
    emit_command_event "$script_command"
  fi
  if [ "$LAST_STATUS" -eq 0 ]; then
    while IFS= read -r validation_command; do
      if [ -n "$validation_command" ]; then
        emit_command_event "$validation_command"
        if [ "${CODEX_CAPSULE_REPEAT_VALIDATIONS:-0}" = 1 ]; then
          emit_command_event "$validation_command"
        fi
      fi
    done < <(printf '%s\n' "$last" | sed -n 's/^- Validation command [0-9][0-9]*: //p')
  fi
fi
if [ -n "${CODEX_CAPSULE_TAMPER_SCRIPT:-}" ]; then
  printf '%s\n' '#!/usr/bin/env bash' 'exit 7' > "$CODEX_CAPSULE_TAMPER_SCRIPT"
  chmod 700 "$CODEX_CAPSULE_TAMPER_SCRIPT"
fi
if [ -n "${CODEX_CAPSULE_SWAP_FINAL_PATH:-}" ]; then
  rm -f "$CODEX_CAPSULE_SWAP_FINAL_PATH"
  ln -s "$CODEX_CAPSULE_SENTINEL" "$CODEX_CAPSULE_SWAP_FINAL_PATH"
fi
if [ "${CODEX_CAPSULE_SKIP_FINAL:-0}" != 1 ]; then
  if [ -n "${CODEX_CAPSULE_FINAL_JSON:-}" ]; then
    printf '%s\n' "$CODEX_CAPSULE_FINAL_JSON" > "$output"
  else
    printf '%s\n' '{"status":"succeeded","summary":"capsule completed","actions":["supervised run.sh"],"validation":[],"errors":[],"recommendations":[]}' > "$output"
  fi
fi
if [ -n "${CODEX_CAPSULE_SWAP_RECEIPT_PATH:-}" ]; then
  rm -f "$CODEX_CAPSULE_SWAP_RECEIPT_PATH"
  ln -s "$CODEX_CAPSULE_SENTINEL" "$CODEX_CAPSULE_SWAP_RECEIPT_PATH"
fi
if [ -n "${CODEX_CAPSULE_REPLACE_RECEIPT_WITH_DIR:-}" ]; then
  rm -f "$CODEX_CAPSULE_REPLACE_RECEIPT_WITH_DIR"
  mkdir "$CODEX_CAPSULE_REPLACE_RECEIPT_WITH_DIR"
fi
if [ -n "${CODEX_CAPSULE_BLOCK_PREDICTABLE_RECOVERY_DIR:-}" ]; then
  for sequence in $(seq 0 64); do
    mkdir "$CODEX_CAPSULE_BLOCK_PREDICTABLE_RECOVERY_DIR/receipt.recovery.$PPID.$sequence.json"
  done
fi
printf '%s\n' '{"type":"thread.started","thread_id":"test-thread"}'
exit "${CODEX_CAPSULE_EXIT_CODE:-0}"
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
    assert_service_envelope(&response, true);
    assert_eq!(
        response["schema_version"],
        "cli.codex-cli.execution-capsule.receipt.v1"
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["access"], "workspace");
    assert_eq!(response["result"]["final"]["status"], "succeeded");
    assert_eq!(response["command"], "agent run");
    assert_eq!(response["result"]["script_runs"][0]["phase"], "attempt-1");
    assert_eq!(response["result"]["script_runs"][0]["passed"], true);
    assert_eq!(response["result"]["script_passed"], true);
    assert_eq!(response["result"]["validations_passed"], true);
    assert_eq!(
        fs::read_to_string(workspace.join("execution-marker")).expect("execution marker"),
        "capsule ran\n"
    );

    let mut invalid = response.clone();
    invalid["result"]["codex_exit_code"] = json!(9);
    assert_receipt_schema_rejects(&invalid, "successful receipt with nonzero Codex exit");
    let mut invalid = response.clone();
    invalid["result"]["entrypoint_integrity_valid"] = json!(false);
    assert_receipt_schema_rejects(&invalid, "successful receipt with failed integrity");
    let mut invalid = response.clone();
    invalid["result"]["helper_integrity_valid"] = json!(false);
    assert_receipt_schema_rejects(&invalid, "successful receipt with failed helper integrity");
    let mut invalid = response.clone();
    invalid["result"]["final_report_valid"] = json!(false);
    assert_receipt_schema_rejects(&invalid, "successful receipt with invalid final report");
    let mut invalid = response.clone();
    invalid["result"]["final"] = Value::Null;
    assert_receipt_schema_rejects(&invalid, "successful receipt with null final report");
    let mut invalid = response.clone();
    invalid["result"]["script_runs"] = json!([]);
    assert_receipt_schema_rejects(&invalid, "successful receipt without a script attestation");
    let mut invalid = response.clone();
    let mut failed_terminal = invalid["result"]["script_runs"][0].clone();
    failed_terminal["passed"] = json!(false);
    failed_terminal["exit_code"] = json!(7);
    invalid["result"]["script_runs"]
        .as_array_mut()
        .expect("script runs")
        .push(failed_terminal);
    assert_receipt_schema_rejects(&invalid, "successful receipt with failed terminal script");
    let mut invalid = response.clone();
    invalid["result"]["validation"] = json!([{
        "name": "failed",
        "argv": ["false"],
        "exit_code": 1,
        "passed": false,
        "command": "false",
        "events": "/tmp/events.jsonl"
    }]);
    assert_receipt_schema_rejects(&invalid, "successful receipt with failed validation");
    let mut invalid = response.clone();
    invalid["result"]["manifest_sha256"] = json!("not-a-digest");
    assert_receipt_schema_rejects(&invalid, "receipt with malformed manifest digest");

    let argv = fs::read_to_string(argv_log)
        .expect("argv log")
        .split('\0')
        .filter(|argument| !argument.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        &argv[..6],
        &[
            "--ask-for-approval",
            "never",
            "exec",
            "--skip-git-repo-check",
            "-C",
            workspace.to_str().unwrap()
        ]
    );
    assert_eq!(&argv[6..9], &["--sandbox", "workspace-write", "--json"]);
    assert_eq!(argv[9], "--output-schema");
    assert_eq!(argv[11], "--output-last-message");
    assert_eq!(argv[13], "--");
    let prompt = &argv[14];
    assert!(prompt.contains("Apply the prepared change and verify it."));
    assert!(prompt.contains(capsule.join("run.sh").to_str().unwrap()));
    assert!(prompt.contains(workspace.to_str().unwrap()));
    assert!(prompt.contains("active home and project instructions"));
    assert!(prompt.contains("Do not bypass them"));
    assert!(prompt.contains("command_execution events"));
    assert!(!argv.iter().any(|arg| arg == "--ignore-user-config"));
    assert!(!argv.iter().any(|arg| arg == "--ignore-rules"));
    assert!(
        !argv
            .iter()
            .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
    );

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
fn pinned_helper_ignores_a_replaced_launch_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let copied_cli = workspace.join("codex-cli-copy");
    fs::copy(codex_cli_bin(), &copied_cli).expect("copy codex-cli");
    set_mode(&copied_cli, 0o755);
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &copied_cli,
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
            )
            .with_env(
                "CODEX_CAPSULE_REPLACE_HELPER_PATH",
                copied_cli.to_str().expect("copied CLI"),
            ),
    );

    let response: Value = output.stdout_json();
    assert!(
        workspace.join("execution-marker").is_file(),
        "the pinned executable must run run.sh even after its launch path is replaced"
    );
    assert!(
        response["result"]["script_passed"] == true
            || response["result"]["helper_integrity_valid"] == false,
        "the replacement must never forge success without a pinned helper execution"
    );
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
        "argv": ["sh", "-c", "printf 'noisy validation\\n'; printf checked > validation-marker"]
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
    assert_eq!(response["result"]["validation"][0]["name"], "marker");
    assert_eq!(response["result"]["validation"][0]["passed"], true);
    assert_eq!(response["result"]["validation"][0]["exit_code"], 0);
    let _: Value = serde_json::from_str(output.stdout_text().trim())
        .expect("JSON stdout must contain exactly one receipt value");
    let validation_events = response["result"]["validation"][0]["events"]
        .as_str()
        .expect("validation events artifact");
    assert!(
        fs::read_to_string(validation_events)
            .expect("validation events")
            .contains("noisy validation"),
        "validation output must remain available in events"
    );
}

#[test]
fn helper_attestations_are_framed_after_output_without_a_newline() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    replace_script(
        &capsule,
        b"#!/usr/bin/env bash\nset -euo pipefail\nprintf script-without-newline\n",
    );
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["validation"] = json!([{
        "name": "no-newline",
        "argv": ["sh", "-c", "printf validation-without-newline"]
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
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, true);
    assert_eq!(response["result"]["script_runs"][0]["passed"], true);
    assert_eq!(response["result"]["validation"][0]["passed"], true);
}

#[test]
fn snapshot_preserves_argv0_and_exposes_explicit_capsule_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    replace_script(
        &capsule,
        b"#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s|%s|%s|%s|%s\\n' \"$0\" \"$#\" \"${BASH_SOURCE[0]}\" \"$EXECUTION_CAPSULE_DIR\" \"$EXECUTION_CAPSULE_ENTRYPOINT\" > snapshot-context\n",
    );
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
    let context = fs::read_to_string(workspace.join("snapshot-context")).expect("context");
    assert_eq!(
        context,
        format!(
            "{}|0|/dev/stdin|{}|{}\n",
            capsule.join("run.sh").display(),
            capsule.display(),
            capsule.join("run.sh").display()
        )
    );
}

#[test]
fn failed_initial_script_is_retried_after_supervisor_correction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    replace_script(
        &capsule,
        b"#!/usr/bin/env bash\nset -euo pipefail\ntest -f corrected\nprintf 'retried\\n' > execution-marker\n",
    );
    let (bin_dir, argv_log) = write_codex_stub(temp.path());
    let correction = workspace.join("corrected");

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
            )
            .with_env(
                "CODEX_CAPSULE_FIX_PATH",
                correction.to_str().expect("correction"),
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
    assert_eq!(response["result"]["script_runs"][0]["phase"], "attempt-1");
    assert_eq!(response["result"]["script_runs"][0]["terminal"], false);
    assert_eq!(response["result"]["script_runs"][0]["passed"], false);
    assert_eq!(response["result"]["script_runs"][1]["phase"], "attempt-2");
    assert_eq!(response["result"]["script_runs"][1]["terminal"], true);
    assert_eq!(response["result"]["script_runs"][1]["passed"], true);
    assert_eq!(response["ok"], true);
    assert_eq!(
        fs::read_to_string(workspace.join("execution-marker")).expect("marker"),
        "retried\n"
    );
}

#[test]
fn final_script_attempt_determines_the_outcome() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    replace_script(
        &capsule,
        b"#!/usr/bin/env bash\nset -euo pipefail\nif test -e script-seen; then exit 7; fi\n: > script-seen\n",
    );
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
            )
            .with_env("CODEX_CAPSULE_REPEAT_SCRIPT", "1"),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["result"]["script_runs"][0]["passed"], true);
    assert_eq!(response["result"]["script_runs"][1]["passed"], false);
    assert_eq!(response["result"]["script_passed"], false);
    assert_eq!(response["error"]["code"], "script-attestation-failed");
}

#[test]
fn final_validation_attempt_determines_the_outcome() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["validation"] = json!([{
        "name": "repeat",
        "argv": ["sh", "-c", "if test -e validation-seen; then exit 6; fi; : > validation-seen"]
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
            )
            .with_env("CODEX_CAPSULE_REPEAT_VALIDATIONS", "1"),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["result"]["validation"][0]["exit_code"], 6);
    assert_eq!(response["result"]["validation"][0]["passed"], false);
    assert_eq!(response["result"]["validations_passed"], false);
    assert_eq!(response["error"]["code"], "validation-failed");
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
    assert_eq!(response["result"]["validation"][0]["passed"], false);
    assert_eq!(response["result"]["validation"][0]["exit_code"], 127);
    assert!(
        response["result"]["validation"][0]["error"]
            .as_str()
            .expect("error")
            .contains("failed to run validation")
    );
    assert!(capsule.join("receipt.json").is_file());
}

#[test]
fn script_does_not_run_without_a_matching_sandbox_helper_event() {
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
            )
            .with_env("CODEX_CAPSULE_SKIP_HELPERS", "1"),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["error"]["code"], "script-attestation-failed");
    assert!(
        response["result"]["script_runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !workspace.join("execution-marker").exists(),
        "the parent wrapper must never execute run.sh outside Codex"
    );
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
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
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
    let response: Value = output.stdout_json();
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
    assert!(argv.contains("--sandbox\0danger-full-access\0"));
    assert!(!argv.contains("--dangerously-bypass-approvals-and-sandbox"));
}

#[test]
fn host_artifact_path_swaps_cannot_overwrite_external_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "host");
    let sentinel = temp.path().join("sentinel");
    fs::write(&sentinel, "preserve me").expect("sentinel");
    let (bin_dir, argv_log) = write_codex_stub(temp.path());

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--allow-host-access",
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env(
                "CODEX_CAPSULE_SWAP_FINAL_PATH",
                capsule.join("final.json").to_str().expect("final path"),
            )
            .with_env(
                "CODEX_CAPSULE_SWAP_RECEIPT_PATH",
                capsule.join("receipt.json").to_str().expect("receipt path"),
            )
            .with_env(
                "CODEX_CAPSULE_SENTINEL",
                sentinel.to_str().expect("sentinel"),
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
    assert_service_envelope(&response, true);
    assert_eq!(
        fs::read_to_string(&sentinel).expect("sentinel"),
        "preserve me"
    );
    for artifact in ["final.json", "receipt.json"] {
        let metadata = fs::symlink_metadata(capsule.join(artifact)).expect("artifact");
        assert!(
            metadata.is_file(),
            "{artifact} must replace the hostile link"
        );
        assert!(!metadata.file_type().is_symlink());
    }
}

#[test]
fn late_receipt_directory_uses_a_durable_recovery_receipt() {
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
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env(
                "CODEX_CAPSULE_REPLACE_RECEIPT_WITH_DIR",
                capsule.join("receipt.json").to_str().expect("receipt path"),
            )
            .with_env(
                "CODEX_CAPSULE_BLOCK_PREDICTABLE_RECOVERY_DIR",
                capsule.to_str().expect("capsule path"),
            ),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["error"]["code"], "receipt-publish-failed");
    assert!(response["result"]["receipt_error"].is_string());
    let recovery = Path::new(
        response["result"]["artifacts"]["receipt"]
            .as_str()
            .expect("recovery receipt"),
    );
    assert!(recovery.is_file());
    let recovery_name = recovery
        .file_name()
        .expect("recovery name")
        .to_string_lossy();
    let recovery_token = recovery_name
        .strip_prefix("receipt.recovery.")
        .and_then(|name| name.strip_suffix(".json"))
        .expect("random recovery name");
    assert_eq!(recovery_token.len(), 32);
    assert!(recovery_token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let stored: Value =
        serde_json::from_slice(&fs::read(recovery).expect("stored receipt")).expect("receipt json");
    assert_eq!(stored, response);
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
    let response: Value = output.stdout_json();
    assert_eq!(response["error"]["code"], "entrypoint-digest-mismatch");
    assert!(
        !argv_log.exists(),
        "Codex must not run for a tampered capsule"
    );
}

#[test]
fn entrypoint_changed_by_supervisor_prevents_successful_receipt() {
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
            )
            .with_env(
                "CODEX_CAPSULE_TAMPER_SCRIPT",
                capsule.join("run.sh").to_str().expect("script"),
            ),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_eq!(response["ok"], false);
    assert_eq!(response["result"]["entrypoint_integrity_valid"], false);
    assert!(
        response["result"]["entrypoint_integrity_error"]
            .as_str()
            .expect("integrity error")
            .contains("entrypoint")
    );
}

#[test]
fn failed_final_report_is_durable_and_returns_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log) = write_codex_stub(temp.path());
    let failed = r#"{"status":"failed","summary":"could not finish","actions":[],"validation":[],"errors":["hook rejected the operation"],"recommendations":["resolve the checkout lease"]}"#;

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_FINAL_JSON", failed),
    );

    assert_eq!(output.code, 1);
    assert!(
        output.stdout_text().contains("hook rejected the operation"),
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    assert!(
        output.stdout_text().contains("resolve the checkout lease"),
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let receipt: Value =
        serde_json::from_slice(&fs::read(capsule.join("receipt.json")).expect("receipt"))
            .expect("receipt json");
    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["result"]["final"]["status"], "failed");
}

#[test]
fn codex_nonzero_exit_is_recorded_in_receipt() {
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
            )
            .with_env("CODEX_CAPSULE_EXIT_CODE", "9"),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_eq!(response["result"]["codex_exit_code"], 9);
    assert_eq!(response["ok"], false);
}

#[test]
fn nonzero_validation_is_recorded_in_receipt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["validation"] = json!([{
        "name": "failing",
        "argv": ["sh", "-c", "printf validation-failed >&2; exit 4"]
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
    assert_eq!(response["result"]["validation"][0]["exit_code"], 4);
    assert_eq!(response["result"]["validation"][0]["passed"], false);
    assert_eq!(response["ok"], false);
}

#[test]
fn hardlinked_artifact_is_rejected_without_truncating_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let outside = temp.path().join("outside");
    fs::write(&outside, "preserve me").expect("outside");
    set_mode(&outside, 0o600);
    fs::hard_link(&outside, capsule.join("result.schema.json")).expect("hard link");
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
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "artifact-path-unsafe"
    );
    assert_eq!(fs::read_to_string(outside).expect("outside"), "preserve me");
    assert!(!argv_log.exists());
}

#[test]
fn unsafe_events_target_is_rejected_before_codex_runs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let outside = temp.path().join("outside-events");
    fs::write(&outside, "preserve me").expect("outside");
    set_mode(&outside, 0o600);
    fs::hard_link(&outside, capsule.join("events.jsonl")).expect("hard link");
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
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["error"]["code"], "artifact-path-unsafe");
    assert_eq!(fs::read_to_string(outside).expect("outside"), "preserve me");
    assert!(!argv_log.exists());
}

#[test]
fn unsafe_receipt_target_is_rejected_before_codex_runs() {
    for target_kind in ["hardlink", "symlink"] {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let capsule = write_capsule(temp.path(), &workspace, "workspace");
        let outside = temp.path().join("outside-receipt");
        fs::write(&outside, "preserve me").expect("outside");
        set_mode(&outside, 0o600);
        if target_kind == "hardlink" {
            fs::hard_link(&outside, capsule.join("receipt.json")).expect("hard link");
        } else {
            std::os::unix::fs::symlink(&outside, capsule.join("receipt.json")).expect("symlink");
        }
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

        assert_eq!(output.code, 65, "{target_kind}");
        let response: Value = output.stdout_json();
        assert_service_envelope(&response, false);
        assert_eq!(response["error"]["code"], "artifact-path-unsafe");
        assert_eq!(fs::read_to_string(outside).expect("outside"), "preserve me");
        assert!(!argv_log.exists(), "Codex must not run for {target_kind}");
    }
}

#[test]
fn unsafe_manifest_and_entrypoint_modes_are_rejected_before_codex() {
    for (target, mode, code) in [
        ("manifest.json", 0o640, "manifest-not-private"),
        ("run.sh", 0o600, "entrypoint-not-executable"),
        ("run.sh", 0o750, "entrypoint-not-private"),
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let capsule = write_capsule(temp.path(), &workspace, "workspace");
        set_mode(&capsule.join(target), mode);
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
        assert_eq!(output.code, 65, "{target} mode {mode:o}");
        assert_eq!(output.stdout_json()["error"]["code"], code);
        assert!(!argv_log.exists());
    }
}

#[test]
fn symlinked_entrypoint_is_rejected_before_codex() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let external = temp.path().join("external.sh");
    fs::write(&external, "#!/usr/bin/env bash\nexit 0\n").expect("external");
    fs::remove_file(capsule.join("run.sh")).expect("remove run.sh");
    std::os::unix::fs::symlink(&external, capsule.join("run.sh")).expect("symlink");
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
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "entrypoint-unreadable"
    );
    assert!(!argv_log.exists());
}

#[test]
fn workspace_allowed_path_cannot_escape_cwd() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["allowed_paths"] = json!([temp.path()]);
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

    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "workspace-path-outside-cwd"
    );
    assert!(!argv_log.exists());
}

#[test]
fn workspace_capsule_inside_cwd_is_rejected_before_codex() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(&workspace, &workspace, "workspace");
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
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["error"]["code"], "workspace-capsule-inside-cwd");
    assert!(!argv_log.exists());
}

#[test]
fn expected_git_branch_mismatch_is_rejected_before_codex() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let git = std::process::Command::new("git")
        .args(["init", "-q", "-b", "capsule-test"])
        .current_dir(&workspace)
        .status()
        .expect("git init");
    assert!(git.success());
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let manifest_path = capsule.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["expected_git"] = json!({"branch": "different-branch"});
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

    assert_eq!(output.code, 65);
    assert_eq!(output.stdout_json()["error"]["code"], "git-branch-mismatch");
    assert!(!argv_log.exists());
}

#[test]
fn codex_launch_failure_still_writes_a_typed_receipt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let empty_bin = temp.path().join("empty-bin");
    fs::create_dir_all(&empty_bin).expect("empty bin");

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
            .with_env("PATH", empty_bin.to_str().expect("empty bin")),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_eq!(response["ok"], false);
    assert_eq!(response["result"]["codex_exit_code"], 127);
    assert!(
        response["result"]["codex_error"]
            .as_str()
            .expect("codex error")
            .contains("failed to start Codex supervisor")
    );
    let receipt: Value =
        serde_json::from_slice(&fs::read(capsule.join("receipt.json")).expect("receipt"))
            .expect("receipt json");
    assert_eq!(receipt["schema_version"], response["schema_version"]);
    assert_eq!(
        receipt["result"]["codex_error"],
        response["result"]["codex_error"]
    );
}

#[test]
fn foreign_owned_capsule_is_rejected_when_running_privileged() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    for path in [
        capsule.join("manifest.json"),
        capsule.join("run.sh"),
        capsule.clone(),
    ] {
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
        assert_eq!(unsafe { libc::chown(path.as_ptr(), 1, 1) }, 0);
    }
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
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "capsule-not-private-directory"
    );
    assert!(!argv_log.exists());
}
