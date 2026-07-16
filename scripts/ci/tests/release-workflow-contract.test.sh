#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi
cd "$repo_root"

assert_contains() {
  local file="$1"
  local pattern="$2"
  local label="$3"
  if ! rg -q --fixed-strings "$pattern" "$file"; then
    echo "FAIL: $label" >&2
    echo "  missing from $file: $pattern" >&2
    exit 1
  fi
  echo "ok: $label"
}

assert_contains .github/workflows/release.yml \
  'require("./.github/scripts/release-ci-gate.cjs")' \
  "release workflow uses checked-in provenance gate"
assert_contains .github/workflows/release.yml "pull-requests: read" \
  "release gate can verify the canonical merged PR"
assert_contains .github/workflows/ci.yml "release_only:" \
  "CI publishes the release-only decision"
assert_contains .github/workflows/ci.yml "scripts/ci/detect-release-only.sh" \
  "CI uses the semantic release classifier"
assert_contains .github/workflows/ci.yml "findTrustedMainCi" \
  "release-only CI requires exact-base full CI proof"
assert_contains .github/workflows/ci.yml "Full validation marker" \
  "full CI is distinguishable from reduced lanes"
assert_contains .github/workflows/ci.yml "scripts/ci/release-only-checks.sh" \
  "Linux and macOS checks expose the reduced lane"
assert_contains .github/workflows/ci.yml "needs.changes.outputs.release_only != 'true'" \
  "coverage work is skipped only after fail-closed classification"
assert_contains .agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh \
  "node scripts/ci/tests/release-ci-gate.test.cjs" \
  "release gate unit tests are in the required suite"
assert_contains DEVELOPMENT.md "bash scripts/ci/tests/detect-release-only.test.sh" \
  "development contract lists the release classifier tests"

echo
echo "PASS: release-workflow-contract.test.sh"
