# api-rest

## Overview

api-rest executes JSON-defined REST request files, prints response bodies to stdout, keeps
optional history, and can generate Markdown reports.

## Usage

```text
Usage: api-rest <command> [args]

Commands:
  call             Execute a request file and print the response body to stdout (default)
  history          Print the last (or last N) history entries
  report           Generate a Markdown API test report
  report-from-cmd  Generate a report from a saved `call` snippet
  completion       Print shell completion script

Common options (see subcommand help for full details):
  --config-dir <dir>   Seed setup/rest discovery (call/history/report)
  -h, --help           Print help

Examples:
  api-rest --help
  api-rest call --help
  api-rest report --help
  api-rest report-from-cmd --help
  api-rest completion zsh
```

## Commands

- `call` (default): Execute a request file and print the response body.
  Positional: `<request.request.json>`.
  Options: `-e/--env <name>`, `-u/--url <url>`, `--token <name>`, `--config-dir <dir>`,
  `--no-history`.
- `history`: Print the last entry or tail N entries.
  Options: `--config-dir <dir>`, `--file <path>`, `--last`, `--tail <n>`, `--command-only`.
- `report`: Generate a Markdown report for a request.
  Required: `--case <name>`, `--request <file>`.
  Options: `--out <path>`, `-e/--env <name>`, `-u/--url <url>`, `--token <name>`,
  `--run`, `--response <file|->`, `--no-redact`, `--no-command`, `--no-command-url`,
  `--project-root <path>`, `--config-dir <dir>`.
- `report-from-cmd`: Generate a Markdown report from a saved `call` command snippet.
  Positional: `[snippet]` (or pass via `--stdin`).
  Options: `--case <name>`, `--out <path>`, `--response <file|->`, `--allow-empty`,
  `--dry-run`, `--stdin`.
- `completion`: Print a shell completion script. Argument: `<SHELL>` (`bash` or `zsh`).

## Docs

- [Docs index](docs/README.md)
