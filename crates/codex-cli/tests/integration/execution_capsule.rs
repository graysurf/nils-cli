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

/// Sentinels planted in the fake active Codex home. A governance-projected
/// supervisor must carry the instruction and hook sentinels into the child home
/// and must leave every MCP, plugin, app, and secret sentinel behind.
const HOME_INSTRUCTION_SENTINEL: &str = "CAPSULE_HOME_INSTRUCTION_SENTINEL_4F1A";
const HOOK_SENTINEL: &str = "CAPSULE_HOOK_SENTINEL_9C22";
const HOOKS_JSON_SENTINEL: &str = "CAPSULE_HOOKS_JSON_SENTINEL_5B70";
const DIRECT_MCP_SENTINEL: &str = "CAPSULE_DIRECT_MCP_SENTINEL_A31D";
const PLUGIN_MCP_SENTINEL: &str = "CAPSULE_PLUGIN_MCP_SENTINEL_7E48";
const APP_SENTINEL: &str = "CAPSULE_APP_SENTINEL_2D95";
const MCP_SECRET_SENTINEL: &str = "CAPSULE_MCP_SECRET_SENTINEL_C604";
const UI_SENTINEL: &str = "CAPSULE_UI_SENTINEL_B117";

/// Build a fake active Codex home containing instruction, hook, MCP, plugin,
/// app, secret, and unrelated user-interface configuration.
fn write_source_codex_home(root: &Path) -> PathBuf {
    let home = root.join("codex-home");
    fs::create_dir_all(&home).expect("codex home");
    set_mode(&home, 0o700);
    fs::write(home.join("AGENTS.md"), HOME_INSTRUCTION_SENTINEL).expect("home instructions");
    fs::write(home.join("auth.json"), "{\"tokens\":{}}\n").expect("auth file");
    set_mode(&home.join("auth.json"), 0o600);
    fs::write(
        home.join("hooks.json"),
        format!("{{\"note\":\"{HOOKS_JSON_SENTINEL}\"}}\n"),
    )
    .expect("hooks json");
    let config = format!(
        r#"model = "gpt-5"
notify = ["notify-send", "{UI_SENTINEL}"]

[tui]
status_line = "{UI_SENTINEL}"

[features]
hooks = true
memories = true
plugins = true

[mcp_servers.atlassian-rovo]
command = "{DIRECT_MCP_SENTINEL}"
bearer_token = "{MCP_SECRET_SENTINEL}"

[mcp_servers.remote-docs]
url = "https://example.invalid/mcp"

[mcp_servers.remote-docs.http_headers]
authorization = "{MCP_SECRET_SENTINEL}"

[plugins."vendor.plugin".mcp_servers.rovo]
command = "{PLUGIN_MCP_SENTINEL}"

[apps.connector]
command = "{APP_SENTINEL}"
token = "{MCP_SECRET_SENTINEL}"

[[hooks.SessionStart]]

[[hooks.SessionStart.hooks]]
type = "command"
command = "{HOOK_SENTINEL}"
timeout = 5

[hooks.state."{}:session_start:0:0"]
trusted_hash = "hash-sentinel"
enabled = true
"#,
        home.join("config.toml").display()
    );
    fs::write(home.join("config.toml"), config).expect("config");
    set_mode(&home.join("config.toml"), 0o600);
    home
}

