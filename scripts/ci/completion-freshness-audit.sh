#!/usr/bin/env bash
set -euo pipefail

COMPLETION_TIMEOUT_SECONDS=30
PLATFORM_EXE_SUFFIX=""
RUN_CODE=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/completion-freshness-audit.sh [--strict] [--bin <name>] [--skip-build] [--root <path>]

Regenerates bash/zsh completions for every completion-required binary and
fails when generated output differs from the committed completion assets.
Runtime adapter wrappers that load generated completions at shell runtime are
detected and skipped.

Options:
  --strict      Compatibility flag. The audit is always strict.
  --bin <name>  Limit the audit to one required binary. Repeatable.
  --skip-build  Do not build workspace binaries before comparing assets.
  --root <path> Use an explicit repository root. Intended for self-tests.
  -h, --help    Show this help
USAGE
}

strict=0
skip_build=0
repo_root_override=""
declare -a requested_bins=()

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --strict)
      strict=1
      shift
      ;;
    --bin)
      if [[ $# -lt 2 ]]; then
        echo "error: --bin requires a value" >&2
        exit 2
      fi
      requested_bins+=("${2:-}")
      shift 2
      ;;
    --bin=*)
      requested_bins+=("${1#--bin=}")
      shift
      ;;
    --skip-build)
      skip_build=1
      shift
      ;;
    --root)
      if [[ $# -lt 2 ]]; then
        echo "error: --root requires a value" >&2
        exit 2
      fi
      repo_root_override="${2:-}"
      shift 2
      ;;
    --root=*)
      repo_root_override="${1#--root=}"
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
: "$strict"

if [[ -n "$repo_root_override" ]]; then
  repo_root="$(cd "$repo_root_override" && pwd)"
else
  repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: must run inside a git work tree or pass --root <path>" >&2
  exit 2
fi
cd "$repo_root"

matrix_path="docs/specs/completion-coverage-matrix-v1.md"
if [[ ! -f "$matrix_path" ]]; then
  echo "FAIL: missing completion matrix: $matrix_path" >&2
  exit 2
fi

parse_required_bins() {
  local matrix="$1"
  awk -F'|' '
    function trim(s) {
      gsub(/^[ \t]+|[ \t]+$/, "", s)
      return s
    }
    {
      bin = trim($2)
      obligation = trim($3)
      if (bin ~ /^`[^`]+`$/ && obligation == "`required`") {
        gsub(/`/, "", bin)
        print bin
      }
    }
  ' "$matrix" | LC_ALL=C sort -u
}

