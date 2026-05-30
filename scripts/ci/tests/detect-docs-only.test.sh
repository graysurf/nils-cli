#!/usr/bin/env bash
set -euo pipefail

# Self-test for scripts/ci/detect-docs-only.sh.
#
# Locks the CI docs-only gate: documentation changes route to the docs-only
# lane, while source/test assets, third-party artifact inputs, and mixed sets
# fall back to full CI. Classification shares scripts/ci/lib/doc_classify.py
# with the local-fast planner.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

script="$repo_root/scripts/ci/detect-docs-only.sh"
if [[ ! -f "$script" ]]; then
  echo "error: missing detect script: $script" >&2
  exit 2
fi

detect() {
  bash "$script" "$@"
}

assert_verdict() {
  local label="$1"
  local expected="$2"
  shift 2
  local actual
  actual="$(detect "$@")"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL: $label"
    echo "  args: $*"
    echo "  expected: $expected"
    echo "  actual:   $actual"
    exit 1
  fi
  echo "ok: $label ($expected)"
}

# Documentation -> docs-only lane.
assert_verdict "repo docs runbook"        true  --changed-file docs/runbooks/example.md
assert_verdict "plan bundle (lint still runs)" true --changed-file docs/plans/foo/foo-plan.md
assert_verdict "root README"              true  --changed-file README.md
assert_verdict "crate README"             true  --changed-file crates/plan-tooling/README.md
assert_verdict "crate docs tree"          true  --changed-file crates/plan-tooling/docs/specs/plan-source-bundle-contract-v1.md
assert_verdict "multiple docs"            true  --changed-file README.md --changed-file docs/runbooks/example.md

# Source / test assets that merely end in .md -> full CI.
assert_verdict "crate src embedded md"    false --changed-file crates/agent-docs/src/templates/agents_default.md
assert_verdict "crate-root embedded md"   false --changed-file crates/plan-tooling/plan-template.md
assert_verdict "crate test fixture md"    false --changed-file crates/plan-tooling/tests/fixtures/plan_bundle/valid-plan.md

# Mixed / code / config -> full CI.
assert_verdict "docs + rust"              false --changed-file docs/runbooks/example.md --changed-file crates/plan-tooling/src/validate.rs
assert_verdict "workflow change"          false --changed-file .github/workflows/ci.yml
assert_verdict "ci script change"         false --changed-file scripts/ci/detect-docs-only.sh

# Third-party artifact inputs escape docs-only (even THIRD_PARTY_*.md).
assert_verdict "Cargo.lock"               false --changed-file Cargo.lock
assert_verdict "third-party notice md"    false --changed-file THIRD_PARTY_LICENSES.md
assert_verdict "crate manifest"           false --changed-file crates/plan-tooling/Cargo.toml

# Degenerate bases fall back to full CI.
assert_verdict "empty base"               false --base ""
assert_verdict "all-zero base"            false --base 0000000000000000000000000000000000000000

echo
echo "PASS: detect-docs-only.test.sh"
