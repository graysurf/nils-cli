# git-summary

## Overview

git-summary prints per-author contribution summaries for a date range, sorted by net contribution
(descending) and then by author. Each row reports added lines, deleted lines, net change, commit
count, and first/last commit dates. Date ranges use local-time boundaries (the active timezone
offset is appended to `--since`/`--until`), and merge commits are excluded via `--no-merges`.
Author identities honor Git's `.mailmap`, `mailmap.file`, and `mailmap.blob` configuration by
default.

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
      --format <FORMAT>  Output format [default: text] [possible values: text, json]
      --no-mailmap       Show raw commit identities instead of canonical mailmap identities
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
- `--format text|json`: Select the human-readable table or the versioned JSON envelope.
- `--no-mailmap`: Disable canonical identity mapping for a raw author audit.

## Output columns

The summary table emits the following columns (in this order):

| Column    | Description                                                 |
| --------- | ----------------------------------------------------------- |
| `Name`    | Canonical commit author name.                               |
| `Email`   | Canonical commit author email (truncated to 40 characters). |
| `Added`   | Total added lines across non-merge commits in the range.    |
| `Deleted` | Total deleted lines across non-merge commits in the range.  |
| `Net`     | `Added - Deleted`; rows are sorted by this column desc.     |
| `Commits` | Number of non-merge commits attributed to the author.       |
| `First`   | Earliest commit date for the author in the range.           |
| `Last`    | Latest commit date for the author in the range.             |

Lockfile changes (`yarn.lock`, `package-lock.json`, `pnpm-lock.yaml`, any `*.lock`) are excluded
from `Added`/`Deleted`/`Net` totals. Binary file diffs are counted as zero.

Authors with no counted code changes (both `Added` and `Deleted` are `0`) are omitted from the
table rather than shown as a `0/0/0` row. This drops lockfile-only and binary-only authors — even
when they have commits in the range — so the report lists only authors who actually changed code.

Example header (real output):

```text
Name                      Email                                       Added  Deleted      Net  Commits        First         Last
----------------------------------------------------------------------------------------------------------------------------------------
```

## Mailmap identity aggregation

By default, `git-summary` uses Git's mailmap-aware `%aN` and `%aE` author fields and aggregates all
mapped aliases into one row. A repository can provide `.mailmap`; a personal cross-repository
mapping can be configured with `mailmap.file`.

```text
Canonical Name <canonical@example.com> <old@example.com>
```

Mailmap changes only reporting. It does not rewrite commit objects or change commit SHAs.

## JSON output

`--format json` emits one `cli.git-summary.summary.v1` envelope:

```json
{
  "schema_version": "cli.git-summary.summary.v1",
  "ok": true,
  "data": {
    "range": {
      "label": "this month: 2026-07-01 to 2026-07-24",
      "from": "2026-07-01",
      "to": "2026-07-24"
    },
    "mailmap": true,
    "authors": [
      {
        "name": "graysurf",
        "email": "graysurf@noreply.codeberg.org",
        "added": 97451,
        "deleted": 46347,
        "net": 51104,
        "commits": 232,
        "first": "2026-07-01",
        "last": "2026-07-24"
      }
    ]
  }
}
```

`git-cli summary ...` delegates to the same library-backed command surface; the standalone
`git-summary` binary remains available as the focused wrapper CLI.

## Exit codes

- `0`: Success and help/version output.
- `1`: Git/runtime errors.
- `64`: Invalid command-line usage.
- `65`: Invalid date or date range.
- `70`: JSON serialization invariant failure.

## Dependencies

- `git` is required for all commands except `completion` and `help`.

## Docs

- [Docs index](docs/README.md)
