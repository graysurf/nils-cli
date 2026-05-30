#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  shared-helper-adoption-audit.sh [--format tsv] [--out <manifest.tsv>] [--root <repo>] [--check-seeds]

Scans for known maintainability candidates where code/tests duplicate primitives that should use
`nils-common` or `nils-test-support`, then writes an issue-centric manifest.

Output columns (TSV):
  path    category helper_target  status  task_id  risk  detection_regex  match_count  match_preview note

Options:
  --format <tsv>   Output format (currently only `tsv` is supported)
  --out <file>     Write manifest TSV to file (also writes summary.md in the same directory)
  --root <dir>     Repo root (defaults to current git worktree root)
  --check-seeds    Fail if any seeded candidate path is missing
  -h, --help       Show this help
USAGE
}

format="tsv"
out_file=""
repo_root=""
check_seeds=0

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --format)
      format="${2:-}"
      shift 2
      ;;
    --out)
      out_file="${2:-}"
      shift 2
      ;;
    --root)
      repo_root="${2:-}"
      shift 2
      ;;
    --check-seeds)
      check_seeds=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: ${1:-}" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "$format" != "tsv" ]]; then
  echo "error: unsupported format: $format (only 'tsv' is supported)" >&2
  exit 2
fi

if [[ -z "$repo_root" ]]; then
  repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: repo root not found (run inside a git worktree or pass --root)" >&2
  exit 2
fi
cd "$repo_root"

if [[ ! -f "Cargo.toml" || ! -d "crates" ]]; then
  echo "error: expected nils-cli workspace root (missing Cargo.toml or crates/)" >&2
  exit 2
fi

tmp_rows="$(mktemp "${TMPDIR:-/tmp}/shared-helper-adoption.XXXXXX.tsv")"
cleanup() {
  rm -f "$tmp_rows"
}
trap cleanup EXIT

write_header() {
  printf 'path\tcategory\thelper_target\tstatus\ttask_id\trisk\tdetection_regex\tmatch_count\tmatch_preview\tnote\n'
}

sanitize_tsv_field() {
  local s="${1:-}"
  s="${s//$'\t'/ }"
  s="${s//$'\n'/; }"
  s="${s//$'\r'/}"
  printf '%s' "$s"
}

detect_matches() {
  local path="$1"
  local regex="$2"
  if [[ ! -f "$path" ]]; then
    printf '0\t%s\n' "missing-file"
    return 0
  fi

  local lines
  lines="$(rg -n -e "$regex" "$path" 2>/dev/null || true)"
  if [[ -z "$lines" ]]; then
    printf '0\t%s\n' "no-hit"
    return 0
  fi

  local count preview
  count="$(printf '%s\n' "$lines" | wc -l | tr -d ' ')"
  preview="$(printf '%s\n' "$lines" | head -n 3 | paste -sd ';' -)"
  preview="$(sanitize_tsv_field "$preview")"
  printf '%s\t%s\n' "$count" "$preview"
}

add_row() {
  local path="$1"
  local category="$2"
  local helper_target="$3"
  local status="$4"
  local task_id="$5"
  local risk="$6"
  local detection_regex="$7"
  local note="$8"

  local detected match_count match_preview
  detected="$(detect_matches "$path" "$detection_regex")"
  match_count="${detected%%$'\t'*}"
  match_preview="${detected#*$'\t'}"

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(sanitize_tsv_field "$path")" \
    "$(sanitize_tsv_field "$category")" \
    "$(sanitize_tsv_field "$helper_target")" \
    "$(sanitize_tsv_field "$status")" \
    "$(sanitize_tsv_field "$task_id")" \
    "$(sanitize_tsv_field "$risk")" \
    "$(sanitize_tsv_field "$detection_regex")" \
    "$(sanitize_tsv_field "$match_count")" \
    "$(sanitize_tsv_field "$match_preview")" \
    "$(sanitize_tsv_field "$note")" \
    >>"$tmp_rows"
}

