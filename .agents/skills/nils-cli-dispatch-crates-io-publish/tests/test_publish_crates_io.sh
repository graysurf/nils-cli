#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_root="$(cd "${script_dir}/.." && pwd)"
entrypoint="${skill_root}/scripts/publish-crates-io.sh"

fail() {
  echo "error: $*" >&2
  exit 1
}

assert_contains() {
  local file="$1"
  local pattern="$2"
  if ! rg -q -- "$pattern" "$file"; then
    echo "error: expected pattern '$pattern' in $file" >&2
    sed -n '1,220p' "$file" >&2 || true
    exit 1
  fi
}

create_temp_repo() {
  local repo_dir="$1"
  git init "$repo_dir" >/dev/null
  git -C "$repo_dir" config user.email "test@example.com"
  git -C "$repo_dir" config user.name "Test User"
  echo "seed" > "${repo_dir}/README.md"
  git -C "$repo_dir" add README.md
  git -C "$repo_dir" commit -m "init" >/dev/null
}

create_mock_cargo() {
  local dir="$1"
  cat > "${dir}/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "metadata" ]]; then
  cat <<'JSON'
{
  "packages": [
    {
      "name": "nils-a",
      "version": "1.2.3",
      "publish": null,
      "dependencies": []
    },
    {
      "name": "nils-b",
      "version": "1.2.4",
      "publish": null,
      "dependencies": [
        {"name": "nils-a", "path": "../nils-a"}
      ]
    }
  ]
}
JSON
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 1
MOCK
  chmod +x "${dir}/cargo"
}

create_mock_gh() {
  local dir="$1"
  cat > "${dir}/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

scenario="${MOCK_GH_SCENARIO:-success}"
calls_file="${MOCK_GH_CALLS:-/tmp/mock-gh-calls}"
echo "$*" >> "$calls_file"

if [[ "${1:-}" == "workflow" && "${2:-}" == "run" ]]; then
  exit 0
fi

if [[ "${1:-}" == "run" && "${2:-}" == "list" ]]; then
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  conclusion="success"
  if [[ "$scenario" == "watch-fail" ]]; then
    conclusion="failure"
  fi
  cat <<JSON
[
  {
    "databaseId": 123456,
    "createdAt": "${now}",
    "headBranch": "main",
    "url": "https://github.com/sympoies/nils-cli/actions/runs/123456",
    "status": "completed",
    "conclusion": "${conclusion}"
  }
]
JSON
  exit 0
fi

if [[ "${1:-}" == "run" && "${2:-}" == "watch" ]]; then
  if [[ "$scenario" == "watch-fail" ]]; then
    exit 1
  fi
  exit 0
fi

if [[ "${1:-}" == "run" && "${2:-}" == "view" ]]; then
  conclusion="success"
  if [[ "$scenario" == "watch-fail" ]]; then
    conclusion="failure"
  fi
  cat <<JSON
{
  "url": "https://github.com/sympoies/nils-cli/actions/runs/123456",
  "status": "completed",
  "conclusion": "${conclusion}",
  "createdAt": "2026-02-11T12:00:00Z",
  "updatedAt": "2026-02-11T12:00:30Z"
}
JSON
  exit 0
fi

if [[ "${1:-}" == "auth" && "${2:-}" == "status" ]]; then
  exit 0
fi

echo "unexpected gh command: $*" >&2
exit 1
MOCK
  chmod +x "${dir}/gh"
}

create_mock_agent_out() {
  local dir="$1"
  cat > "${dir}/agent-out" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "project" ]]; then
  out_dir="${MOCK_AGENT_OUT_DIR:?MOCK_AGENT_OUT_DIR is required}"
  mkdir -p "$out_dir"
  printf '%s\n' "$out_dir"
  exit 0
fi

echo "unexpected agent-out command: $*" >&2
exit 1
MOCK
  chmod +x "${dir}/agent-out"
}

create_mock_status_script() {
  local dir="$1"
  cat > "${dir}/crates-io-status.sh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

scenario="${MOCK_STATUS_SCENARIO:-ok}"
calls_file="${MOCK_STATUS_CALLS:-/tmp/mock-status-calls}"
json_out=""
text_out=""

echo "$*" >> "$calls_file"

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --json-out)
      json_out="${2:-}"
      shift 2
      ;;
    --text-out)
      text_out="${2:-}"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

if [[ -n "$json_out" ]]; then
  mkdir -p "$(dirname "$json_out")"
  cat > "$json_out" <<'JSON'
{"summary":{"published":1,"missing":0,"error":0}}
JSON
fi

if [[ -n "$text_out" ]]; then
  mkdir -p "$(dirname "$text_out")"
  cat > "$text_out" <<'TEXT'
# mock status report
TEXT
fi

if [[ "$scenario" == "fail" ]]; then
  exit 1
fi
exit 0
MOCK
  chmod +x "${dir}/crates-io-status.sh"
}

