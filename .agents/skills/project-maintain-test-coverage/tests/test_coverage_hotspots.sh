#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_dir="$(cd "${script_dir}/.." && pwd)"
repo_root="$(git -C "$skill_dir" rev-parse --show-toplevel)"
script="${skill_dir}/scripts/coverage-hotspots.sh"

fail() {
  echo "error: $*" >&2
  exit 1
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "$haystack" >&2
    fail "expected output to contain: $needle"
  fi
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "$haystack" >&2
    fail "expected output not to contain: $needle"
  fi
}

artifact_root="${CLAUDE_KIT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit}/out/project-maintain-test-coverage-tests"
mkdir -p "$artifact_root"
tmp="$(mktemp -d "${artifact_root}/coverage-hotspots.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

lcov="${tmp}/lcov.info"
cat >"$lcov" <<EOF
TN:
SF:${repo_root}/crates/nils-medium/src/lib.rs
DA:1,1
DA:2,0
DA:3,1
LF:3
LH:2
end_of_record
SF:${repo_root}/crates/nils-low/src/lib.rs
DA:1,0
DA:2,0
LF:2
LH:0
end_of_record
SF:${repo_root}/crates/nils-full/src/lib.rs
DA:1,1
DA:2,1
LF:2
LH:2
end_of_record
SF:${repo_root}/scripts/helper.sh
DA:1,0
LF:1
LH:0
end_of_record
EOF

default_output="$("$script" --lcov "$lcov" --limit 20)"
assert_contains "$default_output" "| \`crates/nils-low/src/lib.rs\` | 0.00% | 0 | 2 | 2 |"
assert_contains "$default_output" "| \`crates/nils-medium/src/lib.rs\` | 66.67% | 2 | 3 | 1 |"
assert_not_contains "$default_output" "crates/nils-full/src/lib.rs"
assert_not_contains "$default_output" "scripts/helper.sh"

limited_output="$("$script" --lcov "$lcov" --limit 1)"
assert_contains "$limited_output" "crates/nils-low/src/lib.rs"
assert_not_contains "$limited_output" "crates/nils-medium/src/lib.rs"

all_output="$("$script" --lcov "$lcov" --all --limit 10)"
assert_contains "$all_output" "scripts/helper.sh"

set +e
"$script" --lcov "${tmp}/missing.info" >"${tmp}/missing.stdout" 2>"${tmp}/missing.stderr"
rc=$?
set -e
if [[ "$rc" -ne 2 ]]; then
  fail "expected missing LCOV exit code 2, got $rc"
fi
assert_contains "$(cat "${tmp}/missing.stderr")" "missing LCOV file"

echo "ok: coverage-hotspots tests passed"
