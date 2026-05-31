#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_dir="$(cd "${script_dir}/.." && pwd)"
script="${skill_dir}/scripts/render-refactor-response-template.sh"

assert_contains() {
  local haystack="${1:-}"
  local needle="${2:-}"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "expected output to contain: $needle" >&2
    exit 1
  fi
}

run_mode() {
  local mode="${1:-}"
  "$script" --mode "$mode"
}

impl_output="$(run_mode implement)"
assert_contains "$impl_output" "## Decision"
assert_contains "$impl_output" "- Implement"

no_action_output="$(run_mode no-action)"
assert_contains "$no_action_output" "## Decision"
assert_contains "$no_action_output" "- No Action"

both_output="$(run_mode both)"
assert_contains "$both_output" "## Changes Implemented"
assert_contains "$both_output" "## Recommendations (Actionable)"

set +e
"$script" --mode invalid >/dev/null 2>&1
rc=$?
set -e
if [[ "$rc" -ne 2 ]]; then
  echo "expected invalid mode exit code 2, got $rc" >&2
  exit 1
fi

echo "ok: render-refactor-response-template tests passed"
