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

assert_not_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if grep -qF "$needle" <<<"$haystack"; then
    echo "FAIL: $label"
    echo "unexpected: $needle"
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

assert_bin_only_package_skips_doctests() {
  echo "== bin-only package skips doctests =="
  local output
  output="$(plan_for --changed-file crates/api-grpc/src/commands/call.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-api-grpc"
  assert_not_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE_DOCTEST=nils-api-grpc"
  echo "ok"
}

assert_library_package_keeps_doctests() {
  echo "== library package keeps doctests =="
  local output
  output="$(plan_for --changed-file crates/api-testing-core/src/auth_env.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-api-testing-core"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE_DOCTEST=nils-api-testing-core"
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

assert_shared_scrub_crate_escalates_to_workspace() {
  echo "== shared nils-scrub crate escalates to workspace =="
  local output
  output="$(plan_for --changed-file crates/nils-scrub/src/lib.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=workspace"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_REASON=shared package changed: nils-scrub"
  assert_not_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-scrub"
  echo "ok"
}

assert_docs_only_uses_docs_mode() {
  echo "== docs-only changes use docs mode =="
  local output
  output="$(plan_for --changed-file docs/runbooks/example.md)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=docs-only"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_CHECKS=1"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_HYGIENE=1"
  echo "ok"
}

assert_rust_only_change_runs_docs_hygiene() {
  echo "== Rust-only change runs docs-hygiene audit (matches unconditional CI run) =="
  local output
  output="$(plan_for --changed-file crates/plan-tooling/src/validate.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_CHECKS=0"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_HYGIENE=1"
  echo "ok"
}

assert_shared_crate_change_runs_docs_hygiene() {
  echo "== shared-crate (workspace) change runs docs-hygiene audit =="
  local output
  output="$(plan_for --changed-file crates/nils-common/src/lib.rs)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=workspace"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_HYGIENE=1"
  echo "ok"
}

assert_docs_only_plan_does_not_require_cargo() {
  echo "== docs-only plan does not require cargo =="
  local tmp_bin
  tmp_bin="$(mktemp -d)"
  for cmd in bash git python3 sed sort mktemp rm; do
    ln -s "$(command -v "$cmd")" "$tmp_bin/$cmd"
  done

  local output status
  set +e
  output="$(PATH="$tmp_bin" bash "$script" --plan-only --changed-file docs/runbooks/example.md 2>&1)"
  status=$?
  set -e
  rm -rf "$tmp_bin"
  if [[ "$status" -ne 0 ]]; then
    echo "FAIL: $FUNCNAME"
    echo "$output"
    exit 1
  fi

  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=docs-only"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_CHECKS=1"
  echo "ok"
}

assert_crate_src_asset_md_runs_package() {
  echo "== crate src embedded .md runs package mode, not docs-only =="
  local output
  output="$(plan_for --changed-file crates/agent-docs/src/templates/agents_default.md)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-agent-docs"
  assert_not_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=docs-only"
  echo "ok"
}

assert_crate_root_embedded_md_runs_package() {
  echo "== crate-root embedded .md runs package mode, not docs-only =="
  local output
  output="$(plan_for --changed-file crates/plan-tooling/plan-template.md)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-plan-tooling"
  assert_not_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=docs-only"
  echo "ok"
}

assert_crate_test_fixture_md_runs_package() {
  echo "== crate test fixture .md runs package mode, not docs-only =="
  local output
  output="$(plan_for --changed-file crates/plan-tooling/tests/fixtures/plan_bundle/valid-plan.md)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-plan-tooling"
  echo "ok"
}

assert_crate_readme_stays_docs_only() {
  echo "== crate README stays docs-only =="
  local output
  output="$(plan_for --changed-file crates/plan-tooling/README.md)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=docs-only"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_CHECKS=1"
  echo "ok"
}

assert_crate_docs_tree_stays_docs_only() {
  echo "== crate docs/ tree stays docs-only =="
  local output
  output="$(plan_for --changed-file crates/plan-tooling/docs/specs/plan-source-bundle-contract-v1.md)"
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
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_THIRD_PARTY_ARTIFACTS=1"
  echo "ok"
}

assert_package_manifest_requests_third_party_artifacts() {
  echo "== package manifest requests third-party artifact audit =="
  local output
  output="$(plan_for --changed-file crates/plan-tooling/Cargo.toml)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=packages"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_PACKAGE=nils-plan-tooling"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_THIRD_PARTY_ARTIFACTS=1"
  echo "ok"
}

assert_third_party_artifact_change_escapes_docs_only() {
  echo "== third-party artifact change escapes docs-only mode =="
  local output
  output="$(plan_for --changed-file THIRD_PARTY_LICENSES.md)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=workspace"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_CHECKS=1"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_THIRD_PARTY_ARTIFACTS=1"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_REASON=third-party artifact output changed: THIRD_PARTY_LICENSES.md"
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
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_DOCS_HYGIENE=1"
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

assert_deleted_shell_script_escalates_without_syntax_check() {
  echo "== deleted shell script escalates without syntax check =="
  local output
  output="$(plan_for --changed-file scripts/ci/deleted-helper.sh)"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_MODE=workspace"
  assert_contains "$FUNCNAME" "$output" "LOCAL_FAST_REASON=workspace-level path changed: scripts/ci/deleted-helper.sh"
  assert_not_contains "$FUNCNAME" "$output" "LOCAL_FAST_SHELL=scripts/ci/deleted-helper.sh"
  echo "ok"
}

assert_package_crate_uses_package_mode
assert_bin_only_package_skips_doctests
assert_library_package_keeps_doctests
assert_shared_crate_escalates_to_workspace
assert_shared_scrub_crate_escalates_to_workspace
assert_docs_only_uses_docs_mode
assert_rust_only_change_runs_docs_hygiene
assert_shared_crate_change_runs_docs_hygiene
assert_docs_only_plan_does_not_require_cargo
assert_crate_src_asset_md_runs_package
assert_crate_root_embedded_md_runs_package
assert_crate_test_fixture_md_runs_package
assert_crate_readme_stays_docs_only
assert_crate_docs_tree_stays_docs_only
assert_workspace_manifest_escalates_to_workspace
assert_package_manifest_requests_third_party_artifacts
assert_third_party_artifact_change_escapes_docs_only
assert_docs_plus_package_runs_both
assert_shell_script_is_reported
assert_deleted_shell_script_escalates_without_syntax_check

echo
echo "PASS: local-fast-checks.test.sh"