test_publish_wait_success() {
  local tmp
  tmp="$(mktemp -d)"
  local repo="${tmp}/repo"
  local bin_dir="${tmp}/bin"
  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo"
  create_mock_cargo "$bin_dir"
  create_mock_gh "$bin_dir"
  create_mock_status_script "$bin_dir"

  local report="${tmp}/report.md"
  local gh_calls="${tmp}/gh-calls.log"
  local status_calls="${tmp}/status-calls.log"

  PATH="${bin_dir}:$PATH" \
    MOCK_GH_CALLS="$gh_calls" \
    MOCK_STATUS_CALLS="$status_calls" \
    PUBLISH_CRATES_IO_STATUS_SCRIPT="${bin_dir}/crates-io-status.sh" \
    "$entrypoint" --crate nils-a --report-file "$report" \
    >"${tmp}/stdout.log" 2>"${tmp}/stderr.log"

  assert_contains "$gh_calls" "workflow run publish-crates.yml --ref main -f crates=nils-a -f mode=publish"
  assert_contains "$status_calls" "--fail-on-missing"
  assert_contains "$status_calls" "--crate nils-a"
  assert_contains "$report" 'Run conclusion: `success`'
  assert_contains "$report" 'Status snapshot: `ok`'
  assert_contains "$report" "\\| nils-a \\| 1.2.3 \\|"
}

test_publish_wait_failure() {
  local tmp
  tmp="$(mktemp -d)"
  local repo="${tmp}/repo"
  local bin_dir="${tmp}/bin"
  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo"
  create_mock_cargo "$bin_dir"
  create_mock_gh "$bin_dir"

  local report="${tmp}/report-failed.md"
  set +e
  PATH="${bin_dir}:$PATH" \
    MOCK_GH_SCENARIO="watch-fail" \
    "$entrypoint" --crate nils-a --skip-status-check --report-file "$report" \
    >"${tmp}/stdout.log" 2>"${tmp}/stderr.log"
  local rc=$?
  set -e

  [[ "$rc" -eq 1 ]] || fail "expected exit code 1 when workflow run fails, got $rc"
  assert_contains "$report" 'Run conclusion: `failure`'
  assert_contains "$report" 'Status snapshot: `skipped`'
}

test_dry_run_no_wait_dispatches_workflow() {
  local tmp
  tmp="$(mktemp -d)"
  local repo="${tmp}/repo"
  local bin_dir="${tmp}/bin"
  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo"
  create_mock_cargo "$bin_dir"
  create_mock_gh "$bin_dir"

  local report="${tmp}/report-dry-run.md"
  local gh_calls="${tmp}/gh-calls.log"

  PATH="${bin_dir}:$PATH" \
    MOCK_GH_CALLS="$gh_calls" \
    "$entrypoint" --crate nils-a --dry-run-only --no-wait --report-file "$report" \
    >"${tmp}/stdout.log" 2>"${tmp}/stderr.log"

  assert_contains "$gh_calls" "workflow run publish-crates.yml --ref main -f crates=nils-a -f mode=dry-run"
  assert_contains "$report" 'Mode: `dry-run`'
  assert_contains "$report" 'Status snapshot: `skipped`'
}

test_default_report_file_uses_agent_out_project_dir() {
  local tmp
  tmp="$(mktemp -d)"
  local repo="${tmp}/repo"
  local bin_dir="${tmp}/bin"
  local report_dir="${tmp}/agent-out-run"
  mkdir -p "$repo" "$bin_dir"
  create_temp_repo "$repo"
  create_mock_cargo "$bin_dir"
  create_mock_gh "$bin_dir"
  create_mock_agent_out "$bin_dir"

  PATH="${bin_dir}:$PATH" \
    MOCK_AGENT_OUT_DIR="$report_dir" \
    "$entrypoint" --crate nils-a --dry-run-only --no-wait \
    >"${tmp}/stdout.log" 2>"${tmp}/stderr.log"

  local report
  report="$(find "$report_dir" -type f -name 'crates-io-publish-report-*.md' -print -quit)"
  [[ -n "$report" ]] || fail "expected default report under agent-out project dir"
  assert_contains "$report" 'Mode: `dry-run`'
  assert_contains "$report" 'Status snapshot: `skipped`'
}

if [[ ! -f "${skill_root}/SKILL.md" ]]; then
  echo "error: missing SKILL.md" >&2
  exit 1
fi
if [[ ! -f "$entrypoint" ]]; then
  echo "error: missing scripts/publish-crates-io.sh" >&2
  exit 1
fi
if [[ ! -f "${skill_root}/references/PUBLISH_REPORT_TEMPLATE.md" ]]; then
  echo "error: missing references/PUBLISH_REPORT_TEMPLATE.md" >&2
  exit 1
fi

test_publish_wait_success
test_publish_wait_failure
test_dry_run_no_wait_dispatches_workflow
test_default_report_file_uses_agent_out_project_dir

echo "ok: project skill smoke checks passed"
