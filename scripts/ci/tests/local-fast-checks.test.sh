#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/nils-cli-local-fast.sh plan detection.
#
# The local fast gate is intentionally conservative: package-scoped validation
# is only used for non-shared crate changes, while shared crates and
# workspace-level files escalate to the workspace Rust gate.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

script="$repo_root/scripts/ci/nils-cli-local-fast.sh"
if [[ ! -f "$script" ]]; then
  echo "error: missing local fast script: $script" >&2
  exit 2
fi

plan_for() {
  bash "$script" --plan-only "$@"
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if ! grep -qF "$needle" <<<"$haystack"; then
    echo "FAIL: $label"
    echo "missing: $needle"
    echo "$haystack"
    exit 1
  fi
}

assert_package_crate_uses_package_mode() {
  echo "== package crate uses package mode =="
  local output
  output="$(plan_for --changed-file crates/plan-tooling/src/validate.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-plan-tooling"
  echo "ok"
}

assert_shared_crate_escalates_to_workspace() {
  echo "== shared crate escalates to workspace =="
  local output
  output="$(plan_for --changed-file crates/nils-common/src/lib.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=workspace"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_REASON=shared package changed: nils-common"
  echo "ok"
}

assert_docs_only_uses_docs_mode() {
  echo "== docs-only changes use docs mode =="
  local output
  output="$(plan_for --changed-file docs/runbooks/example.md)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=docs-only"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_CHECKS=1"
  echo "ok"
}

assert_workspace_manifest_escalates_to_workspace() {
  echo "== workspace manifest escalates to workspace =="
  local output
  output="$(plan_for --changed-file Cargo.toml)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=workspace"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_REASON=workspace manifest changed: Cargo.toml"
  echo "ok"
}

assert_docs_plus_package_runs_both() {
  echo "== docs plus package requests docs and package checks =="
  local output
  output="$(plan_for \
    --changed-file docs/runbooks/example.md \
    --changed-file crates/plan-tooling/src/validate.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_CHECKS=1"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-plan-tooling"
  echo "ok"
}

assert_shell_script_is_reported() {
  echo "== shell script changes are reported for syntax checks =="
  local output
  output="$(plan_for --changed-file scripts/ci/nils-cli-local-fast.sh)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=workspace"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_SHELL=scripts/ci/nils-cli-local-fast.sh"
  echo "ok"
}

assert_package_crate_uses_package_mode
assert_shared_crate_escalates_to_workspace
assert_docs_only_uses_docs_mode
assert_workspace_manifest_escalates_to_workspace
assert_docs_plus_package_runs_both
assert_shell_script_is_reported

echo
echo "PASS: local-fast-checks.test.sh"
