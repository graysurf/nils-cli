# git-lock

## Overview

git-lock is a Rust CLI that saves a Git commit hash under a label and lets you reset, list, copy,
delete, diff, or tag a stored commit. It operates per repository (keyed by the repository folder
name), prompts before destructive actions, and exposes a label-based dispatcher with consistent
help / version handling: `git-lock help` (or `-h`/`--help`) prints top-level usage, and
`git-lock -V`/`--version` prints the binary version.

## Usage

Invoke as `git-lock <command> [args]`. The dispatcher recognizes these subcommands (matching the
binary's `--help` output):

```text
Usage: git-lock <command> [args]

Commands:
  lock [label] [note] [commit]  Save commit hash to lock
  unlock [label]                Reset to a saved commit
  list                          Show all locks for repo
  copy <from> <to>              Duplicate a lock label
  delete [label]                Remove a lock
  diff <l1> <l2> [--no-color]   Compare commits between two locks
  tag <label> <tag> [-m msg]    Create git tag from a lock
  completion <shell>            Export shell completion script
  -V, --version                 Show version
```

## Commands

- `lock [label] [note] [commit]`: Save a commit hash under a label and mark it as the latest label
  for the repository. Defaults: label `default`, commit `HEAD`. The lock file also records a
  `timestamp=` line and the optional note.
- `unlock [label]`: Hard reset to a locked commit. If `label` is omitted, the most recent label
  recorded for this repository is used.
- `list`: List locks for the current repository (newest first by recorded timestamp). The latest
  label is marked with a star.
- `copy <from> <to>`: Duplicate a lock label. If `from` is omitted, the latest label is used.
  Prompts before overwriting an existing target.
- `delete [label]`: Delete a lock label. If `label` is omitted, the latest label is used. Prompts
  before deletion and clears the latest pointer when it matched.
- `diff <label1> <label2> [--no-color]`: Show `git log --oneline --graph --decorate` between two
  locked commits. Honors `NO_COLOR` (or `--no-color`).
- `tag <label> <tag> [-m <msg>] [--push]`: Create an annotated tag at the locked commit. Without
  `-m`, the commit subject is used as the tag message. Use `--push` to push the tag to `origin`
  and then delete the local tag. Prompts before overwriting an existing tag.
- `completion <shell>`: Print a clap-generated completion script for `bash` or `zsh` to stdout
  (suitable for `source <(git-lock completion zsh)`).
- `help`: Show help output.

## Lockfile layout

Locks are flat files stored in a single directory per machine; the repository name is encoded in
the filename so multiple repositories share the same store without collision.

- Lock file: `<lock_dir>/<repo-id>-<label>.lock`
- Latest pointer: `<lock_dir>/<repo-id>-latest` (contains the most recently written label)
- `<repo-id>` is the basename of the repository's `git rev-parse --show-toplevel` output
  (for example, the directory name `nils-cli`).
- `<lock_dir>` is resolved from the `ZSH_CACHE_DIR` environment variable. When set, the store
  lives at `$ZSH_CACHE_DIR/git-locks`; when unset, it defaults to `/git-locks`.

Example with `ZSH_CACHE_DIR=$HOME/.cache/zsh` and a repository checked out at `~/code/nils-cli`:

```text
$HOME/.cache/zsh/git-locks/nils-cli-default.lock
$HOME/.cache/zsh/git-locks/nils-cli-latest
```

Each `.lock` file is a two-line text record of the form `<hash> # <note>` followed by
`timestamp=<YYYY-MM-DD HH:MM:SS>`.

## Exit codes

- `0`: Success and help output.
- `1`: Operational errors, missing labels/locks, or aborted confirmations.

## Dependencies

- `git` is required for all commands.
- The CLI refuses to run inside a non-Git directory (except for `help`, `--version`, and
  `completion`).

## Environment

- `ZSH_CACHE_DIR`: Base directory for lock storage. Locks are stored under
  `$ZSH_CACHE_DIR/git-locks`. If unset, defaults to `/git-locks`.
- `NO_COLOR`: Disable color for `diff` output (equivalent to passing `--no-color`).

## Docs

- [Docs index](docs/README.md)
