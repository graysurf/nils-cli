# nils-cli bash completion adapter common helpers
#
# Shared by thin bash adapters that load clap-generated completion scripts.

if [[ -z ${BASH_VERSION-} ]]; then
  return 0 2>/dev/null || exit 0
fi

if [[ -n ${_NILS_CLI_COMPLETION_ADAPTER_COMMON_BASH_LOADED-} ]]; then
  return 0
fi

_NILS_CLI_COMPLETION_ADAPTER_COMMON_BASH_LOADED=1

_nils_cli_completion_common_fail_closed_bash() {
  # Single-path policy: fail closed and do not route to alternate completers.
  COMPREPLY=()
  return 0
}

_nils_cli_completion_common_warn_once_bash() {
  local state_var="${1-}"
  local cli_bin="${2-}"
  local reason="${3-}"
  local diag_var=''

  if [[ -n "$state_var" ]]; then
    diag_var="${state_var}_DIAG_SHOWN"
    if [[ "${!diag_var:-0}" == "1" ]]; then
      return 0
    fi
    printf -v "$diag_var" '%s' '1'
  fi

  local msg='nils-cli completion (bash): generated completion load failed'
  if [[ -n "$cli_bin" ]]; then
    msg+=" for '${cli_bin}'"
  fi
  msg+='; fail-closed mode active'
  if [[ -n "$reason" ]]; then
    msg+=" (${reason})"
  fi

  printf '%s\n' "$msg" >&2
  return 0
}

_nils_cli_completion_common_has_word_bash() {
  local needle="$1"
  shift

  local w=''
  for w in "$@"; do
    [[ "$w" == "$needle" ]] && return 0
  done
  return 1
}

_nils_cli_completion_common_load_generated_bash() {
  local state_var="${1-}"
  local generated_fn="${2-}"
  local cli_bin="${3-}"
  local generated_symbol="${4-}"
  local strip_begin_regex="${5-}"
  local strip_end_regex="${6-}"

  if [[ -z "$state_var" || -z "$generated_fn" || -z "$cli_bin" || -z "$generated_symbol" ]]; then
    _nils_cli_completion_common_warn_once_bash '' "${cli_bin}" 'invalid helper arguments'
    _nils_cli_completion_common_fail_closed_bash
    return 1
  fi

  local state="${!state_var:-0}"
  if [[ "$state" == "1" ]]; then
    declare -F "$generated_fn" >/dev/null 2>&1 && return 0
    printf -v "$state_var" '%s' '0'
  elif [[ "$state" == "-1" ]]; then
    _nils_cli_completion_common_warn_once_bash \
      "$state_var" \
      "$cli_bin" \
      'cached previous load failure; restart shell after fixing the CLI'
    _nils_cli_completion_common_fail_closed_bash
    return 1
  fi

  local script=''
  local cmd_rc=0
  script="$(command "$cli_bin" completion bash)" || {
    cmd_rc=$?
    printf -v "$state_var" '%s' '-1'
    _nils_cli_completion_common_warn_once_bash \
      "$state_var" \
      "$cli_bin" \
      "command '${cli_bin} completion bash' failed (exit ${cmd_rc})"
    _nils_cli_completion_common_fail_closed_bash
    return 1
  }

  script="${script//${generated_symbol}/${generated_fn}}"

  if [[ -n "$strip_begin_regex" && -n "$strip_end_regex" ]]; then
    script="$(printf '%s\n' "$script" | sed "/${strip_begin_regex}/,/${strip_end_regex}/d")" || {
      printf -v "$state_var" '%s' '-1'
      _nils_cli_completion_common_warn_once_bash \
        "$state_var" \
        "$cli_bin" \
        'failed to post-process generated bash completion script'
      _nils_cli_completion_common_fail_closed_bash
      return 1
    }
  fi

  eval "$script" || {
    printf -v "$state_var" '%s' '-1'
    _nils_cli_completion_common_warn_once_bash \
      "$state_var" \
      "$cli_bin" \
      'failed to eval generated bash completion script'
    _nils_cli_completion_common_fail_closed_bash
    return 1
  }

  declare -F "$generated_fn" >/dev/null 2>&1 || {
    printf -v "$state_var" '%s' '-1'
    _nils_cli_completion_common_warn_once_bash \
      "$state_var" \
      "$cli_bin" \
      "generated function '${generated_fn}' was not defined"
    _nils_cli_completion_common_fail_closed_bash
    return 1
  }

  printf -v "$state_var" '%s' '1'
  printf -v "${state_var}_DIAG_SHOWN" '%s' '0'
  return 0
}

_nils_cli_completion_common_load_dynamic_bash() {
  # Load a `completion_engine=dynamic` CLI's clap_complete `CompleteEnv`
  # registration stub (emitted by `<cli> completion bash`) as an alias-aware
  # generated function.
  #
  # The stub defines `_clap_complete_<name>` (which shells back into the binary
  # at TAB time) and ends with the standard `complete -F _clap_complete_<name>
  # <bin>` guard wrapped in a `BASH_VERSINFO` version check. We rename the
  # completer to the caller's generated symbol and strip that trailing guard so
  # the thin adapter keeps ownership of registration (and its
  # `COMP_WORDS`/`COMP_CWORD` alias rewrite). The guard's shape is identical for
  # static and dynamic clap_complete output, so the same strip bounds apply.
  # Reuses the shared load/eval/fail-closed machinery for a single, vetted path.
  local state_var="${1-}"
  local generated_fn="${2-}"
  local cli_bin="${3-}"
  local dynamic_symbol="${4-}"

  if [[ -z "$dynamic_symbol" ]]; then
    _nils_cli_completion_common_warn_once_bash '' "${cli_bin}" 'invalid dynamic helper arguments'
    _nils_cli_completion_common_fail_closed_bash
    return 1
  fi

  _nils_cli_completion_common_load_generated_bash \
    "$state_var" \
    "$generated_fn" \
    "$cli_bin" \
    "$dynamic_symbol" \
    '^if \[\[ "\${BASH_VERSINFO\[0\]}" -eq 4 ' \
    '^fi$'
}

_nils_cli_completion_common_register_bash() {
  local completion_fn="${1-}"
  shift || true

  if [[ -z "$completion_fn" ]]; then
    return 1
  fi

  complete -F "$completion_fn" "$@"
}
