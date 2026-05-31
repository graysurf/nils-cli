#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  workspace-test-stale-audit.sh [--format tsv] [--out <stale-tests.tsv>] [--root <repo>]

Builds a deterministic stale-test inventory for workspace crates and emits:
  - stale-tests.tsv         (normalized candidate rows)
  - stale-tests.md          (crate summary + signal totals)
  - helper-callgraph.tsv    (helper definition -> callsite fanout graph)
  - helper-orphans.tsv      (callgraph subset where callsite_count=0)
  - decision-rubric.md      (deterministic stale-test decision contract)
  - contract-allowlist.tsv  (contract/parity protection selectors)
  - crate-tiers.tsv         (candidate-tier + owning-task assignments)
  - execution-manifest.md   (tiered, dependency-aware execution plan)

Output columns (TSV):
  id  crate  path  symbol_or_test  signal  proposed_action  confidence

Options:
  --format <tsv>   Output format (currently only `tsv` is supported)
  --out <file>     Target output TSV path (must be under $AGENT_HOME/out/workspace-test-cleanup/)
  --root <dir>     Repo root (defaults to current git worktree root)
  -h, --help       Show this help
USAGE
}

format="tsv"
out_file=""
repo_root=""

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

if ! command -v rg >/dev/null 2>&1; then
  echo "error: ripgrep (rg) is required" >&2
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

audit_root="${AGENT_HOME}/out/workspace-test-cleanup"
if [[ -z "$out_file" ]]; then
  out_file="${audit_root}/stale-tests.tsv"
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
summary_file="${out_dir}/stale-tests.md"
helper_callgraph_file="${out_dir}/helper-callgraph.tsv"
helper_orphans_file="${out_dir}/helper-orphans.tsv"
decision_rubric_file="${out_dir}/decision-rubric.md"
contract_allowlist_file="${out_dir}/contract-allowlist.tsv"
crate_tiers_file="${out_dir}/crate-tiers.tsv"
execution_manifest_file="${out_dir}/execution-manifest.md"

tmp_rows="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.rows.XXXXXX.tsv")"
tmp_sorted="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.sorted.XXXXXX.tsv")"
tmp_crates="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.crates.XXXXXX.list")"
tmp_helpers="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.helpers.XXXXXX.tsv")"
tmp_helpers_sorted="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.helpers.sorted.XXXXXX.tsv")"
tmp_allowlist_raw="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.allowlist.XXXXXX.tsv")"
tmp_allowlist_sorted="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.allowlist.sorted.XXXXXX.tsv")"
tmp_crate_metrics="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.metrics.XXXXXX.tsv")"
tmp_high_overlap="$(mktemp "${TMPDIR:-/tmp}/workspace-stale-tests.high-overlap.XXXXXX.tsv")"

# Frozen serial execution order from Sprint 3 Task 3.1.
frozen_serial_crates=(
  git-cli
  agent-docs
  macos-agent
  fzf-cli
  memo
)

cleanup() {
  rm -f \
    "$tmp_rows" \
    "$tmp_sorted" \
    "$tmp_crates" \
    "$tmp_helpers" \
    "$tmp_helpers_sorted" \
    "$tmp_allowlist_raw" \
    "$tmp_allowlist_sorted" \
    "$tmp_crate_metrics" \
    "$tmp_high_overlap"
}
trap cleanup EXIT

sanitize_tsv_field() {
  local value="${1:-}"
  value="${value//$'\t'/ }"
  value="${value//$'\r'/}"
  value="${value//$'\n'/ }"
  printf '%s' "$value"
}