fn write_codex_stub(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let argv_log = root.join("codex.argv");
    let codex_home = write_source_codex_home(root);
    test_fs::write_executable(
        &bin_dir.join("codex"),
        r#"#!/usr/bin/env bash
set -euo pipefail

# Capability probes used by the governance-projected supervisor preflight.
if [ "${1:-}" = 'exec' ] && [ "${2:-}" = '--help' ]; then
  printf '%s\n' '--ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --disable' \
    '--json --sandbox --output-schema --output-last-message'
  exit 0
fi
if [ "${1:-}" = 'features' ] && [ "${2:-}" = 'list' ]; then
  for feature in hooks plugins remote_plugin apps workspace_dependencies memories goals multi_agent shell_tool unified_exec; do
    if [ "$feature" = "${CODEX_CAPSULE_MISSING_FEATURE:-}" ]; then continue; fi
    printf '%s stable true\n' "$feature"
  done
  exit 0
fi

# Report what the child actually received as its CODEX_HOME.
if [ -n "${CODEX_CAPSULE_HOME_REPORT_DIR:-}" ]; then
  stat_mode() {
    if stat -c '%a' "$1" 2>/dev/null; then
      return
    fi
    stat -f '%Lp' "$1"
  }
  mkdir -p "$CODEX_CAPSULE_HOME_REPORT_DIR"
  printf '%s\n' "${CODEX_HOME:-}" > "$CODEX_CAPSULE_HOME_REPORT_DIR/codex-home"
  printf '%s\n' "$*" > "$CODEX_CAPSULE_HOME_REPORT_DIR/child-argv"
  { env | sort; } > "$CODEX_CAPSULE_HOME_REPORT_DIR/child-env" || true
  if [ -n "${CODEX_HOME:-}" ] && [ -d "${CODEX_HOME:-}" ]; then
    stat_mode "$CODEX_HOME" > "$CODEX_CAPSULE_HOME_REPORT_DIR/home-mode"
    ls -A "$CODEX_HOME" > "$CODEX_CAPSULE_HOME_REPORT_DIR/home-entries"
    for name in config.toml AGENTS.md hooks.json; do
      if [ -f "$CODEX_HOME/$name" ]; then
        cp "$CODEX_HOME/$name" "$CODEX_CAPSULE_HOME_REPORT_DIR/$name"
        stat_mode "$CODEX_HOME/$name" > "$CODEX_CAPSULE_HOME_REPORT_DIR/$name.mode"
      fi
    done
    if [ -L "$CODEX_HOME/auth.json" ]; then
      printf 'symlink %s\n' "$(readlink "$CODEX_HOME/auth.json")" \
        > "$CODEX_CAPSULE_HOME_REPORT_DIR/auth-kind"
    elif [ -e "$CODEX_HOME/auth.json" ]; then
      printf 'file\n' > "$CODEX_CAPSULE_HOME_REPORT_DIR/auth-kind"
    else
      printf 'absent\n' > "$CODEX_CAPSULE_HOME_REPORT_DIR/auth-kind"
    fi
  fi
fi

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

# Emit a first event, then stay slow: proves the deadline bounds startup only.
if [ -n "${CODEX_CAPSULE_DELAY_AFTER_FIRST_EVENT:-}" ]; then
  printf '%s\n' '{"type":"thread.started","thread_id":"slow-thread"}'
  sleep "$CODEX_CAPSULE_DELAY_AFTER_FIRST_EVENT"
fi

# Startup-observability knobs: produce no JSONL event, optionally ignoring the
# first termination signal so the parent must escalate.
if [ -n "${CODEX_CAPSULE_STALL_SECONDS:-}" ]; then
  if [ "${CODEX_CAPSULE_IGNORE_SIGTERM:-0}" = 1 ]; then
    trap '' TERM
    while :; do sleep 0.2; done
  fi
  sleep "$CODEX_CAPSULE_STALL_SECONDS"
  exit 0
fi

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
  local combined command_json observed_command output_json
  set +e
  combined="$(eval "$command" 2>&1)"
  LAST_STATUS=$?
  set -e
  observed_command="$command"
  if [ "${CODEX_CAPSULE_QUOTE_EVENT_COMMAND:-0}" = 1 ]; then
    observed_command="'$command'"
  fi
  observed_command="${observed_command}${CODEX_CAPSULE_EVENT_COMMAND_SUFFIX:-}"
  command_json="$(json_escape "$observed_command")"
  output_json="$(json_escape "$combined")"
  printf '{"type":"item.completed","item":{"id":"item-test","type":"command_execution","command":"/usr/bin/zsh %s %s","aggregated_output":"%s","exit_code":%d,"status":"completed"}}\n' \
    "${CODEX_CAPSULE_EVENT_SHELL_FLAG:--lc}" "$command_json" "$output_json" "$LAST_STATUS"
}

script_command="$(printf '%s\n' "$last" | sed -n 's/^Exact script command: //p' | head -1)"
test -n "$script_command"
restore_capsule_root=''
if [ -n "${CODEX_CAPSULE_SWAP_CAPSULE_ROOT:-}" ]; then
  helper_path="${script_command%% *}"
  helper_name="${helper_path##*/}"
  restore_capsule_root="$CODEX_CAPSULE_SWAP_CAPSULE_ROOT.original"
  mv "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT" "$restore_capsule_root"
  mkdir -m 700 "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT"
  cp "$restore_capsule_root/manifest.json" "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT/manifest.json"
  chmod 600 "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT/manifest.json"
  cat >"$CODEX_CAPSULE_SWAP_CAPSULE_ROOT/$helper_name" <<'HELPER'
#!/usr/bin/env bash
set -euo pipefail
capsule=''
nonce=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --capsule) capsule="$2"; shift 2 ;;
    --nonce) nonce="$2"; shift 2 ;;
    *) shift ;;
  esac
