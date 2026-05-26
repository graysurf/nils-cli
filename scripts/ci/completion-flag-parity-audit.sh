#!/usr/bin/env bash
set -euo pipefail

HELP_TIMEOUT_SECONDS=5
COMPLETION_TIMEOUT_SECONDS=30
MAX_COMMAND_DEPTH=6
ROOT_KEY="__ROOT__"
PATH_SEP=$'\x1f'

RUN_CODE=0
RUN_STDOUT=""
RUN_STDERR=""
ROOT_HELP_TEXT=""
ENSURE_BIN_ERROR=""
PLATFORM_EXE_SUFFIX=""

declare -a PATH_PARTS=()
declare -a FAILURES=()
declare -A HELP_BY_PATH=()
declare -A HELP_FAILURES=()

usage() {
  cat <<'USAGE'
Usage:
  scripts/ci/completion-flag-parity-audit.sh [--strict]

Audit completion flag parity between --help and bash/zsh completions.

Options:
  --strict   Compatibility flag. The audit is always strict.
  -h, --help Show this help
USAGE
}

run_command() {
  local timeout_seconds="$1"
  local cwd="$2"
  shift 2
  local -a cmd=( "$@" )
  local stdout_file
  local stderr_file
  stdout_file="$(mktemp)"
  stderr_file="$(mktemp)"

  if (( timeout_seconds <= 0 )); then
    if (cd "$cwd" && "${cmd[@]}") >"$stdout_file" 2>"$stderr_file"; then
      RUN_CODE=0
    else
      RUN_CODE=$?
    fi
    RUN_STDOUT="$(cat "$stdout_file")"
    RUN_STDERR="$(cat "$stderr_file")"
    rm -f "$stdout_file" "$stderr_file"
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
      RUN_STDOUT=""
      RUN_STDERR="command timed out after ${timeout_seconds}s: ${cmd[*]}"
      rm -f "$stdout_file" "$stderr_file"
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
  RUN_STDOUT="$(cat "$stdout_file")"
  RUN_STDERR="$(cat "$stderr_file")"
  rm -f "$stdout_file" "$stderr_file"
}

parse_required_bins() {
  local matrix_path="$1"
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
  ' "$matrix_path" | LC_ALL=C sort -u
}

parse_commands() {
  local help_text="$1"
  printf '%s\n' "$help_text" | awk '
    BEGIN { in_commands = 0 }
    {
      line = $0
      if (line ~ /^[[:space:]]*Commands:[[:space:]]*$/) {
        in_commands = 1
        next
      }
      if (!in_commands) {
        next
      }
      if (line ~ /^[[:space:]]*$/) {
        exit
      }
      trimmed = line
      sub(/^[[:space:]]+/, "", trimmed)
      if (trimmed ~ /^-/) {
        next
      }
      if (match(line, /^[[:space:]][[:space:]]+[A-Za-z0-9][A-Za-z0-9-]*[[:space:]][[:space:]]+/)) {
        command = substr(line, RSTART, RLENGTH)
        sub(/^[[:space:]]+/, "", command)
        sub(/[[:space:]].*$/, "", command)
        if (command != "help") {
          print command
        }
      }
    }
  '
}

parse_flags() {
  local help_text="$1"
  printf '%s\n' "$help_text" | awk '
    BEGIN { in_options = 0 }
    {
      raw_line = $0
      line = raw_line
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)

      if (line == "Options:") {
        in_options = 1
        next
      }
      if (!in_options) {
        next
      }
      if (line == "") {
        exit
      }

      ltrim = raw_line
      sub(/^[[:space:]]+/, "", ltrim)
      if (substr(ltrim, 1, 1) != "-") {
        next
      }

      spec = line
      spec_end = index(spec, "  ")
      if (spec_end > 0) {
        spec = substr(spec, 1, spec_end - 1)
      }

      # Ignore placeholder metavariables (for example <task-id>) so we do not
      # misinterpret suffixes like "-id" as real flags.
      gsub(/<[^>]*>/, "", spec)
      gsub(/\[[^]]*\]/, "", spec)

      while (match(spec, /(^|[[:space:],])-{1,2}[A-Za-z0-9][A-Za-z0-9-]*/)) {
        token = substr(spec, RSTART, RLENGTH)
        sub(/^[[:space:],]+/, "", token)
        spec = substr(spec, RSTART + RLENGTH)
        if (token == "-h" || token == "--help") {
          continue
        }
        if (!(token in seen)) {
          seen[token] = 1
          print token
        }
      }
    }
  '
}

