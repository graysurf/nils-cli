#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  <ENTRYPOINT> [options]

Options:
  --crate NAME                  Add one crate (repeatable).
  --crates "A B,C"              Add multiple crates (space/comma separated).
  --all                         Publish all crates from the list file.
  --list-file PATH              Crate order file for --all/default.
  --publish                     Trigger workflow in publish mode (default).
  --dry-run-only                Trigger workflow in dry-run mode.
  --ref REF                     Git ref for workflow dispatch (default: main).
  --workflow NAME               Workflow file/name for dispatch (default: publish-crates.yml).
  --registry NAME               Optional cargo registry input for workflow.
  --wait                        Wait for workflow completion (default).
  --no-wait                     Only dispatch workflow; do not wait.
  --discover-timeout-seconds N  Max seconds to discover run id after dispatch (default: 120).
  --poll-seconds N              Poll interval for run discovery (default: 3).
  --skip-status-check           Skip post-run crates.io status snapshot.
  --status-script PATH          Override status snapshot script path.
  --status-json-file PATH       Override status snapshot JSON output path.
  --status-text-file PATH       Override status snapshot text output path.
  --report-file PATH            Markdown report output path.
  -h, --help                    Show help.

Examples:
  <ENTRYPOINT> --crate nils-codex-cli
  <ENTRYPOINT> --crate nils-codex-cli --dry-run-only
  <ENTRYPOINT> --all --ref main
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

note() {
  echo "info: $*" >&2
}

warn() {
  echo "warning: $*" >&2
}