is_helper_module_path() {
  local rel_path="${1:-}"
  case "$rel_path" in
    */tests/common.rs|*/tests/utils.rs|*/tests/harness.rs|*/tests/support/*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

add_row() {
  local crate="${1:-}"
  local rel_path="${2:-}"
  local symbol_or_test="${3:-}"
  local signal="${4:-}"
  local proposed_action="${5:-}"
  local confidence="${6:-}"

  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(sanitize_tsv_field "$crate")" \
    "$(sanitize_tsv_field "$rel_path")" \
    "$(sanitize_tsv_field "$symbol_or_test")" \
    "$(sanitize_tsv_field "$signal")" \
    "$(sanitize_tsv_field "$proposed_action")" \
    "$(sanitize_tsv_field "$confidence")" \
    >>"$tmp_rows"
}

add_helper_row() {
  local crate="${1:-}"
  local rel_path="${2:-}"
  local helper_name="${3:-}"
  local callsite_count="${4:-}"
  local review="${5:-}"
  local note="${6:-}"

  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(sanitize_tsv_field "$crate")" \
    "$(sanitize_tsv_field "$rel_path")" \
    "$(sanitize_tsv_field "$helper_name")" \
    "$(sanitize_tsv_field "$callsite_count")" \
    "$(sanitize_tsv_field "$review")" \
    "$(sanitize_tsv_field "$note")" \
    >>"$tmp_helpers"
}

helper_needs_manual_review() {
  local helper_name="${1:-}"
  local tests_dir="${2:-}"
  local src_dir="${3:-}"
  local test_file="${4:-}"
  local fanout="${5:-0}"

  if (( fanout != 0 )); then
    printf 'auto'
    return 0
  fi

  if rg -q -e 'macro_rules!|#[[:space:]]*macro_export|include!\(' "$test_file" 2>/dev/null; then
    printf 'manual-review'
    return 0
  fi

  if [[ -d "$tests_dir" ]] && rg -q -e "\"${helper_name}\"" "$tests_dir" 2>/dev/null; then
    printf 'manual-review'
    return 0
  fi

  if [[ -d "$src_dir" ]] && rg -q -e "\"${helper_name}\"" "$src_dir" 2>/dev/null; then
    printf 'manual-review'
    return 0
  fi

  printf 'auto'
}

count_occurrences() {
  local root_dir="${1:-}"
  local regex="${2:-}"
  if [[ -z "$root_dir" || ! -d "$root_dir" ]]; then
    printf '0'
    return 0
  fi
  local count
  count="$(rg -n --glob '*.rs' -e "$regex" "$root_dir" 2>/dev/null | wc -l | tr -d ' ')"
  printf '%s' "${count:-0}"
}

scan_crate() {
  local crate_dir="${1:-}"
  local crate tests_dir src_dir

  crate="$(basename "$crate_dir")"
  tests_dir="${crate_dir}/tests"
  src_dir="${crate_dir}/src"

  if [[ ! -d "$tests_dir" ]]; then
    return 0
  fi

  while IFS= read -r test_file; do
    local rel_path module_name
    rel_path="${test_file#$repo_root/}"
    module_name="$(basename "$test_file" .rs)"

    add_row "$crate" "$rel_path" "$module_name" "test_module" "keep" "0.20"

    if printf '%s\n' "$rel_path" | rg -qi 'deprecated|obsolete|legacy'; then
      add_row "$crate" "$rel_path" "path-token" "deprecated_path_marker" "rewrite" "0.75"
    fi

    local allow_hits allow_hit allow_line
    allow_hits="$(rg -n --no-heading -e 'allow[[:space:]]*\([[:space:]]*dead_code[[:space:]]*\)' "$test_file" 2>/dev/null || true)"
    if [[ -n "$allow_hits" ]]; then
      while IFS= read -r allow_hit; do
        [[ -z "$allow_hit" ]] && continue
        allow_line="${allow_hit%%:*}"
        add_row "$crate" "$rel_path" "line:${allow_line}" "allow_dead_code" "rewrite" "0.90"
      done <<<"$allow_hits"
    fi

    local marker_hits marker_hit marker_line
    marker_hits="$(rg -n --no-heading -i -e 'deprecated|obsolete|legacy|todo[^[:alnum:]]*(remove|cleanup)' "$test_file" 2>/dev/null || true)"
    if [[ -n "$marker_hits" ]]; then
      while IFS= read -r marker_hit; do
        [[ -z "$marker_hit" ]] && continue
        marker_line="${marker_hit%%:*}"
        add_row "$crate" "$rel_path" "line:${marker_line}" "deprecated_path_marker" "rewrite" "0.65"
      done <<<"$marker_hits"
    fi

    if is_helper_module_path "$rel_path"; then
      local fn_names fn_name
      fn_names="$(sed -nE 's/^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*\(.*/\4/p' "$test_file" | LC_ALL=C sort -u)"
      if [[ -n "$fn_names" ]]; then
        while IFS= read -r fn_name; do
          [[ -z "$fn_name" ]] && continue

          local tests_hits src_hits def_hits total_hits fanout
          tests_hits="$(count_occurrences "$tests_dir" "\\b${fn_name}[[:space:]]*\\(")"
          src_hits="$(count_occurrences "$src_dir" "\\b${fn_name}[[:space:]]*\\(")"
          def_hits="$(rg -n --no-heading -e "^[[:space:]]*(pub(\\([^)]*\\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+${fn_name}[[:space:]]*\\(" "$test_file" 2>/dev/null | wc -l | tr -d ' ')"
          total_hits=$((tests_hits + src_hits))
          fanout=$((total_hits - def_hits))
          if (( fanout < 0 )); then
            fanout=0
          fi

          local review_mode review_note
          review_mode="$(helper_needs_manual_review "$fn_name" "$tests_dir" "$src_dir" "$test_file" "$fanout")"
          review_note="direct-call-scan"
          if [[ "$review_mode" == "manual-review" ]]; then
            review_note="macro-or-reflection-risk"
          fi
          add_helper_row "$crate" "$rel_path" "$fn_name" "$fanout" "$review_mode" "$review_note"

          if (( fanout == 0 )) && [[ "$review_mode" == "manual-review" ]]; then
            add_row "$crate" "$rel_path" "${fn_name} (fanout=${fanout})" "helper_fanout" "defer" "0.55"
          elif (( fanout == 0 )); then
            add_row "$crate" "$rel_path" "${fn_name} (fanout=${fanout})" "helper_fanout" "remove" "0.95"
          else
            add_row "$crate" "$rel_path" "${fn_name} (fanout=${fanout})" "helper_fanout" "keep" "0.45"
          fi
        done <<<"$fn_names"
      fi
    fi
  done < <(find "$tests_dir" -type f -name '*.rs' -print | LC_ALL=C sort)
}

write_normalized_tsv() {
  local destination="${1:-}"
  local row_count=0

  printf 'id\tcrate\tpath\tsymbol_or_test\tsignal\tproposed_action\tconfidence\n' >"$destination"

  while IFS=$'\t' read -r crate rel_path symbol signal action confidence; do
    row_count=$((row_count + 1))
    printf 'stale-%05d\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$row_count" \
      "$crate" \
      "$rel_path" \
      "$symbol" \
      "$signal" \
      "$action" \
      "$confidence" \
      >>"$destination"
  done <"$tmp_sorted"
}

write_helper_callgraph() {
  local callgraph_path="${1:-}"
  local orphans_path="${2:-}"
  local row_count=0

  printf 'id\tcrate\thelper_name\tcallsite_count\tpath\treview\tnote\n' >"$callgraph_path"
  printf 'id\tcrate\tpath\thelper_name\treview\taction\n' >"$orphans_path"

  if [[ ! -s "$tmp_helpers_sorted" ]]; then
    return 0
  fi

  while IFS=$'\t' read -r crate rel_path helper_name callsite_count review note; do
    row_count=$((row_count + 1))
    local helper_id
    helper_id="$(printf 'helper-%05d' "$row_count")"

    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$helper_id" \
      "$crate" \
      "$helper_name" \
      "$callsite_count" \
      "$rel_path" \
      "$review" \
      "$note" \
      >>"$callgraph_path"

    if [[ "$callsite_count" == "0" ]]; then
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$helper_id" \
        "$crate" \
        "$rel_path" \
        "$helper_name" \
        "$review" \
        "candidate-remove" \
        >>"$orphans_path"
    fi
  done <"$tmp_helpers_sorted"
}

select_allowlist_kind() {
  local rel_path="${1:-}"
  case "$rel_path" in
    *contract*.rs|*runtime_*_contract*.rs)
      printf 'contract'
      ;;
    *parity*.rs|*characterization*.rs)
      printf 'parity'
      ;;
    *json*.rs|*to_json*.rs)
      printf 'json'
      ;;
    *help_outside_repo.rs|*completion_outside_repo.rs)
      printf 'exit-code'
      ;;
    *)
      printf ''
      ;;
  esac
}

