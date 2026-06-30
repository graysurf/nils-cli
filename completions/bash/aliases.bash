# nils-cli aliases (Bash)
#
# Opt-in: source this file from your ~/.bashrc after installing nils-cli.
# Designed to avoid clobbering user-defined aliases/functions.

if [[ -z ${BASH_VERSION-} ]]; then
  return 0 2>/dev/null || exit 0
fi

shopt -s expand_aliases 2>/dev/null || true

_nils_cli__has_alias() {
  alias "$1" >/dev/null 2>&1
}

_nils_cli__has_function() {
  declare -F "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# git-scope (gs*)
# ---------------------------------------------------------------------------
_nils_cli__has_alias gs || alias gs='git-scope'

_nils_cli__has_alias gst || alias gst='git-scope tracked'
_nils_cli__has_alias gss || alias gss='git-scope staged'
_nils_cli__has_alias gsu || alias gsu='git-scope unstaged'
_nils_cli__has_alias gsa || alias gsa='git-scope all'
_nils_cli__has_alias gsun || alias gsun='git-scope untracked'
_nils_cli__has_alias gsc || alias gsc='git-scope commit'
_nils_cli__has_alias gsh || alias gsh='git-scope help'

# ---------------------------------------------------------------------------
# git-cli (gx*)
# ---------------------------------------------------------------------------
_nils_cli__has_alias gx || alias gx='git-cli'
_nils_cli__has_alias gxh || alias gxh='git-cli help'

_nils_cli__has_alias gxu || alias gxu='git-cli utils'
_nils_cli__has_alias gxr || alias gxr='git-cli reset'
_nils_cli__has_alias gxc || alias gxc='git-cli commit'
_nils_cli__has_alias gxb || alias gxb='git-cli branch'
_nils_cli__has_alias gxi || alias gxi='git-cli ci'
_nils_cli__has_alias gxo || alias gxo='git-cli open'
_nils_cli__has_alias gxw || alias gxw='git-cli worktree'

_nils_cli__has_alias gxuz || alias gxuz='git-cli utils zip'
_nils_cli__has_alias gxuc || alias gxuc='git-cli utils copy-staged'
_nils_cli__has_function gxur || gxur() { eval "$(git-cli utils root --shell)"; }
_nils_cli__has_alias gxuh || alias gxuh='git-cli utils commit-hash'

_nils_cli__has_alias gxrs || alias gxrs='git-cli reset soft'
_nils_cli__has_alias gxrm || alias gxrm='git-cli reset mixed'
_nils_cli__has_alias gxrh || alias gxrh='git-cli reset hard'
_nils_cli__has_alias gxru || alias gxru='git-cli reset undo'
_nils_cli__has_alias gxrbh || alias gxrbh='git-cli reset back-head'
_nils_cli__has_alias gxrbc || alias gxrbc='git-cli reset back-checkout'
_nils_cli__has_alias gxrr || alias gxrr='git-cli reset remote'

_nils_cli__has_alias gxcc || alias gxcc='git-cli commit context'
_nils_cli__has_alias gxcj || alias gxcj='git-cli commit context-json'
_nils_cli__has_alias gxcs || alias gxcs='git-cli commit to-stash'

_nils_cli__has_alias gxbc || alias gxbc='git-cli branch cleanup'

_nils_cli__has_alias gxip || alias gxip='git-cli ci pick'

_nils_cli__has_alias gxor || alias gxor='git-cli open repo'
_nils_cli__has_alias gxob || alias gxob='git-cli open branch'
_nils_cli__has_alias gxod || alias gxod='git-cli open default-branch'
_nils_cli__has_alias gxoc || alias gxoc='git-cli open commit'
_nils_cli__has_alias gxocp || alias gxocp='git-cli open compare'
_nils_cli__has_alias gxop || alias gxop='git-cli open pr'
_nils_cli__has_alias gxopl || alias gxopl='git-cli open pulls'
_nils_cli__has_alias gxoi || alias gxoi='git-cli open issues'
_nils_cli__has_alias gxoa || alias gxoa='git-cli open actions'
_nils_cli__has_alias gxorl || alias gxorl='git-cli open releases'
_nils_cli__has_alias gxot || alias gxot='git-cli open tags'
_nils_cli__has_alias gxocs || alias gxocs='git-cli open commits'
_nils_cli__has_alias gxof || alias gxof='git-cli open file'
_nils_cli__has_alias gxobl || alias gxobl='git-cli open blame'

# Jump into a worktree (uses eval to change the parent shell's directory):
_nils_cli__has_function gxwcd || gxwcd() { eval "$(git-cli worktree go --shell "$@")"; }
# Worktree-name completion for the cd helper; candidates come straight from the
# live `git worktree list`. Agents never reach this completion path.
_nils_cli_gxwcd_complete() {
  local cur="${COMP_WORDS[COMP_CWORD]}"
  local cands
  cands="$(command git worktree list --porcelain 2>/dev/null | awk '
    /^worktree /  { n = split($2, p, "/"); print p[n] }
    /^branch /    { b = $2; sub(/^refs\/heads\//, "", b); print b }
  ')"
  mapfile -t COMPREPLY < <(compgen -W "$cands" -- "$cur")
}
complete -F _nils_cli_gxwcd_complete gxwcd 2>/dev/null || true

# ---------------------------------------------------------------------------
# codex-cli (cx*)
# ---------------------------------------------------------------------------
_nils_cli__has_alias cx || alias cx='codex-cli'

_nils_cli__has_alias cxau || alias cxau='codex-cli auth use'
_nils_cli__has_alias cxar || alias cxar='codex-cli auth refresh'
_nils_cli__has_alias cxaa || alias cxaa='codex-cli auth auto-refresh'
_nils_cli__has_alias cxac || alias cxac='codex-cli auth current'
_nils_cli__has_alias cxas || alias cxas='codex-cli auth sync'

_nils_cli__has_alias cxdr || alias cxdr='codex-cli diag rate-limits'
_nils_cli__has_alias cxdra || alias cxdra='codex-cli diag rate-limits --async'

_nils_cli__has_alias cxcs || alias cxcs='codex-cli config show'
_nils_cli__has_alias cxct || alias cxct='codex-cli config set'

_nils_cli__has_alias cxgp || alias cxgp='codex-cli agent prompt'
_nils_cli__has_alias cxga || alias cxga='codex-cli agent advice'
_nils_cli__has_alias cxgk || alias cxgk='codex-cli agent knowledge'
_nils_cli__has_alias cxgc || alias cxgc='codex-cli agent commit'

_nils_cli__has_alias cxps || alias cxps='codex-cli prompt-segment'

# ---------------------------------------------------------------------------
# fzf-cli (fx*)
# ---------------------------------------------------------------------------
_nils_cli__has_alias fx || alias fx='fzf-cli'
_nils_cli__has_alias fxf || alias fxf='fzf-cli file'

# These use eval to preserve parent-shell effects:
_nils_cli__has_function fxd || fxd() { eval "$(fzf-cli directory -- "$@")"; }
_nils_cli__has_function fxh || fxh() { eval "$(fzf-cli history -- "$@")"; }

_nils_cli__has_alias fxgs || alias fxgs='fzf-cli git-status'
_nils_cli__has_alias fxgc || alias fxgc='fzf-cli git-commit'
_nils_cli__has_alias fxgco || alias fxgco='fzf-cli git-checkout'
_nils_cli__has_alias fxgb || alias fxgb='fzf-cli git-branch'
_nils_cli__has_alias fxgt || alias fxgt='fzf-cli git-tag'
_nils_cli__has_alias fxp || alias fxp='fzf-cli process'
_nils_cli__has_alias fxpo || alias fxpo='fzf-cli port'
_nils_cli__has_alias fxenv || alias fxenv='fzf-cli env'
_nils_cli__has_alias fxal || alias fxal='fzf-cli alias'
_nils_cli__has_alias fxfn || alias fxfn='fzf-cli function'
_nils_cli__has_alias fxdef || alias fxdef='fzf-cli def'
