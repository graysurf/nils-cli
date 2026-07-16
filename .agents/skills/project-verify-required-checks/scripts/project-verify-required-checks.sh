#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  project-verify-required-checks.sh [--docs-only] [--help]

Runs the full CI/parity checks from DEVELOPMENT.md:
  - bash scripts/ci/docs-placement-audit.sh --strict
  - bash scripts/ci/docs-hygiene-audit.sh --strict
  - bash scripts/ci/markdownlint-audit.sh --strict
  - bash scripts/ci/plan-bundle-validate.sh --strict
  - bash scripts/ci/cli-output-contract-lint.sh --strict
  - bash scripts/ci/forge-cli-fixture-lint.sh --strict
  - bash scripts/ci/tests/install-local-release-binaries.test.sh
  - bash scripts/ci/tests/completion-freshness-audit.test.sh
  - bash scripts/ci/tests/local-fast-checks.test.sh
  - bash scripts/ci/tests/detect-docs-only.test.sh
  - bash scripts/ci/tests/detect-release-only.test.sh
  - node scripts/ci/tests/release-ci-gate.test.cjs
  - bash scripts/ci/tests/release-workflow-contract.test.sh
  - bash scripts/ci/tests/shared-helper-adoption-audit.test.sh
  - bash scripts/ci/tests/publish-order-audit.test.sh
  - bash scripts/ci/tests/docs-hygiene-audit.test.sh
  - bash scripts/ci/tests/prepare-private-release-workflow.test.sh
  - bash scripts/ci/skill-shell-suites.sh
  - bash scripts/ci/test-stale-audit.sh --strict
  - bash scripts/ci/workspace-version-lockstep.sh --strict
  - bash scripts/ci/crate-naming-audit.sh
  - bash scripts/ci/publish-order-audit.sh --strict
  - bash scripts/ci/third-party-artifacts-audit.sh --strict
  - bash scripts/ci/completion-asset-audit.sh --strict
  - bash scripts/ci/completion-freshness-audit.sh --strict
  - bash scripts/ci/completion-flag-parity-audit.sh --strict
  - zsh -f tests/zsh/completion.test.zsh
  - cargo fmt --all -- --check
  - cargo clippy --all-targets --all-features -- -D warnings
  - cargo test --workspace

Modes:
  (default)
    Run full CI/parity checks.
  --docs-only
    Run documentation-only checks:
      - bash scripts/ci/docs-placement-audit.sh --strict
      - bash scripts/ci/docs-hygiene-audit.sh --strict
      - bash scripts/ci/markdownlint-audit.sh --strict
      - bash scripts/ci/plan-bundle-validate.sh --strict
      - bash scripts/ci/cli-output-contract-lint.sh --strict
    Skip fmt/clippy/workspace tests/zsh completion tests.

Environment:
  NILS_CLI_TEST_RUNNER=nextest
    Run `cargo nextest run --profile ci --workspace` and `cargo test --workspace --doc`
    instead of `cargo test --workspace`.

Exit codes:
  0  all checks passed
  1  a check failed
  2  usage error or missing prerequisites
USAGE
}

docs_only=0
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --docs-only)
      docs_only=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: ${1:-}" >&2
      usage >&2
      exit 2
      ;;
    esac
done

required_cmds=(git node npx rg)
if [[ "$docs_only" -eq 0 ]]; then
  required_cmds+=(cargo python3 zsh)
fi

for cmd in "${required_cmds[@]}"; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: missing required tool on PATH: $cmd" >&2
    exit 2
  fi
done

test_runner="${NILS_CLI_TEST_RUNNER:-}"
if [[ "$docs_only" -eq 0 ]]; then
  case "$test_runner" in
    ""|cargo|cargo-test)
      ;;
    nextest)
      if ! command -v cargo-nextest >/dev/null 2>&1; then
        echo "error: NILS_CLI_TEST_RUNNER=nextest requires cargo-nextest on PATH" >&2
        exit 2
      fi
      ;;
    *)
      echo "error: unsupported NILS_CLI_TEST_RUNNER value: $test_runner (expected 'cargo' or 'nextest')" >&2
      exit 2
      ;;
  esac
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi

cd "$repo_root"

run() {
  local -a cmd=( "$@" )
  echo "+ ${cmd[*]}"
  if "${cmd[@]}"; then
    return 0
  else
    local code=$?
    echo "error: check failed (exit $code): ${cmd[*]}" >&2
    exit 1
  fi
}

run bash scripts/ci/docs-placement-audit.sh --strict
run bash scripts/ci/docs-hygiene-audit.sh --strict
run bash scripts/ci/markdownlint-audit.sh --strict
run bash scripts/ci/plan-bundle-validate.sh --strict
run bash scripts/ci/cli-output-contract-lint.sh --strict
run bash scripts/ci/forge-cli-fixture-lint.sh --strict
if [[ "$docs_only" -eq 1 ]]; then
  echo "ok: docs-only nils-cli checks passed"
  exit 0
fi

run bash scripts/ci/tests/install-local-release-binaries.test.sh
run bash scripts/ci/tests/completion-freshness-audit.test.sh
run bash scripts/ci/tests/local-fast-checks.test.sh
run bash scripts/ci/tests/detect-docs-only.test.sh
run bash scripts/ci/tests/detect-release-only.test.sh
run node scripts/ci/tests/release-ci-gate.test.cjs
run bash scripts/ci/tests/release-workflow-contract.test.sh
run bash scripts/ci/tests/shared-helper-adoption-audit.test.sh
run bash scripts/ci/tests/publish-order-audit.test.sh
run bash scripts/ci/tests/docs-hygiene-audit.test.sh
run bash scripts/ci/tests/prepare-private-release-workflow.test.sh
run bash scripts/ci/skill-shell-suites.sh
run bash scripts/ci/test-stale-audit.sh --strict
run bash scripts/ci/workspace-version-lockstep.sh --strict
run bash scripts/ci/crate-naming-audit.sh
run bash scripts/ci/publish-order-audit.sh --strict
run bash scripts/ci/third-party-artifacts-audit.sh --strict
run bash scripts/ci/completion-asset-audit.sh --strict
run bash scripts/ci/completion-freshness-audit.sh --strict
run bash scripts/ci/completion-flag-parity-audit.sh --strict
run zsh -f tests/zsh/completion.test.zsh
run cargo fmt --all -- --check
run cargo clippy --all-targets --all-features -- -D warnings
if [[ "$test_runner" == "nextest" ]]; then
  run cargo nextest run --profile ci --workspace
  run cargo test --workspace --doc
else
  run cargo test --workspace
fi

echo "ok: all nils-cli checks passed"