seed_contract_allowlist() {
  : >"$tmp_allowlist_raw"
  while IFS= read -r test_file; do
    local rel_path kind guard_reason
    rel_path="${test_file#$repo_root/}"
    kind="$(select_allowlist_kind "$rel_path")"
    if [[ -z "$kind" ]]; then
      continue
    fi
    case "$kind" in
      contract)
        guard_reason="contract behavior surface; replacement test required before deletion"
        ;;
      parity)
        guard_reason="parity guardrail; preserve legacy output and warnings"
        ;;
      json)
        guard_reason="json schema compatibility; keep machine-contract assertions"
        ;;
      exit-code)
        guard_reason="cli exit code/help contract; preserve status behavior"
        ;;
      *)
        guard_reason="manual guard"
        ;;
    esac
    printf '%s\t%s\t%s\n' \
      "$kind" \
      "$rel_path" \
      "$guard_reason" \
      >>"$tmp_allowlist_raw"
  done < <(find crates -type f -path '*/tests/*.rs' -print | LC_ALL=C sort)
}

write_contract_allowlist() {
  local destination="${1:-}"
  local row_count=0

  printf 'id\tselector_type\tselector\tguard_reason\trequired_action\n' >"$destination"
  if [[ ! -s "$tmp_allowlist_sorted" ]]; then
    return 0
  fi

  while IFS=$'\t' read -r kind rel_path guard_reason; do
    row_count=$((row_count + 1))
    printf 'allow-%04d\tfile\t%s\t%s\tkeep-or-rewrite-with-equivalent-coverage\n' \
      "$row_count" \
      "$rel_path" \
      "$guard_reason" \
      >>"$destination"
  done <"$tmp_allowlist_sorted"
}

