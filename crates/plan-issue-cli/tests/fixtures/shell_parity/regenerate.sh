#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${script_dir}/../../../../.." && pwd -P)"
plan_fixture_rel="crates/plan-tooling/tests/fixtures/split_prs/duck-plan.md"

if [[ -z "${PLAN_ISSUE_HOME:-}" ]]; then
  echo "error: PLAN_ISSUE_HOME is required" >&2
  exit 1
fi

cd "$repo_root"

normalize_paths() {
  sed \
    -e "s|${PLAN_ISSUE_HOME%/}|\$PLAN_ISSUE_HOME|g"
}

run_plan_issue_local() {
  cargo run -q -p nils-plan-issue-cli --bin plan-issue-local -- "$@"
}

run_plan_issue_local --format json multi-sprint-guide \
  --plan "$plan_fixture_rel" \
  --dry-run \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["payload"]["result"]["guide"])' \
  | normalize_paths >"$script_dir/multi_sprint_guide_dry_run.txt"

start_json="$(run_plan_issue_local --format json start-sprint \
  --plan "$plan_fixture_rel" \
  --issue 999 \
  --sprint 1 \
  --pr-grouping group \
  --pr-group S1T1=s1-foundation \
  --pr-group S1T2=s1-fixtures \
  --pr-group S1T3=s1-fixtures \
  --no-comment \
  --dry-run)"

comment_path="$(
  printf '%s' "$start_json" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["payload"]["result"]["comment_path"])'
)"

awk '
      /^## Sprint 1 Start$/ {capture=1}
      capture {
        if ($0 == "SPRINT_COMMENT_POSTED=0") exit
        print
      }
    ' "$comment_path" >"$script_dir/comment_template_start.md"

echo "updated shell parity fixtures in $script_dir"
