#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/detect-docs-only.sh [--base <ref>] [--changed-file <path>]...

Description:
  Print "true" when every changed file is documentation and none feeds the
  generated third-party artifacts; otherwise print "false". Used by the ci.yml
  `changes` job to route docs-only PRs/pushes through the docs-only lane while
  keeping the release-gated check names (test / test_macos / coverage) present
  and green via step-level skips.

  Classification reuses scripts/ci/lib/doc_classify.py, the same source of truth
  as the local-fast planner, so the two lanes cannot drift.

Options:
  --base <ref>            Diff base for changed-file detection (compared via
                          merge-base against HEAD). Ignored when --changed-file
                          is given. An empty, all-zero, or unresolvable ref
                          yields "false" (run full CI).
  --changed-file <path>   Override changed-file detection. Repeatable.
                          Intended for regression tests and explicit scopes.
  -h, --help              Show this help.

Exit codes:
  0  verdict printed ("true" or "false")
  2  usage error or missing prerequisites
USAGE
}

base=""
declare -a forced_changed_files=()
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --base)
      if [[ $# -lt 2 ]]; then
        echo "error: --base requires a value" >&2
        exit 2
      fi
      base="${2:-}"
      shift 2
      ;;
    --base=*)
      base="${1#--base=}"
      shift
      ;;
    --changed-file)
      if [[ $# -lt 2 ]]; then
        echo "error: --changed-file requires a value" >&2
        exit 2
      fi
      forced_changed_files+=("${2:-}")
      shift 2
      ;;
    --changed-file=*)
      forced_changed_files+=("${1#--changed-file=}")
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

if ! command -v python3 >/dev/null 2>&1; then
  echo "error: missing required tool on PATH: python3" >&2
  exit 2
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/nils-cli-detect-docs-only.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
changed_file_list="$tmp_dir/changed-files.txt"

collect_changed_files() {
  if [[ "${#forced_changed_files[@]}" -gt 0 ]]; then
    printf '%s\n' "${forced_changed_files[@]}" | sed '/^$/d' | sort -u
    return 0
  fi

  # An empty or all-zero base (initial push, force-push, missing PR base) cannot
  # be diffed; fall back to "not docs-only" so full CI runs.
  if [[ -z "$base" || "$base" =~ ^0+$ ]]; then
    return 0
  fi
  if ! git rev-parse --verify "${base}^{commit}" >/dev/null 2>&1; then
    return 0
  fi

  local merge_base
  merge_base="$(git merge-base HEAD "$base" 2>/dev/null || true)"
  if [[ -z "$merge_base" ]]; then
    return 0
  fi
  git diff --name-only --diff-filter=ACMRT "$merge_base" HEAD | sed '/^$/d' | sort -u
}

collect_changed_files >"$changed_file_list"

python3 - "$repo_root" "$changed_file_list" <<'PY'
import pathlib
import sys

repo = pathlib.Path(sys.argv[1])
changed_path = pathlib.Path(sys.argv[2])
changed = [
    line.strip()
    for line in changed_path.read_text(encoding="utf-8").splitlines()
    if line.strip()
]

sys.path.insert(0, str(repo / "scripts" / "ci" / "lib"))
from doc_classify import affects_third_party_artifacts, is_doc_path

if not changed:
    print("false")
    sys.exit(0)
if any(affects_third_party_artifacts(path) for path in changed):
    print("false")
    sys.exit(0)
print("true" if all(is_doc_path(path) for path in changed) else "false")
PY