is_contract_protected_path() {
  local rel_path="${1:-}"
  awk -F '\t' -v rel="$rel_path" 'NR>1 && $3==rel {found=1; exit} END {if (found) print "yes"; else print "no"}' "$contract_allowlist_file"
}

tier_for_candidate() {
  local rel_path="${1:-}"
  local signal="${2:-}"
  local action="${3:-}"
  local confidence="${4:-}"

  local protected
  protected="$(is_contract_protected_path "$rel_path")"
  if [[ "$protected" == "yes" ]]; then
    printf 'high-risk'
    return 0
  fi

  case "$signal" in
    allow_dead_code)
      printf 'medium'
      return 0
      ;;
    deprecated_path_marker)
      printf 'high-risk'
      return 0
      ;;
    helper_fanout)
      if [[ "$action" == "remove" ]]; then
        printf 'safe'
      elif [[ "$action" == "defer" ]]; then
        printf 'high-risk'
      else
        printf 'medium'
      fi
      return 0
      ;;
    *)
      ;;
  esac

  awk -v c="$confidence" 'BEGIN {if (c+0 >= 0.85) print "safe"; else print "medium"}'
}

owner_task_for_tier() {
  local tier="${1:-}"
  case "$tier" in
    safe) printf 'Task 2.1' ;;
    medium) printf 'Task 2.2' ;;
    high-risk) printf 'Task 2.5' ;;
    *) printf 'Task 2.5' ;;
  esac
}