contains() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    if [[ "$item" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

append_crates_from_words() {
  local raw="$1"
  local item
  raw="${raw//,/ }"
  for item in $raw; do
    [[ -n "$item" ]] || continue
    selected_crates+=("$item")
  done
}

append_crates_from_file() {
  local path="$1"
  [[ -f "$path" ]] || die "crate list file not found: $path"
  local line trimmed
  while IFS= read -r line || [[ -n "$line" ]]; do
    trimmed="$(printf '%s' "$line" | sed -E 's/[[:space:]]*#.*$//; s/^[[:space:]]+//; s/[[:space:]]+$//')"
    [[ -n "$trimmed" ]] || continue
    selected_crates+=("$trimmed")
  done < "$path"
}

now_utc() {
  date -u +"%Y-%m-%dT%H:%M:%SZ"
}

selected_crate_version() {
  local metadata_path="$1"
  local crate="$2"
  "$python_bin" - "$metadata_path" "$crate" <<'PY'
from __future__ import annotations

import json
import sys

metadata_path, crate = sys.argv[1], sys.argv[2]
with open(metadata_path, "r", encoding="utf-8") as fp:
    metadata = json.load(fp)
for pkg in metadata["packages"]:
    if pkg.get("name") == crate:
        print(pkg["version"])
        raise SystemExit(0)
raise SystemExit(1)
PY
}

discover_run_id() {
  local since_epoch="$1"
  local timeout_seconds="$2"
  local poll_seconds="$3"
  local known_run_ids_json="$4"
  local elapsed=0

  while (( elapsed <= timeout_seconds )); do
    local run_list_json
    if ! run_list_json="$("$gh_bin" run list --workflow "$workflow" --event workflow_dispatch --limit 30 --json databaseId,createdAt,headBranch,url,status,conclusion 2>/dev/null)"; then
      run_list_json="[]"
    fi

    local run_id
    run_id="$("$python_bin" - "$since_epoch" "$ref" "$known_run_ids_json" "$run_list_json" <<'PY'
from __future__ import annotations

from datetime import datetime
import json
import sys

since_epoch = int(sys.argv[1])
target_ref = sys.argv[2]
known_raw = sys.argv[3] if len(sys.argv) > 3 else "[]"
raw = sys.argv[4] if len(sys.argv) > 4 else "[]"

try:
    known_ids = set()
    for value in json.loads(known_raw):
        if isinstance(value, dict):
            run_id = value.get("databaseId")
        else:
            run_id = value
        if run_id is not None:
            known_ids.add(str(run_id))
except Exception:
    known_ids = set()

try:
    runs = json.loads(raw)
except Exception:
    runs = []

def to_epoch(value: str) -> int:
    if not value:
        return 0
    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except Exception:
        return 0
    return int(dt.timestamp())

threshold = max(0, since_epoch - 10)

def is_new_candidate(run) -> bool:
    run_id = run.get("databaseId")
    if run_id is None or str(run_id) in known_ids:
        return False
    return to_epoch(run.get("createdAt", "")) >= threshold

def select(candidates):
    if not candidates:
        return None
    return max(candidates, key=lambda run: to_epoch(run.get("createdAt", "")))

filtered = [
    run
    for run in runs
    if is_new_candidate(run) and run.get("headBranch") == target_ref
]
picked = select(filtered)
if picked is None:
    fallback = [run for run in runs if is_new_candidate(run)]
    picked = select(fallback)

if picked is None:
    raise SystemExit(1)

run_id = picked.get("databaseId")
if run_id is None:
    raise SystemExit(1)
print(run_id)
PY
)" || run_id=""

    if [[ -n "$run_id" ]]; then
      printf '%s\n' "$run_id"
      return 0
    fi

    "$sleep_bin" "$poll_seconds"
    elapsed=$((elapsed + poll_seconds))
  done

  return 1
}

write_report() {
  local path="$1"
  local mode_label="$2"
  local started="$3"
  local ended="$4"
  local run_id="$5"
  local run_url="$6"
  local run_status="$7"
  local run_conclusion="$8"
  local status_check_state="$9"
  local status_check_rc="${10}"
  local status_json_file="${11}"
  local status_text_file="${12}"
  "$python_bin" - "$rows_file" "$path" "$mode_label" "$workflow" "$ref" "$started" "$ended" "$run_id" "$run_url" "$run_status" "$run_conclusion" "$status_check_state" "$status_check_rc" "$status_json_file" "$status_text_file" <<'PY'
from __future__ import annotations

import csv
import pathlib
import sys

(
    rows_file,
    out_path,
    mode_label,
    workflow_name,
    git_ref,
    started,
    ended,
    run_id,
    run_url,
    run_status,
    run_conclusion,
    status_check_state,
    status_check_rc,
    status_json_file,
    status_text_file,
) = sys.argv[1:16]

rows = []
with open(rows_file, "r", encoding="utf-8") as fp:
    reader = csv.DictReader(fp, fieldnames=["crate", "version"], delimiter="\t")
    for row in reader:
        rows.append(row)

content = []
content.append("# crates.io Publish Report")
content.append("")
content.append("## Summary")
content.append("")
content.append(f"- Mode: `{mode_label}`")
content.append(f"- Workflow: `{workflow_name}`")
content.append(f"- Ref: `{git_ref}`")
content.append(f"- Started (UTC): `{started}`")
content.append(f"- Ended (UTC): `{ended}`")
content.append(f"- Selected crates: `{len(rows)}`")
content.append(f"- Run ID: `{run_id}`")
content.append(f"- Run URL: `{run_url or '-'}`")
content.append(f"- Run status: `{run_status or '-'}`")
content.append(f"- Run conclusion: `{run_conclusion or '-'}`")
content.append(f"- Status snapshot: `{status_check_state}`")
if status_check_rc:
    content.append(f"- Status snapshot exit code: `{status_check_rc}`")
if status_json_file:
    content.append(f"- Status JSON: `{status_json_file}`")
if status_text_file:
    content.append(f"- Status text: `{status_text_file}`")
content.append("")
content.append("## Selected Crates")
content.append("")
if rows:
    content.append("| Crate | Version |")
    content.append("|---|---:|")
    for row in rows:
        content.append(f"| {row['crate']} | {row['version']} |")
else:
    content.append("_None_")

path = pathlib.Path(out_path)
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text("\n".join(content) + "\n", encoding="utf-8")
PY
}

gh_bin="${PUBLISH_CRATES_IO_GH_BIN:-gh}"
cargo_bin="${PUBLISH_CRATES_IO_CARGO_BIN:-cargo}"
python_bin="${PUBLISH_CRATES_IO_PYTHON_BIN:-python3}"
sleep_bin="${PUBLISH_CRATES_IO_SLEEP_BIN:-sleep}"

mode="publish"
ref="main"
workflow="publish-crates.yml"
registry=""
wait_for_completion=1
discover_timeout_seconds=120
poll_seconds=3
list_file="release/crates-io-publish-order.txt"
select_all=0
report_file=""
skip_status_check=0
status_script="${PUBLISH_CRATES_IO_STATUS_SCRIPT:-}"
status_json_file=""
status_text_file=""
declare -a selected_crates=()

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --crate)
      [[ $# -ge 2 ]] || die "--crate requires a value"
      selected_crates+=("${2:-}")
      shift 2
      ;;
    --crates)
      [[ $# -ge 2 ]] || die "--crates requires a value"
      append_crates_from_words "${2:-}"
      shift 2
      ;;
    --all)
      select_all=1
      shift
      ;;
    --list-file)
      [[ $# -ge 2 ]] || die "--list-file requires a value"
      list_file="${2:-}"
      shift 2
      ;;
    --publish)
      mode="publish"
      shift
      ;;
    --dry-run-only)
      mode="dry-run-only"
      shift
      ;;
    --ref)
      [[ $# -ge 2 ]] || die "--ref requires a value"
      ref="${2:-}"
      shift 2
      ;;
    --workflow)
      [[ $# -ge 2 ]] || die "--workflow requires a value"
      workflow="${2:-}"
      shift 2
      ;;
    --registry)
      [[ $# -ge 2 ]] || die "--registry requires a value"
      registry="${2:-}"
      shift 2
      ;;
    --wait)
      wait_for_completion=1
      shift
      ;;
    --no-wait)
      wait_for_completion=0
      shift
      ;;
    --discover-timeout-seconds)
      [[ $# -ge 2 ]] || die "--discover-timeout-seconds requires a value"
      discover_timeout_seconds="${2:-}"
      shift 2
      ;;
    --poll-seconds)
      [[ $# -ge 2 ]] || die "--poll-seconds requires a value"
      poll_seconds="${2:-}"
      shift 2
      ;;
    --skip-status-check)
      skip_status_check=1
      shift
      ;;
    --status-script)
      [[ $# -ge 2 ]] || die "--status-script requires a value"
      status_script="${2:-}"
      shift 2
      ;;
    --status-json-file)
      [[ $# -ge 2 ]] || die "--status-json-file requires a value"
      status_json_file="${2:-}"
      shift 2
      ;;
    --status-text-file)
      [[ $# -ge 2 ]] || die "--status-text-file requires a value"
      status_text_file="${2:-}"
      shift 2
      ;;
    --report-file)
      [[ $# -ge 2 ]] || die "--report-file requires a value"
      report_file="${2:-}"
      shift 2
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

[[ "$discover_timeout_seconds" =~ ^[0-9]+$ ]] || die "--discover-timeout-seconds must be an integer >= 0"
[[ "$poll_seconds" =~ ^[0-9]+$ ]] || die "--poll-seconds must be an integer >= 0"
(( poll_seconds > 0 )) || die "--poll-seconds must be > 0"

command -v "$gh_bin" >/dev/null 2>&1 || die "gh not found on PATH"
command -v "$cargo_bin" >/dev/null 2>&1 || die "cargo not found on PATH"
command -v "$python_bin" >/dev/null 2>&1 || die "python3 not found on PATH"

if [[ "$select_all" -eq 1 && ${#selected_crates[@]} -gt 0 ]]; then
  die "--all cannot be combined with --crate/--crates"
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$repo_root" ]] || die "must run inside a git work tree"
cd "$repo_root"

if [[ "$select_all" -eq 1 || ${#selected_crates[@]} -eq 0 ]]; then
  append_crates_from_file "$list_file"
fi

declare -a deduped_crates=()
for crate in "${selected_crates[@]}"; do
  [[ "$crate" =~ ^[A-Za-z0-9_-]+$ ]] || die "invalid crate name: '$crate'"
  if ! contains "$crate" "${deduped_crates[@]}"; then
    deduped_crates+=("$crate")
  fi
done
selected_crates=("${deduped_crates[@]}")
[[ ${#selected_crates[@]} -gt 0 ]] || die "no crates selected"

metadata_file="$(mktemp)"
rows_file="$(mktemp)"
cleanup() {
  rm -f "$metadata_file" "$rows_file"
}
trap cleanup EXIT

"$cargo_bin" metadata --format-version 1 --no-deps > "$metadata_file"

"$python_bin" - "$metadata_file" "${selected_crates[@]}" <<'PY'
from __future__ import annotations

import json
import sys

metadata_path = sys.argv[1]
selected = sys.argv[2:]
with open(metadata_path, "r", encoding="utf-8") as fp:
    metadata = json.load(fp)
packages = {pkg["name"]: pkg for pkg in metadata["packages"]}

errors: list[str] = []
order = {name: idx for idx, name in enumerate(selected)}

for name in selected:
    pkg = packages.get(name)
    if pkg is None:
        errors.append(f"selected crate '{name}' is not in this workspace")
        continue
    if pkg.get("publish") == []:
        errors.append(f"selected crate '{name}' has publish=false")

for name in selected:
    pkg = packages.get(name)
    if pkg is None:
        continue
    for dep in pkg.get("dependencies", []):
        dep_name = dep.get("name")
        if dep.get("path") and packages.get(dep_name, {}).get("publish") == []:
            errors.append(
                f"selected crate '{name}' depends on workspace crate '{dep_name}' "
                "which has publish=false"
            )
        if dep.get("path") and dep_name in order and order[dep_name] > order[name]:
            errors.append(
                f"publish order invalid: '{name}' depends on '{dep_name}', "
                "so dependency must appear earlier in the crate list"
            )

if errors:
    for err in errors:
        print(f"error: {err}", file=sys.stderr)
    raise SystemExit(1)
PY

if [[ -z "$report_file" ]]; then
  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  report_dir=""
  if command -v agent-out >/dev/null 2>&1; then
    report_dir="$(agent-out project --topic crates-io-publish --mkdir 2>/dev/null || true)"
  fi
  if [[ -z "$report_dir" ]]; then
    state_home="${CLAUDE_KIT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit}"
    report_dir="${state_home}/out/crates-io-publish"
    mkdir -p "$report_dir"
  fi
  report_file="${report_dir}/crates-io-publish-report-${timestamp}.md"
fi
mkdir -p "$(dirname "$report_file")"

if [[ -z "$status_script" ]]; then
  status_script="${repo_root}/scripts/crates-io-status.sh"
fi
if [[ -z "$status_json_file" ]]; then
  status_json_file="${report_file%.md}.status.json"
fi
if [[ -z "$status_text_file" ]]; then
  status_text_file="${report_file%.md}.status.md"
fi
mkdir -p "$(dirname "$status_json_file")" "$(dirname "$status_text_file")"

for crate in "${selected_crates[@]}"; do
  version="$(selected_crate_version "$metadata_file" "$crate" || true)"
  [[ -n "$version" ]] || version="unknown"
  printf '%s\t%s\n' "$crate" "$version" >> "$rows_file"
done

mode_input="publish"
mode_label="publish"
if [[ "$mode" == "dry-run-only" ]]; then
  mode_input="dry-run"
  mode_label="dry-run"
fi

note "mode: $mode_label"
note "workflow: $workflow"
note "ref: $ref"
note "crates: ${selected_crates[*]}"
note "wait: $wait_for_completion"
note "report: $report_file"

run_started_at="$(now_utc)"
dispatch_start_epoch="$(date +%s)"
known_run_ids_json="$("$gh_bin" run list --workflow "$workflow" --event workflow_dispatch --limit 50 --json databaseId 2>/dev/null || printf '[]')"

declare -a dispatch_cmd=(
  "$gh_bin" workflow run "$workflow"
  --ref "$ref"
  -f "crates=${selected_crates[*]}"
  -f "mode=${mode_input}"
)
if [[ -n "$registry" ]]; then
  dispatch_cmd+=(-f "registry=${registry}")
fi

"${dispatch_cmd[@]}"
note "workflow dispatched"

run_id="$(discover_run_id "$dispatch_start_epoch" "$discover_timeout_seconds" "$poll_seconds" "$known_run_ids_json" || true)"
[[ -n "$run_id" ]] || die "failed to locate dispatched workflow run for '$workflow' within ${discover_timeout_seconds}s"

run_url=""
run_status="queued"
run_conclusion=""
watch_rc=0

if [[ "$wait_for_completion" -eq 1 ]]; then
  note "watching run: $run_id"
  set +e
  "$gh_bin" run watch "$run_id" --exit-status
  watch_rc="$?"
  set -e
fi

run_view_json="$("$gh_bin" run view "$run_id" --json url,status,conclusion,createdAt,updatedAt 2>/dev/null || true)"
mapfile -t run_view_fields < <("$python_bin" - "$run_view_json" <<'PY'
from __future__ import annotations

import json
import sys

raw = sys.argv[1] if len(sys.argv) > 1 else "{}"
try:
    data = json.loads(raw)
except Exception:
    data = {}

def out(key: str) -> str:
    value = data.get(key)
    if value is None:
        return ""
    return str(value)

print(out("url"))
print(out("status"))
print(out("conclusion"))
print(out("createdAt"))
print(out("updatedAt"))
PY
)
run_url="${run_view_fields[0]:-}"
run_status="${run_view_fields[1]:-$run_status}"
run_conclusion="${run_view_fields[2]:-$run_conclusion}"

if [[ "$wait_for_completion" -eq 1 && "$watch_rc" -ne 0 ]]; then
  warn "workflow run failed (run_id=${run_id})"
fi

status_check_state="skipped"
status_check_rc=""
if [[ "$skip_status_check" -eq 0 && "$mode" == "publish" && "$wait_for_completion" -eq 1 && "$run_conclusion" == "success" ]]; then
  if [[ -x "$status_script" ]]; then
    declare -a status_cmd=("$status_script" --format both --json-out "$status_json_file" --text-out "$status_text_file" --fail-on-missing)
    for crate in "${selected_crates[@]}"; do
      status_cmd+=(--crate "$crate")
    done

    note "running status snapshot"
    set +e
    "${status_cmd[@]}"
    status_check_rc="$?"
    set -e
    if [[ "$status_check_rc" == "0" ]]; then
      status_check_state="ok"
    else
      status_check_state="failed"
      warn "status snapshot failed with exit code ${status_check_rc}"
    fi
  else
    warn "status snapshot script missing or not executable: $status_script"
    status_check_state="skipped"
  fi
fi

run_ended_at="$(now_utc)"
write_report \
  "$report_file" \
  "$mode_label" \
  "$run_started_at" \
  "$run_ended_at" \
  "$run_id" \
  "$run_url" \
  "$run_status" \
  "$run_conclusion" \
  "$status_check_state" \
  "$status_check_rc" \
  "$status_json_file" \
  "$status_text_file"

note "report written: $report_file"
if [[ -n "$run_url" ]]; then
  note "run url: $run_url"
fi

overall_failed=0
if [[ "$wait_for_completion" -eq 1 ]]; then
  if [[ "$watch_rc" -ne 0 || "$run_conclusion" != "success" ]]; then
    overall_failed=1
  fi
fi
if [[ "$status_check_state" == "failed" ]]; then
  overall_failed=1
fi

if [[ "$overall_failed" -ne 0 ]]; then
  exit 1
fi

exit 0
