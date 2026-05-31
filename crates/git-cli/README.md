# git-cli

## Overview

git-cli is a Rust CLI that groups Git workflow helpers behind a dispatcher. It exposes eight
command groups (`utils`, `reset`, `commit`, `branch`, `worktree`, `ci`, `open`, `completion`) with
consistent help / version handling: `git-cli help` (or `-h`/`--help`) prints top-level usage,
`git-cli <group> help` prints group usage, and `git-cli -V`/`--version` prints the binary version.

## Usage

Invoke as `git-cli <group> <command> [args]`. The dispatcher recognizes these groups and
subcommands (matching the binary's `--help` output):

- `utils`: `zip`, `copy-staged` (alias `copy`), `root`, `commit-hash` (alias `hash`).
- `reset`: `soft`, `mixed`, `hard`, `undo`, `back-head`, `back-checkout`, `remote`.
- `commit`: `context`, `context-json` (aliases `context_json`, `contextjson`, `json`),
  `to-stash` (alias `stash`).
- `branch`: `cleanup` (alias `delete-merged`).
- `worktree`: `add`, `list`, `remove`, `prune`.
- `ci`: `pick`.
- `open`: `repo`, `branch`, `default-branch` (alias `default`), `commit`, `compare`,
  `pr` (aliases `pull-request`, `mr`, `merge-request`), `pulls` (aliases `prs`,
  `merge-requests`, `mrs`), `issues` (alias `issue`), `actions` (alias `action`),
  `releases` (alias `release`), `tags` (alias `tag`), `commits` (alias `history`),
  `file` (alias `blob`), `blame`.
- `completion`: `bash`, `zsh` (writes a clap-generated completion script to stdout).

## Commands

### utils

- `zip`: Create `backup-<short-sha>.zip` from `HEAD` using `git archive`.
- `copy-staged` (`copy`): Copy staged diff to the clipboard. Use `--stdout` (`-p`/`--print`) to
  print, `--both` to print and copy.
- `root`: Print the repository root. Use `--shell` to output `cd -- <path>` for `eval`.
- `commit-hash` (`hash`): Resolve a ref to a commit SHA.

### reset

- `soft|mixed|hard [N]`: Rewind `HEAD` by N commits (default: 1) with confirmations and summaries.
- `undo`: Move `HEAD` back to the previous reflog entry with safety checks.
- `back-head`: Checkout `HEAD@{1}` (previous position).
- `back-checkout`: Checkout the previously checked-out branch (requires non-detached `HEAD`).
- `remote`: Reset the current branch to a remote-tracking ref.
  Options: `--ref <remote/branch>`, `--remote <name>`, `--branch <name>`, `--no-fetch`, `--prune`,
  `--set-upstream`, `--clean`, `-y/--yes`.

### commit

- `context`: Build a Markdown commit context from staged changes.
  Options: `--stdout` (`-p`/`--print`), `--both`, `--no-color` (or `NO_COLOR`),
  `--include <path/glob>` (repeatable).
- `context-json` (aliases `context_json`, `contextjson`, `json`): Write `commit-context.json` and
  `staged.patch` (default: `<git-dir>/commit-context`).
  Options: `--stdout`, `--both`, `--pretty`, `--bundle`, `--out-dir <path>`.
- `to-stash` (`stash`): Create a stash from a commit and optionally rewrite history via prompts.

### branch

- `cleanup` (`delete-merged`): Delete merged local branches.
  Options: `-b/--base <ref>`, `-s/--squash`, `-w/--remove-worktrees`.

### worktree

- `add <slug>`: Create a managed worktree under
  `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>` on a fresh `feat/<branch-slug>` branch.
  Options: `--from <ref>`, `--format text|json`.
- `list`: List all linked git worktrees and mark entries managed by the git-cli convention.
  Options: `--format text|json`.
- `remove <slug-or-path>`: Remove a linked worktree by managed slug or explicit path, refusing the
  primary checkout and the current worktree. Options: `--format text|json`.
- `prune`: Run `git worktree prune`. Options: `--format text|json`.

### ci

- `pick`: Create and push a `ci/<target>/<name>` branch with cherry-picked commits.
  Options: `-r/--remote <name>`, `--no-fetch`, `-f/--force`, `--stay`.

### open

- `repo [remote]`: Open repository homepage.
- `branch [ref]`: Open tree page for a ref (default: upstream branch).
- `default-branch [remote]` (`default`): Open default branch tree page.
- `commit [ref]`: Open commit page (default: `HEAD`).
- `compare [base] [head]`: Open compare page.
- `pr [number]` (`pull-request`, `mr`, `merge-request`): Open PR/MR page or create/view
  current-branch PR. On GitHub remotes, prefers `gh pr view --web` when available.
- `pulls [number]` (`prs`, `merge-requests`, `mrs`): Open PR/MR list or specific PR/MR.
- `issues [number]` (`issue`): Open issue list or specific issue.
- `actions [workflow]` (`action`): Open GitHub Actions page (GitHub only). Workflow may be a file
  name (`ci.yml`/`ci.yaml`) for the workflow page, or any other token for an actions search.
- `releases [tag]` (`release`): Open releases list or specific release tag.
- `tags [tag]` (`tag`): Open tags list or specific release tag.
- `commits [ref]` (`history`): Open commits history page.
- `file <path> [ref]` (`blob`): Open file blob page.
- `blame <path> [ref]`: Open blame page.
- `GIT_OPEN_COLLAB_REMOTE` can override the remote used for collaboration pages
  (`pr`/`pulls`/`issues`/`actions`/`releases`/`tags`).

### completion

- `bash` / `zsh`: Print a clap-generated shell completion script to stdout (suitable for
  `source <(git-cli completion zsh)`). Any extra arguments are rejected.

## Shell aliases (optional)

- Zsh aliases live in `completions/zsh/aliases.zsh`.
- Bash aliases live in `completions/bash/aliases.bash`.
- `gxur` should be implemented via: `eval "$(git-cli utils root --shell)"`.

## Exit codes

- `0`: Success and help output.
- `1`: Operational errors or aborted confirmations.
- `2`: Usage/parse errors.

## Dependencies

- `git` is required for all commands.
- `git-scope` is required for `commit context`.
- Clipboard integration via `nils-common::clipboard` probes `pbcopy`, `wl-copy`, `xclip`, then
  `xsel` (in that order) for `commit context`, `utils copy-staged`, and any other clipboard
  consumer. Missing or failing clipboard tools emit a warning and the command continues. See the
  workspace [`BINARY_DEPENDENCIES.md`](../../BINARY_DEPENDENCIES.md) for install hints.
- `gh` is preferred for GitHub PR pages (`open pr`); without it the CLI falls back to the
  GitHub compare URL.
- `file` is optionally used for MIME-based binary detection in `commit context`.

## Docs

- [Docs index](docs/README.md)
- [Worktree convention spec](docs/specs/git-cli-worktree-convention.md)
