# nils-cli aliases (Zsh)
#
# Opt-in: source this file from your ~/.zshrc after installing nils-cli.
# Designed to avoid clobbering user-defined aliases/functions.

if [[ -z ${ZSH_VERSION-} ]]; then
  return 0 2>/dev/null || exit 0
fi

# ---------------------------------------------------------------------------
# git-scope (gs*)
# ---------------------------------------------------------------------------
(( $+aliases[gs] )) || alias gs='git-scope'

(( $+aliases[gst] )) || alias gst='git-scope tracked'
(( $+aliases[gss] )) || alias gss='git-scope staged'
(( $+aliases[gsu] )) || alias gsu='git-scope unstaged'
(( $+aliases[gsa] )) || alias gsa='git-scope all'
(( $+aliases[gsun] )) || alias gsun='git-scope untracked'
(( $+aliases[gsc] )) || alias gsc='git-scope commit'
(( $+aliases[gsh] )) || alias gsh='git-scope help'

# ---------------------------------------------------------------------------
# git-cli (gx*)
# ---------------------------------------------------------------------------
(( $+aliases[gx] )) || alias gx='git-cli'
(( $+aliases[gxh] )) || alias gxh='git-cli help'

(( $+aliases[gxu] )) || alias gxu='git-cli utils'
(( $+aliases[gxr] )) || alias gxr='git-cli reset'
(( $+aliases[gxc] )) || alias gxc='git-cli commit'
(( $+aliases[gxb] )) || alias gxb='git-cli branch'
(( $+aliases[gxi] )) || alias gxi='git-cli ci'
(( $+aliases[gxo] )) || alias gxo='git-cli open'
(( $+aliases[gxw] )) || alias gxw='git-cli worktree'

(( $+aliases[gxuz] )) || alias gxuz='git-cli utils zip'
(( $+aliases[gxuc] )) || alias gxuc='git-cli utils copy-staged'
if (( ! $+functions[gxur] )); then
  gxur() { eval "$(git-cli utils root --shell)"; }
fi
(( $+aliases[gxuh] )) || alias gxuh='git-cli utils commit-hash'

(( $+aliases[gxrs] )) || alias gxrs='git-cli reset soft'
(( $+aliases[gxrm] )) || alias gxrm='git-cli reset mixed'
(( $+aliases[gxrh] )) || alias gxrh='git-cli reset hard'
(( $+aliases[gxru] )) || alias gxru='git-cli reset undo'
(( $+aliases[gxrbh] )) || alias gxrbh='git-cli reset back-head'
(( $+aliases[gxrbc] )) || alias gxrbc='git-cli reset back-checkout'
(( $+aliases[gxrr] )) || alias gxrr='git-cli reset remote'

(( $+aliases[gxcc] )) || alias gxcc='git-cli commit context'
(( $+aliases[gxcj] )) || alias gxcj='git-cli commit context-json'
(( $+aliases[gxcs] )) || alias gxcs='git-cli commit to-stash'

(( $+aliases[gxbc] )) || alias gxbc='git-cli branch cleanup'

(( $+aliases[gxip] )) || alias gxip='git-cli ci pick'

(( $+aliases[gxor] )) || alias gxor='git-cli open repo'
(( $+aliases[gxob] )) || alias gxob='git-cli open branch'
(( $+aliases[gxod] )) || alias gxod='git-cli open default-branch'
(( $+aliases[gxoc] )) || alias gxoc='git-cli open commit'
(( $+aliases[gxocp] )) || alias gxocp='git-cli open compare'
(( $+aliases[gxop] )) || alias gxop='git-cli open pr'
(( $+aliases[gxopl] )) || alias gxopl='git-cli open pulls'
(( $+aliases[gxoi] )) || alias gxoi='git-cli open issues'
(( $+aliases[gxoa] )) || alias gxoa='git-cli open actions'
(( $+aliases[gxorl] )) || alias gxorl='git-cli open releases'
(( $+aliases[gxot] )) || alias gxot='git-cli open tags'
(( $+aliases[gxocs] )) || alias gxocs='git-cli open commits'
(( $+aliases[gxof] )) || alias gxof='git-cli open file'
(( $+aliases[gxobl] )) || alias gxobl='git-cli open blame'

# Jump into a worktree (uses eval to change the parent shell's directory).
# Worktree-name completion for `gxwcd` is provided by the autoloaded
# `completions/zsh/_gxwcd`, so it registers via compinit regardless of whether
# this alias file is sourced before or after `compinit` runs. Agents never
# reach the completion path.
if (( ! $+functions[gxwcd] )); then
  gxwcd() { eval "$(git-cli worktree go --shell "$@")"; }
fi

# ---------------------------------------------------------------------------
# codex-cli (cx*)
# ---------------------------------------------------------------------------
(( $+aliases[cx] )) || alias cx='codex-cli'

(( $+aliases[cxau] )) || alias cxau='codex-cli auth use'
(( $+aliases[cxar] )) || alias cxar='codex-cli auth refresh'
(( $+aliases[cxaa] )) || alias cxaa='codex-cli auth auto-refresh'
(( $+aliases[cxac] )) || alias cxac='codex-cli auth current'
(( $+aliases[cxas] )) || alias cxas='codex-cli auth sync'

(( $+aliases[cxdr] )) || alias cxdr='codex-cli diag rate-limits'
(( $+aliases[cxdra] )) || alias cxdra='codex-cli diag rate-limits --async'

(( $+aliases[cxcs] )) || alias cxcs='codex-cli config show'
(( $+aliases[cxct] )) || alias cxct='codex-cli config set'

(( $+aliases[cxgp] )) || alias cxgp='codex-cli agent prompt'
(( $+aliases[cxga] )) || alias cxga='codex-cli agent advice'
(( $+aliases[cxgk] )) || alias cxgk='codex-cli agent knowledge'
(( $+aliases[cxgc] )) || alias cxgc='codex-cli agent commit'

(( $+aliases[cxps] )) || alias cxps='codex-cli prompt-segment'

# ---------------------------------------------------------------------------
# fzf-cli (fx*)
# ---------------------------------------------------------------------------
(( $+aliases[fx] )) || alias fx='fzf-cli'
(( $+aliases[fxf] )) || alias fxf='fzf-cli file'

# These use eval to preserve parent-shell effects:
if (( ! $+functions[fxd] )); then
  fxd() { eval "$(fzf-cli directory -- "$@")"; }
fi
if (( ! $+functions[fxh] )); then
  fxh() { eval "$(fzf-cli history -- "$@")"; }
fi

(( $+aliases[fxgs] )) || alias fxgs='fzf-cli git-status'
(( $+aliases[fxgc] )) || alias fxgc='fzf-cli git-commit'
(( $+aliases[fxgco] )) || alias fxgco='fzf-cli git-checkout'
(( $+aliases[fxgb] )) || alias fxgb='fzf-cli git-branch'
(( $+aliases[fxgt] )) || alias fxgt='fzf-cli git-tag'
(( $+aliases[fxp] )) || alias fxp='fzf-cli process'
(( $+aliases[fxpo] )) || alias fxpo='fzf-cli port'
(( $+aliases[fxenv] )) || alias fxenv='fzf-cli env'
(( $+aliases[fxal] )) || alias fxal='fzf-cli alias'
(( $+aliases[fxfn] )) || alias fxfn='fzf-cli function'
(( $+aliases[fxdef] )) || alias fxdef='fzf-cli def'
