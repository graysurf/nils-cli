# git-scope

## Overview

git-scope is a Git status/commit inspection CLI that renders a change list and directory tree, with
optional file-content printing for worktree, index, or commits. Output is colorized by default; use
--no-color or NO_COLOR to disable color.

## Usage

```text
Usage:
  git-scope <command> [args]

Commands:
  tracked [prefix...]    Show files tracked by Git (prefix filter optional)
  staged                 Show files staged for commit
  unstaged               Show modified files not yet staged
  all                    Show all changes (staged and unstaged)
  untracked              Show untracked files
  commit <id>            Show commit details (use -p to print content)
  completion <shell>     Export shell completion script (bash, zsh)
  help                   Show help

Options:
  --no-color             Disable ANSI colors (also via NO_COLOR)
  -V, --version          Show version
```

## Commands

- `tracked [prefix...]`: List tracked files. Use `-p, --print` to emit worktree contents.
- `staged`: List staged changes. Use `-p, --print` to emit index contents.
- `unstaged`: List unstaged changes. Use `-p, --print` to emit worktree contents.
- `all`: List staged + unstaged changes. Use `-p, --print` to emit index and worktree contents when
  both exist.
- `untracked`: List untracked files. Use `-p, --print` to emit worktree contents.
- `commit <commit-ish>`: Show commit metadata and file list. Options: `-p, --print` and
  `-P, --parent <n>` for merge commits.
- `completion <shell>`: Print a shell completion script for `bash` or `zsh`.
- `help`: Show help output.

## Exit codes

- `0`: Success and help output.
- `1`: Operational errors.

## Dependencies

- `git` is required for all commands.
- `tree` is optional; missing or unsupported versions emit a warning and skip tree output.
- `file` is optional; when unavailable, binary detection falls back to content inspection.

## Environment

- `NO_COLOR`: Disable ANSI colors.
- `GIT_SCOPE_PROGRESS`: Set to `1`, `true`, `yes`, or `on` to enable a progress bar while printing
  file contents.

## Docs

- [Docs index](docs/README.md)
