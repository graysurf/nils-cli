# semantic-commit

## Overview

semantic-commit validates commit messages and commits staged changes. It can also emit staged
change context for message generation.

## Usage

```text
Usage:
  semantic-commit <command> [args]

Commands:
  staged-context  Print staged change context for commit message generation
  commit          Commit staged changes with a prepared commit message
  help            Display help message

Help:
  semantic-commit help
  semantic-commit --help
```

## Commands

### staged-context

- `staged-context [--format <bundle|json|patch>] [--json] [--repo <path>]`
- Output formats:
  - `bundle` (default): `commit-context.json` + `staged.patch`
  - `json`: only `commit-context.json`
  - `patch`: only `staged.patch`
- `--repo <path>` runs against a repository path without changing shell cwd.

### commit

- `commit [options]`
- Message sources:
  - `-m, --message <text>`
  - `-F, --message-file <path>`
  - stdin (disabled with `--automation`)
- Useful options:
  - `--summary <git-scope|git-show|none>` (default: `git-scope` with fallback to `git-show`)
  - `--no-summary`
  - `--validate-only`
  - `--dry-run`
  - `--message-out <path>`
  - `--repo <path>`
  - `--no-progress`
  - `--quiet`

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