done
digest="$(sed -n 's/.*"entrypoint_sha256": *"\([^"]*\)".*/\1/p' "$capsule/manifest.json")"
printf '\n{"schema_version":"cli.codex-cli.execution-capsule.attestation.v1","nonce":"%s","kind":"script","validation_index":null,"entrypoint_sha256":"%s","exit_code":0}\n' \
  "$nonce" "$digest"
HELPER
  chmod 500 "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT/$helper_name"
fi
if [ "${CODEX_CAPSULE_FORGE_REPLACED_HELPER:-0}" = 1 ]; then
  helper_path="${script_command%% *}"
  mv "$helper_path" "$helper_path.original"
  cat >"$helper_path" <<'HELPER'
#!/usr/bin/env bash
set -euo pipefail
capsule=''
nonce=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --capsule) capsule="$2"; shift 2 ;;
    --nonce) nonce="$2"; shift 2 ;;
    *) shift ;;
  esac
done
digest="$(sed -n 's/.*"entrypoint_sha256": *"\([^"]*\)".*/\1/p' "$capsule/manifest.json")"
printf '\n{"schema_version":"cli.codex-cli.execution-capsule.attestation.v1","nonce":"%s","kind":"script","validation_index":null,"entrypoint_sha256":"%s","exit_code":0}\n' \
  "$nonce" "$digest"
HELPER
  chmod 500 "$helper_path"
fi
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
if [ -n "$restore_capsule_root" ]; then
  rm -f "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT"/*
  rmdir "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT"
  mv "$restore_capsule_root" "$CODEX_CAPSULE_SWAP_CAPSULE_ROOT"
fi
if [ -n "${CODEX_CAPSULE_SWAP_FINAL_PATH:-}" ]; then
  rm -f "$CODEX_CAPSULE_SWAP_FINAL_PATH"
  ln -s "$CODEX_CAPSULE_SENTINEL" "$CODEX_CAPSULE_SWAP_FINAL_PATH"
fi
if [ "${CODEX_CAPSULE_SKIP_FINAL:-0}" != 1 ]; then
  if [ "${CODEX_CAPSULE_CLOSE_EXTRA_FDS:-0}" = 1 ]; then
    for descriptor in $(seq 3 32); do
      eval "exec ${descriptor}>&-" 2>/dev/null || true
    done
  fi
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
    (bin_dir, argv_log, codex_home)
}

#[test]
fn workspace_capsule_preserves_governance_and_writes_private_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
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
    assert_eq!(response["result"]["evidence_trust"], "sandbox-attested");
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
    let mut invalid = response.clone();
    invalid["result"]["evidence_trust"] = json!("supervisor-trusted");
    assert_receipt_schema_rejects(&invalid, "workspace receipt with host evidence trust");

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
    assert_eq!(
        &argv[9..18],
        &[
            "--ephemeral",
            "--disable",
            "plugins",
            "--disable",
            "remote_plugin",
            "--disable",
            "apps",
            "--disable",
            "workspace_dependencies"
        ],
        "the default supervisor must disable plugin and app capability loading"
    );
    assert_eq!(argv[18], "--output-schema");
    assert_eq!(argv[20], "--output-last-message");
    assert_eq!(argv[21], "/dev/fd/0");
    assert_eq!(argv[22], "--");
    let prompt = &argv[23];
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
    assert!(
        fs::read_dir(&capsule)
            .expect("capsule entries")
            .all(|entry| !entry
                .expect("capsule entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".execution-capsule-helper.")),
        "private helper snapshots must be removed before returning"
    );
    assert!(
        fs::read_dir(&workspace)
            .expect("workspace entries")
            .all(|entry| !entry
                .expect("workspace entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".final.capture.")),
        "final capture must remain unlinked"
    );
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
fn replaced_host_helper_cannot_forge_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "host");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_FORGE_REPLACED_HELPER", "1"),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_eq!(response["result"]["script_passed"], true);
    assert_eq!(response["result"]["helper_integrity_valid"], false);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "helper-integrity-failed");
}

#[test]
fn swapped_host_capsule_root_cannot_forge_success() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "host");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env(
                "CODEX_CAPSULE_SWAP_CAPSULE_ROOT",
                capsule.to_str().expect("capsule path"),
            ),
    );

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_eq!(response["result"]["script_passed"], true);
    assert_eq!(response["result"]["helper_integrity_valid"], false);
    assert_eq!(response["ok"], false);
    assert!(
        matches!(
            response["error"]["code"].as_str(),
            Some("helper-integrity-failed" | "codex-exit-nonzero")
        ),
        "response={response}"
    );
}

#[test]
fn shell_wrapped_extra_commands_do_not_match_exact_helper_events() {
    for suffix in [" extra-argument", "; true"] {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let capsule = write_capsule(temp.path(), &workspace, "workspace");
        let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
                .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
                .with_env(
                    "CODEX_AUTH_FILE",
                    codex_home.join("auth.json").to_str().expect("auth file"),
                )
                .with_env(
                    "CODEX_CAPSULE_ARGV_LOG",
                    argv_log.to_str().expect("argv log"),
                )
                .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
                .with_env("CODEX_CAPSULE_EVENT_COMMAND_SUFFIX", suffix),
        );

        assert_eq!(output.code, 1, "suffix={suffix}");
        let response: Value = output.stdout_json();
        assert_eq!(response["result"]["script_passed"], false);
        assert_eq!(response["error"]["code"], "script-attestation-failed");
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
        "argv": ["sh", "-c", "printf 'noisy validation\\n'; printf checked > validation-marker"]
    }]);
    test_fs::write_json(&manifest_path, &manifest);
    set_mode(&manifest_path, 0o600);
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    assert_eq!(response["result"]["helper_integrity_valid"], true);
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());
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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    assert_eq!(response["result"]["evidence_trust"], "supervisor-trusted");
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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

    assert_eq!(output.code, 1);
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["result"]["helper_integrity_valid"], false);
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    assert_eq!(response["error"]["code"], "helper-integrity-failed");
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());
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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
        let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
                .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
                .with_env(
                    "CODEX_AUTH_FILE",
                    codex_home.join("auth.json").to_str().expect("auth file"),
                )
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
        let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());
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
                .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
                .with_env(
                    "CODEX_AUTH_FILE",
                    codex_home.join("auth.json").to_str().expect("auth file"),
                )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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
            "--mcp-mode",
            "inherited",
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
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
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

fn read_report(report: &Path, name: &str) -> String {
    fs::read_to_string(report.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn assert_no_secret_sentinels(haystack: &str, case: &str) {
    for sentinel in [
        MCP_SECRET_SENTINEL,
        DIRECT_MCP_SENTINEL,
        PLUGIN_MCP_SENTINEL,
        APP_SENTINEL,
        UI_SENTINEL,
    ] {
        assert!(
            !haystack.contains(sentinel),
            "{case} must not contain {sentinel}"
        );
    }
}

#[test]
fn agent_run_help_exposes_only_the_two_mcp_modes() {
    let output = cmd::run_with(
        &codex_cli_bin(),
        &["agent", "run", "--help"],
        &CmdOptions::default(),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let help = output.stdout_text();
    assert!(help.contains("--mcp-mode <mode>"), "{help}");
    assert!(help.contains("[default: disabled]"), "{help}");
    assert!(
        help.contains("[possible values: disabled, inherited]"),
        "{help}"
    );
    assert!(
        !help.contains("-m, --mcp-mode"),
        "--mcp-mode must not gain a short flag: {help}"
    );
}

#[test]
fn invalid_mcp_mode_is_a_usage_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--mcp-mode",
            "allowlist",
        ],
        &CmdOptions::default().with_cwd(&workspace),
    );

    assert_eq!(output.code, 64);
    assert!(
        output.stderr_text().contains("allowlist"),
        "stderr={}",
        output.stderr_text()
    );
}

#[test]
fn default_mcp_mode_projects_a_private_governance_home() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());
    let report = temp.path().join("home-report");

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env(
                "CODEX_CAPSULE_HOME_REPORT_DIR",
                report.to_str().expect("report dir"),
            )
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
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
    assert_eq!(response["result"]["mcp_mode"], "disabled");
    assert_eq!(
        response["result"]["supervisor_runtime"],
        "governance-projected"
    );
    assert!(
        output
            .stderr_text()
            .contains("codex-cli agent run: starting supervisor (mcp=disabled)"),
        "stderr={}",
        output.stderr_text()
    );
    assert!(
        !output.stderr_text().contains("MCP mode inherited"),
        "the default must not print the inherited notice"
    );

    // The child home is a unique private projection, not the active home.
    let child_home = PathBuf::from(read_report(&report, "codex-home").trim().to_string());
    assert_ne!(child_home, codex_home);
    assert_eq!(read_report(&report, "home-mode").trim(), "700");
    assert_eq!(read_report(&report, "config.toml.mode").trim(), "600");
    assert_eq!(read_report(&report, "AGENTS.md.mode").trim(), "600");

    // Authentication is bridged, never copied.
    assert_eq!(
        read_report(&report, "auth-kind").trim(),
        format!(
            "symlink {}",
            fs::canonicalize(codex_home.join("auth.json"))
                .expect("canonical auth")
                .display()
        )
    );

    // Governance survives: home instructions plus the hook table and its trust
    // state, rekeyed onto the projected config path.
    assert_eq!(
        read_report(&report, "AGENTS.md").trim(),
        HOME_INSTRUCTION_SENTINEL
    );
    let child_config = read_report(&report, "config.toml");
    assert!(child_config.contains("hooks = true"), "{child_config}");
    assert!(child_config.contains(HOOK_SENTINEL), "{child_config}");
    assert!(
        child_config.contains(&format!(
            "{}:session_start:0:0",
            child_home.join("config.toml").display()
        )),
        "hook trust state must be rekeyed onto the projected config path: {child_config}"
    );
    assert!(
        !child_config.contains(&codex_home.join("config.toml").display().to_string()),
        "the projection must not retain the source config path: {child_config}"
    );
    assert!(
        read_report(&report, "hooks.json").contains(HOOKS_JSON_SENTINEL),
        "provider hook file must be projected"
    );

    // No MCP, plugin, app, secret, or unrelated user config reaches the child.
    for excluded in [
        "mcp_servers",
        "plugins",
        "apps",
        "memories",
        "notify",
        "tui",
    ] {
        assert!(
            !child_config.contains(excluded),
            "projected config must not contain {excluded}: {child_config}"
        );
    }
    assert_no_secret_sentinels(&child_config, "projected config");
    assert_no_secret_sentinels(&read_report(&report, "child-argv"), "child argv");
    assert_no_secret_sentinels(&read_report(&report, "child-env"), "child environment");
    assert_no_secret_sentinels(&output.stdout_text(), "JSON stdout");
    assert_no_secret_sentinels(&output.stderr_text(), "stderr");
    assert_no_secret_sentinels(
        &fs::read_to_string(capsule.join("receipt.json")).expect("receipt"),
        "receipt",
    );

    // Control-plane environment is dropped for the child.
    let child_env = read_report(&report, "child-env");
    for removed in ["CODEX_AUTH_FILE=", "CODEX_CLI_AGENT_RUNTIME="] {
        assert!(
            !child_env.contains(removed),
            "child must not inherit {removed}: {child_env}"
        );
    }

    // The temporary home is removed after completion.
    assert!(
        !child_home.exists(),
        "projected supervisor home {} must be removed",
        child_home.display()
    );
}

#[test]
fn inherited_mcp_mode_uses_the_active_home_and_warns() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());
    let report = temp.path().join("home-report");

    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "run",
            "--capsule",
            capsule.to_str().expect("capsule path"),
            "--mcp-mode",
            "inherited",
            "--format",
            "json",
        ],
        &CmdOptions::default()
            .with_cwd(&workspace)
            .with_path_prepend(&bin_dir)
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env(
                "CODEX_CAPSULE_HOME_REPORT_DIR",
                report.to_str().expect("report dir"),
            )
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
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
    assert_eq!(response["result"]["mcp_mode"], "inherited");
    assert_eq!(response["result"]["supervisor_runtime"], "inherited");
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("codex-cli agent run: MCP mode inherited; external tools may initialize"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("codex-cli agent run: starting supervisor (mcp=inherited)"),
        "stderr={stderr}"
    );

    // The child keeps the real home and the prototype launch shape.
    assert_eq!(
        PathBuf::from(read_report(&report, "codex-home").trim().to_string()),
        codex_home
    );
    let argv = fs::read_to_string(&argv_log)
        .expect("argv log")
        .split('\0')
        .filter(|argument| !argument.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(&argv[6..9], &["--sandbox", "workspace-write", "--json"]);
    assert_eq!(argv[9], "--output-schema");
    assert!(
        !argv.iter().any(|argument| argument == "--disable"),
        "inherited mode must not add feature disables: {argv:?}"
    );
    assert!(
        !argv.iter().any(|argument| argument == "--ephemeral"),
        "inherited mode must preserve the prototype launch shape: {argv:?}"
    );
}

#[test]
fn environment_cannot_select_inherited_mcp_mode() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CLI_AGENT_RUNTIME", "inherited")
            .with_env("CODEX_CLI_MCP_MODE", "inherited")
            .with_env("CODEX_MCP_MODE", "inherited")
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let response: Value = output.stdout_json();
    assert_eq!(response["result"]["mcp_mode"], "disabled");
    assert_eq!(
        response["result"]["supervisor_runtime"],
        "governance-projected"
    );
}

fn write_project_config(workspace: &Path, contents: &str) {
    let directory = workspace.join(".codex");
    fs::create_dir_all(&directory).expect("project config dir");
    fs::write(directory.join("config.toml"), contents).expect("project config");
}

fn run_default_capsule(temp: &Path, workspace: &Path) -> (cmd::CmdOutput, PathBuf, PathBuf) {
    let capsule = write_capsule(temp, workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp);
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
            .with_cwd(workspace)
            .with_path_prepend(&bin_dir)
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
    );
    (output, capsule, argv_log)
}

#[test]
fn project_config_declaring_mcp_fails_closed_without_echoing_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    write_project_config(
        &workspace,
        &format!(
            "[mcp_servers.project-rovo]\ncommand = \"{DIRECT_MCP_SENTINEL}\"\nbearer_token = \"{MCP_SECRET_SENTINEL}\"\n"
        ),
    );

    let (output, capsule, argv_log) = run_default_capsule(temp.path(), &workspace);

    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(
        response["schema_version"],
        "cli.codex-cli.execution-capsule.error.v1"
    );
    assert_eq!(response["error"]["code"], "capsule-project-mcp-undeclared");
    assert_eq!(response["error"]["details"]["retryable"], "no");
    assert!(
        response["error"]["details"]["next_action"]
            .as_str()
            .expect("next action")
            .contains("--mcp-mode inherited")
    );
    assert_no_secret_sentinels(&output.stdout_text(), "project MCP rejection stdout");
    assert_no_secret_sentinels(&output.stderr_text(), "project MCP rejection stderr");
    assert!(
        !argv_log.exists(),
        "Codex must not start after a fail-closed project MCP rejection"
    );
    assert!(
        !capsule.join("receipt.json").exists(),
        "a preflight rejection must not publish capsule artifacts"
    );
}

#[test]
fn project_config_plugin_and_app_authority_fails_closed() {
    for declaration in [
        "[plugins.\"vendor.plugin\".mcp_servers.rovo]\ncommand = \"rovo\"\n",
        "[apps.connector]\ncommand = \"connector\"\n",
        "[features]\nplugins = true\n",
    ] {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        write_project_config(&workspace, declaration);

        let (output, _capsule, argv_log) = run_default_capsule(temp.path(), &workspace);

        assert_eq!(output.code, 65, "declaration={declaration}");
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "capsule-project-mcp-undeclared",
            "declaration={declaration}"
        );
        assert!(!argv_log.exists(), "declaration={declaration}");
    }
}

#[test]
fn safe_project_config_and_benign_lookalike_keys_pass() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    write_project_config(
        &workspace,
        r#"model = "gpt-5"
mcp_servers_notes = "documented in the runbook"

[tools]
mcp_servers = "a nested key, not a server table"

[features]
plugins = false
hooks = true

[profiles.apps]
model = "gpt-5"
"#,
    );

    let (output, _capsule, _argv_log) = run_default_capsule(temp.path(), &workspace);

    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    assert_eq!(output.stdout_json()["result"]["mcp_mode"], "disabled");
}

#[test]
fn no_project_config_passes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");

    let (output, _capsule, _argv_log) = run_default_capsule(temp.path(), &workspace);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["result"]["mcp_mode"], "disabled");
}

#[test]
fn malformed_project_config_fails_before_codex_starts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    write_project_config(&workspace, "[mcp_servers\nbroken = \n");

    let (output, _capsule, argv_log) = run_default_capsule(temp.path(), &workspace);

    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "capsule-supervisor-config-invalid"
    );
    assert!(!argv_log.exists());
}

#[test]
fn symlinked_project_config_is_rejected_before_codex_starts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let hostile = temp.path().join("hostile-config.toml");
    fs::write(&hostile, "[mcp_servers.rovo]\ncommand = \"rovo\"\n").expect("hostile config");
    let directory = workspace.join(".codex");
    fs::create_dir_all(&directory).expect("project config dir");
    std::os::unix::fs::symlink(&hostile, directory.join("config.toml")).expect("symlink");

    let (output, _capsule, argv_log) = run_default_capsule(temp.path(), &workspace);

    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "capsule-supervisor-config-invalid"
    );
    assert!(!argv_log.exists());
}

#[test]
fn missing_supervisor_capability_fails_closed_without_falling_back() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_MISSING_FEATURE", "apps"),
    );

    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(response["error"]["code"], "capsule-supervisor-unsupported");
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("apps")
    );
    assert_eq!(response["error"]["details"]["retryable"], "no");
    assert!(
        !argv_log.exists(),
        "an unsupported supervisor must never fall back to the inherited runtime"
    );
    assert!(!capsule.join("receipt.json").exists());
}

fn run_capsule_with_stall(
    temp: &Path,
    workspace: &Path,
    stall_seconds: &str,
    ignore_sigterm: bool,
) -> (cmd::CmdOutput, PathBuf) {
    let capsule = write_capsule(temp, workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp);
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
            .with_cwd(workspace)
            .with_path_prepend(&bin_dir)
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_STALL_SECONDS", stall_seconds)
            .with_env(
                "CODEX_CAPSULE_IGNORE_SIGTERM",
                if ignore_sigterm { "1" } else { "0" },
            )
            .with_env("CODEX_CLI_CAPSULE_FIRST_EVENT_DEADLINE_MS", "400"),
    );
    (output, capsule)
}

#[test]
fn missing_first_event_terminates_with_a_stable_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");

    let (output, capsule) = run_capsule_with_stall(temp.path(), &workspace, "120", false);

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let response: Value = output.stdout_json();
    assert_service_envelope(&response, false);
    assert_eq!(
        response["schema_version"],
        "cli.codex-cli.execution-capsule.receipt.v1"
    );
    assert_eq!(response["ok"], false);
    assert_eq!(
        response["error"]["code"],
        "codex-supervisor-startup-timeout"
    );
    assert_eq!(response["error"]["details"]["retryable"], "conditional");
    assert!(
        response["error"]["details"]["next_action"]
            .as_str()
            .expect("next action")
            .contains("--mcp-mode inherited")
    );
    assert_eq!(response["result"]["mcp_mode"], "disabled");
    let receipt: Value =
        serde_json::from_slice(&fs::read(capsule.join("receipt.json")).expect("receipt"))
            .expect("receipt json");
    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["error"]["code"], "codex-supervisor-startup-timeout");
    // A stable bounded error, never a captured child stderr dump.
    let message = response["error"]["message"].as_str().expect("message");
    assert!(
        message.starts_with("codex-supervisor-startup-timeout:"),
        "{message}"
    );
    assert!(message.len() < 200, "{message}");
}

#[test]
fn supervisor_ignoring_termination_is_forcefully_reaped() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");

    let (output, _capsule) = run_capsule_with_stall(temp.path(), &workspace, "120", true);

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "codex-supervisor-startup-timeout"
    );
}

#[test]
fn slow_turn_after_the_first_event_does_not_trigger_the_startup_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_DELAY_AFTER_FIRST_EVENT", "2")
            .with_env("CODEX_CLI_CAPSULE_FIRST_EVENT_DEADLINE_MS", "400")
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
    );

    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let response: Value = output.stdout_json();
    assert_eq!(response["ok"], true);
    assert!(response.get("error").is_none());
}

#[test]
fn receipt_schema_constrains_the_supervisor_policy_fields() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");

    let (output, _capsule, _argv_log) = run_default_capsule(temp.path(), &workspace);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let response: Value = output.stdout_json();

    let mut invalid = response.clone();
    invalid["result"]["supervisor_runtime"] = json!("inherited");
    assert_receipt_schema_rejects(
        &invalid,
        "disabled mode with an inherited supervisor runtime",
    );
    let mut invalid = response.clone();
    invalid["result"]["mcp_mode"] = json!("inherited");
    assert_receipt_schema_rejects(
        &invalid,
        "inherited mode with a projected supervisor runtime",
    );
    let mut invalid = response.clone();
    invalid["result"]["mcp_mode"] = json!("allowlist");
    assert_receipt_schema_rejects(&invalid, "an unknown MCP mode");
    let mut invalid = response.clone();
    invalid["result"]
        .as_object_mut()
        .expect("result")
        .remove("mcp_mode");
    assert_receipt_schema_rejects(&invalid, "a receipt without the effective MCP mode");
    let mut invalid = response.clone();
    invalid["result"]
        .as_object_mut()
        .expect("result")
        .remove("supervisor_runtime");
    assert_receipt_schema_rejects(&invalid, "a receipt without the supervisor runtime");
}

#[test]
fn plain_shell_helper_events_are_attested_like_login_shell_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            // The projected supervisor home does not carry the operator's
            // shell-snapshot feature, so Codex may use a plain `-c` shell.
            .with_env("CODEX_CAPSULE_EVENT_SHELL_FLAG", "-c")
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
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
    assert_eq!(response["result"]["script_passed"], true);
    assert_eq!(response["result"]["script_runs"][0]["passed"], true);
}

#[test]
fn plain_shell_events_still_reject_extra_commands() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace");
    let capsule = write_capsule(temp.path(), &workspace, "workspace");
    let (bin_dir, argv_log, codex_home) = write_codex_stub(temp.path());

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
            .with_env("CODEX_HOME", codex_home.to_str().expect("codex home"))
            .with_env(
                "CODEX_AUTH_FILE",
                codex_home.join("auth.json").to_str().expect("auth file"),
            )
            .with_env(
                "CODEX_CAPSULE_ARGV_LOG",
                argv_log.to_str().expect("argv log"),
            )
            .with_env("CODEX_CAPSULE_EVENT_SHELL_FLAG", "-c")
            .with_env("CODEX_CAPSULE_QUOTE_EVENT_COMMAND", "1")
            .with_env("CODEX_CAPSULE_EVENT_COMMAND_SUFFIX", " && printf extra")
            .with_env("CODEX_CAPSULE_CLOSE_EXTRA_FDS", "1"),
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let response: Value = output.stdout_json();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "script-attestation-failed");
}