seed_manifest() {
  # path	category	helper_target	status	task_id	risk	detection_regex	note
  add_row "crates/memo-cli/src/output/text.rs" \
    "runtime.no_color" "nils-common::env + nils-test-support guards" "candidate" "Task 2.1" "medium" \
    'NO_COLOR|unsafe \{ std::env::(set_var|remove_var)' \
    "Local NO_COLOR semantics and raw env mutation in unit tests."

  add_row "crates/gemini-cli/src/agent/commit.rs" \
    "runtime.process_probe+source-test-helpers" "nils-common::process + nils-test-support" "candidate" "Task 2.2" "medium" \
    'fn command_exists|split_paths|fn env_lock|struct EnvGuard|write_executable\(' \
    "Manual PATH probe plus local test env/executable helpers in same file."

  add_row "crates/git-lock/src/diff.rs" \
    "runtime.git_process" "nils-common::git/process" "candidate" "Task 2.3" "medium" \
    'Command::new\("git"\)' \
    "Low-level git subprocess plumbing should prefer shared wrappers."
  add_row "crates/git-lock/src/tag.rs" \
    "runtime.git_process" "nils-common::git/process" "candidate" "Task 2.3" "medium" \
    'Command::new\("git"\)' \
    "Low-level git subprocess plumbing should prefer shared wrappers."
  add_row "crates/git-scope/src/print.rs" \
    "runtime.git_process" "nils-common::git/process" "candidate" "Task 2.3" "medium" \
    'Command::new\("git"\)' \
    "Manual git show/cat-file execution is a shared-wrapper candidate."

  add_row "crates/plan-tooling/src/validate.rs" \
    "runtime.git_process" "nils-common::git/process" "candidate" "Task 2.4" "low" \
    'Command::new\("git"\)' \
    "git ls-files discovery wrapper may reuse shared process plumbing."
  add_row "crates/semantic-commit/src/commit.rs" \
    "runtime.git_process" "nils-common::git/process" "candidate" "Task 2.4" "medium" \
    'Command::new\("git"\)' \
    "Semantic commit git execution wrappers overlap shared primitives."
  add_row "crates/semantic-commit/src/staged_context.rs" \
    "runtime.git_process" "nils-common::git/process" "candidate" "Task 2.4" "medium" \
    'Command::new\("git"\)' \
    "Staged context git execution wrappers overlap shared primitives."

  add_row "crates/codex-cli/src/auth/save.rs" \
    "runtime.provider_paths" "shared secret-dir resolver (codex adapter or nils-common)" "candidate" "Task 2.5" "high" \
    'resolve_secret_dir_from_env|CODEX_SECRET_DIR' \
    "Duplicated env-only secret-dir resolver; parity-sensitive behavior."
  add_row "crates/codex-cli/src/auth/remove.rs" \
    "runtime.provider_paths" "shared secret-dir resolver (codex adapter or nils-common)" "candidate" "Task 2.5" "high" \
    'resolve_secret_dir_from_env|CODEX_SECRET_DIR' \
    "Duplicated env-only secret-dir resolver; parity-sensitive behavior."

  add_row "crates/codex-cli/tests/integration/fs.rs" \
    "runtime.fs_primitives" "nils-common::fs (extended)" "candidate" "Task 3.2" "high" \
    'sha256_file|write_atomic|write_timestamp|nils_common::fs' \
    "Codex fs coverage records the shared primitive migration boundary."
  add_row "crates/gemini-cli/tests/integration/fs.rs" \
    "runtime.fs_primitives" "nils-common::fs (extended)" "candidate" "Task 3.2" "high" \
    'sha256_file|write_atomic|write_timestamp|nils_common::fs' \
    "Gemini fs coverage records the shared primitive migration boundary."
  add_row "crates/gemini-cli/src/auth/mod.rs" \
    "runtime.auth_fs_primitives" "nils-common::fs/json helpers (via adapter)" "candidate" "Task 3.3" "high" \
    'pub\(crate\) fn (write_atomic|write_timestamp|strip_newlines|normalize_iso)' \
    "Auth-local storage helpers duplicate broader shared primitive behavior."

  add_row "crates/git-cli/src/commit.rs" \
    "source-test.env_guard" "nils-test-support::EnvGuard/GlobalStateLock" "candidate" "Task 4.1" "low" \
    'struct EnvGuard' \
    "File-local test EnvGuard duplicates nils-test-support."
  add_row "crates/gemini-cli/src/auth/login.rs" \
    "source-test.env_guard+stubs" "nils-test-support guards/stubs/fs" "candidate" "Task 4.2" "medium" \
    'fn env_lock|struct EnvGuard|fn prepend_path|fn write_script' \
    "Source tests define custom env lock/guard/path prepend/script writers."
  add_row "crates/gemini-cli/src/auth/auto_refresh.rs" \
    "source-test.env_guard" "nils-test-support guards" "candidate" "Task 4.2" "low" \
    'fn env_lock|struct EnvGuard' \
    "Source tests define custom env guard pattern."

  add_row "crates/gemini-cli/tests/integration/paths.rs" \
    "integration-test.env_guard" "nils-test-support::EnvGuard/GlobalStateLock" "candidate" "Task 5.1" "medium" \
    'struct EnvVarGuard|fn env_lock|unsafe \{ std::env::(set_var|remove_var)' \
    "Integration tests define custom env guards and raw unsafe env mutation."
  add_row "crates/gemini-cli/tests/integration/prompts.rs" \
    "integration-test.env_guard+fs" "nils-test-support::EnvGuard/GlobalStateLock + fs" "candidate" "Task 5.1" "medium" \
    'struct EnvVarGuard|fn env_lock|set_mode\(|unsafe \{ std::env::(set_var|remove_var)' \
    "Integration tests define custom env guards and manual chmod helper."
  add_row "crates/gemini-cli/tests/integration/agent_prompt.rs" \
    "integration-test.tempdir+fs" "nils-test-support::StubBinDir/fs + tempfile::TempDir" "candidate" "Task 5.1" "medium" \
    'fn temp_dir|fn write_executable\(' \
    "Custom tempdir and executable writer overlap shared helpers."
  add_row "crates/gemini-cli/tests/integration/auth_refresh.rs" \
    "integration-test.path_prepend+fs" "nils-test-support::CmdOptions::with_path_prepend + fs" "candidate" "Task 5.1" "medium" \
    'fn write_curl_stub|fn path_with_stub|set_mode\(|std::env::var\("PATH"\)' \
    "Manual stub writer and PATH prepend helper."

  add_row "crates/codex-cli/tests/integration/agent_commit.rs" \
    "integration-test.git_setup" "nils-test-support::git + fs" "candidate" "Task 5.2" "medium" \
    'Command::new\("git"\)|fn init_repo' \
    "Manual repo init/config/git calls in fallback commit tests."
  add_row "crates/gemini-cli/tests/integration/agent_commit_fallback.rs" \
    "integration-test.git_setup" "nils-test-support::git + fs" "candidate" "Task 5.2" "medium" \
    'Command::new\("git"\)|fn init_repo|fn git_stdout' \
    "Manual repo init/config/git calls in fallback commit tests."

  add_row "crates/git-scope/tests/integration/help_outside_repo.rs" \
    "integration-test.bin_resolve" "nils-test-support::bin + cmd" "candidate" "Task 5.4" "low" \
    'CARGO_BIN_EXE_|fn git_scope_bin' \
    "Manual binary resolution duplicates nils-test-support::bin::resolve."
  add_row "crates/git-scope/tests/integration/edge_cases.rs" \
    "integration-test.allow_fail_runner" "nils-test-support::cmd" "candidate" "Task 5.4" "low" \
    'run_git_scope_allow_fail|std::process::Command::new\(common::git_scope_bin\(\)\)' \
    "Ad-hoc allow-fail command runner duplicates shared cmd helper behavior."
  add_row "crates/git-scope/tests/integration/common.rs" \
    "integration-test.harness_consolidation" "nils-test-support::cmd/bin (thin wrappers only)" "candidate" "Task 5.4" "low" \
    'run_git_scope_output|run_resolved' \
    "Harness should remain thin and own shared wrappers for allow-fail path."

  add_row "crates/api-grpc/tests/integration/integration.rs" \
    "integration-test.stub_executable" "nils-test-support::fs::write_executable" "candidate" "Task 5.5" "low" \
    'fn write_executable_script|set_mode\(0o755\)|set_permissions\(path, perms\)' \
    "Manual script chmod helper overlaps nils-test-support::fs."
  add_row "crates/api-test/tests/integration/grpc_integration.rs" \
    "integration-test.stub_executable" "nils-test-support::fs::write_executable" "candidate" "Task 5.5" "low" \
    'set_mode\(0o755\)|set_permissions\(&mock, perms\)' \
    "Manual grpc mock chmod helper overlaps nils-test-support::fs."
  add_row "crates/api-testing-core/tests/integration/suite_runner_grpc_matrix.rs" \
    "integration-test.stub_executable+env_guard" "nils-test-support::fs + EnvGuard/GlobalStateLock" "candidate" "Task 5.5" "medium" \
    'unsafe \{ std::env::(set_var|remove_var)|set_mode\(0o755\)' \
    "Manual chmod and raw env mutation in test."

  add_row "crates/screen-record/tests/integration/linux_request_permission.rs" \
    "integration-test.stub_executable" "nils-test-support::fs::write_executable" "candidate" "Task 5.6" "low" \
    'ffmpeg_stub_dir|set_mode\(0o755\)|set_permissions\(&ffmpeg_path, perms\)' \
    "Manual ffmpeg stub writer duplicates executable helper."

  add_row "crates/codex-cli/tests/integration/agent_templates.rs" \
    "integration-test.path_prepend" "nils-test-support::CmdOptions::with_path_prepend" "candidate" "Task 5.7" "low" \
    'std::env::var\("PATH"\)|combined_path = format!' \
    "Manual PATH prepend string composition."
  add_row "crates/codex-cli/tests/integration/auth_json_contract.rs" \
    "integration-test.path_prepend" "nils-test-support::CmdOptions::with_path_prepend" "candidate" "Task 5.7" "low" \
    'current_path = std::env::var\("PATH"\)|path = format!\("\{\}:\{current_path\}"' \
    "Manual PATH prepend string composition."
  add_row "crates/gemini-cli/tests/integration/agent_templates.rs" \
    "integration-test.path_prepend" "nils-test-support::CmdOptions::with_path_prepend" "candidate" "Task 5.7" "low" \
    'std::env::var\("PATH"\)|combined_path = format!' \
    "Manual PATH prepend string composition."

  add_row "crates/fzf-cli/tests/integration/open_and_file.rs" \
    "integration-test.path_prepend" "nils-test-support::CmdOptions::with_path_prepend (via harness)" "candidate" "Task 5.7" "low" \
    'fn path_with_stub|std::env::var\("PATH"\)' \
    "Repeated PATH prepend helper."
  add_row "crates/fzf-cli/tests/integration/git_commands.rs" \
    "integration-test.path_prepend" "nils-test-support::CmdOptions::with_path_prepend (via harness)" "candidate" "Task 5.7" "low" \
    'fn path_with_stub|std::env::var\("PATH"\)' \
    "Repeated PATH prepend helper."
  add_row "crates/fzf-cli/tests/integration/git_commit.rs" \
    "integration-test.path_prepend" "nils-test-support::CmdOptions::with_path_prepend (via harness)" "candidate" "Task 5.7" "low" \
    'fn path_with_stub|std::env::var\("PATH"\)' \
    "Repeated PATH prepend helper."
  add_row "crates/fzf-cli/tests/integration/common.rs" \
    "integration-test.harness_consolidation" "nils-test-support::cmd (thin wrappers only)" "candidate" "Task 5.7" "low" \
    'run_fzf_cli|StubBinDir|CmdOptions' \
    "Central harness file likely owns path-prepend helper after sweep."
}

