#!/usr/bin/env -S zsh -f

setopt pipe_fail nounset

SCRIPT_PATH="${0:A}"
REPO_ROOT="${SCRIPT_PATH:h:h:h}"
MATRIX_FILE="$REPO_ROOT/docs/specs/completion-coverage-matrix-v1.md"
ZSH_COMPLETIONS_DIR="$REPO_ROOT/completions/zsh"
ALIASES_FILE="$ZSH_COMPLETIONS_DIR/aliases.zsh"

fail() {
  print -u2 -r -- "FAIL: $*"
  exit 1
}

[[ -f "$MATRIX_FILE" ]] || fail "missing completion coverage matrix: $MATRIX_FILE"
[[ -d "$ZSH_COMPLETIONS_DIR" ]] || fail "missing zsh completions directory: $ZSH_COMPLETIONS_DIR"
[[ -f "$ALIASES_FILE" ]] || fail "missing zsh aliases file: $ALIASES_FILE"

typeset -ga _nils_compdef_calls
compdef() {
  _nils_compdef_calls+=("$*")
}

has_compdef_mapping() {
  local function_name="$1"
  local token="$2"
  local start_index="$3"
  local idx
  local call

  for (( idx = start_index + 1; idx <= ${#_nils_compdef_calls[@]}; idx++ )); do
    call="${_nils_compdef_calls[$idx]}"
    if [[ " $call " == *" $function_name "* && " $call " == *" $token "* ]]; then
      return 0
    fi
  done

  return 1
}

has_any_compdef_mapping() {
  local function_name="$1"
  local token="$2"
  local call

  for call in "${_nils_compdef_calls[@]}"; do
    if [[ " $call " == *" $function_name "* && " $call " == *" $token "* ]]; then
      return 0
    fi
  done

  return 1
}

has_compdef_header_token() {
  local comp_file="$1"
  local token="$2"

  awk -v token="$token" '
/^[[:space:]]*#compdef[[:space:]]+/ {
  for (i = 2; i <= NF; i++) {
    if ($i == token) {
      found = 1
    }
  }
}
END {
  exit(found ? 0 : 1)
}
' "$comp_file"
}

typeset -a matrix_rows
matrix_rows=("${(@f)$(awk '
/^[[:space:]]*\| `[^`]+` +\|/ {
  split($0, cells, "|")
  if (length(cells) < 6) {
    next
  }

  binary = cells[2]
  obligation = cells[3]
  zsh_cell = cells[4]
  bash_cell = cells[5]
  alias_cell = cells[6]
  enforcement_cell = (length(cells) >= 7) ? cells[7] : ""

  gsub(/^[[:space:]]+|[[:space:]]+$/, "", binary)
  gsub(/^[[:space:]]+|[[:space:]]+$/, "", obligation)
  gsub(/^[[:space:]]+|[[:space:]]+$/, "", zsh_cell)
  gsub(/^[[:space:]]+|[[:space:]]+$/, "", bash_cell)
  gsub(/^[[:space:]]+|[[:space:]]+$/, "", alias_cell)

  gsub(/`/, "", binary)
  gsub(/`/, "", obligation)

  alias_required = (alias_cell ~ /`required`/) ? "1" : "0"
  alias_prefix = ""
  if (match(alias_cell, /`[[:alnum:]-]+\*`/)) {
    alias_pattern = substr(alias_cell, RSTART + 1, RLENGTH - 2)
    sub(/\*$/, "", alias_pattern)
    alias_prefix = alias_pattern
  }

  engine = (enforcement_cell ~ /completion_engine=dynamic([;`\t ]|$)/) ? "dynamic" : "static"

  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n", binary, obligation, zsh_cell, bash_cell, alias_required, alias_prefix, engine
}
' "$MATRIX_FILE")}")

