#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  workspace-shared-crate-audit.sh [--format tsv] [--out <crate-matrix.tsv>] [--root <repo>]
  workspace-shared-crate-audit.sh --emit-lanes --in <crate-matrix.tsv> [--out <task-lanes.tsv>] [--root <repo>]

Generates a deterministic shared-crate audit bundle under:
  $AGENT_HOME/out/workspace-shared-audit/

Primary outputs:
  crate-matrix.tsv
  crate-matrix.md
  hotspots-nils-common.md
  hotspots-nils-term.md
  hotspots-nils-test-support.md
  hotspots-index.tsv
  decision-rubric.md
  task-lanes.tsv

Options:
  --format <tsv>   Output format (currently only `tsv` is supported)
  --out <file>     Target output TSV path
  --emit-lanes     Emit task execution lanes from an existing crate matrix TSV
  --in <file>      Input crate matrix TSV (required with --emit-lanes)
  --root <dir>     Repo root (defaults to current git worktree root)
  -h, --help       Show this help
USAGE
}

format="tsv"
out_file=""
in_file=""
repo_root=""
emit_lanes=0

while [[ $# -gt 0 ]]; do
  case "${1:-}" in
    --format)
      format="${2:-}"
      shift 2
      ;;
    --out)
      out_file="${2:-}"
      shift 2
      ;;
    --in)
      in_file="${2:-}"
      shift 2
      ;;
    --emit-lanes)
      emit_lanes=1
      shift
      ;;
    --root)
      repo_root="${2:-}"
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

if [[ "$format" != "tsv" ]]; then
  echo "error: unsupported format: $format (only 'tsv' is supported)" >&2
  exit 2
fi

if [[ -z "${AGENT_HOME:-}" ]]; then
  echo "error: AGENT_HOME is required" >&2
  exit 2
fi

if [[ -z "$repo_root" ]]; then
  repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
if [[ -z "$repo_root" || ! -d "$repo_root" ]]; then
  echo "error: repo root not found (run inside a git worktree or pass --root)" >&2
  exit 2
fi

cd "$repo_root"

if [[ ! -f "Cargo.toml" || ! -d "crates" ]]; then
  echo "error: expected nils-cli workspace root (missing Cargo.toml or crates/)" >&2
  exit 2
fi

audit_root="${AGENT_HOME}/out/workspace-shared-audit"
default_out_name="crate-matrix.tsv"
if [[ "$emit_lanes" -eq 1 ]]; then
  default_out_name="task-lanes.tsv"
fi
if [[ -z "$out_file" ]]; then
  out_file="${audit_root}/${default_out_name}"
