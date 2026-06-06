# fzf-cli

## Overview

fzf-cli is a Rust CLI that provides interactive pickers for files, Git metadata, processes, ports,
shell history, and shell definitions, all powered by `fzf`.

## Usage

```text
Usage:
  fzf-cli <command> [args]

Commands:
  file              Search and preview text files
  directory         Search directories and cd into selection
  git-status        Interactive git status viewer
  git-commit        Browse commits and open changed files in editor
  git-checkout      Pick and checkout a previous commit
  git-branch        Browse and checkout branches interactively
  git-tag           Browse and checkout tags interactively
  process           Browse and kill running processes (confirm before kill)
  port              Browse listening ports and owners (confirm before kill)
  kill-process      Kill one or more process IDs without launching fzf
  kill-port         Kill process owners for listening ports without launching fzf
  history           Search and execute command history
  env               Browse environment variables
  alias             Browse shell aliases
  function          Browse defined shell functions
  def               Browse all definitions (env, alias, functions)
  open-changed-files Open changed files in VS Code
  completion        Export shell completion script

Help:
  fzf-cli help
  fzf-cli --help
  fzf-cli <command> --help
```

## Commands

### file

- `file [--vi|--vscode] [-- <query...>]`: Search files (`bat` preview) and open the selection.

### directory

- `directory [--vi|--vscode] [-- <query...>]`: Pick a directory, then pick a file to open (`bat`
  preview when available, falls back to `sed`). Use `ctrl-d` to emit `cd <path>` to stdout, `esc`
  to step back to the directory picker.

### git-status

- `git-status [query...]`: Interactive `git status -s` viewer with diff previews (`delta` when
  available, falls back to `git diff`).

### git-commit

- `git-commit [--snapshot] [query...]`: Browse commits, then pick changed files. Default action
  opens the worktree files in the configured editor (capped by `OPEN_CHANGED_FILES_MAX_FILES`,
  default `5`); `--snapshot` flips the default to opening file snapshots from the selected commit.
  `ctrl-o` opens the highlighted file from the worktree regardless of `--snapshot`.

### git-checkout

- `git-checkout [query...]`: Pick a commit and checkout (prompts before checkout).

### git-branch

- `git-branch [query...]`: Browse branches and checkout (prompts before checkout).

### git-tag

- `git-tag [query...]`: Browse tags and checkout (prompts before checkout).

### process

- `process [-k|--kill] [-9|--force] [query...]`: Browse processes and optionally kill selected PIDs
  (confirm before kill). `-9`/`--force` upgrades the signal to SIGKILL.

### port

- `port [-k|--kill] [-9|--force] [query...]`: Browse listening ports and optionally kill owning
  PIDs (confirm before kill). Uses `lsof` when available; falls back to `netstat` (no PID column
  in the fallback view).

### kill-process

- `kill-process [-9|--force] <pid> [pid...]`: Kill one or more explicit PIDs without opening
  `fzf`. This is the native backend for direct shell aliases such as `kill-process` / `kpid`.

### kill-port

- `kill-port [-9|--force] <port> [port...]`: Resolve listening TCP/UDP owners with `lsof -t`
  and kill the owning PIDs without opening `fzf`. This is the native backend for direct shell
  aliases such as `kill-port` / `kp`.

### history

- `history [query...]`: Search shell history and print the selected command to stdout.

### env

- `env [query...]`: Browse environment variables.

### alias

- `alias [query...]`: Browse shell aliases.

### function

- `function [query...]`: Browse shell functions.

### def

- `def [query...]`: Browse env, alias, and function definitions.

### open-changed-files

- `open-changed-files [--list|--git] [--workspace-mode pwd|git] [--dry-run] [--verbose]
  [--max-files N] [--] [files...]`: Open explicit files or staged/unstaged/untracked Git files in
  VS Code. The command no-ops cleanly when `code` is disabled or unavailable.

### completion

- `completion <bash|zsh>`: Print the shell completion script for the requested shell.

## Environment

- `FZF_FILE_OPEN_WITH`: Default opener for `file`, `directory`, `git-commit` (`vi` or `vscode`).
- `FZF_FILE_MAX_DEPTH`: Max directory depth for `file` and `directory` (default: `10`).
- `FZF_PREVIEW_WINDOW`: Preview window layout for `directory` file picker (default:
  `right:50%:wrap`).
- `OPEN_CHANGED_FILES_MAX_FILES`: Max number of worktree files `git-commit` opens at once
  (default: `5`).
- `OPEN_CHANGED_FILES_SOURCE`: Default source for `open-changed-files` (`list` or `git`; default:
  `list`).
- `OPEN_CHANGED_FILES_WORKSPACE_MODE`: Workspace grouping for `open-changed-files` (`pwd` or `git`;
  default: `pwd`).
- `OPEN_CHANGED_FILES_CODE_PATH`: VS Code CLI override for `open-changed-files` (`auto`, `none`, or
  a command/path; default: `auto`).
- `FZF_DEF_DELIM` and `FZF_DEF_DELIM_END`: Required delimiters for `env`, `alias`, `function`,
  `def`.
- `FZF_DEF_DOC_CACHE_ENABLED`: Enable definition doc caching.
- `FZF_DEF_DOC_CACHE_EXPIRE_MINUTES`: Cache TTL in minutes (default: `10`).
- `FZF_DEF_DOC_SEPARATOR_PAD`: Padding lines between definition docs (default: `2`).

## Dependencies

- `fzf` is required for all commands.
- `git` is required for `git-*` commands.
- `bat` is required for the `file` preview and is the preferred `directory` preview (the
  `directory` picker degrades to `sed` when `bat` is missing).
- `lsof` is the preferred backend for `port` and is required for `kill-port`; `netstat` is the
  fallback for the interactive `port` view when `lsof` is missing.
- `code` is used for `--vscode` (and the `FZF_FILE_OPEN_WITH=vscode` default) and
  `open-changed-files`; picker commands fall back to `vi` if `code` is unavailable or fails, while
  `open-changed-files` no-ops cleanly.
- See the workspace [`BINARY_DEPENDENCIES.md`](../../BINARY_DEPENDENCIES.md) for the canonical
  external-tool matrix.

## Docs

- [Docs index](docs/README.md)
