#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/workspace-test-stale-audit-test.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
fixture="$tmp_dir/repo"
agent_home="$tmp_dir/agent-home"
mkdir -p "$fixture/crates/demo/tests" "$agent_home/out/workspace-test-cleanup"
: >"$fixture/Cargo.toml"
cat >"$fixture/crates/demo/tests/contracts.rs" <<'RS'
fn legacy_helper() {}
const RETAINED_FIELD: &str = "legacy_contract"; // stale-audit: keep-contract
// TODO: remove superseded fixture
RS

inventory="$agent_home/out/workspace-test-cleanup/stale-tests.tsv"
AGENT_HOME="$agent_home" bash "$repo_root/scripts/dev/workspace-test-stale-audit.sh" \
  --root "$fixture" \
  --out "$inventory" \
  >/dev/null

if ! rg -q $'demo\tcrates/demo/tests/contracts.rs\tline:1\tdeprecated_path_marker' "$inventory"; then
  echo "error: unannotated stale marker was not reported" >&2
  exit 1
fi
if rg -q $'demo\tcrates/demo/tests/contracts.rs\tline:2\tdeprecated_path_marker' "$inventory"; then
  echo "error: retained compatibility contract was reported" >&2
  exit 1
fi
if ! rg -q $'demo\tcrates/demo/tests/contracts.rs\tline:3\tdeprecated_path_marker' "$inventory"; then
  echo "error: TODO removal marker was not reported" >&2
  exit 1
fi

if ! rg -qF 'run bash scripts/ci/tests/workspace-test-stale-audit.test.sh' \
  "$repo_root/.agents/skills/project-verify-required-checks/scripts/project-verify-required-checks.sh"; then
  echo "error: required checks do not invoke the stale-audit self-test" >&2
  exit 1
fi

echo "ok: workspace stale-audit tests passed"
