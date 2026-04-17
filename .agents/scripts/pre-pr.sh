#!/usr/bin/env bash
#
# nils-cli's /pre-pr gate stack. The global dispatcher
# (~/.claude/scripts/pre-pr.sh) execs this when cwd is nils-cli.
#
# Mirrors the canonical pre-delivery command from AGENTS.md §Required
# Local Check Entrypoints:
#   NILS_CLI_TEST_RUNNER=nextest bash scripts/ci/nils-cli-checks-entrypoint.sh --with-coverage
#
# The entrypoint runs the required-checks verify script, optionally under
# xvfb, and optionally with the llvm-cov coverage gate. Extra args forward
# straight through (e.g. `--docs-only` for docs-only changes).
#
# Examples:
#   /pre-pr                  # full pre-delivery check (nextest + coverage gate)
#   /pre-pr --docs-only      # skip heavy cargo checks for docs-only PRs
#
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" ]]; then
  echo "pre-pr: not inside a git work tree" >&2
  exit 2
fi

entrypoint="$repo_root/scripts/ci/nils-cli-checks-entrypoint.sh"
if [[ ! -x "$entrypoint" ]]; then
  echo "pre-pr: missing $entrypoint" >&2
  exit 2
fi

# The entrypoint rejects `--with-coverage` alongside `--docs-only`, so only
# auto-enable the coverage gate on full runs.
has_docs_only=0
for arg in "$@"; do
  if [[ "$arg" == "--docs-only" ]]; then
    has_docs_only=1
    break
  fi
done

export NILS_CLI_TEST_RUNNER=nextest
if [[ "$has_docs_only" -eq 1 ]]; then
  exec bash "$entrypoint" "$@"
fi
exec bash "$entrypoint" --with-coverage "$@"