# Emit `bin<TAB>engine` for each required row. `engine` is `dynamic` only when
# the enforcement-metadata cell (field 6 of the pipe table, `$7` after the
# leading empty field) carries a `completion_engine=dynamic` value, anchored on
# a value boundary (`;`, closing backtick, whitespace, or end). Scoping to that
# cell — not the whole row — and anchoring the value prevents the literal string
# in a free-text column (e.g. Rationale) or a longer value (e.g.
# `dynamic-experimental`) from misclassifying a static CLI as dynamic, which
# would silently skip its freshness diff.
parse_bin_engine() {
  local matrix="$1"
  awk -F'|' '
    function trim(s) {
      gsub(/^[ \t]+|[ \t]+$/, "", s)
      return s
    }
    {
      bin = trim($2)
      obligation = trim($3)
      if (bin ~ /^`[^`]+`$/ && obligation == "`required`") {
        gsub(/`/, "", bin)
        meta = trim($7)
        engine = (meta ~ /completion_engine=dynamic([;`\t ]|$)/) ? "dynamic" : "static"
        print bin "\t" engine
      }
    }
  ' "$matrix"
}

contains_bin() {
  local needle="$1"
  shift
  local candidate
  for candidate in "$@"; do
    if [[ "$candidate" == "$needle" ]]; then
      return 0
    fi
  done
  return 1
}

run_command_to_file() {
  local timeout_seconds="$1"
  local cwd="$2"
  local stdout_file="$3"
  local stderr_file="$4"
  shift 4
  local -a cmd=( "$@" )

  if (( timeout_seconds <= 0 )); then
    if (cd "$cwd" && "${cmd[@]}") >"$stdout_file" 2>"$stderr_file"; then
      RUN_CODE=0
    else
      RUN_CODE=$?
    fi
    return 0
  fi

  (
    cd "$cwd"
    "${cmd[@]}"
  ) >"$stdout_file" 2>"$stderr_file" &
  local pid=$!
  local elapsed_tenths=0
  local max_tenths=$(( timeout_seconds * 10 ))

  while kill -0 "$pid" 2>/dev/null; do
    if (( elapsed_tenths >= max_tenths )); then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      RUN_CODE=124
      : >"$stdout_file"
      printf 'command timed out after %ss: %s\n' "$timeout_seconds" "${cmd[*]}" >"$stderr_file"
      return 0
    fi
    sleep 0.1
    elapsed_tenths=$(( elapsed_tenths + 1 ))
  done

  if wait "$pid"; then
    RUN_CODE=0
  else
    RUN_CODE=$?
  fi
}

target_debug_dir="$repo_root/target/debug"

build_binaries() {
  local stdout_file="$1"
  local stderr_file="$2"
  run_command_to_file 0 "$repo_root" "$stdout_file" "$stderr_file" cargo build --workspace --bins --all-features
  if (( RUN_CODE != 0 )); then
    echo "FAIL: cargo build --workspace --bins --all-features failed (exit ${RUN_CODE})" >&2
    cat "$stderr_file" >&2
    return 1
  fi
}

asset_path_for() {
  local shell_name="$1"
  local binary="$2"
  case "$shell_name" in
    bash) printf 'completions/bash/%s' "$binary" ;;
    zsh) printf 'completions/zsh/_%s' "$binary" ;;
    *)
      echo "error: unsupported shell: $shell_name" >&2
      return 2
      ;;
  esac
}

asset_is_runtime_adapter() {
  local shell_name="$1"
  local asset_path="$2"
  local marker

  case "$shell_name" in
    bash) marker="_nils_cli_completion_common_load_generated_bash" ;;
    zsh) marker="_nils_cli_completion_common_load_generated_zsh" ;;
    *)
      echo "error: unsupported shell: $shell_name" >&2
      return 2
      ;;
  esac

  grep -q "$marker" "$asset_path"
}

mapfile -t all_required_bins < <(parse_required_bins "$matrix_path")
if (( ${#all_required_bins[@]} == 0 )); then
  echo "FAIL: no required binaries found in matrix: $matrix_path" >&2
  exit 2
fi

declare -A bin_engine=()
while IFS=$'\t' read -r engine_bin engine_value; do
  [[ -n "$engine_bin" ]] || continue
  bin_engine["$engine_bin"]="$engine_value"
done < <(parse_bin_engine "$matrix_path")

declare -a audit_bins=()
if (( ${#requested_bins[@]} > 0 )); then
  for requested in "${requested_bins[@]}"; do
    if ! contains_bin "$requested" "${all_required_bins[@]}"; then
      echo "error: requested binary is not required by the completion matrix: $requested" >&2
      exit 2
    fi
    audit_bins+=("$requested")
  done
else
  audit_bins=("${all_required_bins[@]}")
fi

if [[ "${OS:-}" == "Windows_NT" ]]; then
  PLATFORM_EXE_SUFFIX=".exe"
else
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      PLATFORM_EXE_SUFFIX=".exe"
      ;;
  esac
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/completion-freshness-audit.XXXXXX")"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

if (( skip_build == 0 )); then
  if ! build_binaries "$tmp_dir/cargo-build.out" "$tmp_dir/cargo-build.err"; then
    exit 2
  fi
fi

declare -a failures=()
declare -a diff_labels=()
declare -a diff_paths=()
snapshots_checked=0
runtime_adapters_skipped=0
dynamic_engine_skipped=0

for binary in "${audit_bins[@]}"; do
  binary_path="$target_debug_dir/${binary}${PLATFORM_EXE_SUFFIX}"
  if [[ ! -f "$binary_path" ]]; then
    failures+=( "${binary}: missing built binary: ${binary_path#"$repo_root"/}" )
    continue
  fi

  for shell_name in bash zsh; do
    asset_path="$(asset_path_for "$shell_name" "$binary")"
    if [[ ! -f "$asset_path" ]]; then
      failures+=( "${binary}: missing committed ${shell_name} completion asset: $asset_path" )
      continue
    fi

    # A `completion_engine=dynamic` CLI ships a `clap_complete` CompleteEnv
    # registration stub whose contents embed the resolved binary path and are
    # not deterministically reproducible against the committed asset, so there
    # is no static baseline to diff. Require the asset to exist (checked above)
    # but skip the freshness comparison, like a runtime adapter.
    if [[ "${bin_engine[$binary]:-static}" == "dynamic" ]]; then
      dynamic_engine_skipped=$((dynamic_engine_skipped + 1))
      continue
    fi

    if asset_is_runtime_adapter "$shell_name" "$asset_path"; then
      runtime_adapters_skipped=$((runtime_adapters_skipped + 1))
      continue
    fi

    generated_path="$tmp_dir/${binary}.${shell_name}.generated"
    stderr_path="$tmp_dir/${binary}.${shell_name}.stderr"
    run_command_to_file "$COMPLETION_TIMEOUT_SECONDS" "$repo_root" "$generated_path" "$stderr_path" "$binary_path" completion "$shell_name"
    if (( RUN_CODE != 0 )); then
      stderr_compact="$(tr '\n' ' ' <"$stderr_path")"
      failures+=( "${binary}: completion ${shell_name} failed (exit ${RUN_CODE}): ${stderr_compact}" )
      continue
    fi

    if ! cmp -s "$asset_path" "$generated_path"; then
      diff_path="$tmp_dir/${binary}.${shell_name}.diff"
      diff -u "$asset_path" "$generated_path" >"$diff_path" || true
      failures+=( "${binary}: stale ${shell_name} completion asset: $asset_path" )
      diff_labels+=( "${binary} ${shell_name}" )
      diff_paths+=( "$diff_path" )
    fi
    snapshots_checked=$((snapshots_checked + 1))
  done
done

if (( ${#failures[@]} > 0 )); then
  for failure in "${failures[@]}"; do
    echo "FAIL: $failure"
  done
  for idx in "${!diff_paths[@]}"; do
    echo "INFO: diff preview for ${diff_labels[$idx]} completion drift"
    sed -n '1,120p' "${diff_paths[$idx]}"
  done
  echo "FAIL: completion freshness audit (required=${#audit_bins[@]}, snapshots_checked=$snapshots_checked, runtime_adapters_skipped=$runtime_adapters_skipped, dynamic_engine_skipped=$dynamic_engine_skipped, failures=${#failures[@]})"
  exit 1
fi

echo "PASS: completion freshness audit (required=${#audit_bins[@]}, snapshots_checked=$snapshots_checked, runtime_adapters_skipped=$runtime_adapters_skipped, dynamic_engine_skipped=$dynamic_engine_skipped, failures=0)"
