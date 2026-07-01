# nils-cli zsh completion adapter common helpers
#
# Shared by thin zsh adapters that load clap-generated completion scripts.

if [[ -z ${ZSH_VERSION-} ]]; then
  return 0 2>/dev/null || exit 0
fi

if [[ -n ${_NILS_CLI_COMPLETION_ADAPTER_COMMON_ZSH_LOADED-} ]]; then
  return 0
fi

typeset -g _NILS_CLI_COMPLETION_ADAPTER_COMMON_ZSH_LOADED=1

_nils_cli_completion_common_fail_closed_zsh() {
  # Single-path policy: fail closed and do not route to alternate completers.
  return 1
}

_nils_cli_completion_common_warn_once_zsh() {
  emulate -L zsh

  local state_var="${1-}"
  local cli_bin="${2-}"
  local reason="${3-}"
  local diag_var=''

  if [[ -n "$state_var" ]]; then
    diag_var="${state_var}_DIAG_SHOWN"
    if [[ "${(P)diag_var-0}" == "1" ]]; then
      return 0
    fi
    typeset -g "${diag_var}=1"
  fi

  local msg='nils-cli completion (zsh): generated completion load failed'
  if [[ -n "$cli_bin" ]]; then
    msg="${msg} for '${cli_bin}'"
  fi
  msg="${msg}; fail-closed mode active"
  if [[ -n "$reason" ]]; then
    msg="${msg} (${reason})"
  fi

  print -u2 -r -- "$msg"
  return 0
}

_nils_cli_completion_common_load_generated_zsh() {
  emulate -L zsh -o extendedglob

  local state_var="${1-}"
  local generated_fn="${2-}"
  local cli_bin="${3-}"
  local generated_symbol="${4-}"
  local strip_begin_regex="${5-}"
  local strip_end_regex="${6-}"

  if [[ -z "$state_var" || -z "$generated_fn" || -z "$cli_bin" || -z "$generated_symbol" ]]; then
    _nils_cli_completion_common_warn_once_zsh '' "${cli_bin}" 'invalid helper arguments'
    _nils_cli_completion_common_fail_closed_zsh
    return 1
  fi

  local state="${(P)state_var-0}"
  if [[ "$state" == "1" ]]; then
    (( $+functions[$generated_fn] )) && return 0
    typeset -g "${state_var}=0"
  elif [[ "$state" == "-1" ]]; then
    _nils_cli_completion_common_warn_once_zsh \
      "$state_var" \
      "$cli_bin" \
      'cached previous load failure; restart shell after fixing the CLI'
    _nils_cli_completion_common_fail_closed_zsh
    return 1
  fi

  local script=''
  local cmd_rc=0
  script="$(command "$cli_bin" completion zsh)" || {
    cmd_rc=$?
    typeset -g "${state_var}=-1"
    _nils_cli_completion_common_warn_once_zsh \
      "$state_var" \
      "$cli_bin" \
      "command '${cli_bin} completion zsh' failed (exit ${cmd_rc})"
    _nils_cli_completion_common_fail_closed_zsh
    return 1
  }

  script="${script//${generated_symbol}/${generated_fn}}"

  if [[ -n "$strip_begin_regex" && -n "$strip_end_regex" ]]; then
    script="$(printf '%s\n' "$script" | sed "/${strip_begin_regex}/,/${strip_end_regex}/d")" || {
      typeset -g "${state_var}=-1"
      _nils_cli_completion_common_warn_once_zsh \
        "$state_var" \
        "$cli_bin" \
        'failed to post-process generated zsh completion script'
      _nils_cli_completion_common_fail_closed_zsh
      return 1
    }
  fi

  eval "$script" || {
    typeset -g "${state_var}=-1"
    _nils_cli_completion_common_warn_once_zsh \
      "$state_var" \
      "$cli_bin" \
      'failed to eval generated zsh completion script'
    _nils_cli_completion_common_fail_closed_zsh
    return 1
  }

  if (( ! $+functions[$generated_fn] )); then
    typeset -g "${state_var}=-1"
    _nils_cli_completion_common_warn_once_zsh \
      "$state_var" \
      "$cli_bin" \
      "generated function '${generated_fn}' was not defined"
    _nils_cli_completion_common_fail_closed_zsh
    return 1
  fi

  typeset -g "${state_var}=1"
  typeset -g "${state_var}_DIAG_SHOWN=0"
  return 0
}

_nils_cli_completion_common_load_dynamic_zsh() {
  # Load a `completion_engine=dynamic` CLI's clap_complete `CompleteEnv`
  # registration stub (emitted by `<cli> completion zsh`) as an alias-aware
  # generated function.
  #
  # The stub defines `_clap_dynamic_completer_<name>` (which shells back into the
  # binary at TAB time) and ends with `compdef _clap_dynamic_completer_<name>
  # <bin>`. We rename the completer to the caller's generated symbol and strip
  # that trailing `compdef` line so the thin adapter keeps ownership of
  # registration (and its `words`/`CURRENT` alias rewrite). Reuses the shared
  # load/eval/fail-closed machinery for a single, vetted load path.
  emulate -L zsh

  local state_var="${1-}"
  local generated_fn="${2-}"
  local cli_bin="${3-}"
  local dynamic_symbol="${4-}"

  if [[ -z "$dynamic_symbol" ]]; then
    _nils_cli_completion_common_warn_once_zsh '' "${cli_bin}" 'invalid dynamic helper arguments'
    _nils_cli_completion_common_fail_closed_zsh
    return 1
  fi

  # Matches the trailing registration line *after* the symbol rewrite renames
  # `${dynamic_symbol}` to `${generated_fn}`. The begin==end single-line strip is
  # correct because clap_complete emits `compdef` as the final line of the zsh
  # stub; this holds for the exact-pinned `clap_complete =4.6.5` (see Cargo.toml)
  # and is re-checked on any version bump.
  local strip_regex="^compdef ${generated_fn} ${cli_bin}\$"

  _nils_cli_completion_common_load_generated_zsh \
    "$state_var" \
    "$generated_fn" \
    "$cli_bin" \
    "$dynamic_symbol" \
    "$strip_regex" \
    "$strip_regex"
}

_nils_cli_completion_common_register_zsh() {
  emulate -L zsh

  local completion_fn="${1-}"
  shift || true

  if [[ -z "$completion_fn" ]]; then
    return 1
  fi

  (( $+functions[compdef] )) || return 0
  compdef "$completion_fn" "$@"
}
