#!/usr/bin/env bash
set -euo pipefail

# forge-cli-fixture-lint.sh — Sprint 7 Task 7.3 fixture redaction audit.
#
# Scans every file under crates/forge-cli/tests/fixtures/ for token-shaped
# strings (gh{ps}_*, glpat-*, ghr_*, gho_*, Bearer <opaque>) and refuses to
# pass if any are present. Spec: forge-cli-spec-v1 §"Security and redaction
# expectations" + ops YAML §"redaction policy".
#
# Wired into scripts/ci/nils-cli-checks-entrypoint.sh's docs-only lane so PR
# review catches un-redacted fixtures before merge.
#
# Usage:
#   scripts/ci/forge-cli-fixture-lint.sh [--strict] [<fixture-root>]

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/forge-cli-fixture-lint.sh [--strict] [<fixture-root>]

Greps the forge-cli fixture tree for token-shaped strings and fails if any
match. The default root is crates/forge-cli/tests/fixtures/.

  --strict  Reserved for symmetry with sibling lint scripts; this script is
            strict by default (any match is a hard fail). The flag is
            accepted as a no-op so callers can chain it through
            nils-cli-checks-entrypoint.sh without conditional logic.

Replace any match with `<redacted-token>` (or domain-appropriate marker like
`<redacted-jwt>`) before re-running.
USAGE
}

STRICT=0
ROOT="crates/forge-cli/tests/fixtures"
while [ $# -gt 0 ]; do
  case "$1" in
    --strict) STRICT=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) ROOT="$1"; shift ;;
  esac
done
: "${STRICT:=0}" # silence unused-var lint in CI

if [ ! -d "$ROOT" ]; then
  echo "error: fixture root '$ROOT' is not a directory" >&2
  exit 64
fi

# Pattern set matches the spec's enumeration of token shapes:
#  - ghp_<base62> / ghs_<base62> — GitHub personal/server tokens
#  - ghr_<base62> / gho_<base62> — GitHub refresh / OAuth tokens
#  - glpat-<base62> — GitLab personal access tokens
#  - Bearer <opaque> — generic bearer-auth headers
PATTERNS=(
  'gh[ps]_[A-Za-z0-9_]{16,}'
  'ghr_[A-Za-z0-9_]{16,}'
  'gho_[A-Za-z0-9_]{16,}'
  'glpat-[A-Za-z0-9_-]{16,}'
  'Bearer [A-Za-z0-9._-]{16,}'
)

MATCHES=0
TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT

for pattern in "${PATTERNS[@]}"; do
  if grep -RInE "$pattern" "$ROOT" >>"$TMP" 2>/dev/null; then
    :
  fi
done

if [ -s "$TMP" ]; then
  MATCHES=$(wc -l <"$TMP" | tr -d ' ')
  echo "FAIL: forge-cli fixture redaction audit found ${MATCHES} match(es):" >&2
  cat "$TMP" >&2
  echo "" >&2
  echo "Replace each occurrence with <redacted-token> (or <redacted-jwt>" >&2
  echo "for bearer headers) and re-run." >&2
  exit 65
fi

# Count files scanned so the success line carries some visible signal.
FILES=$(find "$ROOT" -type f | wc -l | tr -d ' ')
echo "PASS: forge-cli fixture redaction audit (strict=1, files=${FILES}, matches=0)"
