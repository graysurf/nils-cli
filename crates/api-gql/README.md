# api-gql

## Overview

api-gql executes GraphQL operations (and optional variables), prints response JSON to stdout,
keeps optional history, and can generate Markdown reports.

## Usage

```text
Usage: api-gql <command> [args]

Commands:
  call             Execute an operation and print response JSON (default)
  history          Print the last (or last N) history entries
  report           Generate a Markdown API test report
  report-from-cmd  Generate a report from a command snippet (arg or stdin)
  schema           Resolve a schema file path (or print schema contents)
  completion       Print shell completion script (bash | zsh)

Help:
  api-gql --help
  api-gql <command> --help    # call | history | report | report-from-cmd | schema | completion
```

## Commands

- `call` (default): Execute an operation (and optional variables) and print response JSON.
  Positional: `[operation.graphql]` (path to a `*.graphql` file), `[variables.json]` (optional).
  Options: `-e/--env <name>`, `-u/--url <url>`, `--jwt <name>`, `--config-dir <dir>`,
  `--list-envs`, `--list-jwts`, `--no-history`.
- `history`: Print the last entry or tail N entries.
  Options: `--config-dir <dir>`, `--file <path>`, `--last`, `--tail <n>`, `--command-only`.
- `report`: Generate a Markdown report for an operation.
  Required: `--case <name>`, `--op <file>`. Exactly one of `--run` or `--response <file|->`.
  Options: `--vars <file>`, `--out <path>`, `-e/--env <name>`, `-u/--url <url>`, `--jwt <name>`,
  `--allow-empty`, `--no-redact`, `--no-command`, `--no-command-url`,
  `--project-root <path>`, `--config-dir <dir>`. Aliases: `--operation` for `--op`,
  `--variables` for `--vars`, `--expect-empty` for `--allow-empty`.
- `report-from-cmd`: Generate a report from a command snippet.
  Positional: `[snippet]` (or pass `--stdin` to read from stdin).
  Options: `--case <name>`, `--out <path>`, `--response <file|->`, `--allow-empty`, `--dry-run`,
  `--stdin`.
- `schema`: Resolve a schema file path (or print schema contents).
  Options: `--config-dir <dir>`, `--file <path>` (overrides env + `schema.env`), `--cat`.
- `completion`: Print shell completion script for `bash` or `zsh`.

## Operation and variables files

- The first positional arg is an operation file path; conventional extension is `*.graphql`
  (the runtime treats the whole file as the operation body and does not enforce the suffix).
- The second positional arg is an optional variables file; it must be valid JSON. Numeric
  fields named `limit` are bumped up to `GQL_VARS_MIN_LIMIT` (default `5`) at load time.

## Schema resolution

`api-gql schema` resolves the schema file using this order:

1. `--file <path>` (CLI flag).
2. `GQL_SCHEMA_FILE` environment variable.
3. `GQL_SCHEMA_FILE` declared in `<setup_dir>/schema.env` or `schema.local.env` (last-wins).
4. Fallback filenames inside the setup dir: `schema.gql`, `schema.graphql`, `schema.graphqls`,
   `api.graphql`, `api.gql`.

By default the resolved path is printed; pass `--cat` to print the file contents instead.

## Examples

The following snippets are illustrative — replace paths and env names to match your setup.

```bash
# List endpoint presets discovered under setup/graphql/
api-gql call --list-envs

# Execute an operation against a preset, with variables
api-gql call --env staging operations/users.graphql operations/users.vars.json

# Generate a Markdown report by running the request now
api-gql report --case users-list --op operations/users.graphql \
  --vars operations/users.vars.json --env staging --run

# Generate a report from a previously captured response
api-gql report --case users-list --op operations/users.graphql \
  --response captured/users.json

# Replay a history snippet (e.g. last entry) into a report
api-gql history --last --command-only | api-gql report-from-cmd --stdin --response -

# Resolve and print the schema file path
api-gql schema --config-dir setup/graphql
```

## Auth selection

- `--jwt <name>` or `GQL_JWT_NAME` selects `GQL_JWT_<NAME>` from the setup `jwts.env`/`.local` files.
- If no JWT profile is selected, fallback uses `ACCESS_TOKEN` then `SERVICE_TOKEN`.
- History entries record the env source as `token=ACCESS_TOKEN` or `token=SERVICE_TOKEN`.

## Docs

- [Docs index](docs/README.md)