check_seed_paths() {
  local missing
  missing="$(awk -F '\t' 'NR>1 && $9=="missing-file" {print $1}' "$tmp_rows")"
  if [[ -n "$missing" ]]; then
    echo "FAIL: shared-helper adoption audit has missing seeded paths:" >&2
    echo "$missing" >&2
    return 1
  fi
  echo "PASS: shared-helper adoption audit seed paths exist" >&2
}

render_summary_markdown() {
  local manifest="$1"
  local summary="$2"

  local generated_utc
  generated_utc="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

  {
    echo "# Shared Helper Adoption Audit Summary"
    echo
    echo "- Generated: \`$generated_utc\`"
    echo "- Repo root: \`$repo_root\`"
    echo "- Manifest: \`$manifest\`"
    echo
    echo "## Totals"
    echo
    local total_rows
    total_rows="$(awk -F '\t' 'NR>1 {n++} END {print n+0}' "$manifest")"
    local miss_rows
    miss_rows="$(awk -F '\t' 'NR>1 && $8==0 {n++} END {print n+0}' "$manifest")"
    echo "- Rows: $total_rows"
    echo "- Detection misses (\`match_count=0\`): $miss_rows"
    echo
    echo "## By Category"
    echo
    echo "| Category | Count |"
    echo "| --- | ---: |"
    awk -F '\t' 'NR>1 {count[$2]++} END {for (k in count) printf "| %s | %d |\n", k, count[k]}' "$manifest" | sort
    echo
    echo "## By Task"
    echo
    echo "| Task | Count |"
    echo "| --- | ---: |"
    awk -F '\t' 'NR>1 {count[$5]++} END {for (k in count) printf "| %s | %d |\n", k, count[k]}' "$manifest" | sort
    echo
    echo "## High-Risk Candidates"
    echo
    awk -F '\t' 'NR==1 {next} $6=="high" {printf "- `%s` -> %s (%s)\n", $1, $3, $5}' "$manifest"
    echo
    echo "## Detection Misses"
    echo
    if [[ "$miss_rows" == "0" ]]; then
      echo "- none"
    else
      awk -F '\t' 'NR==1 {next} $8==0 {printf "- `%s` (%s, %s)\n", $1, $2, $5}' "$manifest"
    fi
  } >"$summary"
}

write_manifest() {
  local manifest="$1"
  write_header >"$manifest"
  cat "$tmp_rows" >>"$manifest"
}

seed_manifest

if [[ "$check_seeds" -eq 1 ]]; then
  check_seed_paths
fi

if [[ -n "$out_file" ]]; then
  mkdir -p "$(dirname "$out_file")"
  write_manifest "$out_file"
  summary_file="$(dirname "$out_file")/summary.md"
  render_summary_markdown "$out_file" "$summary_file"
  echo "wrote manifest: $out_file"
  echo "wrote summary:  $summary_file"
else
  write_header
  cat "$tmp_rows"
fi
