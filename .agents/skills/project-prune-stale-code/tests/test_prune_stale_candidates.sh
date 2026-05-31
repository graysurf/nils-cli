#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill_dir="$(cd "${script_dir}/.." && pwd)"
script="${skill_dir}/scripts/project-prune-stale-code.sh"

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
    fail "expected output NOT to contain: $needle"
  fi
}

artifact_root="${CLAUDE_KIT_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/agent-runtime-kit}/out/project-prune-stale-code-tests"
mkdir -p "$artifact_root"
tmp="$(mktemp -d "${artifact_root}/prune-stale.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# Synthetic crate tree: one production suppression, one test suppression, one
# deny_unknown_fields struct, and one fully clean file.
mkdir -p "$tmp/crates/foo/src" "$tmp/crates/foo/tests/integration" \
         "$tmp/crates/bar/src" "$tmp/crates/baz/src"

cat >"$tmp/crates/foo/src/lib.rs" <<'EOF'
#[allow(dead_code)]
fn maybe_unused() {}
EOF

cat >"$tmp/crates/foo/tests/integration/common.rs" <<'EOF'
#![allow(dead_code)]
pub fn helper() {}
EOF

cat >"$tmp/crates/bar/src/model.rs" <<'EOF'
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: Option<String>,
}
EOF

cat >"$tmp/crates/baz/src/clean.rs" <<'EOF'
pub fn live() -> u32 { 1 }
EOF

report="$("$script" --root "$tmp" --no-machete)"

# Section 2a: production suppression surfaced with the repo-relative path.
assert_contains "$report" "In production source (highest signal): 1"
assert_contains "$report" "crates/foo/src/lib.rs:1:#[allow(dead_code)]"

# Section 2b: test suppression routed to the tests bucket, not production.
assert_contains "$report" "crates/foo/tests/integration/common.rs:1:#![allow(dead_code)]"

# Section 3: the deny_unknown_fields struct is flagged as a removal trap.
assert_contains "$report" "deny_unknown_fields"
assert_contains "$report" "crates/bar/src/model.rs"

# The clean file must never appear as a candidate.
assert_not_contains "$report" "crates/baz/src/clean.rs"

# machete is opt-out and the report still renders its section header.
assert_contains "$report" "## 1. Unused direct dependencies"
assert_contains "$report" "Skipped (\`--no-machete\`)"

# --help exits 0.
set +e
"$script" --help >/dev/null 2>&1
help_rc=$?
set -e
[[ "$help_rc" -eq 0 ]] || fail "expected --help exit 0, got $help_rc"

# Unknown argument is a usage error (exit 2).
set +e
"$script" --bogus >/dev/null 2>&1
bad_rc=$?
set -e
[[ "$bad_rc" -eq 2 ]] || fail "expected unknown-arg exit 2, got $bad_rc"

# Missing root directory is a precondition error (exit 2).
set +e
"$script" --root "${tmp}/does-not-exist" >/dev/null 2>&1
missing_rc=$?
set -e
[[ "$missing_rc" -eq 2 ]] || fail "expected missing-root exit 2, got $missing_rc"

echo "ok: prune-stale-code candidate-scanner tests passed"
