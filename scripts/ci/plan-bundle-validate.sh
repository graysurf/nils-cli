#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/plan-bundle-validate.sh [--strict] [--all] [--file <path>]...

Validates touched docs/plans bundles with `plan-tooling validate --file`.

Options:
  --strict       Fail when the plan-tooling entrypoint is unavailable.
  --all          Validate every docs/plans/**/*-plan.md file.
  --file <path>  Validate one specific plan file. May be repeated.
  -h, --help     Show this help.

Default selection:
  Prefer git-touched docs/plans files from origin/main...HEAD, the working tree,
  the index, and untracked files. Any touched bundle sibling maps to its
  docs/plans/<slug>/<slug>-plan.md file. If the git base cannot be resolved,
  validate all plan files.
USAGE
}

strict=0
all=0
declare -a explicit_files=()
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --strict)
      strict=1
      shift
      ;;
    --all)
      all=1
      shift
      ;;
    --file)
      if [[ -z "${2:-}" ]]; then
        echo "error: --file requires a path" >&2
        exit 2
      fi
      explicit_files+=("${2:-}")
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

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

normalize_path() {
  local path="$1"
  path="${path#./}"
  printf '%s\n' "$path"
}

plan_for_path() {
  local path
  path="$(normalize_path "$1")"
  [[ "$path" == docs/plans/* ]] || return 0

  if [[ "$path" == docs/plans/*/* ]]; then
    local rest slug candidate
    rest="${path#docs/plans/}"
    slug="${rest%%/*}"
    candidate="docs/plans/${slug}/${slug}-plan.md"
    if [[ -f "$candidate" ]]; then
      printf '%s\n' "$candidate"
    fi
    return 0
  fi

  if [[ "$path" == docs/plans/*-plan.md && -f "$path" ]]; then
    printf '%s\n' "$path"
  fi
}

all_plan_files() {
  if [[ ! -d docs/plans ]]; then
    return 0
  fi
  while IFS= read -r path; do
    local rest slug expected
    rest="${path#docs/plans/}"
    [[ "$rest" == */* ]] || continue
    slug="${rest%%/*}"
    expected="docs/plans/${slug}/${slug}-plan.md"
    [[ "$path" == "$expected" ]] || continue
    printf '%s\n' "$path"
  done < <(find docs/plans -type f -name '*-plan.md' -print | sort -u)
}

git_diff_base() {
  if git rev-parse --verify origin/main >/dev/null 2>&1; then
    printf '%s\n' "origin/main"
  elif git rev-parse --verify HEAD >/dev/null 2>&1; then
    printf '%s\n' "HEAD"
  else
    return 1
  fi
}

git_touched_paths() {
  local base="$1"
  git diff --name-only --diff-filter=ACMR "${base}...HEAD" -- docs/plans 2>/dev/null || true
  git diff --name-only --diff-filter=ACMR -- docs/plans 2>/dev/null || true
  git diff --name-only --cached --diff-filter=ACMR -- docs/plans 2>/dev/null || true
  git ls-files --others --exclude-standard -- docs/plans 2>/dev/null || true
}

declare -a selected=()
if [[ "${#explicit_files[@]}" -gt 0 ]]; then
  for file in "${explicit_files[@]}"; do
    selected+=("$(normalize_path "$file")")
  done
elif [[ "$all" -eq 1 ]]; then
  while IFS= read -r file; do
    [[ -n "$file" ]] && selected+=("$file")
  done < <(all_plan_files)
else
  if base="$(git_diff_base)"; then
    while IFS= read -r touched; do
      [[ -n "$touched" ]] || continue
      while IFS= read -r plan; do
        [[ -n "$plan" ]] && selected+=("$plan")
      done < <(plan_for_path "$touched")
    done < <(git_touched_paths "$base")
  else
    while IFS= read -r file; do
      [[ -n "$file" ]] && selected+=("$file")
    done < <(all_plan_files)
  fi
fi

if [[ "${#selected[@]}" -eq 0 ]]; then
  echo "ok: no docs/plans plan bundles selected for validation"
  exit 0
fi

plan_tooling=""
if [[ -x "wrappers/plan-tooling" ]]; then
  plan_tooling="wrappers/plan-tooling"
elif command -v plan-tooling >/dev/null 2>&1; then
  plan_tooling="$(command -v plan-tooling)"
fi

if [[ -z "$plan_tooling" ]]; then
  if [[ "$strict" -eq 1 ]]; then
    echo "error: plan-tooling is required for strict plan-bundle validation" >&2
    exit 2
  fi
  echo "warning: plan-tooling not found; skipping plan-bundle validation" >&2
  exit 0
fi

mapfile -t selected < <(printf '%s\n' "${selected[@]}" | sort -u)

for plan in "${selected[@]}"; do
  if [[ ! -f "$plan" ]]; then
    echo "error: selected plan file not found: $plan" >&2
    exit 1
  fi
  cmd=( "$plan_tooling" validate --file "$plan" )
  echo "+ ${cmd[*]}"
  output=""
  set +e
  output="$("${cmd[@]}" 2>&1)"
  code=$?
  set -e
  if [[ "$code" -ne 0 ]]; then
    echo "error: plan-bundle validation failed for $plan (exit $code)" >&2
    if [[ -n "$output" ]]; then
      printf '%s\n' "$output" >&2
    fi
    exit 1
  fi
  if [[ -n "$output" ]]; then
    printf '%s\n' "$output"
  fi
done

echo "ok: plan-bundle validation passed (${#selected[@]} file(s))"