fi
case "$out_file" in
  "$audit_root"/*) ;;
  *)
    echo "error: --out must be under ${audit_root}/" >&2
    exit 2
    ;;
esac

mkdir -p "$(dirname "$out_file")"
out_dir="$(dirname "$out_file")"

crate_matrix_tsv="$out_file"
crate_matrix_md="${out_dir}/crate-matrix.md"
hotspots_common_md="${out_dir}/hotspots-nils-common.md"
hotspots_term_md="${out_dir}/hotspots-nils-term.md"
hotspots_test_support_md="${out_dir}/hotspots-nils-test-support.md"
hotspots_index_tsv="${out_dir}/hotspots-index.tsv"
decision_rubric_md="${out_dir}/decision-rubric.md"
task_lanes_tsv="${out_dir}/task-lanes.tsv"

tmp_hotspots="$(mktemp "${TMPDIR:-/tmp}/workspace-shared-hotspots.XXXXXX")"
tmp_matrix="$(mktemp "${TMPDIR:-/tmp}/workspace-shared-matrix.XXXXXX")"

cleanup() {
  rm -f "$tmp_hotspots" "$tmp_matrix"
}
trap cleanup EXIT

sanitize_tsv_field() {
  local value="${1:-}"
  value="${value//$'\t'/ }"
  value="${value//$'\r'/}"
  value="${value//$'\n'/ }"
  printf '%s' "$value"
}

risk_score() {
  case "${1:-}" in
    high) printf '3' ;;
    medium) printf '2' ;;
    low) printf '1' ;;
    *) printf '0' ;;
  esac
}

owner_task_for() {
  local target="${1:-}"
  local signal="${2:-}"
  case "${target}:${signal}" in
    nils-common:manual_git_process|nils-common:manual_process_probe)
      printf 'Task 2.1'
      ;;
    nils-common:manual_no_color_logic|nils-common:manual_env_mutation)
      printf 'Task 2.2'
      ;;
    nils-common:manual_atomic_fs|nils-common:manual_secret_dir_resolution)
      printf 'Task 2.3'
      ;;
    nils-common:*)
      printf 'Task 2.4'
      ;;
    nils-term:*)
      printf 'Task 3.1'
      ;;
    nils-test-support:manual_git_test_setup)
      printf 'Task 4.3'
      ;;
    nils-test-support:manual_env_guard|nils-test-support:manual_path_prepend|nils-test-support:manual_exec_chmod|nils-test-support:manual_bin_resolution)
      printf 'Task 4.2'
      ;;
    nils-test-support:*)
      printf 'Task 4.1'
      ;;
    *)
      printf 'Task 5.1'
      ;;
  esac
}

requires_serialization() {
  local action="${1:-}"
  local signal="${2:-}"
  case "$action" in
    extend-shared|defer)
      return 0
      ;;
  esac
  case "$signal" in
    manual_atomic_fs|manual_secret_dir_resolution|manual_git_process)
      return 0
      ;;
  esac
  return 1
}

task5_owner_for() {
  local action="${1:-}"
  case "$action" in
    defer|keep-local)
      printf 'Task 5.1'
      ;;
    extend-shared)
      printf 'Task 5.2'
      ;;
    *)
      printf 'Task 5.3'
      ;;
  esac
}

task6_owner_for() {
  local action="${1:-}"
  case "$action" in
    migrate|extend-shared)
      printf 'Task 6.2'
      ;;
    *)
      printf 'Task 6.1'
      ;;
  esac
}

execution_lane_for() {
  local owner_task="${1:-}"
  local action="${2:-}"
  local signal="${3:-}"
  local lane_prefix
  case "$owner_task" in
    "Task 2."*) lane_prefix="s2-runtime" ;;
    "Task 3."*) lane_prefix="s3-progress" ;;
    "Task 4."*) lane_prefix="s4-test-support" ;;
    "Task 5."*) lane_prefix="s5-closeout" ;;
    "Task 6."*) lane_prefix="s6-docs" ;;
    *) lane_prefix="unassigned" ;;
  esac
  if requires_serialization "$action" "$signal"; then
    printf '%s-serialized' "$lane_prefix"
  else
    printf '%s-parallel' "$lane_prefix"
  fi
}

emit_lane_row() {
  local out_path="$1"
  local crate="$2"
  local target="$3"
  local signal="$4"
  local execution_lane="$5"
  local owner_task="$6"
  local phase="$7"
  local source_owner_task="$8"
  local action="$9"
  local serialization="${10}"

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(sanitize_tsv_field "$crate")" \
    "$(sanitize_tsv_field "$target")" \
    "$(sanitize_tsv_field "$signal")" \
    "$(sanitize_tsv_field "$execution_lane")" \
    "$(sanitize_tsv_field "$owner_task")" \
    "$(sanitize_tsv_field "$phase")" \
    "$(sanitize_tsv_field "$source_owner_task")" \
    "$(sanitize_tsv_field "$action")" \
    "$(sanitize_tsv_field "$serialization")" \
    >>"$out_path"
}

emit_task_lanes() {
  local matrix_tsv="$1"
  local lanes_tsv="$2"
  if [[ ! -s "$matrix_tsv" ]]; then
    echo "error: crate matrix input is missing or empty: $matrix_tsv" >&2
    exit 2
  fi

  local tmp_lanes
  tmp_lanes="$(mktemp "${TMPDIR:-/tmp}/workspace-shared-lanes.XXXXXX.tsv")"

  {
    printf 'crate\ttarget_shared_crate\tsignal\texecution_lane\towner_task\tphase\tsource_owner_task\tproposed_action\tserialization\n'
  } >"$tmp_lanes"

  awk -F '\t' 'NR>1 {print $1 "\t" $2 "\t" $3 "\t" $4 "\t" $5}' "$matrix_tsv" \
    | LC_ALL=C sort -t $'\t' -k1,1 -k2,2 -k3,3 -k4,4 -k5,5 \
    | while IFS=$'\t' read -r crate target signal action owner_task; do
      [[ -z "$crate" ]] && continue
      if [[ -z "$owner_task" ]]; then
        owner_task="$(owner_task_for "$target" "$signal")"
      fi
      if [[ -z "$owner_task" ]]; then
        owner_task="Task 5.1"
      fi

      local serialization
      if requires_serialization "$action" "$signal"; then
        serialization="serialized"
      else
        serialization="parallel-safe"
      fi

      local owner_task5 owner_task6
      owner_task5="$(task5_owner_for "$action")"
      owner_task6="$(task6_owner_for "$action")"

      emit_lane_row "$tmp_lanes" \
        "$crate" "$target" "$signal" \
        "$(execution_lane_for "$owner_task" "$action" "$signal")" \
        "$owner_task" \
        "implementation" \
        "$owner_task" \
        "$action" \
        "$serialization"

      emit_lane_row "$tmp_lanes" \
        "$crate" "$target" "$signal" \
        "$(execution_lane_for "$owner_task5" "$action" "$signal")" \
        "$owner_task5" \
        "closeout" \
        "$owner_task" \
        "$action" \
        "$serialization"

      emit_lane_row "$tmp_lanes" \
        "$crate" "$target" "$signal" \
        "$(execution_lane_for "$owner_task6" "$action" "$signal")" \
        "$owner_task6" \
        "docs" \
        "$owner_task" \
        "$action" \
        "$serialization"
    done

  emit_lane_row "$tmp_lanes" \
    "workspace" \
    "workspace" \
    "final-docs-gate" \
    "s6-docs-serialized" \
    "Task 6.3" \
    "final-gate" \
    "Task 6.1" \
    "verify" \
    "serialized"

  mv "$tmp_lanes" "$lanes_tsv"
}

emit_hotspot() {
  local target="$1"
  local risk="$2"
  local crate="$3"
  local file="$4"
  local signal="$5"
  local match_count="$6"
  local action="$7"
  local owner_task="$8"
  local rationale="$9"

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(sanitize_tsv_field "$target")" \
    "$(sanitize_tsv_field "$risk")" \
    "$(sanitize_tsv_field "$crate")" \
    "$(sanitize_tsv_field "$file")" \
    "$(sanitize_tsv_field "$signal")" \
    "$(sanitize_tsv_field "$match_count")" \
    "$(sanitize_tsv_field "$action")" \
    "$(sanitize_tsv_field "$owner_task")" \
    "$(sanitize_tsv_field "$rationale")" \
    >>"$tmp_hotspots"
}

dep_present() {
  local manifest="$1"
  local dep_name="$2"
  rg -q "^[[:space:]]*${dep_name}[[:space:]]*=" "$manifest" 2>/dev/null
}

scan_rule() {
  local crate="$1"
  local crate_dir="$2"
  local target="$3"
  local signal="$4"
  local regex="$5"
  local scope="$6"
  local risk="$7"
  local action="$8"
  local rationale="$9"

  local search_paths=()
  case "$scope" in
    src)
      [[ -d "${crate_dir}/src" ]] && search_paths+=("${crate_dir}/src")
      ;;
    tests)
      [[ -d "${crate_dir}/tests" ]] && search_paths+=("${crate_dir}/tests")
      ;;
    all)
      [[ -d "${crate_dir}/src" ]] && search_paths+=("${crate_dir}/src")
      [[ -d "${crate_dir}/tests" ]] && search_paths+=("${crate_dir}/tests")
      ;;
    cargo)
      search_paths+=("${crate_dir}/Cargo.toml")
      ;;
    *)
      echo "error: unknown scan scope: ${scope}" >&2
      exit 2
      ;;
  esac
  if [[ "${#search_paths[@]}" -eq 0 ]]; then
    return 0
  fi

  local matches
  matches="$(rg -n --no-heading -e "$regex" "${search_paths[@]}" 2>/dev/null || true)"
  if [[ -z "$matches" ]]; then
    return 0
  fi

  local owner_task
  owner_task="$(owner_task_for "$target" "$signal")"

  local files
  files="$(printf '%s\n' "$matches" | cut -d: -f1 | LC_ALL=C sort -u)"
  while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    local file_count
    file_count="$(printf '%s\n' "$matches" | awk -F ':' -v f="$file" '$1==f {n++} END {print n+0}')"
    emit_hotspot \
      "$target" \
      "$risk" \
      "$crate" \
      "$file" \
      "$signal" \
      "$file_count" \
      "$action" \
      "$owner_task" \
      "$rationale"
  done <<<"$files"
}

render_matrix_markdown() {
  local matrix_tsv="$1"
  local matrix_md="$2"

  {
    echo "# Workspace Shared-Crate Matrix"
    echo
    echo "| crate | target_shared_crate | signal | proposed_action | owner_task |"
    echo "| --- | --- | --- | --- | --- |"
    awk -F '\t' 'NR>1 {printf "| `%s` | `%s` | `%s` | `%s` | `%s` |\n", $1, $2, $3, $4, $5}' "$matrix_tsv"
    echo
    echo "## Proposed Action Totals"
    echo
    echo "| proposed_action | count |"
    echo "| --- | ---: |"
    awk -F '\t' 'NR>1 {count[$4]++} END {for (k in count) printf "| `%s` | %d |\n", k, count[k]}' "$matrix_tsv" | LC_ALL=C sort
  } >"$matrix_md"
}

render_hotspot_report() {
  local target="$1"
  local source_tsv="$2"
  local out_md="$3"

  local high_count medium_count low_count
  high_count="$(awk -F '\t' -v t="$target" 'NR>1 && $1==t && $2=="high" {n++} END {print n+0}' "$source_tsv")"
  medium_count="$(awk -F '\t' -v t="$target" 'NR>1 && $1==t && $2=="medium" {n++} END {print n+0}' "$source_tsv")"
  low_count="$(awk -F '\t' -v t="$target" 'NR>1 && $1==t && $2=="low" {n++} END {print n+0}' "$source_tsv")"

  {
    echo "# Hotspots: ${target}"
    echo
    echo "- Risk levels: high | medium | low"
    echo "- Each hotspot row includes rationale."
    echo "- Rows: $((high_count + medium_count + low_count))"
    echo "- high: ${high_count}, medium: ${medium_count}, low: ${low_count}"
    echo
    echo "| Risk | Crate | File | Signal | Matches | Proposed Action | Owner Task | Rationale |"
    echo "| --- | --- | --- | --- | ---: | --- | --- | --- |"
    awk -F '\t' -v t="$target" \
      'NR>1 && $1==t {printf "| %s | `%s` | `%s` | `%s` | %s | `%s` | `%s` | %s |\n", $2, $3, $4, $5, $6, $7, $8, $9}' \
      "$source_tsv"
  } >"$out_md"
}

render_decision_rubric() {
  local out_md="$1"
  cat >"$out_md" <<'RUBRIC'
# Workspace Shared-Crate Migration Decision Rubric

This rubric classifies findings into `migrate`, `extend-shared`, `keep-local`, or `defer` while preserving CLI contracts.

## Contract Guardrails (must hold before and after migration)

1. Output contract parity: human-readable text, warning style, and section ordering stay stable.
2. Exit semantics parity: success/failure exit codes and failure conditions stay stable.
3. Color policy parity: `--no-color` and `NO_COLOR=1` behavior stays stable.
4. JSON parity: machine-readable fields and error envelopes remain backward compatible.

## Test and Delivery Gates

1. Characterization-first for high-risk work before helper extraction or behavior rewiring.
2. Local changed-scope checks from `DEVELOPMENT.md` must pass before delivery.
3. GitHub required checks own full workspace and coverage gates before merge.
4. Any parity-sensitive exception must include explicit tests and rationale in the PR.

## Classification Rules

## `migrate`

- Use when a shared helper already exists and local code only duplicates plumbing.
- Typical signals: repeated git/process wrappers, env guard patterns, repeated PATH/bin/chmod helpers.
- Gate: no observable behavior contract changes.

## `extend-shared`

- Use when two or more crates duplicate a domain-neutral primitive and no suitable shared API exists yet.
- Typical signals: repeated atomic write/timestamp/hash primitives.
- Gate: add helper tests first, then migrate with characterization coverage.

## `keep-local`

- Use when behavior is intentionally crate-specific or parity-sensitive with user-facing text/exit semantics.
- Typical signals: crate-specific adapter messaging or domain-specific command composition.
- Gate: document why sharing would risk contract drift.

## `defer`

- Use when migration path is unclear, risk is high, or required characterization coverage is incomplete.
- Typical signals: auth-path edge cases, provider-specific secret-dir behavior, cross-crate sequencing blockers.
- Gate: track follow-up owner task before implementation.
RUBRIC
}

if [[ "$emit_lanes" -eq 1 ]]; then
  if [[ -z "$in_file" ]]; then
    echo "error: --in is required with --emit-lanes" >&2
    exit 2
  fi
  emit_task_lanes "$in_file" "$out_file"
  echo "wrote lanes:    $out_file"
  exit 0
fi

if [[ -n "$in_file" ]]; then
  echo "error: --in is only valid with --emit-lanes" >&2
  exit 2
fi

crates_list="$(find crates -mindepth 1 -maxdepth 1 -type d -print | sed 's#^crates/##' | LC_ALL=C sort)"
if [[ -z "$crates_list" ]]; then
  echo "error: no crates found under crates/" >&2
  exit 2
fi

: >"$tmp_hotspots"

while IFS= read -r crate; do
  [[ -z "$crate" ]] && continue
  crate_dir="crates/${crate}"
  manifest="${crate_dir}/Cargo.toml"

  scan_rule "$crate" "$crate_dir" "nils-common" "manual_git_process" \
    'Command::new\("git"\)' "all" "medium" "migrate" \
    "Manual git subprocess plumbing can converge on nils-common wrappers."
  scan_rule "$crate" "$crate_dir" "nils-common" "manual_process_probe" \
    'command_exists|split_paths|cmd_exists' "src" "medium" "migrate" \
    "Manual command probing can converge on nils-common process helpers."
  scan_rule "$crate" "$crate_dir" "nils-common" "manual_no_color_logic" \
    'NO_COLOR' "src" "medium" "migrate" \
    "Local NO_COLOR handling should stay centralized through shared env helpers."
  scan_rule "$crate" "$crate_dir" "nils-common" "manual_atomic_fs" \
    'write_atomic|write_timestamp|sha256_file' "src" "high" "extend-shared" \
    "Repeated file primitives indicate shared fs extension opportunities."
  scan_rule "$crate" "$crate_dir" "nils-common" "manual_secret_dir_resolution" \
    'resolve_secret_dir_from_env|CODEX_SECRET_DIR|GEMINI_SECRET_DIR' "src" "high" "defer" \
    "Secret-dir behavior is parity-sensitive and requires characterization-first handling."
  scan_rule "$crate" "$crate_dir" "nils-common" "manual_env_mutation" \
    'unsafe \{ std::env::(set_var|remove_var)' "src" "medium" "migrate" \
    "Raw env mutation should converge on guarded shared patterns."

  scan_rule "$crate" "$crate_dir" "nils-term" "direct_indicatif_usage" \
    'indicatif::|ProgressBar|MultiProgress' "src" "medium" "migrate" \
    "Direct progress implementation may be a nils-term migration candidate."
  scan_rule "$crate" "$crate_dir" "nils-term" "progress_flag_surface" \
    'progress|--progress|-p' "src" "low" "keep-local" \
    "Progress flags need explicit policy: adopt nils-term or keep no-progress behavior."

  scan_rule "$crate" "$crate_dir" "nils-test-support" "manual_env_guard" \
    'struct EnvGuard|struct EnvVarGuard|fn env_lock|GlobalStateLock' "all" "medium" "migrate" \
    "Custom env guard patterns can converge on nils-test-support primitives."
  scan_rule "$crate" "$crate_dir" "nils-test-support" "manual_path_prepend" \
    'std::env::var\("PATH"\)|path_with_stub|combined_path = format!' "all" "low" "migrate" \
    "Manual PATH prepend helpers can converge on nils-test-support cmd options."
  scan_rule "$crate" "$crate_dir" "nils-test-support" "manual_exec_chmod" \
    'set_mode\(0o755\)|set_permissions\(' "all" "low" "migrate" \
    "Manual executable chmod helpers can converge on nils-test-support fs helpers."
  scan_rule "$crate" "$crate_dir" "nils-test-support" "manual_bin_resolution" \
    'CARGO_BIN_EXE_' "all" "low" "migrate" \
    "Manual binary lookup can converge on nils-test-support bin helpers."
  scan_rule "$crate" "$crate_dir" "nils-test-support" "manual_git_test_setup" \
    'Command::new\("git"\)' "tests" "medium" "migrate" \
    "Repeated git setup in tests can converge on nils-test-support git helpers."

  if [[ -f "$manifest" ]]; then
    if dep_present "$manifest" "nils-common"; then
      emit_hotspot \
        "nils-common" \
        "low" \
        "$crate" \
        "$manifest" \
        "dependency_present" \
        "1" \
        "keep-local" \
        "$(owner_task_for "nils-common" "dependency_present")" \
        "Crate already depends on nils-common; validate adapter boundaries before migration."
    fi
    if dep_present "$manifest" "nils-term"; then
      emit_hotspot \
        "nils-term" \
        "low" \
        "$crate" \
        "$manifest" \
        "dependency_present" \
        "1" \
        "keep-local" \
        "$(owner_task_for "nils-term" "dependency_present")" \
        "Crate already depends on nils-term; verify progress policy is explicit."
    fi
    if dep_present "$manifest" "nils-test-support"; then
      emit_hotspot \
        "nils-test-support" \
        "low" \
        "$crate" \
        "$manifest" \
        "dependency_present" \
        "1" \
        "keep-local" \
        "$(owner_task_for "nils-test-support" "dependency_present")" \
        "Crate already depends on nils-test-support; favor helper reuse over local test utilities."
    fi
  fi
done <<<"$crates_list"

{
  printf 'target_shared_crate\trisk\tcrate\tfile\tsignal\tmatch_count\tproposed_action\towner_task\trationale\n'
  if [[ -s "$tmp_hotspots" ]]; then
    awk -F '\t' '
      {
        rr = ($2=="high" ? 3 : ($2=="medium" ? 2 : 1))
        printf "%s\t%d\t%s\n", $1, rr, $0
      }
    ' "$tmp_hotspots" \
      | LC_ALL=C sort -t $'\t' -k1,1 -k2,2nr -k5,5 -k6,6 -k7,7 -k3,3 -k4,4 \
      | cut -f3-
  fi
} >"$hotspots_index_tsv"

{
  printf 'crate\ttarget_shared_crate\tsignal\tproposed_action\towner_task\n'
  while IFS= read -r crate; do
    [[ -z "$crate" ]] && continue

    best_line="$(
      awk -F '\t' -v c="$crate" '
        $3==c {
          rr = ($2=="high" ? 3 : ($2=="medium" ? 2 : 1))
          printf "%d\t%09d\t%s\n", rr, $6, $0
        }
      ' "$hotspots_index_tsv" \
        | LC_ALL=C sort -t $'\t' -k1,1nr -k2,2nr -k3,3 -k5,5 -k6,6 \
        | head -n1 \
        | cut -f3-
    )"

    if [[ -n "$best_line" ]]; then
      IFS=$'\t' read -r target risk crate_name file signal match_count action owner_task rationale <<<"$best_line"
      printf '%s\t%s\t%s\t%s\t%s\n' \
        "$crate_name" \
        "$target" \
        "$signal" \
        "$action" \
        "$owner_task"
    else
      default_target="nils-common"
      default_signal="no-hotspot-detected"
      default_action="defer"
      if [[ "$crate" == "nils-common" || "$crate" == "nils-term" || "$crate" == "nils-test-support" ]]; then
        default_target="$crate"
        default_signal="shared-owner"
        default_action="keep-local"
      fi
      printf '%s\t%s\t%s\t%s\t%s\n' \
        "$crate" \
        "$default_target" \
        "$default_signal" \
        "$default_action" \
        "$(owner_task_for "$default_target" "$default_signal")"
    fi
  done <<<"$crates_list"
} >"$tmp_matrix"

cp "$tmp_matrix" "$crate_matrix_tsv"
emit_task_lanes "$crate_matrix_tsv" "$task_lanes_tsv"

render_matrix_markdown "$crate_matrix_tsv" "$crate_matrix_md"
render_hotspot_report "nils-common" "$hotspots_index_tsv" "$hotspots_common_md"
render_hotspot_report "nils-term" "$hotspots_index_tsv" "$hotspots_term_md"
render_hotspot_report "nils-test-support" "$hotspots_index_tsv" "$hotspots_test_support_md"
render_decision_rubric "$decision_rubric_md"

echo "wrote matrix:   $crate_matrix_tsv"
echo "wrote matrix md: $crate_matrix_md"
echo "wrote hotspots: $hotspots_common_md"
echo "wrote hotspots: $hotspots_term_md"
echo "wrote hotspots: $hotspots_test_support_md"
echo "wrote index:    $hotspots_index_tsv"
echo "wrote rubric:   $decision_rubric_md"
echo "wrote lanes:    $task_lanes_tsv"
