# semantic-commit

## Overview

semantic-commit validates commit messages and commits staged changes. It can also emit staged
change context for message generation.

## Usage

```text
Usage: semantic-commit <command> [args]

Commands:
  staged-context    Print staged change context for commit message generation
  commit            Commit staged changes with a prepared commit message
  completion        Export shell completion script
  help              Display help message
```

Use `semantic-commit --help` (or `semantic-commit <command> --help`) for CLI help text.

### Top-level options

- `-h`, `--help` — print help
- `-V`, `--version` — print version

## Commands and Flags

### `staged-context`

Print staged change context for commit message generation.

Flags:

- `--format <bundle|json|patch>` (default: `bundle`)
- `--json` — equivalent to `--format json`
- `--repo <path>` — run git commands against the repository path

Output formats:

- `bundle` (default): emits both `commit-context.json` and `staged.patch` to
  stdout, separated by `===== commit-context.json =====` and
  `===== staged.patch =====` banners.
- `json`: emits the `commit-context.json` payload only.
- `patch`: emits the `git diff --cached` patch only.

### `commit`

Commit staged changes with a prepared commit message.

Message sources (mutually exclusive):

- `-m`, `--message <text>` — inline commit message
- `-F`, `--message-file <path>` — read commit message from file
- stdin — used when no message flag is supplied and stdin is non-TTY;
  disabled by `--automation` / `--non-interactive`

Flags:

- `--message-out <path>` — write the resolved commit message to a file for recovery
- `--summary <git-scope|git-show|none>` (default: `git-scope` with fallback to `git-show`)
- `--no-summary` — equivalent to `--summary none`
- `--repo <path>` — run git commands against the repository path
- `--automation` (alias: `--non-interactive`) — disallow stdin message fallback
- `--validate-only` — validate the commit message format and exit without committing
- `--dry-run` — run validation and staged-change checks, then skip `git commit`
- `--auto-fix` — normalize the message before validation: wrap body lines at `<= 100` columns
  (whitespace breaks preferred; CJK / unbreakable runs use a codepoint hard-break), uppercase the
  first character of `-` + space bullets, lowercase the header type and `(scope)`, insert a missing
  blank line between header and body, and drop empty lines inside the body. Does **not** shorten
  an over-length header or repair structural header errors — those still exit `4`.
- `--no-progress` — disable the progress spinner
- `--quiet` — suppress progress and summary output (implies `--no-progress` and `--no-summary`)

### `completion`

Print a shell completion script to stdout. Pipe the output into your shell's
completion loader.

```text
semantic-commit completion <bash|zsh>
```

## Commit Message Validation

- Header must be non-empty, `<= 100` characters, and use a lowercase type.
- Header format: `type(scope): subject` or `type: subject`.
- If a body exists, line 2 must be blank and each body line must start with `-` + space, followed by an
  uppercase letter and be `<= 100` characters. A bullet may wrap onto a following line by prefixing that
  continuation line with two spaces (the same indent used on this README bullet); continuation lines
  also have the `<= 100` character cap.

## Exit codes

- `0`: success and help output.
- `1`: usage errors or operational errors.
- `2`: no staged changes.
- `3`: commit message missing/empty.
- `4`: commit message validation failed.
- `5`: required dependency missing (for example, `git`).

## Dependencies

- `git` is required.
- `git-scope` is optional; when unavailable, commit summary falls back to `git show -1`.

## Docs

- [Docs index](docs/README.md)