parse_completion_flag_tokens() {
  local opts_text="$1"
  printf '%s\n' "$opts_text" \
    | tr ' \t' '\n' \
    | awk '
        /^-/ && $0 != "-h" && $0 != "--help" {
          if (!seen[$0]++) {
            print $0
          }
        }
      ' \
    | LC_ALL=C sort
}

parse_zsh_option_descriptions() {
  local zsh_block="$1"
  perl -ne 'while(/'\''([^'\'']+)'\''/g){$spec=$1; next unless $spec =~ /^-{1,2}/; if($spec =~ /^([^[]+)\[(.*?)\]/){$opt=$1; $desc=$2; $opt =~ s/[+=]+$//; print "$opt\t$desc\n";}}' <<< "$zsh_block"
}

decode_path() {
  local path_key="$1"
  PATH_PARTS=()
  if [[ "$path_key" == "$ROOT_KEY" ]]; then
    return 0
  fi
  local IFS="$PATH_SEP"
  read -r -a PATH_PARTS <<< "$path_key"
}

path_to_display() {
  local path_key="$1"
  if [[ "$path_key" == "$ROOT_KEY" ]]; then
    printf '<root>'
    return 0
  fi

  decode_path "$path_key"
  local joined=""
  local segment
  for segment in "${PATH_PARTS[@]}"; do
    if [[ -z "$joined" ]]; then
      joined="$segment"
    else
      joined+=" $segment"
    fi
  done
  printf '%s' "$joined"
}

append_path() {
  local path_key="$1"
  local segment="$2"
  if [[ "$path_key" == "$ROOT_KEY" ]]; then
    printf '%s' "$segment"
  else
    printf '%s%s%s' "$path_key" "$PATH_SEP" "$segment"
  fi
}

bash_case_label() {
  local binary="$1"
  local path_key="$2"
  local label="${binary//-/__}"

  if [[ "$path_key" == "$ROOT_KEY" ]]; then
    printf '%s' "$label"
    return 0
  fi

  decode_path "$path_key"
  local segment
  for segment in "${PATH_PARTS[@]}"; do
    label+="__${segment//-/__}"
  done
  printf '%s' "$label"
}

bash_case_opts() {
  local script_text="$1"
  local label="$2"
  local result

  if ! result="$(
    awk -v marker="${label})" '
      BEGIN { in_case = 0; found = 0 }
      {
        line = $0
        sub(/^[ \t]+/, "", line)
        sub(/[ \t]+$/, "", line)
        if (line == marker) {
          in_case = 1
          next
        }
        if (!in_case) {
          next
        }
        if (line ~ /^opts="/) {
          sub(/^opts="/, "", line)
          split(line, parts, "\"")
          print parts[1]
          found = 1
          exit
        }
        if (line ~ /\)$/ && line !~ /^opts=/) {
          exit
        }
      }
      END {
        if (!found) {
          exit 1
        }
      }
    ' <<< "$script_text"
  )"; then
    return 1
  fi

  printf '%s' "$result"
}

zsh_context_marker() {
  local binary="$1"
  shift
  local -a parents=( "$@" )

  if (( ${#parents[@]} == 0 )); then
    printf '%s' "curcontext=\"\${curcontext%:*:*}:${binary}-command-\$line[1]:\""
    return 0
  fi

  local joined="${parents[0]}"
  local idx
  for (( idx = 1; idx < ${#parents[@]}; idx++ )); do
    joined+="-${parents[$idx]}"
  done
  printf '%s' "curcontext=\"\${curcontext%:*:*}:${binary}-${joined}-command-\$line[1]:\""
}

zsh_leaf_block() {
  local script_text="$1"
  local binary="$2"
  local path_key="$3"
  local args_marker='_arguments "${_arguments_options[@]}" : \'
  local from_args

  if [[ "$path_key" == "$ROOT_KEY" ]]; then
    if [[ "$script_text" != *"$args_marker"* ]]; then
      return 1
    fi
    from_args="${args_marker}${script_text#*"$args_marker"}"
    if [[ "$from_args" != *"&& ret=0"* ]]; then
      return 1
    fi
    printf '%s' "${from_args%%"&& ret=0"*}"
    return 0
  fi

  decode_path "$path_key"
  local parts_len="${#PATH_PARTS[@]}"
  if (( parts_len == 0 )); then
    return 1
  fi

  local leaf_index=$(( parts_len - 1 ))
  local leaf="${PATH_PARTS[$leaf_index]}"
  local -a parents=()
  local idx
  for (( idx = 0; idx < leaf_index; idx++ )); do
    parents+=( "${PATH_PARTS[$idx]}" )
  done

  local marker
  marker="$(zsh_context_marker "$binary" "${parents[@]}")"
  if [[ "$script_text" != *"$marker"* ]]; then
    return 1
  fi
  local from_marker="${script_text#*"$marker"}"
  local leaf_marker="(${leaf})"
  if [[ "$from_marker" != *"$leaf_marker"* ]]; then
    return 1
  fi
  local from_leaf="${from_marker#*"$leaf_marker"}"
  if [[ "$from_leaf" != *"$args_marker"* ]]; then
    return 1
  fi
  from_args="${args_marker}${from_leaf#*"$args_marker"}"
  if [[ "$from_args" != *"&& ret=0"* ]]; then
    return 1
  fi
  printf '%s' "${from_args%%"&& ret=0"*}"
}

escape_ere() {
  printf '%s' "$1" | sed -e 's/[][(){}.^$*+?|\\/]/\\&/g'
}

contains_token() {
  local haystack="$1"
  local token="$2"
  local escaped
  escaped="$(escape_ere "$token")"
  LC_ALL=C grep -Eq "(^|[^[:alnum:]-])${escaped}([^[:alnum:]-]|$)" <<<"$haystack"
}

command_display_for_path() {
  local path_key="$1"
  decode_path "$path_key"
  local cmd="$CURRENT_BINARY_PATH"
  local segment
  for segment in "${PATH_PARTS[@]}"; do
    cmd+=" $segment"
  done
  cmd+=" --help"
  printf '%s' "$cmd"
}

walk_help_path() {
  local path_key="$1"
  local depth="$2"

  if (( depth > MAX_COMMAND_DEPTH )); then
    HELP_FAILURES["$path_key"]="path depth exceeded safety limit (${MAX_COMMAND_DEPTH})"
    return 0
  fi

  local -a cmd=( "$CURRENT_BINARY_PATH" )
  if [[ "$path_key" != "$ROOT_KEY" ]]; then
    decode_path "$path_key"
    cmd+=( "${PATH_PARTS[@]}" )
  fi
  cmd+=( "--help" )

  run_command "$HELP_TIMEOUT_SECONDS" "$CURRENT_REPO_ROOT" "${cmd[@]}"
  if (( RUN_CODE != 0 )); then
    local stderr_compact="${RUN_STDERR//$'\n'/ }"
    HELP_FAILURES["$path_key"]="\`$(command_display_for_path "$path_key")\` failed (exit ${RUN_CODE}): ${stderr_compact}"
    return 0
  fi

  if [[ "$path_key" == "$ROOT_KEY" ]]; then
    ROOT_HELP_TEXT="$RUN_STDOUT"
  elif [[ -n "$ROOT_HELP_TEXT" && "$RUN_STDOUT" == "$ROOT_HELP_TEXT" ]]; then
    HELP_FAILURES["$path_key"]="help output fell back to root help for nested command path \`$(path_to_display "$path_key")\`"
    return 0
  fi

  HELP_BY_PATH["$path_key"]="$RUN_STDOUT"

  local -a commands=()
  mapfile -t commands < <(parse_commands "$RUN_STDOUT")
  local command_name
  for command_name in "${commands[@]}"; do
    walk_help_path "$(append_path "$path_key" "$command_name")" "$(( depth + 1 ))"
  done
}

gather_leaf_help() {
  local repo_root="$1"
  local binary_path="$2"

  CURRENT_REPO_ROOT="$repo_root"
  CURRENT_BINARY_PATH="$binary_path"
  ROOT_HELP_TEXT=""
  HELP_BY_PATH=()
  HELP_FAILURES=()

  walk_help_path "$ROOT_KEY" 0
}

ensure_binaries() {
  local repo_root="$1"
  shift
  local -a bins=( "$@" )

  ENSURE_BIN_ERROR=""
  local target_dir="$repo_root/target/debug"
  local -a missing=()
  local binary
  for binary in "${bins[@]}"; do
    local bin_path="${target_dir}/${binary}${PLATFORM_EXE_SUFFIX}"
    if [[ ! -f "$bin_path" ]]; then
      missing+=( "$bin_path" )
    fi
  done

  if (( ${#missing[@]} == 0 )); then
    return 0
  fi

  # Use --all-features so feature-gated workspace binaries (e.g.
  # `md-render` behind `nils-markdown/bin-cli`) are produced under
  # target/debug/. Without --all-features, `cargo build --workspace
  # --bins` only builds always-on binaries and the audit reports
  # spurious "missing binaries after build" failures for opt-in
  # binaries that are present in the completion matrix.
  run_command 0 "$repo_root" cargo build --workspace --bins --all-features
  if (( RUN_CODE != 0 )); then
    local details="$RUN_STDERR"
    if [[ -z "${details//[[:space:]]/}" ]]; then
      details="$RUN_STDOUT"
    fi
    details="${details//$'\n'/ }"
    ENSURE_BIN_ERROR="cargo build --workspace --bins --all-features failed: ${details}"
    return 1
  fi

  local -a missing_after=()
  for binary in "${bins[@]}"; do
    local bin_path="${target_dir}/${binary}${PLATFORM_EXE_SUFFIX}"
    if [[ ! -f "$bin_path" ]]; then
      missing_after+=( "${bin_path#"$repo_root"/}" )
    fi
  done
  if (( ${#missing_after[@]} > 0 )); then
    ENSURE_BIN_ERROR="missing binaries after build: ${missing_after[*]}"
    return 1
  fi

  return 0
}

audit_binary() {
  local repo_root="$1"
  local binary="$2"
  local binary_path="$3"

  run_command "$COMPLETION_TIMEOUT_SECONDS" "$repo_root" "$binary_path" completion bash
  if (( RUN_CODE != 0 )); then
    FAILURES+=( "${binary}: completion bash failed (exit ${RUN_CODE}): ${RUN_STDERR}" )
    return 0
  fi
  local bash_script="$RUN_STDOUT"

  run_command "$COMPLETION_TIMEOUT_SECONDS" "$repo_root" "$binary_path" completion zsh
  if (( RUN_CODE != 0 )); then
    FAILURES+=( "${binary}: completion zsh failed (exit ${RUN_CODE}): ${RUN_STDERR}" )
    return 0
  fi
  local zsh_script="$RUN_STDOUT"

  gather_leaf_help "$repo_root" "$binary_path"
  if (( ${#HELP_BY_PATH[@]} == 0 )); then
    FAILURES+=( "${binary}: no help paths discovered" )
    return 0
  fi

  local -a failed_paths=()
  if (( ${#HELP_FAILURES[@]} > 0 )); then
    mapfile -t failed_paths < <(printf '%s\n' "${!HELP_FAILURES[@]}" | LC_ALL=C sort)
  fi
  local path_key
  for path_key in "${failed_paths[@]}"; do
    local case_label
    case_label="$(bash_case_label "$binary" "$path_key")"
    local bash_opts=""
    if bash_opts="$(bash_case_opts "$bash_script" "$case_label")"; then
      local -a required_flags=()
      mapfile -t required_flags < <(parse_completion_flag_tokens "$bash_opts")
      if (( ${#required_flags[@]} > 0 )); then
        local joined
        joined="$(path_to_display "$path_key")"
        FAILURES+=(
          "${binary}: unable to read help for \`${joined}\` while completion has flags ${required_flags[*]}; ${HELP_FAILURES[$path_key]}"
        )
      fi
    fi
  done

  local -a help_paths=()
  if (( ${#HELP_BY_PATH[@]} > 0 )); then
    mapfile -t help_paths < <(printf '%s\n' "${!HELP_BY_PATH[@]}" | LC_ALL=C sort)
  fi
  for path_key in "${help_paths[@]}"; do
    local help_text="${HELP_BY_PATH[$path_key]}"
    local -a flags=()
    mapfile -t flags < <(parse_flags "$help_text")
    if (( ${#flags[@]} == 0 )); then
      continue
    fi

    local case_label
    case_label="$(bash_case_label "$binary" "$path_key")"
    local bash_opts
    if ! bash_opts="$(bash_case_opts "$bash_script" "$case_label")"; then
      FAILURES+=( "${binary}: missing bash completion case for $(path_to_display "$path_key") (${case_label})" )
      continue
    fi

    local zsh_block
    if ! zsh_block="$(zsh_leaf_block "$zsh_script" "$binary" "$path_key")"; then
      FAILURES+=( "${binary}: missing zsh completion block for $(path_to_display "$path_key")" )
      continue
    fi

    local -A zsh_help_by_flag=()
    local opt desc
    while IFS=$'\t' read -r opt desc; do
      [[ -n "$opt" ]] || continue
      if [[ ! -v "zsh_help_by_flag[$opt]" ]] || [[ -z "${zsh_help_by_flag[$opt]}" && -n "$desc" ]]; then
        zsh_help_by_flag["$opt"]="$desc"
      fi
    done < <(parse_zsh_option_descriptions "$zsh_block")

    local flag
    for flag in "${flags[@]}"; do
      if ! contains_token "$bash_opts" "$flag"; then
        FAILURES+=( "${binary}: bash completion missing flag \`${flag}\` for command \`$(path_to_display "$path_key")\`" )
      fi
      if ! contains_token "$zsh_block" "$flag"; then
        FAILURES+=( "${binary}: zsh completion missing flag \`${flag}\` for command \`$(path_to_display "$path_key")\`" )
        continue
      fi
      if [[ -v "zsh_help_by_flag[$flag]" ]] && [[ -z "${zsh_help_by_flag[$flag]}" ]]; then
        FAILURES+=( "${binary}: zsh completion empty description for flag \`${flag}\` on command \`$(path_to_display "$path_key")\`" )
      fi
    done
  done
}

main() {
  local strict=0
  while [[ $# -gt 0 ]]; do
    case "${1:-}" in
      --strict)
        strict=1
        shift
        ;;
      -h|--help)
        usage
        return 0
        ;;
      *)
        echo "error: unknown argument: ${1:-}" >&2
        usage >&2
        return 2
        ;;
    esac
  done
  : "$strict"

  local script_dir
  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  local repo_root
  repo_root="$(cd "$script_dir/../.." && pwd)"
  local matrix_path="$repo_root/docs/specs/completion-coverage-matrix-v1.md"

  if [[ ! -f "$matrix_path" ]]; then
    echo "FAIL: missing completion matrix: $matrix_path" >&2
    return 2
  fi

  local -a required_bins=()
  mapfile -t required_bins < <(parse_required_bins "$matrix_path")
  if (( ${#required_bins[@]} == 0 )); then
    echo "FAIL: no required binaries found in matrix: $matrix_path" >&2
    return 2
  fi

  PLATFORM_EXE_SUFFIX=""
  if [[ "${OS:-}" == "Windows_NT" ]]; then
    PLATFORM_EXE_SUFFIX=".exe"
  else
    case "$(uname -s)" in
      MINGW*|MSYS*|CYGWIN*)
        PLATFORM_EXE_SUFFIX=".exe"
        ;;
    esac
  fi

  if ! ensure_binaries "$repo_root" "${required_bins[@]}"; then
    echo "FAIL: $ENSURE_BIN_ERROR" >&2
    return 2
  fi

  local binary
  for binary in "${required_bins[@]}"; do
    audit_binary "$repo_root" "$binary" "$repo_root/target/debug/${binary}${PLATFORM_EXE_SUFFIX}"
  done

  if (( ${#FAILURES[@]} > 0 )); then
    local failure
    for failure in "${FAILURES[@]}"; do
      echo "FAIL: $failure"
    done
    echo "FAIL: completion flag parity audit (required=${#required_bins[@]}, failures=${#FAILURES[@]})"
    return 1
  fi

  echo "PASS: completion flag parity audit (required=${#required_bins[@]}, failures=0)"
  return 0
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
