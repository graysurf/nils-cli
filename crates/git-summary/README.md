# git-summary

## Overview

git-summary prints per-author contribution summaries for a date range, sorted by net contribution
(descending) and then by author. Each row reports added lines, deleted lines, net change, commit
count, and first/last commit dates. Date ranges use local-time boundaries (the active timezone
offset is appended to `--since`/`--until`), and merge commits are excluded via `--no-merges`.

## Usage

```text
Usage:
  git-summary <command> [args]
  git-summary <from> <to>

Commands:
  all                   Entire history
  today                 Today only
  yesterday             Yesterday only
  this-month            1st to today
  last-month            1st to end of last month
  this-week             This Mon-Sun
  last-week             Last Mon-Sun
  completion <shell>    Export shell completion script (bash, zsh)
  <from> <to>           Custom date range (YYYY-MM-DD)
  help                  Show help

Options:
  -h, --help            Show help (also `help`)
  -V, --version         Show version
```

## Commands

- `all`: Summarize the entire Git history.
- `today`: Summarize commits from today only.
- `yesterday`: Summarize commits from yesterday only.
- `this-month`: Summarize commits from the first of the month through today.
- `last-month`: Summarize commits from the entire previous month.
- `this-week`: Summarize commits for the current Monday-Sunday window.
- `last-week`: Summarize commits for the previous Monday-Sunday window.
- `completion <shell>`: Print a shell completion script for `bash` or `zsh`.
- `<from> <to>`: Summarize a custom date range (YYYY-MM-DD). Start must be on or before end.
- `help`: Show help output.

## Output columns

The summary table emits the following columns (in this order):

| Column    | Description                                                |
| --------- | ---------------------------------------------------------- |
| `Name`    | Commit author name (truncated for table alignment).        |
| `Email`   | Commit author email (truncated to 40 characters).          |
| `Added`   | Total added lines across non-merge commits in the range.   |
| `Deleted` | Total deleted lines across non-merge commits in the range. |
| `Net`     | `Added - Deleted`; rows are sorted by this column desc.    |
| `Commits` | Number of non-merge commits attributed to the author.      |
| `First`   | Earliest commit date for the author in the range.          |
| `Last`    | Latest commit date for the author in the range.            |

Lockfile changes (`yarn.lock`, `package-lock.json`, `pnpm-lock.yaml`, any `*.lock`) are excluded
from `Added`/`Deleted`/`Net` totals. Binary file diffs are counted as zero.

Example header (real output):

```text
Name                      Email                                       Added  Deleted      Net  Commits        First         Last
----------------------------------------------------------------------------------------------------------------------------------------
```

## Exit codes

- `0`: Success and help/version output.
- `1`: Validation errors (bad date format, reversed range, missing range pair), Git invocation
  errors, or invalid usage.

## Dependencies

- `git` is required for all commands except `completion` and `help`.

## Docs

- [Docs index](docs/README.md)
