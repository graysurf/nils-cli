#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: release-only-checks.sh --base <git-ref> [--head <git-ref>] --branch <name>

Runs the reduced cross-platform validation lane after the caller has proved
that the base main commit completed full CI.
USAGE
}

base_ref=""
head_ref="HEAD"
branch=""
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --base)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      base_ref="$2"
      shift 2
      ;;
    --head)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      head_ref="$2"
      shift 2
      ;;
    --branch)
      [[ $# -ge 2 ]] || { usage >&2; exit 2; }
      branch="$2"
      shift 2
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

if [[ -z "$base_ref" || -z "$head_ref" || -z "$branch" ]]; then
  usage >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside the nils-cli git work tree" >&2
  exit 2
fi
cd "$repo_root"

verdict="$(
  bash scripts/ci/detect-release-only.sh \
    --base "$base_ref" \
    --head "$head_ref" \
    --branch "$branch"
)"
if [[ "$verdict" != "true" ]]; then
  echo "error: release-only contract is no longer satisfied; refusing the reduced lane" >&2
  exit 1
fi

bash scripts/ci/workspace-version-lockstep.sh --strict
bash scripts/generate-third-party-artifacts.sh --check
bash scripts/ci/publish-order-audit.sh --strict
cargo check --workspace --all-targets --all-features --locked

echo "ok: release-only checks passed"