build_crate_metrics() {
  : >"$tmp_crate_metrics"
  while IFS= read -r crate_dir; do
    local crate
    crate="$(basename "$crate_dir")"
    local total helper allow deprecated score
    total="$(awk -F '\t' -v crate="$crate" 'NR>1 && $2==crate {n++} END {print n+0}' "$out_file")"
    helper="$(awk -F '\t' -v crate="$crate" 'NR>1 && $2==crate && $5=="helper_fanout" {n++} END {print n+0}' "$out_file")"
    allow="$(awk -F '\t' -v crate="$crate" 'NR>1 && $2==crate && $5=="allow_dead_code" {n++} END {print n+0}' "$out_file")"
    deprecated="$(awk -F '\t' -v crate="$crate" 'NR>1 && $2==crate && $5=="deprecated_path_marker" {n++} END {print n+0}' "$out_file")"
    score=$((total + helper * 2 + allow * 3 + deprecated * 2))
    printf '%s\t%s\t%s\t%s\t%s\n' "$crate" "$total" "$helper" "$allow" "$score" >>"$tmp_crate_metrics"
  done <"$tmp_crates"
}

select_high_overlap() {
  : >"$tmp_high_overlap"
  local crate metrics total helper allow score
  for crate in "${frozen_serial_crates[@]}"; do
    metrics="$(awk -F '\t' -v crate="$crate" '
      $1==crate {
        print $2 "\t" $3 "\t" $4 "\t" $5
        found=1
        exit
      }
      END {
        if (!found) {
          print "0\t0\t0\t0"
        }
      }
    ' "$tmp_crate_metrics")"
    IFS=$'\t' read -r total helper allow score <<<"$metrics"
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "$crate" \
      "${total:-0}" \
      "${helper:-0}" \
      "${allow:-0}" \
      "${score:-0}" \
      >>"$tmp_high_overlap"
  done
}

serial_group_for_crate() {
  local crate="${1:-}"
  local order=0
  local serial_crate
  for serial_crate in "${frozen_serial_crates[@]}"; do
    order=$((order + 1))
    if [[ "$crate" == "$serial_crate" ]]; then
      printf 'serial-%d' "$order"
      return 0
    fi
  done
  printf 'parallel'
}

write_crate_tiers() {
  local destination="${1:-}"
  printf 'candidate_id\ttier\towner_task\tcrate\tsignal\tproposed_action\tserial_group\n' >"$destination"

  while IFS=$'\t' read -r candidate_id crate rel_path symbol signal action confidence; do
    local tier owner_task serial_group
    tier="$(tier_for_candidate "$rel_path" "$signal" "$action" "$confidence")"
    owner_task="$(owner_task_for_tier "$tier")"
    serial_group="$(serial_group_for_crate "$crate")"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$candidate_id" \
      "$tier" \
      "$owner_task" \
      "$crate" \
      "$signal" \
      "$action" \
      "$serial_group" \
      >>"$destination"
  done < <(awk -F '\t' 'NR>1 {print}' "$out_file")
}

render_decision_rubric() {
  local destination="${1:-}"
  {
    echo "# Workspace Stale-Test Decision Rubric"
    echo
    echo "## Decision Modes"
    echo
    echo "- \`remove\`: helper has deterministic zero fanout and no contract/parity guard."
    echo "- \`keep\`: helper/test module is still referenced or intentionally shared."
    echo "- \`rewrite\`: stale pattern is valid but implementation must be modernized."
    echo "- \`defer\`: evidence is ambiguous (macro/reflection/hidden indirection); require manual review."
    echo
    echo "## Removal Prerequisites"
    echo
    echo "1. Candidate is not present in \`contract-allowlist.tsv\`."
    echo "2. Candidate has deterministic signal evidence (\`helper_fanout=0\` or equivalent)."
    echo "3. A replacement test exists when behavior affects CLI contract, parity output, exit code, or json."
    echo "4. Tier owner task is assigned in \`crate-tiers.tsv\`."
    echo
    echo "## Stop Conditions"
    echo
    echo "- Stop removal when candidate path is contract/parity protected."
    echo "- Stop removal when confidence is below deterministic threshold or marked \`manual-review\`."
    echo "- Stop removal when replacement coverage is missing for json or exit code behavior."
    echo
    echo "## Contract Protection"
    echo
    echo "- \`contract\`: keep behavior-level assertions unless equivalent tests are added first."
    echo "- \`parity\`: preserve user-visible wording and warnings for parity-targeted CLIs."
    echo "- \`json\`: preserve machine-consumable envelopes and fields."
    echo "- \`exit code\`: preserve status-code and failure-mode semantics."
  } >"$destination"
}

