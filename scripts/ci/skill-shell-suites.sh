#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/skill-shell-suites.sh [--help]

Discovers and runs every project skill shell test suite
(.agents/skills/*/tests/test_*.sh) so their rot is caught by CI instead of going
unnoticed until someone runs a suite by hand (see sympoies/nils-cli#1062).

Each suite is a self-contained, mock-driven smoke test that exits 0 on success.
Suites stub the tools they invoke (gh, brew, cargo, semantic-commit, ...) under
a temp PATH, so no provider access or extra tooling is required. A couple of
cases probe a real Rust toolchain and skip themselves when rustc/cargo are
absent; the suites otherwise run everywhere bash + git + python3 are present.

Exit codes:
  0  all suites passed
  1  one or more suites failed
  2  usage error or no suites found
USAGE
}

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
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

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

suites=()
while IFS= read -r suite; do
  suites+=("$suite")
done < <(find .agents/skills -type f -path '*/tests/test_*.sh' | sort)

if [[ "${#suites[@]}" -eq 0 ]]; then
  echo "error: no skill shell suites found under .agents/skills/*/tests/" >&2
  exit 2
fi

# Bound each suite so a hung suite fails the job instead of stalling it. macOS
# runners lack `timeout` by default, so fall back to a bare run there.
runner=(bash)
if command -v timeout >/dev/null 2>&1; then
  runner=(timeout 600 bash)
fi

failed=()
for suite in "${suites[@]}"; do
  echo "=== running ${suite} ==="
  if "${runner[@]}" "$suite"; then
    echo "PASS ${suite}"
  else
    code=$?
    echo "FAIL ${suite} (exit ${code})" >&2
    failed+=("$suite")
  fi
done

if [[ "${#failed[@]}" -gt 0 ]]; then
  echo "error: ${#failed[@]} of ${#suites[@]} skill shell suite(s) failed: ${failed[*]}" >&2
  exit 1
fi

echo "ok: all ${#suites[@]} skill shell suites passed"
