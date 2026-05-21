#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  docs-placement-audit.sh [--strict]

Checks documentation placement policy:
  - every workspace crate has a required docs index at crates/<crate>/docs/README.md
  - root docs/runbooks entries are approved workspace-level files or compatibility stubs
  - crate-owned root docs patterns are flagged

Options:
  --strict   Treat policy warnings as hard failures
  -h, --help Show this help
USAGE
}

strict=0
while [[ $# -gt 0 ]]; do
  case "${1:-}" in
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

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree" >&2
  exit 2
fi
cd "$repo_root"

if [[ ! -d crates ]]; then
  echo "error: missing crates directory" >&2
  exit 2
fi

declare -a crate_dirs=()
while IFS= read -r dir; do
  [[ -f "$dir/Cargo.toml" ]] || continue
  crate_dirs+=("$dir")
done < <(find crates -mindepth 1 -maxdepth 1 -type d | sort)

if [[ ${#crate_dirs[@]} -eq 0 ]]; then
  echo "error: no workspace crate directories found under crates/" >&2
  exit 2
fi

declare -a crate_names=()
for dir in "${crate_dirs[@]}"; do
  crate_names+=("${dir##*/}")
done

declare -a errors=()
declare -a warnings=()

for dir in "${crate_dirs[@]}"; do
  docs_index="$dir/docs/README.md"
  if [[ ! -f "$docs_index" ]]; then
    errors+=("missing docs index: $docs_index (required docs index)")
  fi
done

runbook_is_approved_workspace_file() {
  local base="$1"
  case "$base" in
    cli-completion-development-standard.md|cli-help-style-guide.md|crates-io-status-script-runbook.md|new-cli-crate-development-standard.md|review-specialists-primitive.md|test-cleanup-governance.md)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

file_is_stub() {
  local file="$1"
  if command -v rg >/dev/null 2>&1; then
    rg -q '^Moved to:' "$file"
  else
    grep -Eq '^Moved to:' "$file"
  fi
}

for file in docs/runbooks/*.md; do
  [[ -e "$file" ]] || continue
  base="$(basename "$file")"

  if runbook_is_approved_workspace_file "$base"; then
    continue
  fi

  if file_is_stub "$file"; then
    continue
  fi

  message="unapproved root runbook path: $file"
  if [[ $strict -eq 1 ]]; then
    errors+=("$message")
  else
    warnings+=("$message")
  fi
done

for section in docs/specs docs/runbooks docs/reports; do
  [[ -d "$section" ]] || continue
  while IFS= read -r file; do
    base="$(basename "$file")"
    if [[ "$section" == "docs/runbooks" ]] && runbook_is_approved_workspace_file "$base"; then
      continue
    fi
    for crate in "${crate_names[@]}"; do
      if [[ "$base" == "$crate"-* ]]; then
        if file_is_stub "$file"; then
          warnings+=("compatibility stub allowed at root: $file")
        else
          message="crate-owned root docs pattern detected: $file"
          if [[ $strict -eq 1 ]]; then
            errors+=("$message")
          else
            warnings+=("$message")
          fi
        fi
        break
      fi
    done
  done < <(find "$section" -maxdepth 1 -type f -name '*.md' | sort)
done

for warn in "${warnings[@]}"; do
  echo "WARN: $warn"
done

if [[ ${#errors[@]} -gt 0 ]]; then
  for err in "${errors[@]}"; do
    echo "FAIL: $err"
  done
  echo "FAIL: docs placement audit (strict=$strict, crates=${#crate_dirs[@]}, errors=${#errors[@]}, warnings=${#warnings[@]})"
  exit 1
fi

echo "PASS: docs placement audit (strict=$strict, crates=${#crate_dirs[@]}, warnings=${#warnings[@]})"