count_tier_for_crate() {
  local crate="${1:-}"
  local tier="${2:-}"
  awk -F '\t' -v crate="$crate" -v tier="$tier" 'NR>1 && $4==crate && $2==tier {n++} END {print n+0}' "$crate_tiers_file"
}

render_execution_manifest() {
  local destination="${1:-}"
  {
    echo "# Workspace Test Cleanup Sprint Execution Manifest"
    echo
    echo "## Inputs"
    echo
    echo "- stale inventory: \`$out_file\`"
    echo "- helper callgraph: \`$helper_callgraph_file\`"
    echo "- contract allowlist: \`$contract_allowlist_file\`"
    echo "- tier map: \`$crate_tiers_file\`"
    echo
    echo "## Tier Lanes"
    echo
    echo "1. \`safe\` lane -> Task 2.1 (parallel allowed unless serial group set)."
    echo "2. \`medium\` lane -> Task 2.2 (parallel allowed unless serial group set)."
    echo "3. \`high-risk\` lane -> Task 2.5 (execute after safe/medium plus contract guard checks)."
    echo
    echo "## Frozen Serialized Crates"
    echo
    echo "Order is frozen by Sprint 3 Task 3.1 to avoid serial-group drift while cleanup lanes run in parallel."
    echo
    echo "| Order | Crate | Candidates | Helper Signals | allow(dead_code) | Score |"
    echo "| ---: | --- | ---: | ---: | ---: | ---: |"
    local order=0
    while IFS=$'\t' read -r crate total helper allow score; do
      order=$((order + 1))
      printf '| %d | %s | %s | %s | %s | %s |\n' "$order" "$crate" "$total" "$helper" "$allow" "$score"
    done <"$tmp_high_overlap"
    if [[ "$order" -eq 0 ]]; then
      echo "| 1 | none | 0 | 0 | 0 | 0 |"
    fi
    echo
    echo "## Crate Tier Assignment Summary"
    echo
    echo "| Crate | Safe | Medium | High-Risk | Serial Group |"
    echo "| --- | ---: | ---: | ---: | --- |"
    while IFS= read -r crate_dir; do
      local crate serial_group safe_count medium_count high_count
      crate="$(basename "$crate_dir")"
      serial_group="$(serial_group_for_crate "$crate")"
      safe_count="$(count_tier_for_crate "$crate" "safe")"
      medium_count="$(count_tier_for_crate "$crate" "medium")"
      high_count="$(count_tier_for_crate "$crate" "high-risk")"
      printf '| %s | %s | %s | %s | %s |\n' \
        "$crate" \
        "$safe_count" \
        "$medium_count" \
        "$high_count" \
        "$serial_group"
    done <"$tmp_crates"
    echo
    echo "## Execution Gates"
    echo
    echo "- Gate 1: validate \`helper-callgraph.tsv\` and \`helper-orphans.tsv\` consistency."
    echo "- Gate 2: enforce contract allowlist before any removal."
    echo "- Gate 3: run required lint/test checks before merge."
  } >"$destination"
}

count_rows_for_crate() {
  local crate="${1:-}"
  awk -F '\t' -v crate="$crate" 'NR>1 && $2==crate {n++} END {print n+0}' "$out_file"
}

count_signal_for_crate() {
  local crate="${1:-}"
  local signal="${2:-}"
  awk -F '\t' -v crate="$crate" -v signal="$signal" 'NR>1 && $2==crate && $5==signal {n++} END {print n+0}' "$out_file"
}

