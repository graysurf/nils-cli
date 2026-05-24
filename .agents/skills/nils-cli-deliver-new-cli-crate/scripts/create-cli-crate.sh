#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  create-cli-crate.sh [--mode plan|implement] [--project-path <path>] [--strict] [--help]

Purpose:
  Fast preflight + reminder utility for the project-local "create-cli-crate" skill.

Options:
  --mode <plan|implement>   Show checklist for planning or implementation (default: plan)
  --project-path <path>     Target project path (default: current directory)
  --strict                  Also run `agent-docs resolve --context project-dev --strict`
  -h, --help                Show help
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

mode="plan"
project_path="."
strict=0

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --mode)
      [[ $# -ge 2 ]] || die "--mode requires a value"
      mode="${2:-}"
      shift 2
      ;;
    --project-path)
      [[ $# -ge 2 ]] || die "--project-path requires a value"
      project_path="${2:-}"
      shift 2
      ;;
    --strict)
      strict=1
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

case "$mode" in
  plan|implement) ;;
  *) die "invalid --mode (expected plan|implement): $mode" ;;
esac

[[ -d "$project_path" ]] || die "project path not found: $project_path"
repo_root="$(cd "$project_path" && git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$repo_root" ]] || die "target is not inside a git work tree: $project_path"

required_docs=(
  "AGENTS.md"
  "DEVELOPMENT.md"
  "docs/runbooks/new-cli-crate-development-standard.md"
  "docs/specs/cli-service-json-contract-guideline-v1.md"
)

missing=0
for rel in "${required_docs[@]}"; do
  if [[ ! -f "${repo_root}/${rel}" ]]; then
    echo "missing: ${rel}" >&2
    missing=1
  fi
done
(( missing == 0 )) || die "required policy docs are missing"

if (( strict == 1 )); then
  command -v agent-docs >/dev/null 2>&1 || die "agent-docs not found on PATH"
  (
    cd "$repo_root"
    agent-docs resolve --context project-dev --strict --format checklist >/dev/null
  ) || die "agent-docs strict resolve failed for project-dev"
fi

echo "ok: create-cli-crate preflight passed"
echo "project: ${repo_root}"
echo "mode: ${mode}"
echo
echo "checklist:"
if [[ "$mode" == "plan" ]]; then
  cat <<'PLAN'
1) Read docs/runbooks/new-cli-crate-development-standard.md.
2) Read docs/specs/cli-service-json-contract-guideline-v1.md.
3) Produce a rigorous plan before implementation.
4) Ensure plan includes human output + JSON contract + publish-readiness gates.
PLAN
else
  cat <<'IMPLEMENT'
1) Ensure command contract covers human-readable mode and JSON mode.
2) Add JSON contract tests (schema envelope, error envelope, no secret leakage).
3) Align Cargo metadata and publishability rules.
4) Run bash scripts/ci/nils-cli-checks-entrypoint.sh --local-fast.
5) If publishable, run scripts/publish-crates.sh --dry-run --crate <crate-package-name>.
6) Use GitHub required checks for full workspace and coverage gates before merge.
IMPLEMENT
fi

exit 0
