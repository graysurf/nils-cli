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
  if ! rg -q --fixed-strings -- "$pattern" "$file"; then
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
assert_contains .github/workflows/ci.yml "git show \"\${base}:scripts/ci/detect-release-only.sh\"" \
  "release-only classification loads protected base policy"
assert_contains .github/workflows/ci.yml "\${{ needs.changes.outputs.base_sha }}:scripts/ci/release-only-checks.sh" \
  "reduced checks load the exact base checker"
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
assert_contains docs/specs/workspace-ci-entrypoint-inventory-v1.md \
  "release_candidate" \
  "CI inventory records the semantic release candidate output"
assert_contains docs/specs/workspace-ci-entrypoint-inventory-v1.md \
  "release_only=true" \
  "CI inventory records the trusted reduced-lane decision"
assert_contains docs/specs/workspace-ci-entrypoint-inventory-v1.md \
  ".github/scripts/release-ci-gate.cjs" \
  "CI inventory owns the shared release gate module"
assert_contains docs/specs/workspace-ci-entrypoint-inventory-v1.md \
  "scripts/ci/detect-release-only.sh" \
  "CI inventory owns the semantic release detector"
assert_contains docs/specs/workspace-ci-entrypoint-inventory-v1.md \
  "scripts/ci/release-only-checks.sh" \
  "CI inventory owns the reduced release checker"
assert_contains .agents/skills/project-bump-version-tag-release/SKILL.md \
  "--prepare-only" \
  "release skill documents the internal producer contract mode"
assert_contains .agents/skills/project-bump-version-tag-release/SKILL.md \
  "falls back to full PR CI" \
  "release skill documents the fail-closed full-CI fallback"
assert_contains .agents/skills/project-bump-version-tag-release/scripts/project-bump-version-tag-release.sh \
  "reuses that exact-SHA PR CI" \
  "release helper help documents tag-gate CI reuse"

echo
echo "PASS: release-workflow-contract.test.sh"
