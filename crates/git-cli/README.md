# git-cli

## Overview

git-cli is a Rust CLI that groups Git workflow helpers behind a dispatcher. It exposes nine
command groups (`utils`, `reset`, `commit`, `branch`, `worktree`, `ci`, `open`, `summary`,
`completion`) with
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
- `worktree`: `add`, `list`, `remove`, `prune`, `go`, `dirty-snapshot`, `adopt-dirty`,
  `revoke-dirty`.
- `ci`: `pick`.
- `open`: `repo`, `branch`, `default-branch` (alias `default`), `commit`, `compare`,
  `pr` (aliases `pull-request`, `mr`, `merge-request`), `pulls` (aliases `prs`,
  `merge-requests`, `mrs`), `issues` (alias `issue`), `actions` (alias `action`),
  `releases` (alias `release`), `tags` (alias `tag`), `commits` (alias `history`),
  `file` (alias `blob`), `blame`.
- `summary`: `all`, `today`, `yesterday`, `this-month`, `last-month`, `this-week`, `last-week`,
  or a custom `<from> <to>` range. This delegates to the library behind the standalone
  `git-summary` binary and supports `--format text|json` plus `--no-mailmap`.
- `completion`: `bash`, `zsh` (writes the completion registration to stdout). git-cli uses
  `clap_complete` `CompleteEnv` dynamic completion, so this emits a thin registration stub that
  computes candidates (e.g. live worktree names) at TAB time rather than a static script.

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
  `$AGENT_HOME/worktrees/<repo-key>/<branch-slug>` on a fresh `<prefix>/<branch-slug>` branch.
  Options: `--from <ref>`, `--kind feature|bug|chore|docs|ci|refactor` (selects the branch prefix;
  default `feature`), `--format text|json`.
- `list`: List all linked git worktrees and mark entries managed by the git-cli convention.
  Options: `--format text|json`.
- `remove <slug-or-path>`: Remove a linked worktree by managed slug or explicit path, refusing the
  primary checkout and the current worktree. Options: `--format text|json`.
- `prune`: Run `git worktree prune`. Options: `--format text|json`.
- `go <slug-or-branch-or-path>`: Resolve a worktree and print its path so you can `cd` into it.
  Matches by branch name, explicit path, managed slug, or worktree directory name. `go` and `remove`
  complete live worktree names/branches via `clap_complete` dynamic completion (superseding the
  static `gxwcd` completion workaround, though `gxwcd` remains for cd-on-select ergonomics). Use
  `--shell` to emit an evaluable `cd -- <path>` command (mirrors `utils root --shell`). Options:
  `--shell`, `--format text|json`.
- `dirty-snapshot`: Compute a bounded, two-pass identity for the current dirty checkout without
  printing raw paths or file content. The identity binds the physical checkout instance, `HEAD`,
  symbolic branch, raw index stages, unstaged content, and untracked regular-file/symlink state.
  Clean checkouts, active Git operations, unmerged stages, dirty submodules, escaping symlinks,
  special filesystem objects, and resource-limit overflow fail closed. Options: `--format
  text|json`.
- `adopt-dirty`: Exchange one unexpired runtime-issued challenge for an opaque adoption receipt.
  Requires `AGENT_RUNTIME_DIRTY_CHECKOUT_ADOPTION=1` plus `--challenge <token>` and
  `--reason-file <path>`. The reason must be a non-empty, no-follow regular file of at most 2,000
  bytes. The command recomputes the exact snapshot under the shared lease lock, consumes the
  challenge once, rejects live foreign ownership, and writes private receipt and lease-v2 state.
  Retained state contains digests, not the bearer token or reason text. Options: `--format
  text|json`.
- `revoke-dirty`: Revoke only the active adoption matching `--receipt <id>` without changing Git
  content. Revocation remains available when the adoption feature gate is disabled. Options:
  `--format text|json`.

`dirty-snapshot`, `adopt-dirty`, and `revoke-dirty` use the standard versioned JSON envelope in
`--format json` mode (`cli.git-cli.worktree.<command>.v1`). Snapshot output contains opaque keys and
hashes rather than raw local paths; adoption output returns only `receipt_id` and `snapshot_id`.
Operational failures use the same envelope and nonzero exit status.

Process containment is generation-bound: live tracking authority protects descendants it discovers
or adopts. If every tracker is lost abnormally before a descendant is observed, there is no
termination guarantee for that unseen descendant. Missing authenticated cleanup proof fails with
`dirty-checkout-resource-unavailable`; post-loss fallback does not signal an unpinned PID or PGID.

The managed layout (`repo-key`, the managed/external classification, and slug-based resolution) is
anchored to the repository's primary worktree, so `worktree` commands behave identically whether run
from the primary checkout or from inside any linked worktree.

### push

- `push`: Publish the checked-out branch to its own branch on the remote, using the fully
  qualified refspec `refs/heads/<branch>:refs/heads/<branch>` so the destination cannot depend on
  `push.default`, `remote.pushDefault`, or configured push refspecs. Sets the upstream on first
  publish. Refuses the remote's default branch (`refuse-default-branch`), a detached HEAD, and an
  unresolvable remote default (`default-branch-unresolved`).
  Options: `--remote <name>`, `--expect-default <branch>`, `--force-with-lease`, `--dry-run`,
  `--format text|json`.

### sync-default

- `sync-default`: Fast-forward the local default branch to its remote-tracking ref, and nothing
  else. Uses `git merge --ff-only` when the default branch is checked out here, and a
  compare-and-swap `git update-ref` when no worktree holds it. Refuses divergence
  (`not-fast-forward`), a dirty checkout, and a default branch held by another worktree.
  Options: `--remote <name>`, `--no-fetch`, `--dry-run`, `--format text|json`.

See the [remote surfaces spec](docs/specs/git-cli-remote-surfaces.md) for the full contract.

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

### summary

- `all|today|yesterday|this-month|last-month|this-week|last-week`: Summarize non-merge code
  contributions by canonical author.
- `<from> <to>`: Summarize a custom `YYYY-MM-DD` date range.
- `--format text|json`: Render the table or the `cli.git-summary.summary.v1` JSON envelope.
- `--no-mailmap`: Show the raw author identities stored in commit objects.

The same behavior remains directly available through `git-summary`; both entrypoints call the same
Rust implementation rather than spawning another binary.

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
- [Remote surfaces spec](docs/specs/git-cli-remote-surfaces.md)