render_summary_markdown() {
  local summary_path="${1:-}"
  local total_rows
  total_rows="$(awk -F '\t' 'NR>1 {n++} END {print n+0}' "$out_file")"

  {
    echo "# Workspace Stale Test Audit"
    echo
    echo "- Repo root: \`$repo_root\`"
    echo "- Inventory: \`$out_file\`"
    echo "- Total candidates: $total_rows"
    echo
    echo "## Crate Summary"
    echo
    echo "| Crate | Test Modules | Helper Functions | allow(dead_code) | Deprecated Markers | Candidates |"
    echo "| --- | ---: | ---: | ---: | ---: | ---: |"
    while IFS= read -r crate_dir; do
      local crate
      crate="$(basename "$crate_dir")"
      local test_modules helper_functions allow_dead_code deprecated_markers candidates
      test_modules="$(count_signal_for_crate "$crate" "test_module")"
      helper_functions="$(count_signal_for_crate "$crate" "helper_fanout")"
      allow_dead_code="$(count_signal_for_crate "$crate" "allow_dead_code")"
      deprecated_markers="$(count_signal_for_crate "$crate" "deprecated_path_marker")"
      candidates="$(count_rows_for_crate "$crate")"
      printf '| %s | %s | %s | %s | %s | %s |\n' \
        "$crate" \
        "$test_modules" \
        "$helper_functions" \
        "$allow_dead_code" \
        "$deprecated_markers" \
        "$candidates"
    done <"$tmp_crates"
    echo
    echo "## Signal Totals"
    echo
    echo "| Signal | Count |"
    echo "| --- | ---: |"
    awk -F '\t' 'NR>1 {count[$5]++} END {for (k in count) printf "%s\t%d\n", k, count[k]}' "$out_file" | LC_ALL=C sort | while IFS=$'\t' read -r signal count; do
      printf '| %s | %s |\n' "$signal" "$count"
    done
  } >"$summary_path"
}

find crates -mindepth 1 -maxdepth 1 -type d -print | LC_ALL=C sort >"$tmp_crates"
while IFS= read -r crate_dir; do
  scan_crate "$crate_dir"
done <"$tmp_crates"

if [[ -s "$tmp_rows" ]]; then
  LC_ALL=C sort -u -t $'\t' -k1,1 -k2,2 -k3,3 -k4,4 -k5,5 -k6,6 "$tmp_rows" >"$tmp_sorted"
else
  : >"$tmp_sorted"
fi

if [[ -s "$tmp_helpers" ]]; then
  LC_ALL=C sort -u -t $'\t' -k1,1 -k2,2 -k3,3 "$tmp_helpers" >"$tmp_helpers_sorted"
else
  : >"$tmp_helpers_sorted"
fi

seed_contract_allowlist
if [[ -s "$tmp_allowlist_raw" ]]; then
  LC_ALL=C sort -u -t $'\t' -k1,1 -k2,2 "$tmp_allowlist_raw" >"$tmp_allowlist_sorted"
else
  : >"$tmp_allowlist_sorted"
fi

write_normalized_tsv "$out_file"
render_summary_markdown "$summary_file"
write_helper_callgraph "$helper_callgraph_file" "$helper_orphans_file"
write_contract_allowlist "$contract_allowlist_file"
render_decision_rubric "$decision_rubric_file"
build_crate_metrics
select_high_overlap
write_crate_tiers "$crate_tiers_file"
render_execution_manifest "$execution_manifest_file"

echo "wrote inventory: $out_file"
echo "wrote summary:   $summary_file"
echo "wrote callgraph: $helper_callgraph_file"
echo "wrote orphans:   $helper_orphans_file"
echo "wrote rubric:    $decision_rubric_file"
echo "wrote allowlist: $contract_allowlist_file"
echo "wrote tiers:     $crate_tiers_file"
echo "wrote manifest:  $execution_manifest_file"
