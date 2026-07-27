#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/tempdir-leak-audit.sh.
#
# Guards the three behaviours the audit is worth having:
#   (a) the real repo is clean
#   (b) each leak pattern is detected in a fixture tree
#   (c) the `tempdir-leak-audit: allow` escape hatch suppresses a finding, on
#       both the offending line and the line above it

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

audit_script="$repo_root/scripts/ci/tempdir-leak-audit.sh"
if [[ ! -f "$audit_script" ]]; then
  echo "error: missing audit script: $audit_script" >&2
  exit 2
fi

fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/tempdir-leak-audit-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/crates"

write_fixture() {
  printf '%s\n' "$1" >"$fixture_root/crates/fixture.rs"
}

expect_status() {
  local expected="$1"
  local label="$2"
  local status=0
  bash "$audit_script" --root "$fixture_root" >/dev/null 2>&1 || status=$?
  if [[ "$status" -ne "$expected" ]]; then
    echo "FAIL: $label (expected exit $expected, got $status)"
    bash "$audit_script" --root "$fixture_root" || true
    exit 1
  fi
  echo "ok: $label"
}

assert_real_repo_is_clean() {
  echo "== baseline: the real repo has no unannotated leak patterns =="
  if ! bash "$audit_script" >/dev/null; then
    echo "FAIL: real repo trips tempdir-leak-audit"
    bash "$audit_script" || true
    exit 1
  fi
  echo "ok"
}

assert_patterns_are_detected() {
  echo "== detection: each leak pattern is rejected =="

  write_fixture 'static CACHE: OnceLock<TempDir> = OnceLock::new();'
  expect_status 1 "static TempDir is rejected"

  write_fixture 'let path = dir.keep().join("db");'
  expect_status 1 "keep() is rejected"

  write_fixture 'let path = dir.into_path();'
  expect_status 1 "into_path() is rejected"

  write_fixture 'std::mem::forget(dir);'
  expect_status 1 "mem::forget is rejected"

  write_fixture 'let dir = TempDir::new().expect("tempdir");'
  expect_status 0 "an ordinary TempDir binding is accepted"
}

assert_allow_marker_suppresses() {
  echo "== escape hatch: the allow marker suppresses a finding =="

  write_fixture 'let path = dir.keep().join("db"); // tempdir-leak-audit: allow — fixture'
  expect_status 0 "allow marker on the offending line"

  printf '%s\n%s\n' \
    '// tempdir-leak-audit: allow — fixture' \
    'let path = dir.keep().join("db");' >"$fixture_root/crates/fixture.rs"
  expect_status 0 "allow marker on the preceding line"

  printf '%s\n%s\n%s\n' \
    '// tempdir-leak-audit: allow — fixture' \
    '' \
    'let path = dir.keep().join("db");' >"$fixture_root/crates/fixture.rs"
  expect_status 1 "allow marker two lines above does not suppress"
}

assert_usage_errors() {
  echo "== usage =="
  local status=0
  bash "$audit_script" --nope >/dev/null 2>&1 || status=$?
  if [[ "$status" -ne 2 ]]; then
    echo "FAIL: unexpected argument should exit 2, got $status"
    exit 1
  fi
  status=0
  bash "$audit_script" --root >/dev/null 2>&1 || status=$?
  if [[ "$status" -ne 2 ]]; then
    echo "FAIL: --root without a value should exit 2, got $status"
    exit 1
  fi
  echo "ok"
}

assert_real_repo_is_clean
assert_patterns_are_detected
assert_allow_marker_suppresses
assert_usage_errors

echo "tempdir-leak-audit.test.sh: all checks passed"
