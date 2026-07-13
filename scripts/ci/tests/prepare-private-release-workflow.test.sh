#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
WORKFLOW="$ROOT/.github/workflows/prepare-private-release.yml"

fail() {
  echo "FAIL: prepare-private-release-workflow.test.sh: $*" >&2
  exit 1
}

require_text() {
  local expected="$1"
  grep -Fq -- "$expected" "$WORKFLOW" || fail "missing workflow contract: $expected"
}

reject_text() {
  local unexpected="$1"
  if grep -Fq -- "$unexpected" "$WORKFLOW"; then
    fail "unsafe workflow surface present: $unexpected"
  fi
}

[[ -f "$WORKFLOW" ]] || fail "workflow is missing"

require_text 'workflow_dispatch:'
require_text 'version:'
require_text 'request_id:'
require_text 'permissions:'
require_text 'contents: read'
require_text 'runs-on: ubuntu-24.04'
require_text 'cargo build --locked -p nils-semantic-commit -p nils-git-scope'
require_text 'bash .agents/scripts/release.sh'
require_text '--skip-push'
require_text '--skip-local-brew-upgrade'
require_text '--skip-dev-clean'
require_text 'release.patch'
require_text 'actions/upload-artifact@'

reject_text 'pull_request:'
reject_text 'push:'
reject_text 'schedule:'
reject_text 'repository_dispatch:'
reject_text 'self-hosted'
reject_text 'secrets.'
reject_text 'github-app-cli'
reject_text 'ssh '
reject_text 'cargo build --locked -p semantic-commit -p nils-git-scope'

job_count="$(awk '
  /^jobs:/ { in_jobs = 1; next }
  in_jobs && /^[^[:space:]]/ { in_jobs = 0 }
  in_jobs && /^  [A-Za-z0-9_-]+:$/ { count++ }
  END { print count + 0 }
' "$WORKFLOW")"
[[ "$job_count" == 1 ]] || fail "expected exactly one hosted preparation job, found $job_count"

echo "PASS: prepare-private-release-workflow.test.sh"