(( ${#matrix_rows[@]} > 0 )) || fail "no matrix rows found in $MATRIX_FILE"

typeset -A seen_binaries
typeset -A required_alias_prefix_by_binary
integer required_count=0

for row in "${matrix_rows[@]}"; do
  # Split on tabs WITHOUT collapsing empties: `read` merges consecutive IFS
  # delimiters, which drops an empty `alias_prefix` and shifts `engine` off the
  # end — silently disabling the dynamic-engine assertion for dynamic CLIs that
  # have no alias family (e.g. agent-memory, secrets). `(@ps:\t:)` preserves the
  # empty field so every column lands in its intended slot.
  local -a _row_fields=("${(@ps:\t:)row}")
  binary="${_row_fields[1]-}"
  obligation="${_row_fields[2]-}"
  zsh_cell="${_row_fields[3]-}"
  bash_cell="${_row_fields[4]-}"
  alias_required="${_row_fields[5]-}"
  alias_prefix="${_row_fields[6]-}"
  engine="${_row_fields[7]-}"

  [[ -n "$binary" ]] || continue

  if [[ -n "${seen_binaries[$binary]-}" ]]; then
    fail "duplicate binary row in matrix: $binary"
  fi
  seen_binaries[$binary]=1

  case "$obligation" in
    required)
      required_count+=1
      [[ "$zsh_cell" == *'`present`'* ]] || fail "matrix marks required CLI '$binary' as zsh '$zsh_cell'"

      comp_file="$ZSH_COMPLETIONS_DIR/_$binary"
      [[ -f "$comp_file" ]] || fail "required zsh completion file missing for '$binary': $comp_file"

      integer compdef_before=${#_nils_compdef_calls[@]}
      source "$comp_file" || fail "failed to source zsh completion file for '$binary': $comp_file"

      function_name="_$binary"
      (( $+functions[$function_name] )) || fail "zsh completion function '$function_name' not defined by $comp_file"

      if ! has_compdef_mapping "$function_name" "$binary" "$compdef_before" && \
         ! has_compdef_header_token "$comp_file" "$binary"; then
        fail "zsh completion '$function_name' missing registration for '$binary' (compdef/#compdef)"
      fi

      # A `completion_engine=dynamic` CLI ships a clap_complete `CompleteEnv`
      # registration stub, so its committed asset must load it through the shared
      # dynamic adapter helper (which renames the completer and strips the raw
      # `compdef` line) rather than the static `generate()` load path. This keeps
      # the dynamic registration shape asserted rather than assumed.
      if [[ "$engine" == "dynamic" ]]; then
        grep -q '_nils_cli_completion_common_load_dynamic_zsh' "$comp_file" \
          || fail "dynamic CLI '$binary' must load its CompleteEnv stub via _nils_cli_completion_common_load_dynamic_zsh"
      fi

      if [[ "$alias_required" == "1" ]]; then
        [[ -n "$alias_prefix" ]] || fail "matrix alias requirement for '$binary' is missing prefix metadata"
        required_alias_prefix_by_binary[$binary]="$alias_prefix"
      fi
      ;;
    excluded)
      ;;
    *)
      fail "unsupported obligation '$obligation' for '$binary'"
      ;;
  esac
done

(( required_count > 0 )) || fail "matrix produced zero required CLIs"

forge_cli_completion="$ZSH_COMPLETIONS_DIR/_forge-cli"
[[ -f "$forge_cli_completion" ]] || fail "missing forge-cli zsh completion: $forge_cli_completion"
grep -Eq -- "'--repo=\\[[^']*GitHub[^']*GitLab[^']*Local[^']*\\]:REPOSITORY:_default'" "$forge_cli_completion" \
  || fail "forge-cli --repo completion must describe GitHub/GitLab/Local shapes with REPOSITORY value name"
if grep -Fq -- ':owner/name:_default' "$forge_cli_completion"; then
  fail "forge-cli --repo completion must not use the GitHub-only owner/name value name"
fi

source "$ALIASES_FILE" || fail "failed to source zsh aliases file: $ALIASES_FILE"

for binary in ${(k)required_alias_prefix_by_binary}; do
  alias_prefix="${required_alias_prefix_by_binary[$binary]}"
  function_name="_$binary"
  comp_file="$ZSH_COMPLETIONS_DIR/_$binary"

  typeset -a family_aliases
  family_aliases=()
  for alias_name in ${(k)aliases}; do
    [[ "$alias_name" == ${alias_prefix}* ]] && family_aliases+=("$alias_name")
  done

  (( ${#family_aliases[@]} > 0 )) || fail "no zsh aliases found for required alias prefix '${alias_prefix}*' (binary '$binary')"

  for alias_name in "${family_aliases[@]}"; do
    if ! has_any_compdef_mapping "$function_name" "$alias_name" && \
       ! has_compdef_header_token "$comp_file" "$alias_name"; then
      fail "zsh completion '$function_name' missing alias registration for '$alias_name' from aliases.zsh"
    fi
  done
done

print -r -- "OK"
