#!/usr/bin/env bash
#
# nils-cli's /pre-pr gate stack. The global dispatcher execs this when cwd is
# nils-cli.
#
# Mirrors the default local development command from AGENTS.md:
#   bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast
#
# The full workspace and coverage gates are CI responsibilities for normal PRs.
# Use --full or --with-coverage when a local CI-parity run is explicitly needed.
# Extra args forward straight through.
#
# Examples:
#   /pre-pr                  # changed-scope local-fast check
#   /pre-pr --docs-only      # docs-only check
#   /pre-pr --full           # local full CI-parity check with nextest
#   /pre-pr --with-coverage  # local full CI-parity + coverage check
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

has_docs_only=0
force_full=0
declare -a entry_args=()
for arg in "$@"; do
  case "$arg" in
    --docs-only)
      has_docs_only=1
      entry_args+=("$arg")
      ;;
    --full)
      force_full=1
      ;;
    --with-coverage)
      force_full=1
      entry_args+=("$arg")
      ;;
    *)
      entry_args+=("$arg")
      ;;
  esac
done

if [[ "$has_docs_only" -eq 1 ]]; then
  if [[ "$force_full" -eq 1 ]]; then
    echo "pre-pr: --docs-only cannot be combined with --full or --with-coverage" >&2
    exit 2
  fi
  exec bash "$entrypoint" "${entry_args[@]}"
fi
if [[ "$force_full" -eq 1 ]]; then
  export NILS_CLI_TEST_RUNNER=nextest
  exec bash "$entrypoint" "${entry_args[@]}"
fi
exec bash "$entrypoint" --local-fast "${entry_args[@]}"
